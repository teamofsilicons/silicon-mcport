//! Stateless public client for Silicon MCPort.
//!
//! The client performs no implicit login, refresh, filesystem access or mutation retries.
//! Supply the current caller and testing context explicitly to each operation. A caller
//! storing refresh tokens must serialize refresh and persist the returned rotation.
//!
//! ```no_run
//! # async fn example() -> Result<(), mcport_api::Error> {
//! use mcport_api::{Client, RequestContext};
//! let client = Client::new("http://127.0.0.1:4380")?;
//! let public = RequestContext::default();
//! let session = client.login(&public, "app-bound-short-lived-token").await?;
//! let context = RequestContext::authenticated(&session.access_token);
//! let connections = client.connections(&context).await?;
//! # Ok(()) }
//! ```

pub use mcport_core::*;
use reqwest::{Method, StatusCode};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::Duration;
use url::Url;

/// Per-request identity. The client never retains or changes these credentials.
#[derive(Clone, Default)]
pub struct RequestContext {
    pub access_token: Option<String>,
    pub test_id: Option<String>,
    pub isi: Option<String>,
    /// Propagate the caller's telemetry preference without including request contents.
    pub telemetry: Option<bool>,
}
impl RequestContext {
    pub fn authenticated(token: &str) -> Self {
        Self {
            access_token: Some(token.into()),
            ..Self::default()
        }
    }
    pub fn testing(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }
}

/// Explicit machine credentials for the restricted outbound host protocol.
/// These credentials are distinct from an application's user session and cannot
/// select a different environment through an ambient setting.
#[derive(Clone)]
pub struct HostContext {
    pub host_id: String,
    pub host_token: String,
    pub environment: String,
    pub isi: Option<String>,
}
impl HostContext {
    fn request_context(&self) -> Result<RequestContext, Error> {
        if self.host_id.is_empty() || self.host_token.is_empty() || self.environment.is_empty() {
            return Err(Error::Configuration {
                message: "Host ID, host token and registered environment are required.".into(),
            });
        }
        Ok(RequestContext {
            access_token: Some(self.host_token.clone()),
            test_id: (self.environment != "production").then(|| self.environment.clone()),
            isi: self.isi.clone(),
            telemetry: None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{message}")]
    Configuration { message: String },
    #[error("MCPort request failed: {message}")]
    Transport {
        message: String,
        outcome_unknown: bool,
    },
    #[error("MCPort returned HTTP {status}: {}", error.message)]
    Api { status: u16, error: ApiError },
    #[error("MCPort returned an invalid response: {message}")]
    Protocol {
        message: String,
        outcome_unknown: bool,
    },
}
impl Error {
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Api { status: 401, .. })
    }
    pub fn public(&self) -> ApiError {
        match self {
            Self::Api { error, .. } => error.clone(),
            Self::Configuration { message } => ApiError { code: "client_configuration".into(), message: message.clone(), recovery: Some("Check mcport config show and the MCPORT_URL or --backend override.".into()), outcome_unknown: false },
            Self::Transport { message, outcome_unknown } => ApiError { code: "transport_error".into(), message: message.clone(), recovery: Some(if *outcome_unknown { "The request may have completed. Inspect mcport activity ls before deciding whether another invocation is safe." } else { "Check the backend URL, network and server availability, then retry this read." }.into()), outcome_unknown: *outcome_unknown },
            Self::Protocol { message, outcome_unknown } => ApiError { code: "invalid_server_response".into(), message: message.clone(), recovery: Some("Check server compatibility and activity before retrying a mutation; report this error with client/server versions.".into()), outcome_unknown: *outcome_unknown },
        }
    }
}

#[derive(Clone)]
pub struct Client {
    base: Url,
    http: reqwest::Client,
}

impl Client {
    pub fn new(backend_url: &str) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .user_agent(concat!("mcport-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Configuration {
                message: e.to_string(),
            })?;
        Self::with_http(backend_url, http)
    }

    /// Inject an HTTP client for application-specific connection configuration.
    /// Do not configure automatic mutation retries or credential-forwarding redirects.
    pub fn with_http(backend_url: &str, http: reqwest::Client) -> Result<Self, Error> {
        let mut base = Url::parse(backend_url).map_err(|e| Error::Configuration {
            message: format!("Invalid backend URL: {e}"),
        })?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(Error::Configuration {
                message: "Backend must be an HTTP(S) URL without credentials, query or fragment."
                    .into(),
            });
        }
        let loopback = match base.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if base.scheme() == "http" && !loopback {
            return Err(Error::Configuration {
                message:
                    "Backend connections require HTTPS, except for loopback development servers."
                        .into(),
            });
        }
        let path = base.path().trim_end_matches('/');
        let api_path = if path.ends_with("/api/v1") {
            format!("{path}/")
        } else {
            format!("{path}/api/v1/")
        };
        base.set_path(&api_path);
        Ok(Self { base, http })
    }

    pub fn backend_url(&self) -> String {
        self.base
            .as_str()
            .trim_end_matches("api/v1/")
            .trim_end_matches('/')
            .to_owned()
    }

    fn endpoint(&self, parts: &[&str]) -> Url {
        let mut url = self.base.clone();
        let mut segments = url
            .path_segments_mut()
            .expect("HTTP URL supports path segments");
        segments.pop_if_empty();
        for part in parts {
            segments.push(part);
        }
        drop(segments);
        url
    }

    async fn request<T: DeserializeOwned>(
        &self,
        context: &RequestContext,
        method: Method,
        path: &[&str],
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<T, Error> {
        self.request_options(context, method, path, query, body, None)
            .await
    }

    async fn request_options<T: DeserializeOwned>(
        &self,
        context: &RequestContext,
        method: Method,
        path: &[&str],
        query: &[(&str, &str)],
        body: Option<Value>,
        limits: Option<(Duration, usize)>,
    ) -> Result<T, Error> {
        let may_mutate = !matches!(method, Method::GET | Method::HEAD);
        let mut url = self.endpoint(path);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let mut request = self.http.request(method, url);
        if let Some(token) = &context.access_token {
            request = request.bearer_auth(token);
        }
        if let Some(test_id) = &context.test_id {
            request = request.header("X-MCPort-Test", test_id);
        }
        if let Some(isi) = &context.isi {
            request = request.header("X-MCPort-ISI", isi);
        }
        if let Some(telemetry) = context.telemetry {
            request = request.header(
                "X-MCPort-Telemetry",
                if telemetry { "true" } else { "false" },
            );
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        if let Some((timeout, _)) = limits {
            request = request.timeout(timeout);
        }
        let mut response = request.send().await.map_err(|e| Error::Transport {
            message: e.without_url().to_string(),
            outcome_unknown: may_mutate,
        })?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| Error::Transport {
            message: e.without_url().to_string(),
            outcome_unknown: may_mutate,
        })? {
            if limits.is_some_and(|(_, max)| bytes.len() + chunk.len() > max) {
                return Err(Error::Protocol {
                    message: "Host protocol response exceeds its size limit.".into(),
                    outcome_unknown: may_mutate,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let error = serde_json::from_slice::<ErrorEnvelope>(&bytes).map(|e| e.error).unwrap_or_else(|_| ApiError {
                code: if status == StatusCode::UNAUTHORIZED { "unauthenticated" } else { "http_error" }.into(),
                message: format!("Backend returned HTTP {} without a valid MCPort error envelope.", status.as_u16()),
                recovery: Some(if status == StatusCode::UNAUTHORIZED { "Run mcport login <app-bound-slt> in this backend and testing context." } else { "Check backend logs and report the HTTP status; response content was not printed because it may contain private data." }.into()),
                outcome_unknown: may_mutate && status.is_server_error(),
            });
            return Err(Error::Api {
                status: status.as_u16(),
                error,
            });
        }
        serde_json::from_slice::<Envelope<T>>(&bytes)
            .map(|e| e.data)
            .map_err(|e| Error::Protocol {
                message: e.to_string(),
                outcome_unknown: may_mutate,
            })
    }

    async fn get<T: DeserializeOwned>(
        &self,
        ctx: &RequestContext,
        path: &[&str],
    ) -> Result<T, Error> {
        self.request(ctx, Method::GET, path, &[], None).await
    }
    async fn post<T: DeserializeOwned>(
        &self,
        ctx: &RequestContext,
        path: &[&str],
        body: impl Serialize,
    ) -> Result<T, Error> {
        self.request(ctx, Method::POST, path, &[], Some(to_value(body)?))
            .await
    }
    async fn delete<T: DeserializeOwned>(
        &self,
        ctx: &RequestContext,
        path: &[&str],
    ) -> Result<T, Error> {
        self.request(ctx, Method::DELETE, path, &[], None).await
    }

    pub async fn iam(&self, ctx: &RequestContext) -> Result<Value, Error> {
        self.get(ctx, &["iam"]).await
    }
    pub async fn login(&self, ctx: &RequestContext, slt: &str) -> Result<Session, Error> {
        self.post(ctx, &["auth", "login"], json!({"slt":slt})).await
    }
    pub async fn refresh(
        &self,
        ctx: &RequestContext,
        refresh_token: &str,
    ) -> Result<Session, Error> {
        self.post(
            ctx,
            &["auth", "refresh"],
            json!({"refresh_token":refresh_token}),
        )
        .await
    }
    pub async fn status(&self, ctx: &RequestContext) -> Result<Value, Error> {
        self.get(ctx, &["auth", "status"]).await
    }
    pub async fn logout(&self, ctx: &RequestContext) -> Result<Value, Error> {
        self.post(ctx, &["auth", "logout"], json!({})).await
    }

    /// Search community references and the caller's organization directory.
    pub async fn directory(
        &self,
        ctx: &RequestContext,
        search: Option<&str>,
    ) -> Result<Vec<DirectoryEntry>, Error> {
        let query = search.map(|q| vec![("q", q)]).unwrap_or_default();
        self.request(ctx, Method::GET, &["directory"], &query, None)
            .await
    }
    /// Read an exact directory ID, including its source and optional template.
    pub async fn directory_entry(
        &self,
        ctx: &RequestContext,
        id: &str,
    ) -> Result<DirectoryEntry, Error> {
        self.get(ctx, &["directory", id]).await
    }
    /// Create an entry owned by the caller in their current organization/environment.
    pub async fn create_directory_entry(
        &self,
        ctx: &RequestContext,
        input: &DirectoryInput,
    ) -> Result<DirectoryEntry, Error> {
        self.post(ctx, &["directory"], input).await
    }
    /// Replace an owned entry. Its expected version prevents stale overwrites.
    pub async fn update_directory_entry(
        &self,
        ctx: &RequestContext,
        id: &str,
        input: &DirectoryUpdate,
    ) -> Result<DirectoryEntry, Error> {
        self.request(
            ctx,
            Method::PUT,
            &["directory", id],
            &[],
            Some(to_value(input)?),
        )
        .await
    }
    /// Remove an owned directory entry; existing connections are unaffected.
    pub async fn delete_directory_entry(
        &self,
        ctx: &RequestContext,
        id: &str,
    ) -> Result<Value, Error> {
        self.delete(ctx, &["directory", id]).await
    }

    pub async fn connections(&self, ctx: &RequestContext) -> Result<Vec<Connection>, Error> {
        self.get(ctx, &["connections"]).await
    }
    pub async fn create_connection(
        &self,
        ctx: &RequestContext,
        input: &ConnectionInput,
    ) -> Result<Connection, Error> {
        self.post(ctx, &["connections"], input).await
    }
    pub async fn connection(&self, ctx: &RequestContext, id: &str) -> Result<Connection, Error> {
        self.get(ctx, &["connections", id]).await
    }
    pub async fn update_connection(
        &self,
        ctx: &RequestContext,
        id: &str,
        input: &ConnectionUpdate,
    ) -> Result<Connection, Error> {
        self.request(
            ctx,
            Method::PATCH,
            &["connections", id],
            &[],
            Some(to_value(input)?),
        )
        .await
    }
    pub async fn delete_connection(&self, ctx: &RequestContext, id: &str) -> Result<Value, Error> {
        self.delete(ctx, &["connections", id]).await
    }
    pub async fn access(&self, ctx: &RequestContext, id: &str) -> Result<Vec<AccessGrant>, Error> {
        self.get(ctx, &["connections", id, "access"]).await
    }
    pub async fn grant_access(
        &self,
        ctx: &RequestContext,
        id: &str,
        principal: &str,
    ) -> Result<AccessGrant, Error> {
        self.post(
            ctx,
            &["connections", id, "access"],
            json!({"account":principal}),
        )
        .await
    }
    pub async fn revoke_access(
        &self,
        ctx: &RequestContext,
        id: &str,
        principal: &str,
    ) -> Result<Value, Error> {
        self.delete(ctx, &["connections", id, "access", principal])
            .await
    }
    pub async fn tool_policies(
        &self,
        ctx: &RequestContext,
        id: &str,
    ) -> Result<Vec<ToolPolicy>, Error> {
        self.get(ctx, &["connections", id, "policies"]).await
    }
    pub async fn set_tool_policy(
        &self,
        ctx: &RequestContext,
        id: &str,
        input: &ToolPolicyInput,
    ) -> Result<ToolPolicy, Error> {
        self.request(
            ctx,
            Method::PUT,
            &["connections", id, "policies"],
            &[],
            Some(to_value(input)?),
        )
        .await
    }

    pub async fn account(&self, ctx: &RequestContext, id: &str) -> Result<AccountStatus, Error> {
        self.get(ctx, &["connections", id, "account"]).await
    }
    /// Supply bearer/header setup as required by the public API. Secrets must not be logged.
    pub async fn connect_account(
        &self,
        ctx: &RequestContext,
        id: &str,
        input: &Value,
    ) -> Result<AccountStatus, Error> {
        self.post(ctx, &["connections", id, "account"], input).await
    }
    pub async fn authorize_account(
        &self,
        ctx: &RequestContext,
        id: &str,
        client_id: Option<&str>,
    ) -> Result<Value, Error> {
        self.post(
            ctx,
            &["connections", id, "account", "authorize"],
            json!({"client_id":client_id}),
        )
        .await
    }
    pub async fn disconnect_account(&self, ctx: &RequestContext, id: &str) -> Result<Value, Error> {
        self.delete(ctx, &["connections", id, "account"]).await
    }

    /// Invoke one allowed MCP operation. Results, content blocks and pagination remain intact.
    pub async fn rpc(
        &self,
        ctx: &RequestContext,
        id: &str,
        input: &RpcInput,
    ) -> Result<RpcOutput, Error> {
        self.post(ctx, &["connections", id, "mcp"], input).await
    }
    async fn mcp(
        &self,
        ctx: &RequestContext,
        id: &str,
        method: &str,
        params: Value,
    ) -> Result<RpcOutput, Error> {
        self.rpc(
            ctx,
            id,
            &RpcInput {
                method: method.into(),
                params,
                timeout_ms: None,
                idempotency_key: None,
            },
        )
        .await
    }
    pub async fn tools(
        &self,
        ctx: &RequestContext,
        id: &str,
        cursor: Option<&str>,
    ) -> Result<RpcOutput, Error> {
        self.mcp(ctx, id, "tools/list", cursor_params(cursor)).await
    }
    pub async fn tool(&self, ctx: &RequestContext, id: &str, name: &str) -> Result<Value, Error> {
        let mut cursor: Option<String> = None;
        let mut visited = BTreeSet::new();
        loop {
            let page = self.tools(ctx, id, cursor.as_deref()).await?;
            if let Some(tool) =
                page.result
                    .get("tools")
                    .and_then(Value::as_array)
                    .and_then(|tools| {
                        tools
                            .iter()
                            .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
                    })
            {
                return Ok(tool.clone());
            }
            match page.result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if visited.insert(next.to_owned()) => cursor = Some(next.into()),
                Some(_) => {
                    return Err(Error::Protocol {
                        message: "MCP repeated a pagination cursor while looking up the tool."
                            .into(),
                        outcome_unknown: false,
                    });
                }
                None => {
                    return Err(Error::Api {
                        status: 404,
                        error: ApiError {
                            code: "tool_not_found".into(),
                            message: format!(
                                "No tool named {name} is advertised by this connection."
                            ),
                            recovery: Some(
                                "Run mcport tool ls <connection> and use an exact tool name."
                                    .into(),
                            ),
                            outcome_unknown: false,
                        },
                    });
                }
            }
        }
    }
    pub async fn call_tool(
        &self,
        ctx: &RequestContext,
        id: &str,
        name: &str,
        arguments: Value,
    ) -> Result<RpcOutput, Error> {
        self.mcp(
            ctx,
            id,
            "tools/call",
            json!({"name":name,"arguments":arguments}),
        )
        .await
    }
    pub async fn resources(
        &self,
        ctx: &RequestContext,
        id: &str,
        cursor: Option<&str>,
    ) -> Result<RpcOutput, Error> {
        self.mcp(ctx, id, "resources/list", cursor_params(cursor))
            .await
    }
    pub async fn resource_templates(
        &self,
        ctx: &RequestContext,
        id: &str,
        cursor: Option<&str>,
    ) -> Result<RpcOutput, Error> {
        self.mcp(ctx, id, "resources/templates/list", cursor_params(cursor))
            .await
    }
    pub async fn read_resource(
        &self,
        ctx: &RequestContext,
        id: &str,
        uri: &str,
    ) -> Result<RpcOutput, Error> {
        self.mcp(ctx, id, "resources/read", json!({"uri":uri}))
            .await
    }
    pub async fn prompts(
        &self,
        ctx: &RequestContext,
        id: &str,
        cursor: Option<&str>,
    ) -> Result<RpcOutput, Error> {
        self.mcp(ctx, id, "prompts/list", cursor_params(cursor))
            .await
    }
    pub async fn get_prompt(
        &self,
        ctx: &RequestContext,
        id: &str,
        name: &str,
        arguments: Value,
    ) -> Result<RpcOutput, Error> {
        self.mcp(
            ctx,
            id,
            "prompts/get",
            json!({"name":name,"arguments":arguments}),
        )
        .await
    }

    /// Complete a prompt argument or resource-template argument with the full
    /// MCP completion params object, preserving context and returned pagination.
    pub async fn complete(
        &self,
        ctx: &RequestContext,
        id: &str,
        params: Value,
    ) -> Result<RpcOutput, Error> {
        self.mcp(ctx, id, "completion/complete", params).await
    }

    pub async fn hosts(&self, ctx: &RequestContext) -> Result<Vec<Host>, Error> {
        self.get(ctx, &["hosts"]).await
    }
    pub async fn create_host(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> Result<HostRegistration, Error> {
        self.post(ctx, &["hosts"], json!({"name":name})).await
    }
    pub async fn host(&self, ctx: &RequestContext, id: &str) -> Result<Host, Error> {
        self.get(ctx, &["hosts", id]).await
    }
    pub async fn delete_host(&self, ctx: &RequestContext, id: &str) -> Result<Value, Error> {
        self.delete(ctx, &["hosts", id]).await
    }
    pub async fn activity(
        &self,
        ctx: &RequestContext,
        connection: Option<&str>,
    ) -> Result<Vec<Invocation>, Error> {
        let query: Vec<(&str, &str)> = connection
            .map(|id| vec![("connection_id", id)])
            .unwrap_or_default();
        self.request(ctx, Method::GET, &["calls"], &query, None)
            .await
    }
    pub async fn invocation(&self, ctx: &RequestContext, id: &str) -> Result<Value, Error> {
        self.get(ctx, &["calls", id]).await
    }
    pub async fn cancel(&self, ctx: &RequestContext, id: &str) -> Result<Invocation, Error> {
        self.post(ctx, &["calls", id, "cancel"], json!({})).await
    }
    /// List server-extracted embedded assets, checked against current access and tool policy.
    pub async fn assets(
        &self,
        ctx: &RequestContext,
        call: &str,
    ) -> Result<Vec<ResultAsset>, Error> {
        self.get(ctx, &["calls", call, "assets"]).await
    }

    /// Download embedded bytes by index. This never follows a provider-supplied URL.
    /// The caller decides how to store or render the bytes; no file is created by this SDK.
    pub async fn download_asset(
        &self,
        ctx: &RequestContext,
        call: &str,
        index: u32,
    ) -> Result<Vec<u8>, Error> {
        let mut request =
            self.http
                .get(self.endpoint(&["calls", call, "assets", &index.to_string()]));
        if let Some(token) = &ctx.access_token {
            request = request.bearer_auth(token);
        }
        if let Some(test_id) = &ctx.test_id {
            request = request.header("X-MCPort-Test", test_id);
        }
        if let Some(isi) = &ctx.isi {
            request = request.header("X-MCPort-ISI", isi);
        }
        if let Some(telemetry) = ctx.telemetry {
            request = request.header(
                "X-MCPort-Telemetry",
                if telemetry { "true" } else { "false" },
            );
        }
        let mut response = request.send().await.map_err(|e| Error::Transport {
            message: e.without_url().to_string(),
            outcome_unknown: false,
        })?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| Error::Transport {
            message: e.without_url().to_string(),
            outcome_unknown: false,
        })? {
            if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
                return Err(Error::Protocol {
                    message: "Asset exceeds the 16 MiB download limit.".into(),
                    outcome_unknown: false,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let error = serde_json::from_slice::<ErrorEnvelope>(&bytes)
                .map(|e| e.error)
                .unwrap_or(ApiError {
                    code: "http_error".into(),
                    message: format!("Asset download returned HTTP {}.", status.as_u16()),
                    recovery: Some("Check your current connection and tool access.".into()),
                    outcome_unknown: false,
                });
            return Err(Error::Api {
                status: status.as_u16(),
                error,
            });
        }
        Ok(bytes)
    }

    /// Perform one bounded long poll. Leasing and retry decisions belong to the host daemon.
    /// Responses are capped at 4 MiB; this call times out after 25 seconds.
    pub async fn host_poll(
        &self,
        host: &HostContext,
        poll: &HostPoll,
    ) -> Result<HostPollResult, Error> {
        self.request_options(
            &host.request_context()?,
            Method::POST,
            &["hosts", &host.host_id, "poll"],
            &[],
            Some(to_value(poll)?),
            Some((Duration::from_secs(25), 4 * 1024 * 1024)),
        )
        .await
    }

    /// Upload an already journaled result once. The caller may retry the same
    /// job/result through its durable outbox; this client performs no retry.
    pub async fn host_result(
        &self,
        host: &HostContext,
        job: &str,
        result: &HostJobResult,
    ) -> Result<Value, Error> {
        self.request_options(
            &host.request_context()?,
            Method::POST,
            &["hosts", &host.host_id, "jobs", job, "result"],
            &[],
            Some(to_value(result)?),
            Some((Duration::from_secs(3), 64 * 1024)),
        )
        .await
    }

    /// Forward one progress update using only the registered host identity.
    pub async fn host_progress(
        &self,
        host: &HostContext,
        job: &str,
        progress: &Value,
    ) -> Result<Value, Error> {
        self.request_options(
            &host.request_context()?,
            Method::POST,
            &["hosts", &host.host_id, "jobs", job, "progress"],
            &[],
            Some(json!({"progress":progress})),
            Some((Duration::from_secs(25), 64 * 1024)),
        )
        .await
    }

    pub async fn settings(&self, ctx: &RequestContext) -> Result<Value, Error> {
        self.get(ctx, &["settings"]).await
    }
    pub async fn set_telemetry(&self, ctx: &RequestContext, enabled: bool) -> Result<Value, Error> {
        self.request(
            ctx,
            Method::PATCH,
            &["settings"],
            &[],
            Some(json!({"telemetry":enabled})),
        )
        .await
    }
    pub async fn report(
        &self,
        ctx: &RequestContext,
        message: &str,
        pr: Option<&str>,
    ) -> Result<Value, Error> {
        self.post(ctx, &["reports"], json!({"message":message,"pr":pr}))
            .await
    }

    /// Submit a bounded diagnostic event through MCPort. Never pass MCP inputs or results.
    /// The backend applies the user's opt-out and its environment-specific Space Station sink.
    pub async fn telemetry(&self, ctx: &RequestContext, event: &Value) -> Result<Value, Error> {
        self.post(ctx, &["telemetry"], event).await
    }
}

fn to_value(value: impl Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|e| Error::Configuration {
        message: e.to_string(),
    })
}
fn cursor_params(cursor: Option<&str>) -> Value {
    cursor
        .map(|cursor| json!({"cursor":cursor}))
        .unwrap_or_else(|| json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[test]
    fn backend_credentials_require_tls_except_loopback_development() {
        assert!(Client::new("http://example.com").is_err());
        assert!(Client::new("http://192.168.1.2").is_err());
        assert!(Client::new("https://example.com").is_ok());
        assert!(Client::new("http://127.0.0.1:4380").is_ok());
        assert!(Client::new("http://[::1]:4380").is_ok());
        assert!(Client::new("https://user:secret@example.com").is_err());
    }

    async fn request_bytes(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut received = Vec::new();
        loop {
            let mut buffer = [0; 2048];
            let count = stream.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            received.extend_from_slice(&buffer[..count]);
            if let Some(end) = received.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&received[..end]);
                let len = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if received.len() >= end + 4 + len {
                    break;
                }
            }
        }
        received
    }
    #[test]
    fn identifiers_are_encoded_as_single_segments() {
        let client = Client::new("https://example.test/proxy").unwrap();
        assert_eq!(
            client.endpoint(&["connections", "a/b?token=bad"]).as_str(),
            "https://example.test/proxy/api/v1/connections/a%2Fb%3Ftoken=bad"
        );
        assert!(Client::new("https://secret@example.test").is_err());
        assert!(Client::new("https://example.test?token=bad").is_err());
    }

    #[tokio::test]
    async fn directory_crud_preserves_context_search_and_expected_version() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let entry = json!({"id":"dir/one","name":"Team docs","description":"Shared reference","category":"Documentation","source":"personal","source_url":"https://example.test","source_revision":null,"owner":{"uuid":"Wr1","id":"si:writer","kind":"silicon","display_name":"Writer"},"can_manage":true,"template":{"transport":"http","url":"https://example.test/mcp","command":null,"args":[],"auth_mode":"per-user"},"version":7,"created_at":1,"updated_at":2});
        let response_entry = entry.clone();
        let server = tokio::spawn(async move {
            let mut requests = vec![];
            for data in [
                json!([response_entry.clone()]),
                response_entry.clone(),
                response_entry.clone(),
                response_entry,
                json!({"deleted":true}),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                requests.push(String::from_utf8(request_bytes(&mut stream).await).unwrap());
                let body = json!({"data":data}).to_string();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            requests
        });
        let client = Client::new(&format!("http://{address}")).unwrap();
        let ctx = RequestContext::authenticated("directory-fixture").testing("test-one");
        let entries = client.directory(&ctx, Some("docs & tools")).await.unwrap();
        assert_eq!(entries.len(), 1);
        let current = client.directory_entry(&ctx, "dir/one").await.unwrap();
        assert_eq!(serde_json::to_value(&current).unwrap(), entry);
        let input = DirectoryInput {
            name: "New docs".into(),
            description: current.description,
            category: current.category,
            source_url: current.source_url,
            template: current.template,
        };
        client.create_directory_entry(&ctx, &input).await.unwrap();
        client
            .update_directory_entry(
                &ctx,
                "dir/one",
                &DirectoryUpdate {
                    input,
                    version: current.version,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            client
                .delete_directory_entry(&ctx, "dir/one")
                .await
                .unwrap()["deleted"],
            true
        );
        let requests = server.await.unwrap();
        for (request, expected) in requests.iter().zip([
            "GET /api/v1/directory?q=docs+%26+tools HTTP/1.1",
            "GET /api/v1/directory/dir%2Fone HTTP/1.1",
            "POST /api/v1/directory HTTP/1.1",
            "PUT /api/v1/directory/dir%2Fone HTTP/1.1",
            "DELETE /api/v1/directory/dir%2Fone HTTP/1.1",
        ]) {
            assert!(request.starts_with(expected), "{request}");
            assert!(request.contains("authorization: Bearer directory-fixture"));
            assert!(request.contains("x-mcport-test: test-one"));
        }
        let update: Value =
            serde_json::from_str(requests[3].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(update["version"], 7);
        assert_eq!(update["input"]["name"], "New docs");
        assert!(update["input"].get("owner_id").is_none());
    }

    #[tokio::test]
    async fn preserves_multipart_results_and_sends_explicit_identity_context() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let expected = json!({"call_id":"call-1","result":{"content":[{"type":"text","text":"answer"},{"type":"image","mimeType":"image/png","data":"YQ=="},{"type":"resource_link","uri":"asset://a","name":"chart"}],"structuredContent":{"count":42},"isError":false,"custom":"preserved"}});
        let body = json!({"data":expected}).to_string();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = String::from_utf8(request_bytes(&mut stream).await).unwrap();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
            request
        });
        let client = Client::new(&format!("http://{address}")).unwrap();
        let mut context = RequestContext::authenticated("test-access").testing("isolated-1");
        context.isi = Some("research".into());
        let output = client
            .call_tool(&context, "connection-1", "draw", json!({"title":"result"}))
            .await
            .unwrap();
        assert_eq!(serde_json::to_value(output).unwrap(), expected);
        let request = server.await.unwrap().to_ascii_lowercase();
        assert!(request.contains("authorization: bearer test-access"));
        assert!(request.contains("x-mcport-test: isolated-1"));
        assert!(request.contains("x-mcport-isi: research"));
    }

    #[tokio::test]
    async fn host_gateway_calls_keep_registered_identity_and_environment_explicit() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for data in [
                json!({"jobs":[],"cancelled":[]}),
                json!({"accepted":true}),
                json!({"accepted":true}),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                requests.push(String::from_utf8(request_bytes(&mut stream).await).unwrap());
                let body = json!({"data":data}).to_string();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            requests
        });
        let client = Client::new(&format!("http://{address}/api/v1")).unwrap();
        let context = HostContext {
            host_id: "host/fixture".into(),
            host_token: "fixture-host-token".into(),
            environment: "isolated-test".into(),
            isi: Some("fixture-isi".into()),
        };
        assert!(
            client
                .host_poll(&context, &HostPoll::default())
                .await
                .unwrap()
                .jobs
                .is_empty()
        );
        client
            .host_progress(&context, "job/one", &json!({"progress":1}))
            .await
            .unwrap();
        client
            .host_result(
                &context,
                "job/one",
                &HostJobResult {
                    result: Some(json!({"complete":true})),
                    error: None,
                },
            )
            .await
            .unwrap();
        let requests = server.await.unwrap();
        for (request, path) in
            requests
                .iter()
                .zip(["poll", "jobs/job%2fone/progress", "jobs/job%2fone/result"])
        {
            let lowered = request.to_ascii_lowercase();
            assert!(lowered.starts_with(&format!(
                "post /api/v1/hosts/host%2ffixture/{path} http/1.1"
            )));
            assert!(lowered.contains("authorization: bearer fixture-host-token"));
            assert!(lowered.contains("x-mcport-test: isolated-test"));
            assert!(lowered.contains("x-mcport-isi: fixture-isi"));
        }
        assert!(requests[2].contains("\"complete\":true"));
    }

    #[tokio::test]
    async fn host_poll_rejects_oversized_chunked_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request_bytes(&mut stream).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            let chunk = vec![b' '; 4 * 1024 * 1024 + 1];
            let _ = stream
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await;
            let _ = stream.write_all(&chunk).await;
            let _ = stream.write_all(b"\r\n0\r\n\r\n").await;
        });
        let client = Client::new(&format!("http://{address}")).unwrap();
        let context = HostContext {
            host_id: "host".into(),
            host_token: "fixture".into(),
            environment: "production".into(),
            isi: None,
        };
        let error = client
            .host_poll(&context, &HostPoll::default())
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Protocol { .. }));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn lost_mutation_response_is_unknown_and_is_not_replayed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request_bytes(&mut stream).await;
            drop(stream);
            assert!(
                tokio::time::timeout(Duration::from_millis(300), listener.accept())
                    .await
                    .is_err(),
                "Mutation was automatically replayed"
            );
        });
        let client = Client::new(&format!("http://{address}")).unwrap();
        let error = client
            .call_tool(
                &RequestContext::authenticated("test"),
                "conn",
                "write",
                json!({}),
            )
            .await
            .unwrap_err();
        assert!(error.public().outcome_unknown);
        server.await.unwrap();
    }
}
