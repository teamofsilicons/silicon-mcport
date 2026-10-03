//! Explicit local host operations. No home, session, environment or executable is
//! discovered implicitly: callers supply their scope, registry path and runtime.
//! Registry persistence and process start/stop happen only when requested.
use crate::{Connection, HostRegistration};
use fs2::FileExt;
pub use mcport_daemon::{DaemonError, HostConfig, LocalAccount, RegisteredConnection, Registry};
pub use mcport_mcp::Endpoint;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Daemon(#[from] DaemonError),
    #[error("Local host I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Local host data is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}
pub type Result<T> = std::result::Result<T, Error>;
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Identity already authenticated by the caller through MCPort. This value is
/// not itself a credential; the gateway remains the authority for remote actions.
#[derive(Clone, Debug)]
pub struct Scope {
    pub backend_url: String,
    pub environment: String,
    pub principal_id: String,
    pub org_id: String,
}
impl Scope {
    pub fn validate_registry(&self, registry: &Registry, host_id: &str) -> Result<()> {
        if registry.host.host_id != host_id
            || registry.host.org_id != self.org_id
            || registry.host.environment != self.environment
            || registry.host.backend_url.trim_end_matches('/')
                != self.backend_url.trim_end_matches('/')
        {
            return Err(invalid(
                "Local host registry does not match the selected backend, organization and testing environment.",
            ));
        }
        Ok(())
    }
    fn validate_connection(&self, registry: &Registry, connection: &Connection) -> Result<()> {
        let host_id = connection
            .host_id
            .as_deref()
            .ok_or_else(|| invalid("This connection has no local execution host."))?;
        self.validate_registry(registry, host_id)?;
        if connection.org_id != self.org_id || connection.environment != self.environment {
            return Err(invalid(
                "Connection does not match the selected organization and testing environment.",
            ));
        }
        Ok(())
    }
}

/// Turn a newly issued host registration into an explicit caller-owned registry.
pub fn registry_for_host(
    registration: HostRegistration,
    scope: &Scope,
    isi: Option<String>,
) -> Result<Registry> {
    let host = registration.host;
    if host.owner_id != scope.principal_id
        || host.org_id != scope.org_id
        || host.environment != scope.environment
    {
        return Err(invalid(
            "The registered host belongs to a different account, organization or environment.",
        ));
    }
    Ok(Registry::new(HostConfig {
        backend_url: scope.backend_url.clone(),
        host_id: host.id,
        host_token: registration.host_token,
        environment: host.environment,
        org_id: host.org_id,
        owner_id: host.owner_id,
        isi,
    }))
}

/// Validate and map central connection metadata into a local execution endpoint.
/// Existing registration is preserved, including all locally stored accounts.
/// Returns true only when a new registration was inserted. This does not save it.
pub fn register_connection(
    registry: &mut Registry,
    connection: &Connection,
    scope: &Scope,
    env: BTreeMap<String, String>,
) -> Result<bool> {
    scope.validate_connection(registry, connection)?;
    if connection.owner_id != scope.principal_id || registry.host.owner_id != scope.principal_id {
        return Err(invalid(
            "Only the connection and host owner may approve local execution.",
        ));
    }
    if registry.connections.contains_key(&connection.id) {
        return Ok(false);
    }
    validate_env(&env)?;
    let endpoint = match connection.transport.as_str() {
        "http" => {
            if !env.is_empty() {
                return Err(invalid(
                    "Environment variables apply only to stdio processes.",
                ));
            }
            Endpoint::http(
                connection
                    .url
                    .clone()
                    .ok_or_else(|| invalid("HTTP connection has no endpoint URL."))?,
            )
        }
        "stdio" => {
            let command = connection
                .command
                .clone()
                .ok_or_else(|| invalid("stdio connection has no executable."))?;
            if !Path::new(&command).is_absolute() {
                return Err(invalid(
                    "The configured stdio command must be an absolute path. MCPort never runs it through a shell.",
                ));
            }
            Endpoint::Stdio {
                command,
                args: connection.args.clone(),
                env,
                cwd: None,
            }
        }
        _ => return Err(invalid("Unsupported local execution transport.")),
    };
    registry.register(&connection.id, endpoint, &connection.auth_mode)?;
    Ok(true)
}

fn selected<'a>(
    registry: &'a mut Registry,
    connection: &Connection,
    scope: &Scope,
    mutation: bool,
) -> Result<&'a mut RegisteredConnection> {
    scope.validate_connection(registry, connection)?;
    if mutation && connection.auth_mode == "shared" && connection.owner_id != scope.principal_id {
        return Err(invalid(
            "Only the connection owner can change its shared provider account.",
        ));
    }
    let registered = registry
        .connections
        .get_mut(&connection.id)
        .ok_or_else(|| {
            invalid("Connection is not registered in this host's local execution allowlist.")
        })?;
    if registered.auth_mode != connection.auth_mode {
        return Err(invalid(
            "Local registration and current connection authentication mode disagree. Re-register the connection on its host.",
        ));
    }
    Ok(registered)
}

/// Apply a local credential to the supplied registry. No credential is uploaded,
/// persisted or verified against its provider by this operation.
pub fn connect_account(
    registry: &mut Registry,
    connection: &Connection,
    scope: &Scope,
    input: Value,
) -> Result<Value> {
    if connection.auth_mode == "none" {
        return Err(invalid(
            "This connection uses no authentication. Create a shared or per-user connection to configure credentials.",
        ));
    }
    let account = account_from_input(input, &connection.transport, &connection.auth_mode)?;
    let registered = selected(registry, connection, scope, true)?;
    let label = account.label.clone();
    if connection.auth_mode == "shared" {
        registered.shared_account = Some(account);
        registered.shared_account_disconnected = false;
    } else {
        registered
            .personal_accounts
            .insert(scope.principal_id.clone(), account);
    }
    Ok(
        json!({"connected":true,"owner_id":scope.principal_id,"label":label,"kind":"local","credentials_uploaded":false,"provider_verified":false}),
    )
}
pub fn disconnect_account(
    registry: &mut Registry,
    connection: &Connection,
    scope: &Scope,
) -> Result<Value> {
    let registered = selected(registry, connection, scope, true)?;
    if connection.auth_mode == "shared" {
        registered.shared_account = None;
        registered.shared_account_disconnected = true;
    } else {
        registered.personal_accounts.remove(&scope.principal_id);
    }
    Ok(
        json!({"disconnected":true,"note":"Saved MCPort credentials were removed. A desktop application's own session remains controlled by that application."}),
    )
}
pub fn account_status(
    registry: &Registry,
    connection: &Connection,
    scope: &Scope,
) -> Result<Value> {
    scope.validate_connection(registry, connection)?;
    let registered = registry.connections.get(&connection.id).ok_or_else(|| {
        invalid("Connection is not registered in this host's local execution allowlist.")
    })?;
    if registered.auth_mode != connection.auth_mode {
        return Err(invalid(
            "Local registration and current connection authentication mode disagree. Re-register the connection on its host.",
        ));
    }
    let account = if connection.auth_mode == "shared" {
        registered.shared_account.as_ref()
    } else {
        registered.personal_accounts.get(&scope.principal_id)
    };
    Ok(
        json!({"connected":account.is_some(),"owner_id":if connection.auth_mode == "shared" { &connection.owner_id } else { &scope.principal_id },"label":account.map(|a|a.label.as_str()),"kind":"local","uses_host_account":connection.auth_mode == "shared" && account.is_none() && !registered.shared_account_disconnected,"disconnected":registered.shared_account_disconnected,"provider_verified":false}),
    )
}
pub fn unregister_connection(
    registry: &mut Registry,
    connection: &Connection,
    scope: &Scope,
) -> Result<bool> {
    scope.validate_connection(registry, connection)?;
    if connection.owner_id != scope.principal_id || registry.host.owner_id != scope.principal_id {
        return Err(invalid(
            "Only the connection and host owner may remove its local registration.",
        ));
    }
    Ok(registry.connections.remove(&connection.id).is_some())
}

/// Parse transport-specific local account data without doing I/O.
pub fn account_from_input(input: Value, transport: &str, auth_mode: &str) -> Result<LocalAccount> {
    let kind = input
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("bearer");
    if !matches!(auth_mode, "shared" | "per-user") {
        return Err(invalid(
            "Local account authentication mode must be shared or per-user.",
        ));
    }
    if !matches!(transport, "http" | "stdio") {
        return Err(invalid("Unsupported local account transport."));
    }
    if kind == "host" && auth_mode != "shared" {
        return Err(invalid(
            "The execution host's existing account can only be used by an explicitly shared connection.",
        ));
    }
    if (matches!(kind, "bearer" | "header") && transport != "http")
        || (kind == "env" && transport != "stdio")
    {
        return Err(invalid(
            "HTTP MCPs accept bearer or header credentials; stdio MCPs accept environment credentials.",
        ));
    }
    let mut account = LocalAccount {
        label: input
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("Configured local account")
            .into(),
        ..Default::default()
    };
    match kind {
        "bearer" => {
            let secret = input
                .get("secret")
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty() && !v.contains(['\r', '\n']))
                .ok_or_else(|| {
                    invalid("Bearer configuration requires a nonempty secret without line breaks.")
                })?;
            account.bearer_token = Some(secret.into());
        }
        "header" => {
            let name = input
                .get("header_name")
                .and_then(Value::as_str)
                .filter(|v| {
                    !v.is_empty()
                        && v.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                })
                .ok_or_else(|| invalid("Header configuration requires a valid header_name."))?;
            let secret = input
                .get("secret")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty() && !v.contains(['\r', '\n']))
                .ok_or_else(|| {
                    invalid("Header configuration requires a secret without line breaks.")
                })?;
            account.headers.insert(name.into(), secret.into());
        }
        "env" => {
            account.env = serde_json::from_value(input.get("env").cloned().ok_or_else(|| {
                invalid("Environment configuration requires an env object of key/value strings.")
            })?)?;
            validate_env(&account.env)?;
            if account.env.is_empty() {
                return Err(invalid(
                    "Environment credentials need at least one variable.",
                ));
            }
        }
        "host" => {}
        _ => {
            return Err(invalid(
                "Local account kind must be bearer, header, env or host.",
            ));
        }
    }
    Ok(account)
}
fn validate_env(env: &BTreeMap<String, String>) -> Result<()> {
    if env
        .iter()
        .any(|(key, value)| key.is_empty() || key.contains(['=', '\0']) || value.contains('\0'))
    {
        return Err(invalid(
            "Environment variables need nonempty valid names and values without null bytes.",
        ));
    }
    Ok(())
}

fn parent(path: &Path) -> Result<&Path> {
    if path.file_name().is_none_or(|name| name != "registry.json") {
        return Err(invalid(
            "Each host needs its own trusted directory containing registry.json.",
        ));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| invalid("Registry path must include its parent directory."))?;
    reject_symlink(parent)?;
    // Preserve the CLI's managed subtree boundary. A caller-provided base outside
    // .mcport is trusted; system aliases such as macOS /var are not modified.
    if parent
        .ancestors()
        .any(|p| p.file_name().is_some_and(|name| name == ".mcport"))
    {
        for ancestor in parent.ancestors() {
            reject_symlink(ancestor)?;
            if ancestor.file_name().is_some_and(|name| name == ".mcport") {
                break;
            }
        }
    }
    Ok(parent)
}
fn reject_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(invalid("Local runtime paths must not be symbolic links."));
    }
    Ok(())
}
fn secure_directory(path: &Path) -> Result<()> {
    reject_symlink(path)?;
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
/// Serialize explicit caller-owned registry edits across processes. Hold this
/// guard through load, mutation and save; no global lock or default home exists.
pub fn lock_registry(path: &Path) -> Result<File> {
    secure_directory(parent(path)?)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = parent(path)?.join("registry-edit.lock");
    reject_symlink(&lock)?;
    let file = options.open(lock)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}
fn running(path: &Path) -> Result<bool> {
    let path = parent(path)?.join("daemon.lock");
    if !path.exists() {
        return Ok(false);
    }
    reject_symlink(&path)?;
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&file)?;
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(e) => Err(e.into()),
    }
}
pub fn status(registry_path: &Path) -> Result<Value> {
    parent(registry_path)?;
    let registry = Registry::load(registry_path)?;
    let connected = running(registry_path)?;
    let path = parent(registry_path)?.join("daemon-status.json");
    let prior: Value = if path.exists() {
        reject_symlink(&path)?;
        let mut bytes = Vec::new();
        File::open(path)?.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(invalid("Daemon status exceeds 64 KiB."));
        }
        serde_json::from_slice(&bytes)?
    } else {
        json!({})
    };
    if connected
        && prior.get("host_id").and_then(Value::as_str) != Some(registry.host.host_id.as_str())
    {
        return Err(invalid(
            "The running daemon has not confirmed this registry's host identity. Each host must use a separate directory.",
        ));
    }
    let last_poll_at = prior.get("last_poll_at").and_then(Value::as_i64);
    Ok(
        json!({"host_id":registry.host.host_id,"running":connected,"connected_recently":connected && last_poll_at.is_some_and(|stamp| stamp > now()-60),"last_poll_at":last_poll_at,"registry":registry_path}),
    )
}
/// Run the local connector in this process until explicitly cancelled.
/// Use one trusted directory per host; the registry must be named registry.json.
pub async fn run(registry_path: impl AsRef<Path>, shutdown: CancellationToken) -> Result<()> {
    let registry_path = registry_path.as_ref();
    let directory = parent(registry_path)?;
    for name in [
        "daemon.lock",
        "daemon-status.json",
        "stop.request",
        "journal.json",
    ] {
        reject_symlink(&directory.join(name))?;
    }
    Ok(mcport_daemon::run(registry_path, shutdown).await?)
}

/// Launch the supplied MCPort CLI executable with `daemon run --registry PATH`.
/// The executable and registry are explicit; no process is selected by stale PID.
pub async fn start(registry_path: &Path, executable: &Path) -> Result<Value> {
    parent(registry_path)?;
    Registry::load(registry_path)?;
    if !executable.is_absolute() {
        return Err(invalid(
            "The MCPort runtime executable must be an absolute path.",
        ));
    }
    if running(registry_path)? {
        return status(registry_path);
    }
    let directory = parent(registry_path)?;
    secure_directory(directory)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let log_path = directory.join("daemon.log");
    reject_symlink(&log_path)?;
    let log = options.open(log_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        log.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let mut command = Command::new(executable);
    command
        .arg("daemon")
        .arg("run")
        .arg("--registry")
        .arg(registry_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200);
    }
    let mut child = command.spawn()?;
    for _ in 0..30 {
        if running(registry_path)?
            && let Ok(status) = status(registry_path)
        {
            return Ok(status);
        }
        if let Some(exit) = child.try_wait()? {
            return Err(Error::Invalid(format!(
                "Local daemon exited during startup ({exit}). Inspect {} and retry.",
                directory.join("daemon.log").display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(Error::Invalid(format!(
        "The daemon has not confirmed startup. Inspect {} and status before starting another copy.",
        directory.join("daemon.log").display()
    )))
}
pub async fn stop(registry_path: &Path) -> Result<Value> {
    let current = status(registry_path)?;
    if current.get("running").and_then(Value::as_bool) != Some(true) {
        return Ok(current);
    }
    let path = parent(registry_path)?.join("stop.request");
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(b"{}")?;
            file.sync_all()?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    for _ in 0..80 {
        if !running(registry_path)? {
            return status(registry_path);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(
        json!({"stop_requested":true,"running":true,"note":"The daemon is finishing cancellation and durable result handling. Inspect status; do not kill an unrelated process by a stale PID."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            backend_url: "http://127.0.0.1:4380".into(),
            environment: "test-one".into(),
            principal_id: "ca:owner".into(),
            org_id: "org-one".into(),
        }
    }
    fn registry() -> Registry {
        let scope = scope();
        Registry::new(HostConfig {
            backend_url: scope.backend_url,
            host_id: "host-one".into(),
            host_token: "local-test-token".into(),
            environment: scope.environment,
            org_id: scope.org_id,
            owner_id: scope.principal_id,
            isi: None,
        })
    }
    fn connection(mode: &str) -> Connection {
        serde_json::from_value(json!({
            "id":"connection-one", "name":"Local fixture", "description":"", "org_id":"org-one", "owner_id":"ca:owner", "environment":"test-one", "transport":"http", "url":"http://127.0.0.1:4392/mcp", "host_id":"host-one", "command":null, "args":[], "auth_mode":mode, "visibility":"invited", "status":"checking", "can_manage":true, "account":null, "created_at":0, "updated_at":0, "version":1
        })).unwrap()
    }

    #[test]
    fn registration_is_explicit_scoped_and_preserves_credentials() {
        let mut registry = registry();
        let connection = connection("shared");
        let scope = scope();
        assert!(register_connection(&mut registry, &connection, &scope, BTreeMap::new()).unwrap());
        connect_account(
            &mut registry,
            &connection,
            &scope,
            json!({"kind":"bearer", "secret":"owner-token"}),
        )
        .unwrap();
        let before = serde_json::to_value(&registry).unwrap();
        assert!(!register_connection(&mut registry, &connection, &scope, BTreeMap::new()).unwrap());
        assert_eq!(before, serde_json::to_value(&registry).unwrap());
        for altered in [
            Scope {
                environment: "production".into(),
                ..scope.clone()
            },
            Scope {
                org_id: "org-two".into(),
                ..scope.clone()
            },
            Scope {
                backend_url: "http://localhost:4380".into(),
                ..scope.clone()
            },
            Scope {
                principal_id: "si:other".into(),
                ..scope.clone()
            },
        ] {
            assert!(
                register_connection(&mut registry, &connection, &altered, BTreeMap::new()).is_err()
            );
            assert!(unregister_connection(&mut registry, &connection, &altered).is_err());
        }
        let mut wrong_host = connection.clone();
        wrong_host.host_id = Some("host-two".into());
        assert!(register_connection(&mut registry, &wrong_host, &scope, BTreeMap::new()).is_err());
        assert_eq!(before, serde_json::to_value(&registry).unwrap());
    }

    #[test]
    fn per_user_credentials_do_not_fall_back_or_cross_accounts() {
        let mut registry = registry();
        let connection = connection("per-user");
        let owner = scope();
        register_connection(&mut registry, &connection, &owner, BTreeMap::new()).unwrap();
        connect_account(
            &mut registry,
            &connection,
            &owner,
            json!({"secret":"owner-token"}),
        )
        .unwrap();
        let invited = Scope {
            principal_id: "si:invited".into(),
            ..owner.clone()
        };
        assert_eq!(
            account_status(&registry, &connection, &invited).unwrap()["connected"],
            false
        );
        connect_account(
            &mut registry,
            &connection,
            &invited,
            json!({"secret":"invited-token"}),
        )
        .unwrap();
        disconnect_account(&mut registry, &connection, &owner).unwrap();
        assert_eq!(
            account_status(&registry, &connection, &owner).unwrap()["connected"],
            false
        );
        assert_eq!(
            account_status(&registry, &connection, &invited).unwrap()["connected"],
            true
        );
        assert_eq!(
            registry.connections[&connection.id].personal_accounts["si:invited"]
                .bearer_token
                .as_deref(),
            Some("invited-token")
        );
        assert!(
            connect_account(&mut registry, &connection, &invited, json!({"kind":"host"})).is_err()
        );
    }

    #[test]
    fn shared_disconnect_is_explicit_and_only_owner_mutates() {
        let mut registry = registry();
        let connection = connection("shared");
        let owner = scope();
        register_connection(&mut registry, &connection, &owner, BTreeMap::new()).unwrap();
        let invited = Scope {
            principal_id: "si:invited".into(),
            ..owner.clone()
        };
        assert!(
            connect_account(
                &mut registry,
                &connection,
                &invited,
                json!({"secret":"invited-token"})
            )
            .is_err()
        );
        assert!(disconnect_account(&mut registry, &connection, &invited).is_err());
        assert_eq!(
            account_status(&registry, &connection, &invited).unwrap()["uses_host_account"],
            true
        );
        disconnect_account(&mut registry, &connection, &owner).unwrap();
        assert_eq!(
            account_status(&registry, &connection, &invited).unwrap()["uses_host_account"],
            false
        );
        connect_account(&mut registry, &connection, &owner, json!({"kind":"host"})).unwrap();
        assert_eq!(
            account_status(&registry, &connection, &owner).unwrap()["disconnected"],
            false
        );
        let mut stale = connection.clone();
        stale.auth_mode = "per-user".into();
        assert!(account_status(&registry, &stale, &owner).is_err());
        assert!(connect_account(&mut registry, &stale, &owner, json!({"secret":"token"})).is_err());
    }

    #[test]
    fn account_inputs_enforce_transport_and_do_not_expose_secrets() {
        for (input, transport, mode) in [
            (json!({"secret":"bad\r\nvalue"}), "http", "shared"),
            (
                json!({"kind":"header", "header_name":"bad name", "secret":"value"}),
                "http",
                "shared",
            ),
            (
                json!({"kind":"header", "header_name":"x-token", "secret":"value"}),
                "stdio",
                "shared",
            ),
            (
                json!({"kind":"env", "env":{"TOKEN":"value"}}),
                "http",
                "shared",
            ),
            (
                json!({"kind":"env", "env":{"BAD=NAME":"value"}}),
                "stdio",
                "per-user",
            ),
            (json!({"kind":"env", "env":{}}), "stdio", "per-user"),
            (json!({"secret":"value"}), "http", "none"),
        ] {
            assert!(account_from_input(input, transport, mode).is_err());
        }
        let account = account_from_input(
            json!({"kind":"env", "env":{"API_TOKEN":"private-value"}}),
            "stdio",
            "per-user",
        )
        .unwrap();
        assert_eq!(account.env["API_TOKEN"], "private-value");
        let mut registry = registry();
        let connection = connection("shared");
        let scope = scope();
        register_connection(&mut registry, &connection, &scope, BTreeMap::new()).unwrap();
        let output = connect_account(
            &mut registry,
            &connection,
            &scope,
            json!({"secret":"private-value"}),
        )
        .unwrap();
        assert!(!output.to_string().contains("private-value"));
        assert!(
            !account_status(&registry, &connection, &scope)
                .unwrap()
                .to_string()
                .contains("private-value")
        );
    }

    #[test]
    fn stdio_registration_keeps_arguments_literal_and_rejects_relative_commands() {
        let mut registry = registry();
        let mut connection = connection("per-user");
        let scope = scope();
        connection.transport = "stdio".into();
        connection.url = None;
        connection.command = Some("relative-binary".into());
        assert!(register_connection(&mut registry, &connection, &scope, BTreeMap::new()).is_err());
        connection.command = Some(std::env::current_exe().unwrap().to_string_lossy().into());
        connection.args = vec!["$(never-a-shell)".into(), "argument with spaces".into()];
        let env = BTreeMap::from([("TOKEN".into(), "value".into())]);
        register_connection(&mut registry, &connection, &scope, env.clone()).unwrap();
        match &registry.connections[&connection.id].endpoint {
            Endpoint::Stdio {
                command,
                args,
                env: stored,
                ..
            } => {
                assert_eq!(Some(command), connection.command.as_ref());
                assert_eq!(args, &connection.args);
                assert_eq!(stored, &env);
            }
            _ => panic!("expected stdio"),
        }
    }

    #[tokio::test]
    async fn runtime_status_and_stop_use_only_the_explicit_registry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("registry.json");
        registry().save(&path).unwrap();
        let result = status(&path).unwrap();
        assert_eq!(result["running"], false);
        assert_eq!(result["connected_recently"], false);
        assert_eq!(stop(&path).await.unwrap()["running"], false);
        assert!(!directory.path().join("stop.request").exists());
        assert!(start(&path, Path::new("relative-mcport")).await.is_err());
        fs::write(
            directory.path().join("daemon-status.json"),
            vec![b' '; 65537],
        )
        .unwrap();
        assert!(status(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn edit_lock_is_private_and_does_not_follow_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("registry.json");
        let guard = lock_registry(&path).unwrap();
        assert_eq!(
            guard.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(guard);
        fs::remove_file(directory.path().join("registry-edit.lock")).unwrap();
        let outside = directory.path().join("outside");
        fs::write(&outside, b"untouched").unwrap();
        symlink(&outside, directory.path().join("registry-edit.lock")).unwrap();
        assert!(lock_registry(&path).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"untouched");
    }
    #[tokio::test]
    async fn runtime_rejects_ambiguous_files_and_wrong_running_host() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("registry.json");
        registry().save(&path).unwrap();
        let other = directory.path().join("other.json");
        registry().save(&other).unwrap();
        assert!(status(&other).is_err());
        assert!(stop(&other).await.is_err());
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.path().join("daemon.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        fs::write(
            directory.path().join("daemon-status.json"),
            br#"{"host_id":"another-host","last_poll_at":0}"#,
        )
        .unwrap();
        assert!(status(&path).is_err());
        assert!(stop(&path).await.is_err());
        assert!(!directory.path().join("stop.request").exists());
        FileExt::unlock(&lock).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_rejects_linked_managed_ancestors_and_status_files() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("actual");
        fs::create_dir(&real).unwrap();
        let parent = directory.path().join(".mcport");
        fs::create_dir(&parent).unwrap();
        symlink(&real, parent.join("context")).unwrap();
        assert!(lock_registry(&parent.join("context/host/registry.json")).is_err());
        let path = real.join("registry.json");
        registry().save(&path).unwrap();
        let outside = directory.path().join("outside.json");
        fs::write(&outside, b"{}").unwrap();
        symlink(&outside, real.join("daemon-status.json")).unwrap();
        assert!(status(&path).is_err());
        let link = directory.path().join("host-link");
        symlink(&real, &link).unwrap();
        assert!(lock_registry(&link.join("registry.json")).is_err());
    }
}
