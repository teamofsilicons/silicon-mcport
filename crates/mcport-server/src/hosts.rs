//! Local MCP hosts: registered machines whose daemon long-polls for jobs.
//! A host belongs to the account that registered it; the custodian of a Silicon
//! owner may list, show and delete it, but only the owner adds connections to it.
use crate::{
    accounts::{self, AccountRow},
    auth::{self, Auth, Live},
    error::{Error, Result},
    execution::{self, CallRecord},
    state::{App, hash, now, secret},
    store::ENV,
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use mcport_core::{
    Actor, Host, HostJob, HostJobResult, HostPoll, HostPollResult, HostRegistration,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct HostData {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub owner_uuid: String,
    pub created_at: i64,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct HostRecord {
    pub host: HostData,
    pub token_hash: String,
    pub last_seen: i64,
    pub registered: Vec<String>,
    pub capabilities: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}
impl HostRecord {
    /// The grouping value of a host registered before 0.3.0 (kept by link-identities).
    pub fn legacy_org(&self) -> Option<String> {
        self.other
            .get("legacy")
            .and_then(|legacy| legacy.get("fields"))
            .and_then(|fields| fields.get("host.org_id"))
            .or_else(|| self.host.other.get("org_id"))
            .and_then(Value::as_str)
            .filter(|org| !org.is_empty())
            .map(str::to_owned)
    }
    /// Whether the host's daemon still keys personal accounts by pre-0.3.0 ids: a
    /// host registered before 0.3.0 whose daemon has not reported registry v2.
    pub fn legacy_registry(&self) -> bool {
        self.legacy_org().is_some()
            && self
                .capabilities
                .get("registry_version")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                < 2
    }
}
/// Keys a host registry may use for `uuid`'s personal account: the uuid, plus
/// the account's linked pre-0.3.0 ids while the registry is not migrated.
pub fn registry_keys(app: &App, uuid: &str, legacy: bool) -> Result<Vec<String>> {
    let mut keys = vec![uuid.to_owned()];
    if legacy {
        keys.extend(app.store.legacy_ids(uuid)?);
    }
    Ok(keys)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HostAccess {
    Owner,
    Custodian,
}
async fn host_access(app: &App, h: &HostRecord, who: &AccountRow) -> Result<Option<HostAccess>> {
    if h.host.owner_uuid.is_empty() {
        return Ok(None);
    }
    if h.host.owner_uuid == who.uuid {
        return Ok(Some(HostAccess::Owner));
    }
    if let Some(owner) = app.store.account(&h.host.owner_uuid)?
        && accounts::looks_after(app, who, &owner).await
    {
        return Ok(Some(HostAccess::Custodian));
    }
    Ok(None)
}
/// A host the caller owns or looks after, by exact id or name.
pub async fn resolve(app: &App, who: &AccountRow, name: &str) -> Result<(HostRecord, HostAccess)> {
    if let Some(h) = app.store.get::<HostRecord>("host", name)?
        && let Some(access) = host_access(app, &h, who).await?
    {
        return Ok((h, access));
    }
    let mut candidates = Vec::new();
    for h in app.store.list::<HostRecord>("host", Some(ENV))? {
        if h.host.name == name
            && let Some(access) = host_access(app, &h, who).await?
        {
            candidates.push((h, access));
        }
    }
    if candidates
        .iter()
        .any(|(_, access)| *access == HostAccess::Owner)
    {
        candidates.retain(|(_, access)| *access == HostAccess::Owner);
    }
    if candidates.len() > 1 {
        return Err(Error::new(
            409,
            "ambiguous_name",
            format!("More than one host you look after is named `{name}`."),
            "Use the host ID shown by host ls.",
        ));
    }
    candidates.pop().ok_or_else(Error::missing)
}
/// A host the caller itself registered (required to add connections to it).
pub async fn owned(app: &App, who: &AccountRow, name: &str) -> Result<HostRecord> {
    match resolve(app, who, name).await? {
        (h, HostAccess::Owner) => Ok(h),
        _ => Err(Error::new(
            403,
            "access_denied",
            "Only the account that registered a host can add connections to it.",
            "Ask the host's owner to add the connection, or register your own host.",
        )),
    }
}
fn view(app: &App, h: HostRecord, can_manage: bool) -> Host {
    Host {
        id: h.host.id,
        name: h.host.name,
        owner: accounts::reference(app, &h.host.owner_uuid),
        online: h.last_seen > now() - 35,
        last_seen: (h.last_seen > 0).then_some(h.last_seen),
        created_at: h.host.created_at,
        can_manage,
    }
}
pub async fn list(State(app): State<App>, a: Auth) -> Result<Json<Value>> {
    let mut out = Vec::new();
    for h in app.store.list::<HostRecord>("host", Some(ENV))? {
        if host_access(&app, &h, &a.account).await?.is_some() {
            out.push(view(&app, h, true));
        }
    }
    Ok(Json(json!({"data":out})))
}
#[derive(Deserialize)]
pub struct HostInput {
    pub name: String,
}
pub async fn create(
    State(app): State<App>,
    Live(a): Live,
    Json(input): Json<HostInput>,
) -> Result<Json<Value>> {
    if input.name.is_empty()
        || input.name.len() > 80
        || !input
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(Error::bad(
            "Host names must contain 1–80 letters, digits, hyphens or underscores.",
        ));
    }
    let token = secret("mph_");
    let owner = a.uuid().to_owned();
    let mut record = HostRecord {
        host: HostData {
            id: String::new(),
            name: input.name,
            owner_uuid: owner.clone(),
            created_at: now(),
            other: Map::new(),
        },
        token_hash: hash(&token),
        last_seen: 0,
        registered: vec![],
        capabilities: BTreeMap::new(),
        other: Map::new(),
    };
    let name = record.host.name.clone();
    let (record, _) =
        app.store
            .create_public("host", ENV, &owner, &owner, Some(&name), None, |id| {
                record.host.id = id;
                record
            })?;
    Ok(Json(
        json!({"data":HostRegistration{host:view(&app, record, true),host_token:token}}),
    ))
}
pub async fn get(State(app): State<App>, a: Auth, Path(name): Path<String>) -> Result<Json<Value>> {
    let (h, _) = resolve(&app, &a.account, &name).await?;
    Ok(Json(json!({"data":view(&app, h, true)})))
}
pub async fn remove(
    State(app): State<App>,
    Live(a): Live,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let (h, _) = resolve(&app, &a.account, &name).await?;
    let lock = app.lock(&format!("host:{}", h.host.id));
    let _guard = lock.lock().await;
    resolve(&app, &a.account, &h.host.id).await?;
    app.store.delete("host", &h.host.id)?;
    for c in app
        .store
        .list::<crate::connections::ConnectionRecord>("connection", None)?
    {
        if c.host_id.as_deref() == Some(&h.host.id) {
            execution::invalidate_connection(&app, &c.id)?;
        }
    }
    Ok(Json(json!({"data":{"deleted":true}})))
}
/// Host daemons authenticate with their host token (`mph_…`), never a user token.
fn authenticate(app: &App, headers: &HeaderMap, id: &str) -> Result<HostRecord> {
    use subtle::ConstantTimeEq;
    let h = app
        .store
        .get::<HostRecord>("host", id)?
        .ok_or_else(host_rejected)?;
    let token = auth::bearer(headers).ok_or_else(host_rejected)?;
    if !bool::from(hash(&token).as_bytes().ct_eq(h.token_hash.as_bytes())) {
        return Err(host_rejected());
    }
    Ok(h)
}
fn host_rejected() -> Error {
    Error::new(
        401,
        "host_authentication_required",
        "This host token is missing, wrong or belongs to a deleted host.",
        "Register the host again with mcport host new on that machine.",
    )
}
fn active_job_ids(host: &HostRecord) -> HashSet<String> {
    host.capabilities
        .get("active_job_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
fn available_slots(host: &HostRecord, calls: &[CallRecord]) -> usize {
    let maximum = host
        .capabilities
        .get("max_concurrent_jobs")
        .and_then(Value::as_u64)
        .unwrap_or(4)
        .min(64) as usize;
    let reported = host
        .capabilities
        .get("active_jobs")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(64) as usize;
    let leased: HashSet<String> = calls
        .iter()
        .filter(|record| {
            record.host_id.as_deref() == Some(&host.host.id)
                && record.invocation.status == "running"
        })
        .map(|record| record.invocation.id.clone())
        .collect();
    let active = active_job_ids(host);
    // Terminal jobs may still be cancelling on the daemon. Count their slots
    // plus any new durable leases that its most recent heartbeat has not seen.
    let occupied = reported.max(active.len()) + leased.difference(&active).count();
    maximum.saturating_sub(occupied)
}
/// A terminal transition wins even if it raced an earlier authorization read.
fn try_lease(app: &App, candidate: &CallRecord) -> Result<Option<CallRecord>> {
    let mut leased = false;
    let result = app
        .store
        .update::<CallRecord>("call", &candidate.invocation.id, |current| {
            if current.invocation.status != "queued"
                || current.expires_at <= now()
                || current.host_id != candidate.host_id
                || current.caller_uuid != candidate.caller_uuid
                || current.fingerprint != candidate.fingerprint
            {
                return false;
            }
            current.invocation.status = "running".into();
            leased = true;
            true
        })?;
    Ok(result.filter(|_| leased))
}
/// The job identity sent to the daemon, with the transition fields daemons
/// released before 0.3.0 read (`principal_id` keys personal accounts in their
/// registry; `org_id` must equal the registry's grouping value).
fn job_actor(app: &App, host: &HostRecord, caller: &AccountRow) -> Result<Actor> {
    let legacy = host.legacy_registry();
    let principal_id = if legacy {
        app.store
            .legacy_ids(&caller.uuid)?
            .into_iter()
            .next()
            .unwrap_or_else(|| caller.uuid.clone())
    } else {
        caller.uuid.clone()
    };
    Ok(Actor {
        uuid: caller.uuid.clone(),
        id: caller.id.clone(),
        kind: caller.kind.clone(),
        display_name: caller.display_name.clone(),
        principal_id,
        identity_kind: caller.kind.clone(),
        org_id: host.legacy_org().filter(|_| legacy).unwrap_or_default(),
    })
}
pub async fn poll(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<HostPoll>,
) -> Result<Json<Value>> {
    if input.registered_connections.len() > 2000 || input.capabilities.len() > 2100 {
        return Err(Error::bad("Host registration exceeds the supported limit."));
    }
    if input
        .capabilities
        .get("active_job_ids")
        .is_some_and(|value| {
            value.as_array().is_none_or(|ids| {
                ids.len() > 64
                    || ids
                        .iter()
                        .any(|id| id.as_str().is_none_or(|id| id.is_empty() || id.len() > 128))
            })
        })
    {
        return Err(Error::bad("Host active job IDs are invalid."));
    }
    let host_lock = app.lock(&format!("host:{id}"));
    let guard = host_lock.lock().await;
    let mut host = authenticate(&app, &headers, &id)?;
    // A connector can only register its owner's explicitly configured connections.
    let mut registered = Vec::new();
    for cid in input.registered_connections {
        if let Some(c) = app
            .store
            .get::<crate::connections::ConnectionRecord>("connection", &cid)?
            && c.host_id.as_deref() == Some(&id)
            && !c.owner_uuid.is_empty()
            && c.owner_uuid == host.host.owner_uuid
        {
            registered.push(cid)
        }
    }
    host.registered = registered;
    host.capabilities = input.capabilities;
    host.last_seen = now();
    app.store.put(
        "host",
        &id,
        ENV,
        &host.host.owner_uuid,
        &host.host.owner_uuid,
        Some(&host.host.name),
        &host,
        None,
    )?;
    drop(guard);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let notified = app.jobs.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let current_host = authenticate(&app, &headers, &id)?;
        let active_ids = active_job_ids(&current_host);
        let mut jobs = Vec::new();
        let mut cancelled = Vec::new();
        for r in app.store.list::<CallRecord>("call", None)? {
            if r.host_id.as_deref() != Some(&id) {
                continue;
            }
            if !r.pending() {
                // Acknowledge only jobs the daemon still reports running; old
                // cancellation history must not make every long poll return.
                if active_ids.contains(&r.invocation.id) {
                    cancelled.push(r.invocation.id.clone());
                }
                continue;
            }
            if r.invocation.status == "running" && r.expires_at <= now() {
                let mut error = Error::new(
                    408,
                    "expired",
                    "This local request exceeded its execution deadline.",
                    "Inspect the provider before repeating an action.",
                );
                error.1.outcome_unknown = true;
                execution::finish(
                    &app,
                    &r.invocation.id,
                    HostJobResult {
                        result: None,
                        error: Some(error.1),
                    },
                )
                .await?;
                if active_ids.contains(&r.invocation.id) {
                    cancelled.push(r.invocation.id.clone());
                }
                continue;
            }
            if r.invocation.status != "queued" {
                continue;
            }
            if r.expires_at <= now() {
                execution::finish(
                    &app,
                    &r.invocation.id,
                    HostJobResult {
                        result: None,
                        error: Some(
                            Error::new(
                                408,
                                "expired",
                                "This local request expired before dispatch.",
                                "Start a new request when its host is available.",
                            )
                            .1,
                        ),
                    },
                )
                .await?;
                continue;
            }
            if !current_host
                .registered
                .contains(&r.invocation.connection_id)
            {
                continue;
            }
            let calls = app.store.list::<CallRecord>("call", None)?;
            if available_slots(&current_host, &calls) == 0 {
                continue;
            }
            let (a, c) = match execution::revalidate(&app, &r).await {
                Ok(v) => v,
                Err(e) => {
                    execution::finish(
                        &app,
                        &r.invocation.id,
                        HostJobResult {
                            result: None,
                            error: Some(e.1),
                        },
                    )
                    .await?;
                    continue;
                }
            };
            // Concurrent pollers share the capacity check and lease boundary.
            let lease_lock = app.lock(&format!("host-lease:{id}"));
            let _lease_guard = lease_lock.lock().await;
            // No await between the final local state checks and the durable lease.
            let fresh_host = authenticate(&app, &headers, &id)?;
            let Some(fresh_connection) = app
                .store
                .get::<crate::connections::ConnectionRecord>("connection", &c.id)?
            else {
                continue;
            };
            if !fresh_host.registered.contains(&c.id)
                || fresh_connection.version != r.connection_version
            {
                continue;
            }
            let calls = app.store.list::<CallRecord>("call", None)?;
            if available_slots(&fresh_host, &calls) == 0 {
                continue;
            }
            if let Some(t) = &r.invocation.tool_name
                && !crate::connections::allowed_tool(&app, &fresh_connection, a.uuid(), t)?
            {
                continue;
            }
            let actor = job_actor(&app, &fresh_host, &a.account)?;
            let Some(current) = try_lease(&app, &r)? else {
                continue;
            };
            jobs.push(HostJob {
                id: current.invocation.id,
                connection_id: c.id,
                method: current.invocation.method,
                params: current.params,
                timeout_ms: current.timeout_ms,
                expires_at: current.expires_at,
                actor,
            });
            if jobs.len() >= 16 {
                break;
            }
        }
        cancelled.truncate(2000);
        if !jobs.is_empty() || !cancelled.is_empty() || tokio::time::Instant::now() >= deadline {
            return Ok(Json(json!({"data":HostPollResult{jobs,cancelled}})));
        }
        tokio::select! {_=&mut notified=>{},_=tokio::time::sleep_until(deadline)=>{}}
    }
}
pub async fn complete(
    State(app): State<App>,
    headers: HeaderMap,
    Path((host_id, job_id)): Path<(String, String)>,
    Json(mut result): Json<HostJobResult>,
) -> Result<Json<Value>> {
    authenticate(&app, &headers, &host_id)?;
    let r = app
        .store
        .get::<CallRecord>("call", &job_id)?
        .ok_or_else(Error::missing)?;
    if r.host_id.as_deref() != Some(&host_id) {
        return Err(Error::denied());
    }
    if r.invocation.status == "queued" {
        return Err(Error::new(
            409,
            "job_not_leased",
            "This job has not been dispatched to the host.",
            "Only complete a job returned by poll.",
        ));
    }
    if r.pending() {
        match execution::revalidate(&app, &r).await {
            Ok((a, c)) => {
                if r.invocation.method == "tools/list"
                    && let Some(v) = &mut result.result
                {
                    execution::decorate_tools(&app, &c, a.uuid(), v)?;
                }
            }
            Err(e) => {
                result = HostJobResult {
                    result: None,
                    error: Some(e.1),
                }
            }
        }
    }
    execution::finish(&app, &job_id, result).await?;
    Ok(Json(json!({"data":{"accepted":true}})))
}
#[derive(Deserialize)]
pub struct Progress {
    pub progress: Value,
}
pub async fn progress(
    State(app): State<App>,
    headers: HeaderMap,
    Path((host_id, job_id)): Path<(String, String)>,
    Json(input): Json<Progress>,
) -> Result<Json<Value>> {
    authenticate(&app, &headers, &host_id)?;
    if serde_json::to_vec(&input.progress)?.len() > 65536 {
        return Err(Error::bad("Progress payload exceeds 64 KiB."));
    }
    let r = app
        .store
        .get::<CallRecord>("call", &job_id)?
        .ok_or_else(Error::missing)?;
    if r.host_id.as_deref() != Some(&host_id) {
        return Err(Error::denied());
    }
    app.store.update::<CallRecord>("call", &job_id, |current| {
        if current.invocation.status != "running" {
            return false;
        }
        current.progress = Some(input.progress);
        true
    })?;
    Ok(Json(json!({"data":{"accepted":true}})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        execution::InvocationData,
        test_support::{Fixture, fixture},
    };
    use axum::http::StatusCode;

    fn record(owner: &str) -> HostRecord {
        HostRecord {
            host: HostData {
                id: "host".into(),
                name: "desktop".into(),
                owner_uuid: owner.into(),
                created_at: now(),
                other: Map::new(),
            },
            token_hash: hash("host-token"),
            last_seen: now(),
            registered: vec!["connection".into()],
            capabilities: BTreeMap::from([
                ("max_concurrent_jobs".into(), json!(4)),
                ("active_jobs".into(), json!(0)),
                ("active_job_ids".into(), json!([])),
            ]),
            other: Map::new(),
        }
    }
    fn stored(f: &Fixture) -> HostRecord {
        let host = record("Ada");
        f.app
            .store
            .put(
                "host",
                "host",
                ENV,
                "Ada",
                "Ada",
                Some("desktop"),
                &host,
                Some(0),
            )
            .unwrap();
        host
    }
    fn call(id: &str, status: &str) -> CallRecord {
        CallRecord {
            invocation: InvocationData {
                id: id.into(),
                connection_id: "connection".into(),
                connection_name: "fixture".into(),
                method: "tools/call".into(),
                tool_name: Some("echo".into()),
                status: status.into(),
                created_at: now(),
                ..Default::default()
            },
            caller_uuid: "Ada".into(),
            host_id: Some("host".into()),
            params: json!({"name":"echo","arguments":{}}),
            timeout_ms: 30000,
            expires_at: now() + 30,
            connection_version: 1,
            fingerprint: format!("fingerprint-{id}"),
            ..Default::default()
        }
    }
    fn headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        headers
    }

    #[tokio::test]
    async fn host_tokens_cannot_cross_hosts_and_user_tokens_are_not_host_tokens() {
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        stored(&f);
        assert!(authenticate(&f.app, &headers("host-token"), "host").is_ok());
        assert!(authenticate(&f.app, &headers("host-token"), "other-host").is_err());
        assert!(authenticate(&f.app, &headers("unrelated-token"), "host").is_err());
        let user = f.token("Ada");
        let error = authenticate(&f.app, &headers(&user), "host").err().unwrap();
        assert_eq!(error.1.code, "host_authentication_required");
        // A testing header from older daemons is ignored, not a separate world.
        let mut legacy = headers("host-token");
        legacy.insert(
            "x-mcport-test",
            "11111111-1111-4111-8111-111111111111".parse().unwrap(),
        );
        assert!(authenticate(&f.app, &legacy, "host").is_ok());
    }
    #[tokio::test]
    async fn capacity_counts_cancelling_jobs_and_unacknowledged_leases() {
        let f = fixture().await;
        let mut host = stored(&f);
        assert_eq!(available_slots(&host, &[call("one", "running")]), 3);
        host.capabilities.insert("active_jobs".into(), json!(2));
        host.capabilities
            .insert("active_job_ids".into(), json!(["one", "cancelled"]));
        assert_eq!(
            available_slots(
                &host,
                &[
                    call("one", "running"),
                    call("new-lease", "running"),
                    call("cancelled", "cancelled")
                ]
            ),
            1
        );
        host.capabilities.insert("active_jobs".into(), json!(4));
        assert_eq!(available_slots(&host, &[]), 0);
        host.capabilities
            .insert("max_concurrent_jobs".into(), json!(0));
        assert_eq!(available_slots(&host, &[]), 0);
    }
    #[tokio::test]
    async fn concurrent_terminal_transitions_cannot_be_resurrected_by_leasing() {
        let f = fixture().await;
        stored(&f);
        for terminal in ["cancelled", "completed"] {
            for index in 0..20 {
                let record = call(&format!("{terminal}-{index}"), "queued");
                execution::save(&f.app, &record).unwrap();
                let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
                std::thread::scope(|scope| {
                    let other = f.app.clone();
                    let candidate = record.clone();
                    let start = barrier.clone();
                    scope.spawn(move || {
                        start.wait();
                        try_lease(&other, &candidate).unwrap();
                    });
                    barrier.wait();
                    f.app
                        .store
                        .update::<CallRecord>("call", &record.invocation.id, |current| {
                            current.invocation.status = terminal.into();
                            true
                        })
                        .unwrap();
                });
                let current = f
                    .app
                    .store
                    .get::<CallRecord>("call", &record.invocation.id)
                    .unwrap()
                    .unwrap();
                assert_eq!(current.invocation.status, terminal);
                assert!(try_lease(&f.app, &record).unwrap().is_none());
            }
        }
    }
    #[tokio::test]
    async fn cancelled_history_does_not_short_circuit_long_poll() {
        let f = fixture().await;
        stored(&f);
        execution::save(&f.app, &call("old-cancelled", "cancelled")).unwrap();
        let input = HostPoll {
            registered_connections: vec![],
            capabilities: BTreeMap::from([
                ("active_jobs".into(), json!(0)),
                ("active_job_ids".into(), json!([])),
            ]),
        };
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                poll(
                    State(f.app.clone()),
                    headers("host-token"),
                    Path("host".into()),
                    Json(input)
                )
            )
            .await
            .is_err()
        );
        let input = HostPoll {
            registered_connections: vec![],
            capabilities: BTreeMap::from([
                ("active_jobs".into(), json!(1)),
                ("active_job_ids".into(), json!(["old-cancelled"])),
            ]),
        };
        let Json(response) = poll(
            State(f.app.clone()),
            headers("host-token"),
            Path("host".into()),
            Json(input),
        )
        .await
        .unwrap();
        assert_eq!(response["data"]["cancelled"], json!(["old-cancelled"]));
    }

    async fn host_with_connection(
        f: &Fixture,
        owner: &str,
        auth_mode: &str,
    ) -> (String, String, String) {
        let (status, body) = f
            .as_(
                owner,
                "POST",
                "/api/v1/hosts",
                Some(json!({"name":"studio"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let host = body["data"]["host"]["id"].as_str().unwrap().to_owned();
        let token = body["data"]["host_token"].as_str().unwrap().to_owned();
        let (status, body) = f.as_(owner, "POST", "/api/v1/connections", Some(json!({"name":"figma","transport":"http","url":"http://127.0.0.1:3845/mcp","host_id":host,"auth_mode":auth_mode,"visibility":"circle"}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        (host, token, body["data"]["id"].as_str().unwrap().to_owned())
    }

    #[tokio::test]
    async fn custodians_see_and_delete_a_silicons_host_but_never_add_to_it() {
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        f.silicon("Scout", "si:scout", "Ada");
        f.carbon("Cy", "c:cy");
        let (host, _, _) = host_with_connection(&f, "Scout", "none").await;
        let (_, body) = f.as_("Ada", "GET", "/api/v1/hosts", None).await;
        assert_eq!(body["data"][0]["id"], host.as_str());
        assert_eq!(body["data"][0]["owner"]["id"], "si:scout");
        assert_eq!(
            f.as_("Cy", "GET", "/api/v1/hosts", None).await.1["data"],
            json!([])
        );
        assert_eq!(
            f.as_("Cy", "GET", &format!("/api/v1/hosts/{host}"), None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let (status, body) = f.as_("Ada", "POST", "/api/v1/connections", Some(json!({"name":"mine","transport":"http","url":"http://127.0.0.1:3845/mcp","host_id":host,"auth_mode":"none"}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(
            f.as_("Ada", "DELETE", &format!("/api/v1/hosts/{host}"), None)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            f.as_("Scout", "GET", &format!("/api/v1/hosts/{host}"), None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    /// Register `connection` through a long poll, make `caller` call it, and
    /// return the job the poll hands the daemon (then complete it).
    async fn dispatch(
        f: &Fixture,
        caller: &str,
        host: &str,
        token: &str,
        connection: &str,
        capabilities: Value,
    ) -> HostJob {
        let poll_body = json!({"registered_connections":[connection],"capabilities":capabilities});
        let poll = {
            let app = f.app.clone();
            let headers = headers(token);
            let host = host.to_owned();
            let input: HostPoll = serde_json::from_value(poll_body.clone()).unwrap();
            tokio::spawn(async move { poll(State(app), headers, Path(host), Json(input)).await })
        };
        for _ in 0..200 {
            if f.app
                .store
                .get::<HostRecord>("host", host)
                .unwrap()
                .unwrap()
                .registered
                .contains(&connection.to_owned())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pending = {
            let app = f.app.clone();
            let caller_token = f.token(caller);
            let connection = connection.to_owned();
            tokio::spawn(async move {
                use tower::ServiceExt;
                crate::router(app)
                    .oneshot(
                        axum::http::Request::post(format!("/api/v1/connections/{connection}/mcp"))
                            .header("authorization", format!("Bearer {caller_token}"))
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(
                                json!({"method":"tools/list","timeout_ms":5000}).to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap()
                    .status()
            })
        };
        let Json(reply) = tokio::time::timeout(Duration::from_secs(10), poll)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let job: HostJob = serde_json::from_value(reply["data"]["jobs"][0].clone()).unwrap();
        let (status, _) = f
            .call(
                "POST",
                &format!("/api/v1/hosts/{host}/jobs/{}/result", job.id),
                Some(token),
                Some(json!({"result":{"tools":[{"name":"echo"}]},"error":null})),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(pending.await.unwrap(), StatusCode::OK);
        job
    }

    #[tokio::test]
    async fn jobs_name_the_caller_and_keep_transition_fields_for_older_daemons() {
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        f.silicon("Scout", "si:scout", "Ada");
        let (host, token, connection) = host_with_connection(&f, "Ada", "none").await;
        let job = dispatch(&f, "Scout", &host, &token, &connection, json!({})).await;
        assert_eq!(job.actor.uuid, "Scout");
        assert_eq!(job.actor.id, "si:scout");
        assert_eq!(job.actor.kind, "silicon");
        assert_eq!(job.actor.principal_id, "Scout");
        assert_eq!(job.actor.org_id, "");
        // A host registered before 0.3.0 whose daemon still uses its old registry.
        f.app
            .store
            .update::<HostRecord>("host", &host, |h| {
                h.other.insert(
                    "legacy".into(),
                    json!({"fields":{"host.org_id":"tos","host.owner_id":"c:ada"}}),
                );
                true
            })
            .unwrap();
        f.app.store.db.lock().unwrap().execute(
            "INSERT INTO identity_links(iam_principal_id,iam_public_id,accounts_uuid,linked_at,source) VALUES('si:scout-old','si:scout-old','Scout',1,'test')",
            [],
        ).unwrap();
        let job = dispatch(&f, "Scout", &host, &token, &connection, json!({})).await;
        assert_eq!(job.actor.principal_id, "si:scout-old");
        assert_eq!(job.actor.org_id, "tos");
        assert_eq!(job.actor.identity_kind, "silicon");
        // Once the daemon reports a migrated registry, jobs use uuids only.
        let job = dispatch(
            &f,
            "Scout",
            &host,
            &token,
            &connection,
            json!({"registry_version":2}),
        )
        .await;
        assert_eq!(job.actor.principal_id, "Scout");
        assert_eq!(job.actor.org_id, "");
    }
}
