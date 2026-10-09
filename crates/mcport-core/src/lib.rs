//! Public, serializable MCPort API contracts. No storage or runtime side effects.
//!
//! Identity: every Carbon and Silicon is a Silicon Accounts account. MCPort keys
//! everything on the account's permanent `uuid` (short, case-sensitive) and only
//! displays its current public id (`c:ada`, `si:scout`), which can change.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A Carbon or Silicon as MCPort shows it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountRef {
    /// Permanent Silicon Accounts uuid. Key on this, never on `id`.
    pub uuid: String,
    /// Current public id (`c:…` or `si:…`). Empty when MCPort does not know it.
    #[serde(default)]
    pub id: String,
    /// `carbon` or `silicon`; empty when unknown.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp_url: Option<String>,
}

/// The signed-in Carbon or Silicon (`GET /api/v1/me`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Me {
    #[serde(flatten)]
    pub account: AccountRef,
    /// A Silicon's custodian. Always `None` for Carbons.
    #[serde(default)]
    pub custodian: Option<AccountRef>,
    /// The access token's expiry (Unix seconds).
    #[serde(default)]
    pub expires_at: i64,
}

/// Public service discovery (`GET /api/v1/discovery`). Needs no sign-in.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Discovery {
    pub app_id: String,
    /// Silicon Accounts public URL (token issuer, device and sign-in pages).
    pub accounts_url: String,
    /// The OAuth client id public clients (the CLI) use: the app id.
    pub client_id: String,
    pub backend_url: String,
    pub website_url: String,
    pub repository_url: String,
    pub docs_url: String,
    /// The Rust package (`mcport-client` on crates.io).
    pub package_url: String,
    /// The `mcport` listing on Silicon Apps (`silicon-apps install mcport`).
    pub install_url: String,
    /// MCPort API contract version.
    pub version: String,
}

/// The account a host job runs for.
///
/// `uuid`, `id`, `kind` and `display_name` identify the caller. `principal_id`,
/// `identity_kind` and `org_id` are transition fields for host daemons released
/// before 0.3.0, which still read them; they are removed in a later release.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Actor {
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub display_name: String,
    /// Key of the caller's personal account in the host registry: the caller's
    /// pre-0.3.0 id for hosts whose registry was not migrated, otherwise `uuid`.
    #[serde(default)]
    pub principal_id: String,
    /// Same as `kind`, for daemons released before 0.3.0.
    #[serde(default)]
    pub identity_kind: String,
    /// The host registry's pre-0.3.0 grouping value (empty for new hosts).
    #[serde(default)]
    pub org_id: String,
}

/// Session tokens issued by MCPort backends released before 0.3.0. 0.3.0 backends
/// issue no sessions: callers send Silicon Accounts access tokens instead.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub actor: Actor,
    pub environment: String,
}

/// A configured MCP and who may use it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub description: String,
    /// The Carbon or Silicon that created it.
    #[serde(default)]
    pub owner: AccountRef,
    pub transport: String,
    pub url: Option<String>,
    pub host_id: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub auth_mode: String,
    /// `invited`: the owner and accounts it adds. `circle`: also the owner's
    /// circle (a Carbon and the Silicons it looks after, or a Silicon, its
    /// custodian and the custodian's other Silicons).
    pub visibility: String,
    pub status: String,
    pub can_manage: bool,
    /// Why the caller can see it: `owner`, `custodian` (the owner is a Silicon
    /// the caller looks after), `circle` or `invited`.
    #[serde(default)]
    pub access: String,
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
    /// `invited` (default) or `circle`. `private` is accepted as `invited`.
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

/// Whose provider account a connection uses for the caller. Never contains secrets.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AccountStatus {
    pub connected: bool,
    /// The account whose provider credentials run calls: the owner for shared
    /// connections, the caller (or the account asked about) for per-user ones.
    /// `None` for connections without provider authentication.
    #[serde(default)]
    pub account: Option<AccountRef>,
    pub label: String,
    pub kind: String,
}

/// Permission to use a connection, given to one account.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AccessGrant {
    pub account: AccountRef,
    pub created_at: i64,
    /// Who added it; `None` for grants made by the operator at cutover.
    #[serde(default)]
    pub created_by: Option<AccountRef>,
}

/// `POST …/access`: the account to add, by `c:`/`si:` id or uuid.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessInput {
    #[serde(alias = "principal_id")]
    pub account: String,
}

/// A tool switch for a whole connection (`account` is `None`) or one account.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolPolicy {
    pub tool: String,
    #[serde(default)]
    pub account: Option<AccountRef>,
    pub enabled: bool,
}

/// `PUT …/policies`: `account` is a `c:`/`si:` id or uuid, or `null` for everyone.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPolicyInput {
    pub tool: String,
    #[serde(default, alias = "principal_id")]
    pub account: Option<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Host {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub owner: AccountRef,
    pub online: bool,
    pub last_seen: Option<i64>,
    pub created_at: i64,
    /// The caller owns it, or looks after the Silicon that does (list, show, delete).
    #[serde(default)]
    pub can_manage: bool,
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

/// One MCP request made through a connection.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Invocation {
    pub id: String,
    pub connection_id: String,
    pub connection_name: String,
    /// The Carbon or Silicon that made the call.
    #[serde(default)]
    pub caller: AccountRef,
    /// Whose provider account ran it; `None` without provider authentication.
    #[serde(default)]
    pub execution_account: Option<AccountRef>,
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

/// Downloadable content already returned by an invocation. The URL carries no
/// credentials: request it with the same account's access token, or ask for a
/// one-time download ticket.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultAsset {
    pub index: u32,
    pub name: String,
    pub mime_type: String,
    pub size: u64,
    pub source_uri: Option<String>,
    pub download_url: String,
}

/// A one-time, short-lived link for one result asset (no credentials needed).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadTicket {
    /// Absolute URL; works once, until `expires_at`.
    pub url: String,
    pub expires_at: i64,
}

/// An account allowed to share with a Silicon from outside its circle.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Allowance {
    /// The Silicon whose allow list this is.
    pub silicon: AccountRef,
    /// The account it accepts shares from.
    pub account: AccountRef,
    pub created_at: i64,
    #[serde(default)]
    pub created_by: Option<AccountRef>,
}

/// `POST /api/v1/allow`: allow `account` (`c:`/`si:` id or uuid) to share with
/// `silicon` (default: the caller, which must be a Silicon).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowanceInput {
    pub account: String,
    #[serde(default)]
    pub silicon: Option<String>,
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
    /// `community` (the bundled public catalog) or `personal`.
    pub source: String,
    pub source_url: Option<String>,
    pub source_revision: Option<String>,
    /// The creator of a personal entry; `None` for community entries.
    #[serde(default)]
    pub owner: Option<AccountRef>,
    pub can_manage: bool,
    pub template: Option<DirectoryTemplate>,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}
