use crate::{
    auth::{self, Auth},
    connections as con,
    error::{Error, Result},
    state::{App, hash, id, now},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use mcport_core::{ApiError, Connection, HostJobResult, Invocation, RpcInput, RpcOutput};
use mcport_mcp::{Endpoint, ExecutionOptions, McpSession, NetworkPolicy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize)]
pub struct CallRecord {
    pub invocation: Invocation,
    pub environment: String,
    pub org_id: String,
    pub family: String,
    pub generation: i64,
    pub host_id: Option<String>,
    pub params: Value,
    pub timeout_ms: u64,
    pub expires_at: i64,
    pub connection_version: i64,
    pub fingerprint: String,
    pub progress: Option<Value>,
    #[serde(default)]
    pub telemetry_enabled: bool,
}
impl CallRecord {
    pub fn pending(&self) -> bool {
        matches!(self.invocation.status.as_str(), "queued" | "running")
    }
}
pub fn save(app: &App, r: &CallRecord) -> Result<()> {
    app.store.put(
        "call",
        &r.invocation.id,
        &r.environment,
        &r.org_id,
        &r.invocation.actor_id,
        None,
        r,
        None,
    )
}
pub async fn network_policy(app: &App, url: &str) -> Result<NetworkPolicy> {
    let parsed = url::Url::parse(url).map_err(|_| Error::bad("Invalid MCP URL."))?;
    if app
        .config
        .upstream_origins
        .contains(&parsed.origin().ascii_serialization())
    {
        return Ok(NetworkPolicy::LocalHost);
    }
    mcport_mcp::validate_public_url(url).await?;
    Ok(NetworkPolicy::PublicInternet)
}
pub fn invalidated_error(unknown: bool) -> ApiError {
    ApiError{code:"access_changed".into(),message:"Access, account configuration or connection state changed while this request was pending.".into(),recovery:Some("Refresh access and inspect activity before repeating an action.".into()),outcome_unknown:unknown}
}
pub fn invalidate_connection(app: &App, cid: &str) -> Result<()> {
    for r in app.store.list::<CallRecord>("call", None)? {
        if r.invocation.connection_id == cid && r.pending() {
            if let Some(cancel) = app
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&r.invocation.id)
            {
                cancel.cancel();
            }
            app.store
                .update::<CallRecord>("call", &r.invocation.id, |current| {
                    if !current.pending() {
                        return false;
                    }
                    current.invocation.error =
                        Some(invalidated_error(current.invocation.status == "running"));
                    current.invocation.status = "cancelled".into();
                    current.invocation.completed_at = Some(now());
                    true
                })?;
        }
    }
    app.jobs.notify_waiters();
    Ok(())
}
pub async fn revalidate(app: &App, r: &CallRecord) -> Result<(Auth, Connection)> {
    app.assert_generation(&r.environment, r.generation)?;
    let a = auth::authorize_family(app, &r.family, &r.environment).await?;
    if a.actor().org_id != r.org_id || a.actor().principal_id != r.invocation.actor_id {
        return Err(Error::denied());
    }
    let c = con::resolve(app, &a, &r.invocation.connection_id, false)?;
    if c.version != r.connection_version {
        return Err(Error::new(
            409,
            "connection_changed",
            "The connection changed while the request was pending.",
            "Refresh connection details and start a new request.",
        ));
    }
    if let Some(tool) = &r.invocation.tool_name
        && !con::allowed_tool(app, &c, &a, tool)?
    {
        return Err(Error::denied());
    }
    if !con::account_status(app, &c, &a)?.connected {
        return Err(Error::new(
            401,
            "provider_authentication_required",
            "This execution account is not connected.",
            "Connect your provider account or ask the shared account owner to reconnect.",
        ));
    }
    Ok((a, c))
}
fn result_response(r: &CallRecord) -> Result<Json<Value>> {
    if let Some(e) = &r.invocation.error {
        return Err(Error(StatusCode::BAD_GATEWAY, e.clone()));
    }
    if let Some(result) = &r.invocation.result {
        return Ok(Json(
            json!({"data":RpcOutput{call_id:r.invocation.id.clone(),result:result.clone()}}),
        ));
    }
    Err(Error::new(
        409,
        "operation_in_progress",
        format!(
            "Operation {} has already been accepted and is still running.",
            r.invocation.id
        ),
        "Use call show to inspect its outcome; do not repeat it with another idempotency key.",
    ))
}
async fn remote(app: &App, r: &CallRecord, cancellation: CancellationToken) -> Result<Value> {
    let (a, c) = revalidate(app, r).await?;
    let url = c.url.clone().ok_or_else(Error::internal)?;
    let policy = network_policy(app, &url).await?;
    let mut headers = BTreeMap::new();
    let mut bearer = None;
    if c.auth_mode != "none" {
        let g = crate::oauth::execution_grant(app, &c, &a).await?;
        if g.kind == "header" {
            headers.insert(g.header_name.ok_or_else(Error::internal)?, g.secret);
        } else {
            bearer = Some(g.secret);
        }
    }
    let endpoint = Endpoint::Http {
        url,
        headers,
        bearer_token: bearer,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(16);
    let progress_app = app.clone();
    let progress_id = r.invocation.id.clone();
    let progress_task = tokio::spawn(async move {
        while let Some(progress) = rx.recv().await {
            let _ = progress_app
                .store
                .update::<CallRecord>("call", &progress_id, |record| {
                    if !record.pending() {
                        return false;
                    }
                    record.progress = Some(progress);
                    true
                });
        }
    });
    let mut options = ExecutionOptions {
        timeout: Duration::from_millis(r.timeout_ms),
        network_policy: policy,
        cancellation,
        progress: Some(tx),
        ..Default::default()
    };
    let mut session = match McpSession::connect(&endpoint, &options).await {
        Ok(session) => session,
        Err(error) => {
            progress_task.abort();
            return Err(error.into());
        }
    };
    let outcome = async {
        if r.invocation.method == "tools/call" {
            let target = r
                .invocation
                .tool_name
                .as_deref()
                .ok_or_else(|| Error::bad("A tool name is required."))?;
            let schemas = session.tool_schemas(target, &options).await?;
            options.input_schema = Some(schemas.input_schema);
            options.output_schema = schemas.output_schema;
        }
        // Discovery may take time; re-check policy immediately before actual dispatch.
        let (current_actor, current_connection) = revalidate(app, r).await?;
        let mut result = session
            .request(&r.invocation.method, r.params.clone(), options)
            .await?;
        if r.invocation.method == "tools/list" {
            decorate_tools(app, &current_connection, &current_actor, &mut result)?;
        }
        Ok(result)
    }
    .await;
    session.close().await;
    progress_task.abort();
    outcome
}
pub fn decorate_tools(app: &App, c: &Connection, a: &Auth, result: &mut Value) -> Result<()> {
    if let Some(tools) = result.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            let enabled = con::allowed_tool(
                app,
                c,
                a,
                tool.get("name").and_then(Value::as_str).unwrap_or(""),
            )?;
            if let Some(map) = tool.as_object_mut() {
                map.insert("enabled".into(), json!(enabled));
            }
        }
    }
    Ok(())
}

pub async fn execute(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(input): Json<RpcInput>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let c = con::resolve(&app, &a, &name, false)?;
    if !matches!(
        input.method.as_str(),
        "tools/list"
            | "tools/call"
            | "resources/list"
            | "resources/templates/list"
            | "resources/read"
            | "prompts/list"
            | "prompts/get"
            | "completion/complete"
    ) {
        return Err(Error::bad(
            "This MCP method is not supported. Use tools, resources, prompts or completion.",
        ));
    }
    let params = if input.params.is_null() {
        json!({})
    } else {
        input.params
    };
    if !params.is_object() {
        return Err(Error::bad("MCP parameters must be a JSON object."));
    }
    let tool = if input.method == "tools/call" {
        Some(
            params
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Error::bad("A tool name is required."))?
                .to_owned(),
        )
    } else {
        None
    };
    if let Some(t) = &tool
        && !con::allowed_tool(&app, &c, &a, t)?
    {
        return Err(Error::new(
            403,
            "tool_disabled",
            "This tool is disabled for your account.",
            "Ask the connection owner to change its tool permissions.",
        ));
    }
    if !con::account_status(&app, &c, &a)?.connected {
        return Err(Error::new(
            401,
            "provider_authentication_required",
            "Your execution account is not connected.",
            "Run account connect for this connection.",
        ));
    }
    // MCP health is an advisory snapshot. A slow initialization or a provider
    // accepting only one process must still be callable within the call timeout.
    // Admission depends on the actual connector registration and current account.
    if let Some(host) = &c.host_id
        && app
            .store
            .get::<crate::hosts::HostRecord>("host", host)?
            .is_none_or(|h| h.last_seen <= now() - 35 || !h.registered.contains(&c.id))
    {
        return Err(Error::new(
            503,
            "host_offline",
            "The configured MCP host is offline or has not registered this connection.",
            "Start its host daemon and keep the MCP application running.",
        ));
    }
    let fingerprint = hash(&json!([c.id, input.method, params]).to_string());
    let call_id = if let Some(k) = input.idempotency_key {
        if k.is_empty() || k.len() > 255 {
            return Err(Error::bad("Idempotency key must contain 1–255 characters."));
        }
        hash(&json!([a.env(), a.actor().org_id, a.actor().principal_id, c.id, k]).to_string())
    } else {
        id()
    };
    let lock = app.lock(&format!("call:{call_id}"));
    let guard = lock.lock().await;
    let environment_guard = auth::mutation_guard(&app, &a).await?;
    let current_connection = con::resolve(&app, &a, &c.id, false)?;
    if current_connection.version != c.version
        || !con::account_status(&app, &current_connection, &a)?.connected
        || tool.as_deref().is_some_and(|name| {
            !con::allowed_tool(&app, &current_connection, &a, name).unwrap_or(false)
        })
    {
        return Err(Error::denied());
    }
    if let Some(r) = app.store.get::<CallRecord>("call", &call_id)? {
        if r.fingerprint != fingerprint {
            return Err(Error::new(
                409,
                "idempotency_conflict",
                "This key was used for a different request.",
                "Use a new key only for a new operation.",
            ));
        }
        return result_response(&r);
    }
    let timeout = input.timeout_ms.unwrap_or(120000).clamp(100, 600000);
    let r = CallRecord {
        invocation: Invocation {
            id: call_id.clone(),
            connection_id: c.id.clone(),
            connection_name: c.name.clone(),
            actor_id: a.actor().principal_id.clone(),
            execution_account_id: if c.auth_mode == "shared" {
                c.owner_id.clone()
            } else {
                a.actor().principal_id.clone()
            },
            method: input.method,
            tool_name: tool,
            status: if c.host_id.is_some() {
                "queued"
            } else {
                "running"
            }
            .into(),
            created_at: now(),
            completed_at: None,
            result: None,
            error: None,
        },
        environment: a.env().into(),
        org_id: a.actor().org_id.clone(),
        family: a.session.family.clone(),
        generation: a.session.generation,
        host_id: c.host_id.clone(),
        params,
        timeout_ms: timeout,
        expires_at: now() + timeout.div_ceil(1000) as i64,
        connection_version: c.version,
        fingerprint,
        progress: None,
        telemetry_enabled: !headers
            .get("x-mcport-telemetry")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| matches!(v, "false" | "off" | "0")),
    };
    save(&app, &r)?;
    record_execution(&app, &r, "backend", "dispatch", "pending");
    drop(environment_guard);
    drop(guard);
    if c.host_id.is_none() {
        let cancel = CancellationToken::new();
        app.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(call_id.clone(), cancel.clone());
        let task_app = app.clone();
        let task = r.clone();
        tokio::spawn(async move {
            let output = tokio::time::timeout(
                Duration::from_millis(task.timeout_ms),
                remote(&task_app, &task, cancel.clone()),
            )
            .await;
            let output = match output {
                Ok(output) => output,
                Err(_) => {
                    cancel.cancel();
                    let mut error = Error::new(
                        504,
                        "timeout",
                        "The MCP request timed out; its outcome may be unknown.",
                        "Inspect the provider before repeating an action.",
                    );
                    error.1.outcome_unknown = true;
                    Err(error)
                }
            };
            let result = match output {
                Ok(v) => HostJobResult {
                    result: Some(v),
                    error: None,
                },
                Err(e) => HostJobResult {
                    result: None,
                    error: Some(e.1),
                },
            };
            let _ = finish(&task_app, &task.invocation.id, result).await;
            task_app
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&task.invocation.id);
        });
    }
    app.jobs.notify_waiters();
    loop {
        let current = app
            .store
            .get::<CallRecord>("call", &call_id)?
            .ok_or_else(Error::missing)?;
        if !current.pending() {
            revalidate(&app, &current).await?;
            return result_response(&current);
        }
        if now() > current.expires_at {
            let unknown = current.invocation.status == "running";
            let error = ApiError {
                code: "timeout".into(),
                message: if unknown {
                    "The operation timed out after dispatch. Its outcome is unknown."
                } else {
                    "The local host did not accept the request before its deadline."
                }
                .into(),
                recovery: Some(
                    "Inspect activity and the upstream provider before trying another call.".into(),
                ),
                outcome_unknown: unknown,
            };
            finish(
                &app,
                &call_id,
                HostJobResult {
                    result: None,
                    error: Some(error),
                },
            )
            .await?;
        }
        tokio::select! {_=app.jobs.notified()=>{},_=tokio::time::sleep(Duration::from_millis(150))=>{}}
    }
}
pub async fn finish(app: &App, id: &str, result: HostJobResult) -> Result<()> {
    let lock = app.lock(&format!("call:{id}"));
    let _guard = lock.lock().await;
    if result.result.is_some() == result.error.is_some() {
        return Err(Error::bad("Return exactly one of result or error."));
    }
    let mut changed = false;
    let finished = app.store.update::<CallRecord>("call", id, |r| {
        if !r.pending() {
            return false;
        }
        r.invocation.status = if result.error.as_ref().is_some_and(|e| e.outcome_unknown) {
            "unknown"
        } else if result.error.is_some() {
            "failed"
        } else if result.result.as_ref().and_then(|v| v.get("isError")) == Some(&Value::Bool(true))
        {
            "tool_error"
        } else {
            "completed"
        }
        .into();
        r.invocation.result = result.result;
        r.invocation.error = result.error;
        r.invocation.completed_at = Some(now());
        changed = true;
        true
    })?;
    if changed && let Some(record) = finished {
        let outcome = if record.invocation.status == "completed" {
            "success"
        } else {
            "failure"
        };
        record_execution(
            app,
            &record,
            if record.host_id.is_some() {
                "daemon"
            } else {
                "backend"
            },
            "complete",
            outcome,
        );
    }
    app.jobs.notify_waiters();
    Ok(())
}
fn record_execution(app: &App, r: &CallRecord, source: &str, step: &str, outcome: &str) {
    if !r.telemetry_enabled || app.assert_generation(&r.environment, r.generation).is_err() {
        return;
    }
    let Ok(sessions) = app
        .store
        .list::<auth::StoredSession>("session", Some(&r.environment))
    else {
        return;
    };
    let Some(session) = sessions.into_iter().find(|s| {
        s.family == r.family
            && s.actor.org_id == r.org_id
            && s.actor.principal_id == r.invocation.actor_id
    }) else {
        return;
    };
    let auth = Auth { session };
    if auth::check_current(app, &auth).is_err() {
        return;
    }
    let operation = match r.invocation.method.as_str() {
        "tools/list" => "tool.list",
        "tools/call" => "tool.call",
        "resources/list" | "resources/templates/list" => "resource.list",
        "resources/read" => "resource.read",
        "prompts/list" => "prompt.list",
        "prompts/get" => "prompt.get",
        _ => "completion.complete",
    };
    let _ = crate::operations::record(
        app,
        &auth,
        &HeaderMap::new(),
        &crate::operations::TelemetryInput {
            source: source.into(),
            operation: operation.into(),
            step: step.into(),
            outcome: outcome.into(),
            progress: None,
            correlation_id: uuid::Uuid::parse_str(&r.invocation.id)
                .ok()
                .map(|id| id.to_string()),
            duration_ms: r
                .invocation
                .completed_at
                .map(|end| ((end - r.invocation.created_at).max(0) as u64 * 1000).min(86_400_000)),
        },
    );
}
#[derive(Deserialize)]
pub struct CallQuery {
    connection_id: Option<String>,
}
pub async fn list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<CallQuery>,
) -> Result<Json<Value>> {
    let a = auth::authenticate(&app, &headers).await?;
    let cid = query
        .connection_id
        .map(|n| con::resolve(&app, &a, &n, false).map(|c| c.id))
        .transpose()?;
    let out = app
        .store
        .list::<CallRecord>("call", Some(a.env()))?
        .into_iter()
        .filter(|r| {
            r.invocation.actor_id == a.actor().principal_id
                && r.org_id == a.actor().org_id
                && cid
                    .as_ref()
                    .is_none_or(|id| id == &r.invocation.connection_id)
        })
        .take(200)
        .map(|mut r| {
            r.invocation.result = None;
            r.invocation
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"data":out})))
}
pub fn owned(app: &App, a: &Auth, id: &str) -> Result<CallRecord> {
    let r = app
        .store
        .get::<CallRecord>("call", id)?
        .ok_or_else(Error::missing)?;
    if r.environment != a.env()
        || r.org_id != a.actor().org_id
        || r.invocation.actor_id != a.actor().principal_id
    {
        return Err(Error::missing());
    }
    Ok(r)
}
pub async fn get(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::authenticate(&app, &headers).await?;
    let r = owned(&app, &a, &id)?;
    app.assert_generation(&r.environment, r.generation)?;
    let connection = con::resolve(&app, &a, &r.invocation.connection_id, false)?;
    if let Some(tool) = &r.invocation.tool_name
        && !con::allowed_tool(&app, &connection, &a, tool)?
    {
        return Err(Error::denied());
    }
    let mut value = serde_json::to_value(r.invocation)?;
    value["progress"] = r.progress.unwrap_or(Value::Null);
    Ok(Json(json!({"data":value})))
}
pub async fn cancel(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let lock = app.lock(&format!("call:{id}"));
    let _guard = lock.lock().await;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    owned(&app, &a, &id)?;
    if let Some(c) = app
        .active
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
    {
        c.cancel()
    }
    let r = app
        .store
        .update::<CallRecord>("call", &id, |r| {
            if !r.pending() {
                return false;
            }
            let unknown = r.invocation.status == "running";
            r.invocation.status = "cancelled".into();
            r.invocation.completed_at = Some(now());
            r.invocation.error = Some(ApiError {
                code: "cancelled".into(),
                message: "Cancellation requested. Completed actions cannot be undone.".into(),
                recovery: Some("Inspect the provider if dispatch had already started.".into()),
                outcome_unknown: unknown,
            });
            true
        })?
        .ok_or_else(Error::missing)?;
    app.jobs.notify_waiters();
    let mut invocation = r.invocation;
    invocation.result = None;
    Ok(Json(json!({"data":invocation})))
}
pub fn recover(app: &App) -> Result<()> {
    for mut r in app.store.list::<CallRecord>("call", None)? {
        if r.pending() {
            let unknown = r.invocation.status == "running";
            r.invocation.status = if unknown { "unknown" } else { "failed" }.into();
            r.invocation.completed_at = Some(now());
            r.invocation.error=Some(ApiError{code:"service_restarted".into(),message:"The service restarted before recording a final outcome. This operation was not replayed.".into(),recovery:Some("Inspect the provider before starting another operation.".into()),outcome_unknown:unknown});
            save(app, &r)?;
        }
    }
    Ok(())
}
