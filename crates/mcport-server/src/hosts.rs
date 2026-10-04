use crate::{
    auth::{self, Auth},
    error::{Error, Result},
    execution::{self, CallRecord},
    state::{App, hash, now, secret},
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use mcport_core::{
    Connection, Host, HostJob, HostJobResult, HostPoll, HostPollResult, HostRegistration,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct HostRecord {
    pub host: Host,
    pub token_hash: String,
    pub generation: i64,
    pub last_seen: i64,
    pub registered: Vec<String>,
    pub capabilities: BTreeMap<String, Value>,
}
pub fn resolve(app: &App, a: &Auth, name: &str) -> Result<HostRecord> {
    let hosts = if let Some(h) = app.store.get::<HostRecord>("host", name)?
        && h.host.environment == a.env()
        && h.host.org_id == a.actor().org_id
        && h.host.owner_id == a.actor().principal_id
    {
        vec![h]
    } else {
        app.store
            .list::<HostRecord>("host", Some(a.env()))?
            .into_iter()
            .filter(|h| h.host.name == name)
            .collect()
    };
    hosts
        .into_iter()
        .find(|h| {
            h.host.environment == a.env()
                && h.host.org_id == a.actor().org_id
                && h.host.owner_id == a.actor().principal_id
        })
        .ok_or_else(Error::missing)
}
fn view(mut h: HostRecord) -> Host {
    h.host.online = h.last_seen > now() - 35;
    h.host.last_seen = if h.last_seen > 0 {
        Some(h.last_seen)
    } else {
        None
    };
    h.host
}
pub async fn list(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let a = auth::authenticate(&app, &headers).await?;
    let out = app
        .store
        .list::<HostRecord>("host", Some(a.env()))?
        .into_iter()
        .filter(|h| h.host.owner_id == a.actor().principal_id && h.host.org_id == a.actor().org_id)
        .map(view)
        .collect::<Vec<_>>();
    Ok(Json(json!({"data":out})))
}
#[derive(Deserialize)]
pub struct HostInput {
    pub name: String,
}
pub async fn create(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<HostInput>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
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
    let host = Host {
        id: String::new(),
        name: input.name,
        owner_id: a.actor().principal_id.clone(),
        org_id: a.actor().org_id.clone(),
        environment: a.env().into(),
        online: false,
        last_seen: None,
        created_at: now(),
    };
    let mut record = HostRecord {
        host,
        token_hash: hash(&token),
        generation: a.session.generation,
        last_seen: 0,
        registered: vec![],
        capabilities: BTreeMap::new(),
    };
    let name = record.host.name.clone();
    let (record, _) = app.store.create_public(
        "host",
        a.env(),
        &a.actor().org_id,
        &a.actor().principal_id,
        Some(&name),
        None,
        |id| {
            record.host.id = id;
            record
        },
    )?;
    Ok(Json(
        json!({"data":HostRegistration{host:record.host,host_token:token}}),
    ))
}
pub async fn get(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::authenticate(&app, &headers).await?;
    Ok(Json(json!({"data":view(resolve(&app,&a,&name)?)})))
}
pub async fn remove(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let h = resolve(&app, &a, &name)?;
    let lock = app.lock(&format!("host:{}", h.host.id));
    let _guard = lock.lock().await;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    resolve(&app, &a, &h.host.id)?;
    app.store.delete("host", &h.host.id)?;
    for c in app.store.list::<Connection>("connection", Some(a.env()))? {
        if c.host_id.as_deref() == Some(&h.host.id) {
            execution::invalidate_connection(&app, &c.id)?;
        }
    }
    Ok(Json(json!({"data":{"deleted":true}})))
}
fn authenticate(app: &App, headers: &HeaderMap, id: &str) -> Result<HostRecord> {
    use subtle::ConstantTimeEq;
    let h = app
        .store
        .get::<HostRecord>("host", id)?
        .ok_or_else(Error::expired)?;
    let token = auth::bearer(headers).ok_or_else(Error::expired)?;
    if !bool::from(hash(&token).as_bytes().ct_eq(h.token_hash.as_bytes()))
        || auth::environment_header(headers)? != h.host.environment
    {
        return Err(Error::expired());
    }
    app.assert_generation(&h.host.environment, h.generation)?;
    Ok(h)
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
                || current.environment != candidate.environment
                || current.org_id != candidate.org_id
                || current.generation != candidate.generation
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
    let environment_guard = app
        .lock(&format!(
            "environment:{}",
            auth::environment_header(&headers)?
        ))
        .lock_owned()
        .await;
    let mut host = authenticate(&app, &headers, &id)?;
    // A connector can only register this owner's explicitly configured connections.
    let mut registered = Vec::new();
    for cid in input.registered_connections {
        if let Some(c) = app.store.get::<Connection>("connection", &cid)?
            && c.host_id.as_deref() == Some(&id)
            && c.environment == host.host.environment
            && c.org_id == host.host.org_id
            && c.owner_id == host.host.owner_id
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
        &host.host.environment,
        &host.host.org_id,
        &host.host.owner_id,
        Some(&host.host.name),
        &host,
        None,
    )?;
    drop(environment_guard);
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
        for r in app
            .store
            .list::<CallRecord>("call", Some(&host.host.environment))?
        {
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
            let calls = app
                .store
                .list::<CallRecord>("call", Some(&host.host.environment))?;
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
            let _environment_guard = auth::mutation_guard(&app, &a).await?;
            // No await between the final local state checks and the durable lease.
            let fresh_host = authenticate(&app, &headers, &id)?;
            let fresh_connection = crate::connections::resolve(&app, &a, &c.id, false)?;
            if !fresh_host.registered.contains(&c.id)
                || fresh_connection.version != r.connection_version
            {
                continue;
            }
            let calls = app
                .store
                .list::<CallRecord>("call", Some(&host.host.environment))?;
            if available_slots(&fresh_host, &calls) == 0 {
                continue;
            }
            if let Some(t) = &r.invocation.tool_name
                && !crate::connections::allowed_tool(&app, &fresh_connection, &a, t)?
            {
                continue;
            }
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
                actor: a.actor().clone(),
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
    let h = authenticate(&app, &headers, &host_id)?;
    let r = app
        .store
        .get::<CallRecord>("call", &job_id)?
        .ok_or_else(Error::missing)?;
    if r.host_id.as_deref() != Some(&host_id)
        || r.environment != h.host.environment
        || r.org_id != h.host.org_id
    {
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
                    execution::decorate_tools(&app, &c, &a, v)?;
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
    let h = authenticate(&app, &headers, &host_id)?;
    if serde_json::to_vec(&input.progress)?.len() > 65536 {
        return Err(Error::bad("Progress payload exceeds 64 KiB."));
    }
    let r = app
        .store
        .get::<CallRecord>("call", &job_id)?
        .ok_or_else(Error::missing)?;
    if r.host_id.as_deref() != Some(&host_id)
        || r.environment != h.host.environment
        || r.org_id != h.host.org_id
    {
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
    use mcport_core::Invocation;

    fn fixture() -> (App, tempfile::TempDir, HostRecord) {
        let directory = tempfile::tempdir().unwrap();
        let mut config = crate::state::Config::from_env();
        config.data_dir = directory.path().into();
        let app = App::new(config).unwrap();
        let host = HostRecord {
            host: Host {
                id: "host".into(),
                name: "desktop".into(),
                owner_id: "c:alice".into(),
                org_id: "tos".into(),
                environment: "production".into(),
                online: true,
                last_seen: Some(now()),
                created_at: now(),
            },
            token_hash: hash("host-token"),
            generation: 0,
            last_seen: now(),
            registered: vec!["connection".into()],
            capabilities: BTreeMap::from([
                ("max_concurrent_jobs".into(), json!(4)),
                ("active_jobs".into(), json!(0)),
                ("active_job_ids".into(), json!([])),
            ]),
        };
        app.store
            .put(
                "host",
                &host.host.id,
                "production",
                "tos",
                "c:alice",
                Some(&host.host.name),
                &host,
                Some(0),
            )
            .unwrap();
        (app, directory, host)
    }
    fn call(id: &str, status: &str) -> CallRecord {
        CallRecord {
            invocation: Invocation {
                id: id.into(),
                connection_id: "connection".into(),
                connection_name: "fixture".into(),
                actor_id: "c:alice".into(),
                execution_account_id: "c:alice".into(),
                method: "tools/call".into(),
                tool_name: Some("echo".into()),
                status: status.into(),
                created_at: now(),
                completed_at: None,
                result: None,
                error: None,
            },
            environment: "production".into(),
            org_id: "tos".into(),
            family: "family".into(),
            generation: 0,
            host_id: Some("host".into()),
            params: json!({"name":"echo","arguments":{}}),
            timeout_ms: 30000,
            expires_at: now() + 30,
            connection_version: 1,
            fingerprint: format!("fingerprint-{id}"),
            progress: None,
            telemetry_enabled: false,
        }
    }
    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer host-token".parse().unwrap());
        headers
    }

    #[test]
    fn host_tokens_cannot_cross_environment_or_host() {
        let (app, _directory, _host) = fixture();
        let mut headers = headers();
        assert!(authenticate(&app, &headers, "host").is_ok());
        assert!(authenticate(&app, &headers, "other-host").is_err());
        headers.insert("x-mcport-test", "other-test".parse().unwrap());
        assert!(authenticate(&app, &headers, "host").is_err());
        headers.remove("x-mcport-test");
        headers.insert("authorization", "Bearer unrelated-token".parse().unwrap());
        assert!(authenticate(&app, &headers, "host").is_err());
    }
    #[test]
    fn capacity_counts_cancelling_jobs_and_unacknowledged_leases() {
        let (_app, _directory, mut host) = fixture();
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
    #[test]
    fn concurrent_terminal_transitions_cannot_be_resurrected_by_leasing() {
        let (app, _directory, _host) = fixture();
        for terminal in ["cancelled", "completed"] {
            for index in 0..20 {
                let record = call(&format!("{terminal}-{index}"), "queued");
                execution::save(&app, &record).unwrap();
                let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
                std::thread::scope(|scope| {
                    let other = app.clone();
                    let candidate = record.clone();
                    let start = barrier.clone();
                    scope.spawn(move || {
                        start.wait();
                        try_lease(&other, &candidate).unwrap();
                    });
                    barrier.wait();
                    app.store
                        .update::<CallRecord>("call", &record.invocation.id, |current| {
                            current.invocation.status = terminal.into();
                            true
                        })
                        .unwrap();
                });
                let current = app
                    .store
                    .get::<CallRecord>("call", &record.invocation.id)
                    .unwrap()
                    .unwrap();
                assert_eq!(current.invocation.status, terminal);
                assert!(try_lease(&app, &record).unwrap().is_none());
            }
        }
    }
    #[tokio::test]
    async fn cancelled_history_does_not_short_circuit_long_poll() {
        let (app, _directory, _host) = fixture();
        execution::save(&app, &call("old-cancelled", "cancelled")).unwrap();
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
                    State(app.clone()),
                    headers(),
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
            State(app.clone()),
            headers(),
            Path("host".into()),
            Json(input),
        )
        .await
        .unwrap();
        assert_eq!(response["data"]["cancelled"], json!(["old-cancelled"]));
    }
}
