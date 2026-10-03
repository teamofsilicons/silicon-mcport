//! MCP execution at a single, explicitly selected execution host.
//!
//! Authorization belongs to the caller. This crate never retries a tool call and
//! never obtains another user's credential when the supplied credential fails.
mod network;
mod transport;

pub use network::{NetworkPolicy, validate_public_url, validated_http_client};
use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::{
        ClientCapabilities, ClientConfig, ClientRequest, Implementation, ProgressNotificationParam,
        ProgressToken, ProtocolVersion,
    },
    service::{NotificationContext, PeerRequestOptions, RunningService},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Endpoints contain secrets: do not log or send them to remote callers.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum Endpoint {
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        bearer_token: Option<String>,
    },
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
}
impl Endpoint {
    pub fn http(url: impl Into<String>) -> Self {
        Self::Http {
            url: url.into(),
            headers: BTreeMap::new(),
            bearer_token: None,
        }
    }
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        Self::Stdio {
            command: command.into(),
            args,
            env: BTreeMap::new(),
            cwd: None,
        }
    }
}

#[derive(Clone)]
pub struct ExecutionOptions {
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub max_response_bytes: usize,
    pub network_policy: NetworkPolicy,
    pub cancellation: CancellationToken,
    pub progress: Option<mpsc::Sender<Value>>,
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    /// Set only for a configured server known to require the legacy handshake.
    pub legacy_handshake: bool,
}

/// Schemas discovered through the exact execution session and provider account.
pub struct ToolSchemas {
    pub input_schema: Value,
    pub output_schema: Option<Value>,
}
impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(20),
            max_response_bytes: 16 * 1024 * 1024,
            network_policy: NetworkPolicy::PublicInternet,
            cancellation: CancellationToken::new(),
            progress: None,
            input_schema: None,
            output_schema: None,
            legacy_handshake: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct McpError {
    pub code: String,
    pub message: String,
    /// True means the upstream may have acted. Do not automatically retry.
    pub outcome_unknown: bool,
}
impl McpError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            outcome_unknown: false,
        }
    }
    fn unknown(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            outcome_unknown: true,
        }
    }
}

fn connection_error(error: impl std::fmt::Display) -> McpError {
    let message = error.to_string();
    if message.contains("provider_authentication_required") {
        McpError::new(
            "provider_authentication_required",
            "The MCP provider rejected its credential. Connect or refresh this account before retrying.",
        )
    } else if message.contains("provider_permission_denied") {
        McpError::new(
            "provider_permission_denied",
            "The MCP provider denied the requested operation. Authorize the required provider scopes.",
        )
    } else {
        McpError::new(
            "connection_failed",
            "MCP connection failed. Check endpoint availability, protocol support and provider authentication.",
        )
    }
}

type ProgressSink = Arc<Mutex<Option<(ProgressToken, mpsc::Sender<Value>)>>>;
#[derive(Clone)]
struct Handler {
    progress: ProgressSink,
    unsupported: Arc<Mutex<Option<&'static str>>>,
}
impl Handler {
    fn unsupported(&self, capability: &'static str) -> rmcp::ErrorData {
        *self.unsupported.lock().expect("capability lock") = Some(capability);
        rmcp::ErrorData::new(
            rmcp::model::ErrorCode::METHOD_NOT_FOUND,
            format!(
                "MCPort does not support {capability}. Use a client that supports the server's interactive capabilities."
            ),
            None,
        )
    }
}
#[allow(deprecated)] // Decline legacy requests without inventing roots or sampling.
impl ClientHandler for Handler {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("silicon-mcport", env!("CARGO_PKG_VERSION")),
        )
    }
    async fn create_message(
        &self,
        _: rmcp::model::CreateMessageRequestParams,
        _: rmcp::service::RequestContext<RoleClient>,
    ) -> Result<rmcp::model::CreateMessageResult, rmcp::ErrorData> {
        Err(self.unsupported("sampling"))
    }
    async fn list_roots(
        &self,
        _: rmcp::service::RequestContext<RoleClient>,
    ) -> Result<rmcp::model::ListRootsResult, rmcp::ErrorData> {
        Err(self.unsupported("filesystem roots"))
    }
    async fn create_elicitation(
        &self,
        _: rmcp::model::ElicitRequestParams,
        _: rmcp::service::RequestContext<RoleClient>,
    ) -> Result<rmcp::model::ElicitResult, rmcp::ErrorData> {
        Err(self.unsupported("interactive elicitation"))
    }
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        if let Some((token, tx)) = &*self.progress.lock().expect("progress lock")
            && token == &params.progress_token
        {
            let _ = tx.try_send(serde_json::to_value(params).unwrap_or(Value::Null));
        }
    }
}

fn validate_schema(schema: &Value, value: &Value, code: &str) -> Result<(), McpError> {
    // No remote reference resolution: schemas may come from untrusted servers.
    let validator = jsonschema::validator_for(schema).map_err(|_| {
        McpError::new(
            "invalid_schema",
            "The MCP server provided an invalid or externally referenced JSON schema.",
        )
    })?;
    if !validator.is_valid(value) {
        return Err(McpError::new(
            code,
            "The value does not match the MCP JSON schema. Inspect the tool schema and correct the input.",
        ));
    }
    Ok(())
}

/// Execute a single operation without retaining transport or process state.
/// For a host-owned persistent process use an explicit [`McpSession`].
pub async fn execute(
    endpoint: &Endpoint,
    method: &str,
    params: Value,
    options: ExecutionOptions,
) -> Result<Value, McpError> {
    let request = prepare_request(method, params, &options)?;
    let mut session = McpSession::connect(endpoint, &options).await?;
    let result = session.run_prepared(method, request, &options).await;
    session.close().await;
    result
}

fn prepare_request(
    method: &str,
    mut params: Value,
    options: &ExecutionOptions,
) -> Result<ClientRequest, McpError> {
    if !matches!(
        method,
        "tools/list"
            | "tools/call"
            | "resources/list"
            | "resources/templates/list"
            | "resources/read"
            | "prompts/list"
            | "prompts/get"
            | "completion/complete"
    ) {
        return Err(McpError::new(
            "unsupported_method",
            format!("MCP method {method} is not supported by this executor."),
        ));
    }
    if options.max_response_bytes == 0 || options.timeout.is_zero() {
        return Err(McpError::new(
            "invalid_limits",
            "Execution limits must be positive.",
        ));
    }
    if params.is_null() {
        params = json!({});
    }
    if !params.is_object() {
        return Err(McpError::new(
            "invalid_params",
            "MCP parameters must be a JSON object.",
        ));
    }
    if params.get("_meta").is_some() {
        return Err(McpError::new(
            "invalid_params",
            "MCP protocol metadata is controlled by the execution host.",
        ));
    }
    if params.get("task").is_some() {
        return Err(McpError::new(
            "unsupported_tasks",
            "MCPort does not submit MCP tasks. Use a synchronous tool or a client with task support.",
        ));
    }
    if method == "tools/call"
        && let Some(schema) = &options.input_schema
    {
        validate_schema(
            schema,
            params.get("arguments").unwrap_or(&json!({})),
            "invalid_arguments",
        )?;
    }
    let request: ClientRequest = serde_json::from_value(json!({"method":method,"params":params}))
        .map_err(|_| {
        McpError::new(
            "invalid_params",
            "Parameters are not valid for this MCP operation.",
        )
    })?;
    if options.cancellation.is_cancelled() {
        return Err(McpError::new(
            "cancelled",
            "Execution was cancelled before dispatch.",
        ));
    }
    Ok(request)
}

/// An explicitly owned transport. The caller scopes it to one configured
/// connection and account; state is never shared implicitly across principals.
pub struct McpSession {
    service: RunningService<RoleClient, Handler>,
    child: Option<OwnedProcess>,
}
struct OwnedProcess(Option<Box<dyn process_wrap::tokio::ChildWrapper>>);
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.start_kill();
        }
    }
}
impl McpSession {
    pub async fn connect(
        endpoint: &Endpoint,
        options: &ExecutionOptions,
    ) -> Result<Self, McpError> {
        let handler = Handler {
            progress: Default::default(),
            unsupported: Default::default(),
        };
        let lifecycle = if options.legacy_handshake {
            ClientLifecycleMode::Initialize
        } else {
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::LATEST],
                legacy_version: Some(ProtocolVersion::LATEST_WITH_INITIALIZE),
            }
        };
        let service: RunningService<RoleClient, Handler>;
        let mut child = None;
        match endpoint {
            Endpoint::Http {
                url,
                headers,
                bearer_token,
            } => {
                let client = validated_http_client(
                    url,
                    options.network_policy,
                    options.connect_timeout,
                    Duration::from_secs(600),
                )
                .await?;
                let http = transport::BoundedHttp::new(client, options.max_response_bytes);
                let config = transport::http_config(
                    url,
                    headers,
                    bearer_token.as_deref(),
                    options.max_response_bytes,
                )?;
                let transport =
                rmcp::transport::streamable_http_client::StreamableHttpClientTransport::with_client(
                    http, config,
                );
                service = tokio::select! {
                    _ = options.cancellation.cancelled() => return Err(McpError::new("cancelled", "Execution cancelled during connection.")),
                    result = tokio::time::timeout(options.connect_timeout, handler.serve_with_lifecycle(transport, lifecycle)) => result.map_err(|_| McpError::new("connection_timeout", "MCP connection timed out. Check the endpoint and host status."))?.map_err(connection_error)?,
                };
            }
            Endpoint::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                if options.network_policy != NetworkPolicy::LocalHost {
                    return Err(McpError::new(
                        "local_host_required",
                        "stdio MCPs must execute on their explicitly registered local host.",
                    ));
                }
                if command.is_empty() || !PathBuf::from(command).is_absolute() {
                    return Err(McpError::new(
                        "invalid_command",
                        "Register an absolute executable path for a local stdio MCP.",
                    ));
                }
                let mut cmd = tokio::process::Command::new(command);
                cmd.args(args)
                    .env_clear()
                    .envs(env)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                for key in ["PATH", "HOME", "LANG", "TMPDIR", "SYSTEMROOT"] {
                    if !env.contains_key(key)
                        && let Some(value) = std::env::var_os(key)
                    {
                        cmd.env(key, value);
                    }
                }
                if let Some(cwd) = cwd {
                    cmd.current_dir(cwd);
                }
                let mut command = process_wrap::tokio::CommandWrap::from(cmd);
                command.wrap(process_wrap::tokio::KillOnDrop);
                #[cfg(unix)]
                command.wrap(process_wrap::tokio::ProcessGroup::leader());
                #[cfg(windows)]
                command.wrap(process_wrap::tokio::JobObject);
                let mut process = OwnedProcess(Some(command.spawn().map_err(|_| McpError::new("process_start_failed", "Could not start the registered MCP executable. Verify its absolute path, working directory and permissions on this host."))?));
                let stdout = process
                    .0
                    .as_mut()
                    .expect("spawned process")
                    .stdout()
                    .take()
                    .expect("piped stdout");
                let stdin = process
                    .0
                    .as_mut()
                    .expect("spawned process")
                    .stdin()
                    .take()
                    .expect("piped stdin");
                let transport = rmcp::transport::async_rw::AsyncRwTransport::new_client(
                    transport::LimitedRead::new(stdout, options.max_response_bytes),
                    stdin,
                );
                child = Some(process);
                service = tokio::select! {
                    _ = options.cancellation.cancelled() => return Err(McpError::new("cancelled", "Execution cancelled during connection.")),
                    result = tokio::time::timeout(options.connect_timeout, handler.serve_with_lifecycle(transport, lifecycle)) => result.map_err(|_| McpError::new("connection_timeout", "The registered stdio MCP did not complete protocol discovery."))?.map_err(|_| McpError::new("connection_failed", "The registered stdio process did not establish an MCP connection."))?,
                };
            }
        }
        Ok(Self { service, child })
    }
    pub fn is_closed(&self) -> bool {
        self.service.is_closed() || self.service.peer().is_transport_closed()
    }
    /// Find a tool in its current account's catalog, following bounded pagination.
    /// Errors here precede tool dispatch and never imply the tool may have acted.
    pub async fn tool_schemas(
        &mut self,
        name: &str,
        options: &ExecutionOptions,
    ) -> Result<ToolSchemas, McpError> {
        let deadline = tokio::time::Instant::now() + options.timeout;
        let mut cursor = None;
        let mut seen = HashSet::new();
        for _ in 0..100 {
            let mut discovery = options.clone();
            discovery.progress = None;
            discovery.input_schema = None;
            discovery.output_schema = None;
            discovery.timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            if discovery.timeout.is_zero() {
                return Err(McpError::new(
                    "timeout",
                    "Tool discovery exceeded the execution deadline; the tool was not called.",
                ));
            }
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
            let catalog = self
                .request("tools/list", params, discovery)
                .await
                .map_err(|mut error| {
                    if error.outcome_unknown {
                        error.message = "Tool discovery did not complete. The tool was not called; check the MCP endpoint before trying again.".into();
                    }
                    error.outcome_unknown = false;
                    error
                })?;
            let tools = catalog
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    McpError::new(
                        "invalid_catalog",
                        "The MCP provider did not return a valid tools catalog.",
                    )
                })?;
            if let Some(tool) = tools
                .iter()
                .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
            {
                return Ok(ToolSchemas {
                    input_schema: tool.get("inputSchema").cloned().ok_or_else(|| {
                        McpError::new(
                            "invalid_schema",
                            "The MCP tool did not provide an input schema.",
                        )
                    })?,
                    output_schema: tool
                        .get("outputSchema")
                        .filter(|value| !value.is_null())
                        .cloned(),
                });
            }
            match catalog.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() && seen.insert(next.to_owned()) => {
                    cursor = Some(next.to_owned())
                }
                Some(_) => {
                    return Err(McpError::new(
                        "invalid_catalog",
                        "MCP tool pagination did not advance.",
                    ));
                }
                None => {
                    return Err(McpError::new(
                        "tool_not_found",
                        "This tool is not available to the selected MCP account.",
                    ));
                }
            }
        }
        Err(McpError::new(
            "catalog_too_large",
            "Tool discovery exceeded the maximum of 100 catalog pages.",
        ))
    }
    pub async fn request(
        &mut self,
        method: &str,
        params: Value,
        options: ExecutionOptions,
    ) -> Result<Value, McpError> {
        let request = prepare_request(method, params, &options)?;
        self.run_prepared(method, request, &options).await
    }
    async fn run_prepared(
        &mut self,
        method: &str,
        request: ClientRequest,
        options: &ExecutionOptions,
    ) -> Result<Value, McpError> {
        *self
            .service
            .service()
            .unsupported
            .lock()
            .expect("capability lock") = None;
        let result = run_request(&self.service, request, options).await;
        *self
            .service
            .service()
            .progress
            .lock()
            .expect("progress lock") = None;
        validate_result(method, result?, options)
    }
    pub async fn close(&mut self) {
        let _ = self
            .service
            .close_with_timeout(Duration::from_secs(2))
            .await;
        if let Some(mut process) = self.child.take()
            && let Some(mut child) = process.0.take()
        {
            let _ = Box::into_pin(child.kill()).await;
        }
    }
}

fn validate_result(
    method: &str,
    value: Value,
    options: &ExecutionOptions,
) -> Result<Value, McpError> {
    if value.get("resultType").and_then(Value::as_str) == Some("input_required") {
        return Err(McpError::unknown(
            "unsupported_client_capability",
            "The MCP server requires interactive input that MCPort cannot provide. Use an interactive MCP client; inspect the provider before repeating the operation.",
        ));
    }
    if value.get("resultType").and_then(Value::as_str) == Some("task") {
        return Err(McpError::unknown(
            "unsupported_tasks",
            "The MCP server started an asynchronous task. MCPort cannot resume or collect that task; inspect it in a client with MCP task support before repeating the operation.",
        ));
    }
    if serde_json::to_vec(&value).map_or(true, |v| v.len() > options.max_response_bytes) {
        return Err(McpError::unknown(
            "result_too_large",
            "The MCP result exceeded the configured size limit. The operation may have completed.",
        ));
    }
    if method == "tools/call"
        && value.get("isError") != Some(&Value::Bool(true))
        && let Some(schema) = &options.output_schema
    {
        let data = value.get("structuredContent").ok_or_else(|| McpError::unknown("invalid_result", "The MCP result omitted the structured output required by its schema. The operation may have completed."))?;
        validate_schema(schema, data, "invalid_result").map_err(|mut e| {
            e.outcome_unknown = true;
            e
        })?;
    }
    Ok(value)
}

async fn run_request(
    service: &RunningService<RoleClient, Handler>,
    request: ClientRequest,
    options: &ExecutionOptions,
) -> Result<Value, McpError> {
    let mut handle = service
        .peer()
        .send_cancellable_request(request, PeerRequestOptions::with_timeout(options.timeout))
        .await
        .map_err(|_| {
            McpError::unknown(
                "dispatch_failed",
                "MCP dispatch failed; check the provider before retrying.",
            )
        })?;
    *service.service().progress.lock().expect("progress lock") = options
        .progress
        .as_ref()
        .map(|tx| (handle.progress_token.clone(), tx.clone()));
    let reply = tokio::select! {
        _ = options.cancellation.cancelled() => { let _ = handle.cancel(Some("Caller cancelled".into())).await; return Err(McpError::unknown("cancelled", "Cancellation was requested. It cannot undo an action that already completed.")); },
        _ = tokio::time::sleep(options.timeout) => { let _ = handle.cancel(Some("MCPort execution timeout".into())).await; return Err(McpError::unknown("timeout", "MCP execution timed out. Its outcome is unknown; inspect the provider before retrying.")); },
        response = &mut handle.rx => response.map_err(|_| McpError::unknown("connection_lost", "MCP disconnected before returning a result; its outcome is unknown."))?,
    };
    match reply {
        Ok(value) => serde_json::to_value(value)
            .map_err(|_| McpError::unknown("invalid_result", "MCP result could not be encoded.")),
        Err(rmcp::ServiceError::McpError(error)) => {
            if let Some(capability) = *service
                .service()
                .unsupported
                .lock()
                .expect("capability lock")
            {
                return Err(McpError {
                    code: "unsupported_client_capability".into(),
                    message: format!(
                        "This MCP operation requires {capability}, which MCPort does not support. Use a compatible interactive MCP client; inspect the provider before repeating the operation."
                    ),
                    outcome_unknown: true,
                });
            }
            Err(McpError::new(
                "upstream_error",
                format!(
                    "The MCP server rejected the operation (JSON-RPC code {}). Inspect the tool's required parameters and account permissions.",
                    error.code.0
                ),
            ))
        }
        Err(_) => Err(McpError::unknown(
            "upstream_transport_error",
            "The MCP transport failed before a result was received; its outcome is unknown.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unfinished_protocol_results_never_claim_completion() {
        for (kind, code) in [
            ("input_required", "unsupported_client_capability"),
            ("task", "unsupported_tasks"),
        ] {
            let error = validate_result(
                "tools/call",
                json!({"resultType":kind}),
                &ExecutionOptions::default(),
            )
            .unwrap_err();
            assert_eq!(error.code, code);
            assert!(error.outcome_unknown);
        }
        let error = prepare_request(
            "tools/call",
            json!({"name":"echo","task":{}}),
            &ExecutionOptions::default(),
        )
        .err()
        .unwrap();
        assert_eq!(error.code, "unsupported_tasks");
        assert!(!error.outcome_unknown);
    }
}
