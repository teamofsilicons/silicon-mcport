//! Public, serializable MCPort API contracts. No storage or runtime side effects.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Actor {
    pub principal_id: String,
    pub identity_kind: String,
    pub org_id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub actor: Actor,
    pub environment: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub description: String,
    pub org_id: String,
    pub owner_id: String,
    pub environment: String,
    pub transport: String,
    pub url: Option<String>,
    pub host_id: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub auth_mode: String,
    pub visibility: String,
    pub status: String,
    pub can_manage: bool,
    pub account: Option<AccountStatus>,
    pub created_at: i64,
    pub updated_at: i64,
    pub version: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionInput {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub transport: String,
    pub url: Option<String>,
    pub host_id: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub auth_mode: String,
    #[serde(default)]
    pub visibility: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub visibility: Option<String>,
    pub version: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountStatus {
    pub connected: bool,
    pub owner_id: String,
    pub label: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccessGrant {
    pub principal_id: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolPolicy {
    pub tool: String,
    pub principal_id: Option<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    pub org_id: String,
    pub environment: String,
    pub online: bool,
    pub last_seen: Option<i64>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostRegistration {
    pub host: Host,
    pub host_token: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcInput {
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub timeout_ms: Option<u64>,
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcOutput {
    pub call_id: String,
    pub result: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostJob {
    pub id: String,
    pub connection_id: String,
    pub method: String,
    pub params: Value,
    pub timeout_ms: u64,
    pub expires_at: i64,
    pub actor: Actor,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HostPoll {
    #[serde(default)]
    pub registered_connections: Vec<String>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostPollResult {
    pub jobs: Vec<HostJob>,
    pub cancelled: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostJobResult {
    pub result: Option<Value>,
    pub error: Option<ApiError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invocation {
    pub id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub actor_id: String,
    pub execution_account_id: String,
    pub method: String,
    pub tool_name: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub result: Option<Value>,
    pub error: Option<ApiError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub recovery: Option<String>,
    #[serde(default)]
    pub outcome_unknown: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub data: T,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ApiError,
}

/// Downloadable content already returned by a caller-owned MCP invocation.
/// The URL carries no credentials and must be requested in the same account/environment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultAsset {
    pub index: u32,
    pub name: String,
    pub mime_type: String,
    pub size: u64,
    pub source_uri: Option<String>,
    pub download_url: String,
}

/// Public discovery metadata. Templates never contain credentials or execute on selection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectoryTemplate {
    pub transport: String,
    pub url: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub auth_mode: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectoryInput {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    pub source_url: Option<String>,
    pub template: Option<DirectoryTemplate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryUpdate {
    pub input: DirectoryInput,
    pub version: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub source: String,
    pub source_url: Option<String>,
    pub source_revision: Option<String>,
    pub owner_id: String,
    pub org_id: String,
    pub environment: String,
    pub can_manage: bool,
    pub template: Option<DirectoryTemplate>,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}
