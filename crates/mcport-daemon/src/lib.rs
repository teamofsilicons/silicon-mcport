//! The local host connector only executes endpoints present in its own registry.
//! Jobs never contain executable commands, URLs, or upstream credentials.
mod assets;
mod health;

use mcport_api::{Client, HostContext};
use mcport_core::{Actor, ApiError, HostJob, HostJobResult, HostPoll, HostPollResult};
use mcport_mcp::{Endpoint, ExecutionOptions, McpSession, NetworkPolicy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("Local connector storage error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Local connector configuration is invalid: {0}")]
    Config(String),
    #[error("Host connector authentication was revoked or rejected. Register this host again.")]
    Unauthorized,
    #[error("The connector is already running for this registry.")]
    AlreadyRunning,
}

/// The registry format this release writes. Version 2 keys the owner and personal
/// provider accounts by Silicon Accounts uuid. Version 1 (written before 0.3.0) keyed
/// them by the old ids; it still loads and runs until `mcport host migrate` rewrites it.
pub const REGISTRY_VERSION: u32 = 2;

/// The host a registry serves.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub backend_url: String,
    pub host_id: String,
    pub host_token: String,
    /// Version 2: the owner's Silicon Accounts uuid.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner_uuid: String,
    /// Version 1 only: the pre-0.3.0 environment, grouping value and owner id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub environment: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub org_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner_id: String,
    #[serde(default)]
    pub isi: Option<String>,
}
impl HostConfig {
    /// A version 2 host registered by `owner_uuid`.
    pub fn new(
        backend_url: impl Into<String>,
        host_id: impl Into<String>,
        host_token: impl Into<String>,
        owner_uuid: impl Into<String>,
        isi: Option<String>,
    ) -> Self {
        Self {
            backend_url: backend_url.into(),
            host_id: host_id.into(),
            host_token: host_token.into(),
            owner_uuid: owner_uuid.into(),
            environment: String::new(),
            org_id: String::new(),
            owner_id: String::new(),
            isi,
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAccount {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredConnection {
    pub endpoint: Endpoint,
    pub auth_mode: String,
    #[serde(default)]
    pub shared_account: Option<LocalAccount>,
    /// Explicit disconnection must not fall back to the desktop application's account.
    #[serde(default)]
    pub shared_account_disconnected: bool,
    #[serde(default)]
    /// Each caller's own provider account, keyed by its Silicon Accounts uuid
    /// (version 2) or its pre-0.3.0 id (version 1).
    pub personal_accounts: BTreeMap<String, LocalAccount>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub account_uuid_migrations: mcport_core::uuid_mapping::Mapping,
    pub host: HostConfig,
    pub connections: BTreeMap<String, RegisteredConnection>,
}

impl Registry {
    pub fn new(host: HostConfig) -> Self {
        Self {
            version: REGISTRY_VERSION,
            account_uuid_migrations: BTreeMap::new(),
            host,
            connections: BTreeMap::new(),
        }
    }
    /// Re-key declared account references without touching endpoints, credentials or journal IDs.
    /// The caller holds the registry and daemon locks and persists the result atomically.
    pub fn migrate_account_uuids(
        &mut self,
        mapping: &mcport_core::uuid_mapping::Mapping,
    ) -> Result<usize, DaemonError> {
        use mcport_core::uuid_mapping::{fresh, mapped};
        if self.is_legacy() {
            return Err(DaemonError::Config(
                "Migrate the old host registry to Accounts before changing account UUIDs".into(),
            ));
        }
        let fresh = fresh(mapping, &self.account_uuid_migrations).map_err(DaemonError::Config)?;
        for link in fresh.values() {
            if self.host.owner_uuid == link.new_uuid
                || self
                    .connections
                    .values()
                    .any(|c| c.personal_accounts.contains_key(&link.new_uuid))
            {
                return Err(DaemonError::Config(
                    "Target account already occurs in registry; merging credentials is forbidden"
                        .into(),
                ));
            }
        }
        self.host.owner_uuid = mapped(&fresh, &self.host.owner_uuid).to_owned();
        for c in self.connections.values_mut() {
            let mut accounts = BTreeMap::new();
            for (old, account) in std::mem::take(&mut c.personal_accounts) {
                accounts.insert(mapped(&fresh, &old).to_owned(), account);
            }
            c.personal_accounts = accounts;
        }
        let count = fresh.len();
        self.account_uuid_migrations.extend(fresh);
        self.validate()?;
        Ok(count)
    }
    /// Written before 0.3.0: personal accounts are keyed by the callers' old ids.
    pub fn is_legacy(&self) -> bool {
        self.version < REGISTRY_VERSION
    }
    /// The key of the caller's personal provider account in this registry: its uuid,
    /// or (version 1) the old id the service sends for registries not yet migrated.
    /// Empty when the job does not name one; never falls back to a public id.
    pub fn account_key<'a>(&self, actor: &'a Actor) -> &'a str {
        if self.is_legacy() {
            &actor.principal_id
        } else {
            &actor.uuid
        }
    }
    pub fn load(path: impl AsRef<Path>) -> Result<Self, DaemonError> {
        let path = path.as_ref();
        ensure_private_file(path)?;
        let bytes = fs::read(path)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(DaemonError::Config("registry exceeds 4 MiB".into()));
        }
        let registry: Self = serde_json::from_slice(&bytes)
            .map_err(|_| DaemonError::Config("registry JSON cannot be decoded".into()))?;
        registry.validate()?;
        Ok(registry)
    }
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), DaemonError> {
        self.validate()?;
        private_json(path.as_ref(), self)
    }
    pub fn register(
        &mut self,
        connection_id: impl Into<String>,
        endpoint: Endpoint,
        auth_mode: impl Into<String>,
    ) -> Result<(), DaemonError> {
        let id = connection_id.into();
        let auth_mode = auth_mode.into();
        if !matches!(auth_mode.as_str(), "none" | "shared" | "per-user") {
            return Err(DaemonError::Config(
                "auth_mode must be none, shared or per-user".into(),
            ));
        }
        if id.is_empty() {
            return Err(DaemonError::Config("connection ID is required".into()));
        }
        self.connections.insert(
            id,
            RegisteredConnection {
                endpoint,
                auth_mode,
                shared_account: None,
                shared_account_disconnected: false,
                personal_accounts: BTreeMap::new(),
            },
        );
        Ok(())
    }
    fn validate(&self) -> Result<(), DaemonError> {
        if !matches!(self.version, 1 | REGISTRY_VERSION) {
            return Err(DaemonError::Config(format!(
                "registry version {} is not supported by this mcport ({}); install the current mcport",
                self.version,
                env!("CARGO_PKG_VERSION")
            )));
        }
        let url = url::Url::parse(&self.host.backend_url)
            .map_err(|_| DaemonError::Config("backend URL is invalid".into()))?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && local) {
            return Err(DaemonError::Config(
                "backend URL must use HTTPS (HTTP is allowed only for loopback development)".into(),
            ));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(DaemonError::Config(
                "backend URL cannot contain credentials, query or fragment".into(),
            ));
        }
        let host = &self.host;
        let (required, absent): (Vec<&String>, Vec<&String>) = if self.is_legacy() {
            (
                vec![&host.org_id, &host.owner_id, &host.environment],
                vec![&host.owner_uuid],
            )
        } else {
            (
                vec![&host.owner_uuid],
                vec![&host.org_id, &host.owner_id, &host.environment],
            )
        };
        if host.host_id.is_empty()
            || host.host_token.is_empty()
            || required.iter().any(|value| value.is_empty())
        {
            return Err(DaemonError::Config(
                "host id, owner and token are required".into(),
            ));
        }
        if absent.iter().any(|value| !value.is_empty()) {
            return Err(DaemonError::Config(format!(
                "a version {} registry mixes owner fields of different registry versions",
                self.version
            )));
        }
        for connection in self.connections.values() {
            if !matches!(
                connection.auth_mode.as_str(),
                "none" | "shared" | "per-user"
            ) {
                return Err(DaemonError::Config("invalid registered auth mode".into()));
            }
            if let Endpoint::Stdio { command, .. } = &connection.endpoint
                && !Path::new(command).is_absolute()
            {
                return Err(DaemonError::Config(
                    "stdio commands must be absolute paths".into(),
                ));
            }
        }
        Ok(())
    }
}

fn ensure_private_file(path: &Path) -> Result<(), DaemonError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DaemonError::Config(
            "registry must be a regular file, not a symlink".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(DaemonError::Config(
                "registry containing credentials must have mode 0600; run chmod 600 on it".into(),
            ));
        }
    }
    Ok(())
}

fn private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), DaemonError> {
    let parent = path
        .parent()
        .ok_or_else(|| DaemonError::Config("storage path has no parent".into()))?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let temp = parent.join(format!(
        ".mcport-{}-{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let body = serde_json::to_vec(value)
        .map_err(|_| DaemonError::Config("storage value cannot be encoded".into()))?;
    if let Err(error) = file
        .write_all(&body)
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&temp, path))
    {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    // Make the lease/result journal durable across host restarts.
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn error(code: &str, message: &str, unknown: bool) -> HostJobResult {
    HostJobResult {
        result: None,
        error: Some(ApiError {
            code: code.into(),
            message: message.into(),
            recovery: Some(
                "Inspect connection and provider status before retrying an unknown outcome.".into(),
            ),
            outcome_unknown: unknown,
        }),
    }
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// The endpoint to run for a caller whose personal-account key in this registry is
/// `account` (see [`Registry::account_key`]; empty when the job names none).
fn endpoint_for(
    connection: &RegisteredConnection,
    account: &str,
) -> Result<Endpoint, HostJobResult> {
    if connection.auth_mode == "shared" && connection.shared_account_disconnected {
        return Err(error(
            "provider_authentication_required",
            "This shared account was explicitly disconnected on its execution host. Reconnect it before invoking the connection.",
            false,
        ));
    }
    let account=match connection.auth_mode.as_str() {
        "none"=>None,
        "shared"=>connection.shared_account.as_ref(), // Desktop MCP may use its existing local app account.
        "per-user"=>Some(connection.personal_accounts.get(account).filter(|_| !account.is_empty()).ok_or_else(||error("provider_authentication_required","This caller has no account configured on the execution host. Personal authentication never falls back to a shared account.",false))?),
        _=>return Err(error("invalid_configuration","The local connection has an invalid account mode.",false)),
    };
    let mut endpoint = connection.endpoint.clone();
    if let Some(account) = account {
        match &mut endpoint {
            Endpoint::Http {
                headers,
                bearer_token,
                ..
            } => {
                if !account.env.is_empty()
                    || (connection.auth_mode == "per-user"
                        && account.bearer_token.as_deref().is_none_or(str::is_empty)
                        && account.headers.is_empty())
                {
                    return Err(error(
                        "provider_authentication_required",
                        "This HTTP account requires its own bearer token or HTTP credential header; environment and inherited host credentials cannot authenticate it.",
                        false,
                    ));
                }
                if connection.auth_mode == "per-user" {
                    headers.clear();
                }
                headers.extend(account.headers.clone());
                *bearer_token = account.bearer_token.clone();
            }
            Endpoint::Stdio { env, .. } => {
                if account.bearer_token.is_some()
                    || !account.headers.is_empty()
                    || (connection.auth_mode == "per-user" && account.env.is_empty())
                {
                    return Err(error(
                        "provider_authentication_required",
                        "This stdio account requires its own environment configuration; HTTP credentials and inherited shared configuration cannot authenticate it.",
                        false,
                    ));
                }
                if connection.auth_mode == "per-user" {
                    env.clear();
                }
                env.extend(account.env.clone());
            }
        }
    }
    Ok(endpoint)
}

fn reload_registry(path: &Path, previous: &Registry) -> Result<Registry, DaemonError> {
    let fresh = Registry::load(path)?;
    if fresh.host != previous.host || fresh.version != previous.version {
        return Err(DaemonError::Config(
            "host identity changed; restart the connector".into(),
        ));
    }
    Ok(fresh)
}

struct SessionEntry {
    fingerprint: [u8; 32],
    session: Option<McpSession>,
    last_used: Instant,
}
struct ActiveJob {
    connection_id: String,
    actor: Actor,
    fingerprint: [u8; 32],
    cancellation: CancellationToken,
}
fn fingerprint(endpoint: &Endpoint) -> [u8; 32] {
    Sha256::digest(serde_json::to_vec(endpoint).expect("endpoint serialization")).into()
}
/// Provider sessions per (connection, caller's account key).
type SessionPool = HashMap<(String, String), Arc<tokio::sync::Mutex<SessionEntry>>>;

#[derive(Clone, Serialize, Deserialize)]
struct JournalEntry {
    started_at: i64,
    result: Option<HostJobResult>,
    delivered: bool,
}
#[derive(Default, Serialize, Deserialize)]
struct Journal {
    jobs: BTreeMap<String, JournalEntry>,
}

#[derive(Clone)]
struct Backend {
    client: Client,
    context: HostContext,
}
impl Backend {
    async fn poll(
        &self,
        registry: &Registry,
        active: Vec<String>,
        health: BTreeMap<String, Value>,
    ) -> Result<HostPollResult, DaemonError> {
        let mut capabilities = BTreeMap::new();
        capabilities.insert("active_jobs".into(), json!(active.len()));
        capabilities.insert("active_job_ids".into(), json!(active));
        capabilities.insert("max_concurrent_jobs".into(), json!(4));
        // Version 2: personal accounts and health below are keyed by account uuid.
        capabilities.insert("registry_version".into(), json!(registry.version));
        for (id, connection) in &registry.connections {
            capabilities.insert(id.clone(),json!({"account_owners":connection.personal_accounts.keys().collect::<Vec<_>>(),"shared_account":connection.shared_account.is_some(),"shared_account_disconnected":connection.shared_account_disconnected,"health":health.get(id)}));
        }
        self.client
            .host_poll(
                &self.context,
                &HostPoll {
                    registered_connections: registry.connections.keys().cloned().collect(),
                    capabilities,
                },
            )
            .await
            .map_err(|failure| match failure {
                mcport_api::Error::Api {
                    status: 401 | 403 | 404,
                    ..
                } => DaemonError::Unauthorized,
                _ => DaemonError::Config(
                    "Backend host poll is unavailable or returned invalid bounded job data.".into(),
                ),
            })
    }
    async fn complete(&self, id: &str, result: &HostJobResult) -> bool {
        match self.client.host_result(&self.context, id, result).await {
            Ok(_) => true,
            Err(mcport_api::Error::Api {
                status: 404 | 409 | 410,
                ..
            }) => true,
            Err(_) => false,
        }
    }
}

/// Run one outbound connector; the CLI can call this or launch the daemon binary.
/// A durable journal records before dispatch and prevents side-effect replay even
/// after daemon crashes or duplicate deliveries. Failed result uploads are retried.
pub async fn run(
    registry_path: impl AsRef<Path>,
    shutdown: CancellationToken,
) -> Result<(), DaemonError> {
    let _cancel_on_exit = shutdown.clone().drop_guard();
    let registry_path = registry_path.as_ref().to_path_buf();
    let mut registry = Registry::load(&registry_path)?;
    tracing::info!(source="daemon",operation="host.connect",host_id=%registry.host.host_id,registry_version=registry.version,outcome="starting");
    let parent = registry_path
        .parent()
        .ok_or_else(|| DaemonError::Config("registry path has no parent".into()))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join("daemon.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| DaemonError::AlreadyRunning)?;
    let stop_path = parent.join("stop.request");
    let status_path = parent.join("daemon-status.json");
    if stop_path.exists() {
        fs::remove_file(&stop_path)?;
    }
    let started_at = now();
    let mut last_poll_at = 0i64;
    private_json(
        &status_path,
        &json!({"pid":std::process::id(),"host_id":registry.host.host_id,"started_at":started_at,"last_poll_at":last_poll_at}),
    )?;
    let journal_path = parent.join("journal.json");
    let mut journal: Journal = if journal_path.exists() {
        ensure_private_file(&journal_path)?;
        serde_json::from_slice(&fs::read(&journal_path)?).map_err(|_| {
            DaemonError::Config(
                "execution journal invalid; do not delete it while outcomes are unresolved".into(),
            )
        })?
    } else {
        Journal::default()
    };
    for entry in journal.jobs.values_mut() {
        if entry.result.is_none() && !entry.delivered {
            entry.result = Some(error(
                "host_restarted",
                "The host restarted during execution. The provider may have completed the operation; it was not replayed.",
                true,
            ));
        }
    }
    private_json(&journal_path, &journal)?;
    let backend = Backend {
        client: Client::new(&registry.host.backend_url)
            .map_err(|_| DaemonError::Config("Backend client setup failed".into()))?,
        context: HostContext {
            host_id: registry.host.host_id.clone(),
            host_token: registry.host.host_token.clone(),
            isi: registry.host.isi.clone(),
        },
    };
    let (completed_tx, mut completed_rx) = mpsc::channel::<(String, HostJobResult)>(16);
    let mut active: HashMap<String, ActiveJob> = HashMap::new();
    let mut sessions: SessionPool = HashMap::new();
    let mut health = health::Monitor::default();
    let mut polling: Option<tokio::task::JoinHandle<Result<HostPollResult, DaemonError>>> = None;
    let mut poll_at = tokio::time::Instant::now();
    let mut failures = 0u32;
    let mut registry_modified = fs::metadata(&registry_path)?.modified()?;
    let mut control_tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        if polling.is_none() && tokio::time::Instant::now() >= poll_at {
            registry = reload_registry(&registry_path, &registry)?;
            health.sync(&registry);
            health.tick(&sessions);
            cancel_changed_jobs(&registry, &active, &journal);
            sessions.retain(|(connection, account), entry| match entry.try_lock() {
                Ok(entry) => {
                    registry
                        .connections
                        .get(connection)
                        .and_then(|connection| endpoint_for(connection, account).ok())
                        .is_some_and(|endpoint| fingerprint(&endpoint) == entry.fingerprint)
                        && entry.last_used.elapsed() < Duration::from_secs(300)
                }
                Err(_) => true,
            });
            let backend = backend.clone();
            let snapshot = registry.clone();
            let active_ids = active.keys().cloned().collect();
            let health_snapshot = health.snapshot();
            polling = Some(tokio::spawn(async move {
                backend.poll(&snapshot, active_ids, health_snapshot).await
            }));
        }
        tokio::select! {
            _=control_tick.tick()=>{
                health.tick(&sessions);
                if stop_path.exists(){let _=fs::remove_file(&stop_path);shutdown.cancel();}
                let modified=fs::metadata(&registry_path)?.modified()?;
                if modified!=registry_modified{
                    registry=reload_registry(&registry_path,&registry)?;
                    registry_modified=modified;
                    health.sync(&registry);
                    health.tick(&sessions);
                    cancel_changed_jobs(&registry,&active,&journal);
                    // The gateway may already have leased a job into this
                    // response. Drain the bounded poll instead of discarding
                    // its only delivery, then advertise the latest registry.
                    poll_at=tokio::time::Instant::now();
                }
            },
            _=shutdown.cancelled()=>{for job in active.values(){job.cancellation.cancel();}if let Some(task)=polling.take(){task.abort();}break;},
            Some((id,result))=completed_rx.recv()=>{
                active.remove(&id);
                if let Some(entry)=journal.jobs.get_mut(&id){entry.result=Some(result);}
                private_json(&journal_path,&journal)?;
                flush_results(&backend,&mut journal,&journal_path).await?;
            },
            poll=async {polling.as_mut().expect("guarded poll").await},if polling.is_some()=>{
                polling=None;
                match poll {
                    Ok(Ok(reply))=>{
                        // A reply may arrive in the same tick as a local account
                        // disconnect; resolve every job against the latest registry.
                        registry=reload_registry(&registry_path,&registry)?;
                        last_poll_at=now();
                        private_json(&status_path,&json!({"pid":std::process::id(),"host_id":registry.host.host_id,"started_at":started_at,"last_poll_at":last_poll_at}))?;
                        failures=0;poll_at=tokio::time::Instant::now()+Duration::from_millis(250);
                        let cancelled:HashSet<String>=reply.cancelled.into_iter().collect();
                        for id in &cancelled {if let Some(job)=active.get(id){job.cancellation.cancel();}}
                        for job in reply.jobs {
                            if journal.jobs.contains_key(&job.id){continue;}
                            let id=job.id.clone();
                            journal.jobs.insert(id.clone(),JournalEntry{started_at:now(),result:None,delivered:false});
                            private_json(&journal_path,&journal)?;
                            let reject=if cancelled.contains(&id){Some(error("cancelled","This job was cancelled before dispatch.",false))}
                                else if job.expires_at<=now(){Some(error("expired","The job expired before reaching its host.",false))}
                                else if active.len()>=4{Some(error("host_busy","This host is executing its maximum number of concurrent jobs. Try again after current work finishes.",false))}
                                else {None};
                            if let Some(result)=reject {journal.jobs.get_mut(&id).unwrap().result=Some(result);continue;}
                            let account=registry.account_key(&job.actor).to_owned();
                            let endpoint=registry.connections.get(&job.connection_id).ok_or_else(||error("unregistered_connection","This connection is not registered on this host. Register it locally before calling it.",false)).and_then(|connection|endpoint_for(connection,&account));
                            match endpoint {
                                Err(result)=>journal.jobs.get_mut(&id).unwrap().result=Some(result),
                                Ok(endpoint)=>{
                                    let key=(job.connection_id.clone(),account);
                                    if sessions.len()>=32 && !sessions.contains_key(&key) {journal.jobs.get_mut(&id).unwrap().result=Some(error("host_capacity","This host has reached its active connection capacity. Wait for idle sessions to close.",false));continue;}
                                    let hash=fingerprint(&endpoint);
                                    let session=sessions.entry(key).or_insert_with(||Arc::new(tokio::sync::Mutex::new(SessionEntry{fingerprint:hash,session:None,last_used:Instant::now()}))).clone();
                                    let cancellation=shutdown.child_token();active.insert(id.clone(),ActiveJob{connection_id:job.connection_id.clone(),actor:job.actor.clone(),fingerprint:hash,cancellation:cancellation.clone()});
                                    let completed=completed_tx.clone();let backend=backend.clone();
                                    tokio::spawn(async move {let result=execute_job(endpoint,job,cancellation,backend,session).await;let _=completed.send((id,result)).await;});
                                }
                            }
                        }
                        private_json(&journal_path,&journal)?;
                        flush_results(&backend,&mut journal,&journal_path).await?;
                    },
                    Ok(Err(DaemonError::Unauthorized))=>{for job in active.values(){job.cancellation.cancel();}return Err(DaemonError::Unauthorized);},
                    _=>{failures=failures.saturating_add(1);poll_at=tokio::time::Instant::now()+Duration::from_secs((1u64<<failures.min(5)).min(30));},
                }
            },
            _=tokio::time::sleep_until(poll_at),if polling.is_none()=>{},
        }
    }
    // Allow cancellation results a short window to reach the durable journal.
    let end = tokio::time::Instant::now() + Duration::from_secs(3);
    while !active.is_empty() {
        match tokio::time::timeout_at(end, completed_rx.recv()).await {
            Ok(Some((id, result))) => {
                active.remove(&id);
                if let Some(entry) = journal.jobs.get_mut(&id) {
                    entry.result = Some(result);
                }
            }
            _ => break,
        }
    }
    private_json(&journal_path, &journal)?;
    let _ = fs::remove_file(&status_path);
    Ok(())
}

fn cancel_changed_jobs(
    registry: &Registry,
    active: &HashMap<String, ActiveJob>,
    journal: &Journal,
) {
    for (id, job) in active {
        let changed = registry
            .connections
            .get(&job.connection_id)
            .and_then(|connection| endpoint_for(connection, registry.account_key(&job.actor)).ok())
            .is_none_or(|endpoint| fingerprint(&endpoint) != job.fingerprint);
        if changed
            || journal
                .jobs
                .get(id)
                .is_some_and(|entry| entry.started_at + 600 < now())
        {
            job.cancellation.cancel();
        }
    }
}

async fn flush_results(
    backend: &Backend,
    journal: &mut Journal,
    path: &Path,
) -> Result<(), DaemonError> {
    for (id, entry) in journal
        .jobs
        .iter_mut()
        .filter(|(_, entry)| !entry.delivered)
    {
        if let Some(result) = &entry.result {
            if backend.complete(id, result).await {
                entry.delivered = true;
                entry.result = None;
            } else {
                break;
            }
        }
    }
    // Keep compact durable tombstones. They must not be mistaken for interrupted jobs.
    private_json(path, journal)
}

async fn execute_job(
    endpoint: Endpoint,
    job: HostJob,
    cancellation: CancellationToken,
    backend: Backend,
    entry: Arc<tokio::sync::Mutex<SessionEntry>>,
) -> HostJobResult {
    tracing::info!(source="daemon",operation="mcp.execute",call_id=%job.id,connection_id=%job.connection_id,method=%job.method,outcome="started");
    let (progress_tx, mut progress_rx) = mpsc::channel::<Value>(16);
    let progress_job = job.id.clone();
    let progress_task = tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            let _ = backend
                .client
                .host_progress(&backend.context, &progress_job, &progress)
                .await;
        }
    });
    let deadline_ms = (job.expires_at - now()).max(1) as u64 * 1000;
    let options = ExecutionOptions {
        timeout: Duration::from_millis(job.timeout_ms.clamp(1, 600_000).min(deadline_ms)),
        network_policy: NetworkPolicy::LocalHost,
        cancellation: cancellation.clone(),
        progress: Some(progress_tx),
        ..Default::default()
    };
    let mut output = execute_in_session(entry, &endpoint, &job, options).await;
    if matches!(job.method.as_str(), "tools/call" | "resources/read")
        && let Ok(result) = &mut output
    {
        let remaining = Duration::from_secs((job.expires_at - now()).max(0) as u64);
        assets::materialize(&endpoint, result, &cancellation, remaining).await;
    }
    tracing::info!(source="daemon",operation="mcp.execute",call_id=%job.id,connection_id=%job.connection_id,method=%job.method,outcome=if output.is_ok(){"completed"}else{"failed"},error_code=output.as_ref().err().map(|error|error.code.as_str()).unwrap_or(""));
    progress_task.abort();
    match output {
        Ok(result) => HostJobResult {
            result: Some(result),
            error: None,
        },
        Err(error) => HostJobResult {
            result: None,
            error: Some(ApiError {
                code: error.code,
                message: error.message,
                recovery: Some(
                    "Check the registered MCP and provider account on the execution host.".into(),
                ),
                outcome_unknown: error.outcome_unknown,
            }),
        },
    }
}

async fn execute_in_session(
    entry: Arc<tokio::sync::Mutex<SessionEntry>>,
    endpoint: &Endpoint,
    job: &HostJob,
    mut options: ExecutionOptions,
) -> Result<Value, mcport_mcp::McpError> {
    let deadline = Instant::now() + options.timeout;
    let mut entry = tokio::select! {
        _=options.cancellation.cancelled()=>return Err(mcport_mcp::McpError::new("cancelled","Job cancelled before it entered the local process.")),
        entry=entry.lock()=>entry,
    };
    if job.expires_at <= now() {
        return Err(mcport_mcp::McpError::new(
            "expired",
            "Job expired while waiting for its local process.",
        ));
    }
    let hash = fingerprint(endpoint);
    if entry.fingerprint != hash || entry.session.as_ref().is_some_and(McpSession::is_closed) {
        if let Some(mut session) = entry.session.take() {
            session.close().await;
        }
        entry.fingerprint = hash;
    }
    if entry.session.is_none() {
        entry.session = Some(McpSession::connect(endpoint, &options).await?);
    }
    let result = async {
        let session = entry.session.as_mut().expect("connected session");
        if job.method == "tools/call" {
            let name = job
                .params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    mcport_mcp::McpError::new("invalid_params", "Provide the MCP tool name.")
                })?;
            options.timeout = deadline.saturating_duration_since(Instant::now());
            let schemas = session.tool_schemas(name, &options).await?;
            options.input_schema = Some(schemas.input_schema);
            options.output_schema = schemas.output_schema;
        }
        options.timeout = deadline.saturating_duration_since(Instant::now());
        if options.timeout.is_zero() || job.expires_at <= now() {
            return Err(mcport_mcp::McpError::new(
                "expired",
                "Job expired during discovery; the tool was not called.",
            ));
        }
        session
            .request(&job.method, job.params.clone(), options)
            .await
    }
    .await;
    entry.last_used = Instant::now();
    if result.as_ref().is_err_and(|error| error.outcome_unknown)
        && let Some(mut session) = entry.session.take()
    {
        session.close().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn host() -> HostConfig {
        HostConfig::new("http://127.0.0.1:4380", "host", "secret", "Own", None)
    }
    /// A registry as mcport 0.2 wrote it (version 1, keyed by old ids).
    pub(super) fn legacy_json() -> Value {
        json!({"version":1,"host":{"backend_url":"http://127.0.0.1:4380","host_id":"host","host_token":"secret","environment":"production","org_id":"tos","owner_id":"c:owner","isi":null},"connections":{"conn":{"endpoint":{"transport":"http","url":"http://127.0.0.1:1/mcp","headers":{},"bearer_token":null},"auth_mode":"per-user","shared_account":null,"shared_account_disconnected":false,"personal_accounts":{"c:owner":{"label":"Owner","bearer_token":"owner-secret","headers":{},"env":{}}}}}})
    }
    fn actor(uuid: &str, principal: &str) -> Actor {
        Actor {
            uuid: uuid.into(),
            principal_id: principal.into(),
            ..Default::default()
        }
    }
    #[test]
    fn registry_keeps_secrets_private() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("host/registry.json");
        let mut registry = Registry::new(host());
        registry
            .register(
                "connection",
                Endpoint::http("http://127.0.0.1:1234/mcp"),
                "none",
            )
            .unwrap();
        registry.save(&path).unwrap();
        let loaded = Registry::load(&path).unwrap();
        assert!(loaded.connections.contains_key("connection"));
        assert_eq!(loaded.version, 2);
        // Version 2 files carry no pre-0.3.0 owner fields.
        let raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["host"]["owner_uuid"], "Own");
        assert!(raw["host"].get("org_id").is_none() && raw["host"].get("environment").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn legacy_registries_load_and_key_accounts_by_old_id_until_migrated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("registry.json");
        private_json(&path, &legacy_json()).unwrap();
        let legacy = Registry::load(&path).unwrap();
        assert!(legacy.is_legacy());
        // The service sends the old id as principal_id for registries not yet migrated.
        let caller = actor("Ow1", "c:owner");
        assert_eq!(legacy.account_key(&caller), "c:owner");
        let connection = &legacy.connections["conn"];
        let Endpoint::Http { bearer_token, .. } =
            endpoint_for(connection, legacy.account_key(&caller)).unwrap()
        else {
            panic!("HTTP endpoint")
        };
        assert_eq!(bearer_token.as_deref(), Some("owner-secret"));
        // A version 2 registry uses the uuid, never the id.
        let mut migrated = legacy.clone();
        migrated.version = 2;
        migrated.host = HostConfig::new("http://127.0.0.1:4380", "host", "secret", "Ow1", None);
        assert_eq!(migrated.account_key(&caller), "Ow1");
        assert!(endpoint_for(connection, migrated.account_key(&caller)).is_err());
        assert_eq!(migrated.account_key(&actor("", "c:owner")), "");
        // Saving re-validates: version 2 must not keep the old owner fields.
        let mut mixed = migrated.clone();
        mixed.host.org_id = "tos".into();
        assert!(
            mixed
                .save(directory.path().join("mixed/registry.json"))
                .is_err()
        );
        let mut unknown = legacy_json();
        unknown["version"] = json!(3);
        private_json(&path, &unknown).unwrap();
        let error = Registry::load(&path).err().unwrap().to_string();
        assert!(error.contains("version 3"), "{error}");
    }
    #[test]
    fn personal_account_never_falls_back() {
        let connection = RegisteredConnection {
            endpoint: Endpoint::http("http://localhost/mcp"),
            auth_mode: "per-user".into(),
            shared_account: Some(LocalAccount {
                bearer_token: Some("other-account".into()),
                ..Default::default()
            }),
            shared_account_disconnected: false,
            personal_accounts: BTreeMap::new(),
        };
        assert!(endpoint_for(&connection, "caller").is_err());
        assert!(endpoint_for(&connection, "").is_err());
        let mut shared = connection.clone();
        shared.auth_mode = "shared".into();
        shared.shared_account = None;
        assert!(endpoint_for(&shared, "caller").is_ok()); // Initial desktop app session is intentional.
        shared.shared_account_disconnected = true;
        assert!(endpoint_for(&shared, "caller").is_err()); // Explicit disconnect cannot use it again.
    }
    #[test]
    fn personal_credentials_replace_shared_material_and_match_transport() {
        let mut registry = Registry::new(host());
        registry
            .register(
                "http",
                Endpoint::Http {
                    url: "http://localhost/mcp".into(),
                    headers: BTreeMap::from([("X-Shared-Token".into(), "shared-secret".into())]),
                    bearer_token: Some("shared-secret".into()),
                },
                "per-user",
            )
            .unwrap();
        let connection = registry.connections.get_mut("http").unwrap();
        connection
            .personal_accounts
            .insert("caller".into(), LocalAccount::default());
        assert!(endpoint_for(connection, "caller").is_err());
        connection
            .personal_accounts
            .get_mut("caller")
            .unwrap()
            .bearer_token = Some("personal-secret".into());
        let Endpoint::Http {
            headers,
            bearer_token,
            ..
        } = endpoint_for(connection, "caller").unwrap()
        else {
            panic!("HTTP endpoint")
        };
        assert!(headers.is_empty());
        assert_eq!(bearer_token.as_deref(), Some("personal-secret"));
        connection.endpoint = Endpoint::Stdio {
            command: "/absolute/program".into(),
            args: vec![],
            env: BTreeMap::from([("SHARED_TOKEN".into(), "shared-secret".into())]),
            cwd: None,
        };
        assert!(endpoint_for(connection, "caller").is_err());
        let account = connection.personal_accounts.get_mut("caller").unwrap();
        account.bearer_token = None;
        account
            .env
            .insert("PERSONAL_TOKEN".into(), "personal-secret".into());
        let Endpoint::Stdio { env, .. } = endpoint_for(connection, "caller").unwrap() else {
            panic!("stdio endpoint")
        };
        assert!(!env.contains_key("SHARED_TOKEN"));
        assert_eq!(env["PERSONAL_TOKEN"], "personal-secret");
    }
}
