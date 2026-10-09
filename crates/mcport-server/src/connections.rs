//! Connections, who may use and manage them, and their provider accounts.
//!
//! Access: the owner; the custodian of a Silicon owner (manages, never acts as
//! the Silicon); the owner's circle when visibility is `circle`; and accounts the
//! owner (or custodian) added. Records of owners MCPort cannot attribute to an
//! Accounts uuid yet (written before `link-identities`) are visible to nobody.
use crate::{
    accounts::{self, AccountRow},
    auth::{Auth, Live},
    error::{Error, Result},
    state::{App, hash, now},
    store::ENV,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use mcport_core::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Stored connection. `other` keeps fields of earlier releases verbatim (IAM-era
/// identity before `link-identities`, its `legacy` record after); never shown.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ConnectionRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub owner_uuid: String,
    pub transport: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub host_id: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub auth_mode: String,
    pub visibility: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub version: i64,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}
impl ConnectionRecord {
    pub fn save(&self, app: &App, expected: Option<i64>) -> Result<()> {
        app.store.put(
            "connection",
            &self.id,
            ENV,
            &self.owner_uuid,
            &self.owner_uuid,
            Some(&self.name),
            self,
            expected,
        )
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GrantRecord {
    pub connection_id: String,
    #[serde(default)]
    pub account_uuid: String,
    #[serde(default)]
    pub created_at: i64,
    /// Who added it (`None`: added by the operator at cutover).
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PolicyRecord {
    pub connection_id: String,
    #[serde(default)]
    pub tool: String,
    /// `None`: the whole connection.
    #[serde(default)]
    pub account_uuid: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderGrant {
    pub connection_id: String,
    /// Whose provider account this is (the owner for shared, the caller for per-user).
    #[serde(default)]
    pub owner_uuid: String,
    pub label: String,
    pub kind: String,
    pub secret: String,
    pub header_name: Option<String>,
    pub oauth: Option<Value>,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

pub fn grant_key(connection: &str, account: &str) -> String {
    hash(&json!([connection, account]).to_string())
}
/// The provider account record for `account` on `connection`.
pub fn credential_key(connection: &str, account: &str) -> String {
    hash(&json!([connection, account]).to_string())
}
pub fn policy_key(connection: &str, tool: &str, account: Option<&str>) -> String {
    hash(&json!([connection, tool, account]).to_string())
}
/// Whose provider account runs `who`'s calls on `c`.
pub fn execution_account<'a>(c: &'a ConnectionRecord, who: &'a str) -> &'a str {
    if c.auth_mode == "shared" {
        &c.owner_uuid
    } else {
        who
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Owner,
    /// The owner is a Silicon the caller looks after.
    Custodian,
    Circle,
    Invited,
}
impl Access {
    pub fn manages(self) -> bool {
        matches!(self, Access::Owner | Access::Custodian)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Owner => "owner",
            Access::Custodian => "custodian",
            Access::Circle => "circle",
            Access::Invited => "invited",
        }
    }
}
/// Why `who` may use `c`, if at all.
pub async fn access(app: &App, c: &ConnectionRecord, who: &AccountRow) -> Result<Option<Access>> {
    if c.owner_uuid.is_empty() || who.uuid.is_empty() || !who.active() {
        return Ok(None);
    }
    if c.owner_uuid == who.uuid {
        return Ok(Some(Access::Owner));
    }
    let owner = app.store.account(&c.owner_uuid)?;
    if let Some(owner) = &owner {
        if accounts::looks_after(app, who, owner).await {
            return Ok(Some(Access::Custodian));
        }
        // An owner that removed MCPort's access or was deleted freezes what it shares.
        if !owner.active() {
            return Ok(None);
        }
        if matches!(c.visibility.as_str(), "circle" | "org")
            && accounts::same_circle(app, who, owner).await
        {
            return Ok(Some(Access::Circle));
        }
    }
    Ok(app
        .store
        .get::<GrantRecord>("grant", &grant_key(&c.id, &who.uuid))?
        .filter(|grant| grant.account_uuid == who.uuid && grant.connection_id == c.id)
        .map(|_| Access::Invited))
}
pub fn allowed_tool(app: &App, c: &ConnectionRecord, who: &str, name: &str) -> Result<bool> {
    let global = app
        .store
        .get::<PolicyRecord>("policy", &policy_key(&c.id, name, None))?;
    let personal = app
        .store
        .get::<PolicyRecord>("policy", &policy_key(&c.id, name, Some(who)))?;
    Ok(!global.is_some_and(|p| !p.enabled) && !personal.is_some_and(|p| !p.enabled))
}
/// Find a usable connection by exact id, then by name: the caller's own first,
/// then the ones it looks after or was given. Ambiguity answers 409 with the ids.
pub async fn resolve(
    app: &App,
    who: &AccountRow,
    selector: &str,
    manage: bool,
) -> Result<(ConnectionRecord, Access)> {
    let mut found = None;
    if let Some(c) = app.store.get::<ConnectionRecord>("connection", selector)?
        && let Some(access) = access(app, &c, who).await?
    {
        found = Some((c, access));
    }
    if found.is_none() {
        let mut candidates = Vec::new();
        for c in app
            .store
            .list::<ConnectionRecord>("connection", Some(ENV))?
        {
            if c.name == selector
                && let Some(access) = access(app, &c, who).await?
            {
                candidates.push((c, access));
            }
        }
        if candidates
            .iter()
            .filter(|(_, a)| *a == Access::Owner)
            .count()
            == 1
        {
            candidates.retain(|(_, a)| *a == Access::Owner);
        }
        if candidates.len() > 1 {
            let listed = candidates
                .iter()
                .map(|(c, _)| {
                    format!(
                        "{} (owner {})",
                        c.id,
                        accounts::reference(app, &c.owner_uuid).id
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::new(
                409,
                "ambiguous_name",
                format!("More than one connection you can use is named `{selector}`: {listed}."),
                "Use the connection ID shown by connection ls.",
            ));
        }
        found = candidates.pop();
    }
    let (c, access) = found.ok_or_else(Error::missing)?;
    if manage && !access.manages() {
        return Err(Error::new(
            403,
            "access_denied",
            "Only the connection's owner, or the custodian of a Silicon owner, can change it.",
            "Ask the owner to make this change.",
        ));
    }
    Ok((c, access))
}

/// The provider account status `account` has on `c`.
pub fn account_status(
    app: &App,
    c: &ConnectionRecord,
    account: &AccountRow,
) -> Result<AccountStatus> {
    if c.auth_mode == "none" {
        return Ok(AccountStatus {
            connected: true,
            account: None,
            label: "No authentication".into(),
            kind: "none".into(),
        });
    }
    let owner = execution_account(c, &account.uuid).to_owned();
    let reference = Some(accounts::reference(app, &owner));
    if let Some(hid) = &c.host_id {
        let capability = app
            .store
            .get::<crate::hosts::HostRecord>("host", hid)?
            .map(|h| (h.legacy_registry(), h.capabilities.get(&c.id).cloned()));
        let configured = if c.auth_mode == "shared" {
            !capability
                .as_ref()
                .and_then(|(_, v)| v.as_ref())
                .and_then(|v| v.get("shared_account_disconnected"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        } else {
            let keys = crate::hosts::registry_keys(
                app,
                &owner,
                capability.as_ref().is_some_and(|(legacy, _)| *legacy),
            )?;
            capability
                .as_ref()
                .and_then(|(_, v)| v.as_ref())
                .and_then(|v| v.get("account_owners"))
                .and_then(Value::as_array)
                .is_some_and(|owners| {
                    owners
                        .iter()
                        .any(|v| v.as_str().is_some_and(|key| keys.iter().any(|k| k == key)))
                })
        };
        return Ok(AccountStatus {
            connected: configured,
            account: reference,
            label: if c.auth_mode == "shared" {
                "Host application account"
            } else {
                "Personal host account"
            }
            .into(),
            kind: "host".into(),
        });
    }
    let grant = app
        .store
        .get::<ProviderGrant>("credential", &credential_key(&c.id, &owner))?;
    Ok(match grant {
        Some(g) => AccountStatus {
            connected: true,
            account: reference,
            label: g.label,
            kind: g.kind,
        },
        None => AccountStatus {
            connected: false,
            account: reference,
            label: "Not connected".into(),
            kind: c.auth_mode.clone(),
        },
    })
}
pub fn view(
    app: &App,
    c: ConnectionRecord,
    access: Access,
    who: &AccountRow,
) -> Result<Connection> {
    let account = account_status(app, &c, who)?;
    let status = if let Some(host) = &c.host_id {
        match app.store.get::<crate::hosts::HostRecord>("host", host)? {
            Some(h) if h.last_seen > now() - 35 && h.registered.contains(&c.id) => {
                if !account.connected {
                    "authentication_required"
                } else {
                    let health_key = if c.auth_mode == "per-user" {
                        crate::hosts::registry_keys(app, &who.uuid, h.legacy_registry())?
                    } else {
                        vec![String::new()]
                    };
                    let health = h.capabilities.get(&c.id).and_then(|v| v.get("health"));
                    local_mcp_status(
                        health_key
                            .iter()
                            .find_map(|key| health.and_then(|v| v.get(key))),
                        h.last_seen,
                        now(),
                    )
                }
            }
            _ => "offline",
        }
    } else if !account.connected {
        "authentication_required"
    } else {
        "ready"
    };
    let manages = access.manages();
    Ok(Connection {
        owner: accounts::reference(app, &c.owner_uuid),
        status: status.into(),
        can_manage: manages,
        access: access.as_str().into(),
        account: Some(account),
        url: if manages { c.url } else { None },
        command: if manages { c.command } else { None },
        args: if manages { c.args } else { vec![] },
        id: c.id,
        name: c.name,
        description: c.description,
        transport: c.transport,
        host_id: c.host_id,
        auth_mode: c.auth_mode,
        visibility: if c.visibility == "org" {
            "circle".into()
        } else {
            c.visibility
        },
        created_at: c.created_at,
        updated_at: c.updated_at,
        version: c.version,
    })
}
pub(crate) fn local_mcp_status(health: Option<&Value>, reported_at: i64, at: i64) -> &'static str {
    let age = health
        .and_then(|v| v.get("age_seconds"))
        .and_then(Value::as_u64);
    if !age.is_some_and(|age| age.saturating_add(at.saturating_sub(reported_at).max(0) as u64) < 90)
    {
        return "checking";
    }
    match health
        .and_then(|v| v.get("online"))
        .and_then(Value::as_bool)
    {
        Some(true) => "ready",
        Some(false) => "offline",
        None => "checking",
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 80
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(Error::bad(
            "Names must contain 1–80 letters, digits, hyphens or underscores.",
        ));
    }
    Ok(())
}
/// `invited` (default; `private` is accepted for older clients) or `circle`.
fn visibility(requested: &str) -> Result<&'static str> {
    match requested {
        "" | "invited" | "private" => Ok("invited"),
        "circle" => Ok("circle"),
        "org" => Err(Error::new(
            400,
            "visibility_removed",
            "Visibility `org` is no longer supported.",
            "Use `circle` to share with you and the Silicons you look after (for a Silicon: its custodian and the custodian's other Silicons), or `invited` to share only with the accounts you add.",
        )),
        _ => Err(Error::bad("Choose invited or circle visibility.")),
    }
}
pub async fn list(State(app): State<App>, a: Auth) -> Result<Json<Value>> {
    let mut out = Vec::new();
    for c in app
        .store
        .list::<ConnectionRecord>("connection", Some(ENV))?
    {
        if let Some(access) = access(&app, &c, &a.account).await? {
            out.push(view(&app, c, access, &a.account)?);
        }
    }
    Ok(Json(json!({"data":out})))
}
pub async fn get(State(app): State<App>, a: Auth, Path(name): Path<String>) -> Result<Json<Value>> {
    let (c, access) = resolve(&app, &a.account, &name, false).await?;
    Ok(Json(json!({"data":view(&app, c, access, &a.account)?})))
}
pub async fn create(
    State(app): State<App>,
    a: Auth,
    Json(input): Json<ConnectionInput>,
) -> Result<Json<Value>> {
    validate_name(&input.name)?;
    let visibility = visibility(&input.visibility)?;
    if !matches!(input.transport.as_str(), "http" | "stdio")
        || !matches!(input.auth_mode.as_str(), "none" | "per-user" | "shared")
    {
        return Err(Error::bad(
            "Choose a supported transport, authentication mode and visibility.",
        ));
    }
    if input.description.len() > 4096
        || input.args.len() > 128
        || input.args.iter().any(|x| x.len() > 8192)
    {
        return Err(Error::bad("Connection configuration is too large."));
    }
    let host = if let Some(host) = &input.host_id {
        Some(crate::hosts::owned(&app, &a.account, host).await?.host.id)
    } else {
        None
    };
    if input.transport == "stdio" {
        if host.is_none()
            || input
                .command
                .as_ref()
                .is_none_or(|p| !std::path::Path::new(p).is_absolute())
        {
            return Err(Error::bad(
                "stdio requires a registered host you own and an absolute executable path.",
            ));
        }
    } else {
        let u = input
            .url
            .as_ref()
            .ok_or_else(|| Error::bad("HTTP connections require a URL."))?;
        let parsed = url::Url::parse(u).map_err(|_| Error::bad("The MCP URL is invalid."))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(Error::bad(
                "Use an HTTP(S) MCP URL without embedded credentials or fragments.",
            ));
        }
        if host.is_none() {
            crate::execution::network_policy(&app, u).await?;
        }
    }
    let owner = a.uuid().to_owned();
    let mut c = ConnectionRecord {
        id: String::new(),
        name: input.name,
        description: input.description,
        owner_uuid: owner.clone(),
        transport: input.transport,
        url: input.url,
        host_id: host,
        command: input.command,
        args: input.args,
        auth_mode: input.auth_mode,
        visibility: visibility.into(),
        created_at: now(),
        updated_at: now(),
        version: 1,
        other: Map::new(),
    };
    let name = c.name.clone();
    let (c, _) =
        app.store
            .create_public("connection", ENV, &owner, &owner, Some(&name), None, |id| {
                c.id = id;
                c
            })?;
    Ok(Json(
        json!({"data":view(&app, c, Access::Owner, &a.account)?}),
    ))
}
pub async fn update(
    State(app): State<App>,
    Live(a): Live,
    Path(name): Path<String>,
    Json(input): Json<ConnectionUpdate>,
) -> Result<Json<Value>> {
    let (mut c, access) = resolve(&app, &a.account, &name, true).await?;
    let previous = c.version;
    if input.version.is_some_and(|v| v != previous) {
        return Err(Error::new(
            409,
            "revision_conflict",
            "This connection changed.",
            "Refresh before applying the change.",
        ));
    }
    if let Some(name) = input.name {
        validate_name(&name)?;
        c.name = name
    }
    if let Some(d) = input.description {
        if d.len() > 4096 {
            return Err(Error::bad("Description is too long."));
        }
        c.description = d
    }
    let reset = input.visibility.as_deref() == Some("private");
    if let Some(v) = input.visibility {
        c.visibility = visibility(&v)?.into();
    }
    c.updated_at = now();
    c.version += 1;
    // The record's storage revision equals the connection version read above, so
    // a concurrent edit makes this write fail with revision_conflict.
    if reset {
        // An explicit owner-only reset revokes existing invitations atomically.
        app.store
            .put_connection_reset_grants(&c.id, &c.owner_uuid, &c.name, &c, previous)?;
    } else {
        c.save(&app, Some(previous))?;
    }
    crate::execution::invalidate_connection(&app, &c.id)?;
    Ok(Json(json!({"data":view(&app, c, access, &a.account)?})))
}
pub async fn remove(
    State(app): State<App>,
    Live(a): Live,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let (c, _) = resolve(&app, &a.account, &name, true).await?;
    app.store.delete_connection(&c.id)?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    Ok(Json(json!({"data":{"deleted":true}})))
}

fn grant_view(app: &App, g: &GrantRecord) -> AccessGrant {
    AccessGrant {
        account: accounts::reference(app, &g.account_uuid),
        created_at: g.created_at,
        created_by: g
            .created_by
            .as_deref()
            .map(|uuid| accounts::reference(app, uuid)),
    }
}
pub async fn access_list(
    State(app): State<App>,
    a: Auth,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let (c, _) = resolve(&app, &a.account, &name, true).await?;
    let values = app
        .store
        .list::<GrantRecord>("grant", None)?
        .into_iter()
        .filter(|g| g.connection_id == c.id && !g.account_uuid.is_empty())
        .map(|g| grant_view(&app, &g))
        .collect::<Vec<_>>();
    Ok(Json(json!({"data":values})))
}
/// Silicons are not open to the world: sharing with a Silicon outside the
/// sharer's circle needs the Silicon (or its custodian) to have allowed the sharer.
pub async fn ensure_reachable(app: &App, from: &AccountRow, to: &AccountRow) -> Result<()> {
    if !to.is_silicon()
        || accounts::same_circle(app, from, to).await
        || app.store.allows(&to.uuid, &from.uuid)?
    {
        return Ok(());
    }
    Err(Error::new(
        403,
        "silicon_not_reachable",
        format!(
            "{} accepts shares only from its custodian, the custodian's other Silicons and accounts it has allowed.",
            to.label()
        ),
        &format!(
            "Ask {} or its custodian to allow {} (mcport allow add {}), then share again.",
            to.label(),
            from.label(),
            from.label()
        ),
    ))
}
pub async fn invite(
    State(app): State<App>,
    Live(a): Live,
    Path(name): Path<String>,
    Json(input): Json<AccessInput>,
) -> Result<Json<Value>> {
    let (c, _) = resolve(&app, &a.account, &name, true).await?;
    let target = accounts::resolve(&app, &input.account).await?;
    if target.uuid == c.owner_uuid {
        return Err(Error::bad(
            "The owner already has access to its own connection.",
        ));
    }
    ensure_reachable(&app, &a.account, &target).await?;
    let lock = app.lock(&format!("connection:{}", c.id));
    let _guard = lock.lock().await;
    let key = grant_key(&c.id, &target.uuid);
    let grant = match app.store.get::<GrantRecord>("grant", &key)? {
        Some(existing) if existing.account_uuid == target.uuid => existing,
        _ => {
            let grant = GrantRecord {
                connection_id: c.id.clone(),
                account_uuid: target.uuid.clone(),
                created_at: now(),
                created_by: Some(a.uuid().into()),
                other: Map::new(),
            };
            app.store.put(
                "grant",
                &key,
                ENV,
                &c.owner_uuid,
                &c.owner_uuid,
                None,
                &grant,
                None,
            )?;
            grant
        }
    };
    Ok(Json(json!({"data":grant_view(&app, &grant)})))
}
/// The uuid a path segment names: a uuid with a grant, a cached current id, or
/// (for anything else) a lookup in Accounts.
async fn named_account(app: &App, connection: &str, input: &str) -> Result<String> {
    if app
        .store
        .get::<GrantRecord>("grant", &grant_key(connection, input))?
        .is_some()
    {
        return Ok(input.into());
    }
    Ok(accounts::resolve(app, input).await?.uuid)
}
pub async fn uninvite(
    State(app): State<App>,
    Live(a): Live,
    Path((name, account)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let (c, _) = resolve(&app, &a.account, &name, true).await?;
    let uuid = named_account(&app, &c.id, &account).await?;
    app.store.delete("grant", &grant_key(&c.id, &uuid))?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    Ok(Json(json!({"data":{"deleted":true}})))
}
fn policy_view(app: &App, p: PolicyRecord) -> ToolPolicy {
    ToolPolicy {
        account: p
            .account_uuid
            .as_deref()
            .map(|uuid| accounts::reference(app, uuid)),
        tool: p.tool,
        enabled: p.enabled,
    }
}
pub async fn policies(
    State(app): State<App>,
    a: Auth,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let (c, access) = resolve(&app, &a.account, &name, false).await?;
    let out = app
        .store
        .list::<PolicyRecord>("policy", None)?
        .into_iter()
        .filter(|p| {
            p.connection_id == c.id
                && !p.tool.is_empty()
                && (access.manages()
                    || p.account_uuid.is_none()
                    || p.account_uuid.as_deref() == Some(a.uuid()))
        })
        .map(|p| policy_view(&app, p))
        .collect::<Vec<_>>();
    Ok(Json(json!({"data":out})))
}
pub async fn set_policy(
    State(app): State<App>,
    Live(a): Live,
    Path(name): Path<String>,
    Json(p): Json<ToolPolicyInput>,
) -> Result<Json<Value>> {
    let (c, _) = resolve(&app, &a.account, &name, true).await?;
    if p.tool.is_empty() || p.tool.len() > 256 {
        return Err(Error::bad("A tool name is required."));
    }
    let account = match p.account.as_deref().filter(|s| !s.is_empty()) {
        Some(input) => Some(accounts::resolve(&app, input).await?.uuid),
        None => None,
    };
    let record = PolicyRecord {
        connection_id: c.id.clone(),
        tool: p.tool,
        account_uuid: account,
        enabled: p.enabled,
        other: Map::new(),
    };
    app.store.put(
        "policy",
        &policy_key(&c.id, &record.tool, record.account_uuid.as_deref()),
        ENV,
        &c.owner_uuid,
        &c.owner_uuid,
        None,
        &record,
        None,
    )?;
    if !record.enabled {
        crate::execution::invalidate_connection(&app, &c.id)?;
    }
    Ok(Json(json!({"data":policy_view(&app, record)})))
}

#[derive(Deserialize, Default)]
pub struct AccountQuery {
    /// Whose personal provider account to inspect or disconnect: a Silicon the
    /// caller looks after (`c:`/`si:` id or uuid). Default: the caller.
    pub account: Option<String>,
}
/// The account a provider-account request is about: the caller, or a Silicon the
/// caller looks after (custodians may inspect and disconnect, never connect).
async fn subject(app: &App, a: &Auth, query: &AccountQuery) -> Result<AccountRow> {
    let Some(input) = query.account.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(a.account.clone());
    };
    let target = accounts::resolve(app, input).await?;
    if target.uuid == a.uuid() {
        return Ok(a.account.clone());
    }
    if !accounts::looks_after(app, &a.account, &target).await {
        return Err(Error::new(
            403,
            "access_denied",
            "You can only manage the provider accounts of Silicons you look after.",
            "Leave out account to manage your own provider account.",
        ));
    }
    Ok(target)
}
pub async fn account_get(
    State(app): State<App>,
    Live(a): Live,
    Path(name): Path<String>,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>> {
    let (c, _) = resolve(&app, &a.account, &name, false).await?;
    let subject = subject(&app, &a, &query).await?;
    Ok(Json(json!({"data":account_status(&app, &c, &subject)?})))
}
#[derive(Deserialize)]
pub struct AccountInput {
    pub kind: String,
    pub secret: String,
    pub label: Option<String>,
    pub header_name: Option<String>,
}
pub async fn account_set(
    State(app): State<App>,
    Live(a): Live,
    Path(name): Path<String>,
    Json(input): Json<AccountInput>,
) -> Result<Json<Value>> {
    let (c, access) = resolve(&app, &a.account, &name, false).await?;
    if c.host_id.is_some() {
        return Err(Error::new(
            400,
            "configure_on_host",
            "Local connection credentials belong on their execution host.",
            "Run account connect on the registered host.",
        ));
    }
    if c.auth_mode == "none" || c.auth_mode == "shared" && !access.manages() {
        return Err(Error::denied());
    }
    if !matches!(input.kind.as_str(), "bearer" | "header")
        || input.secret.is_empty()
        || input.secret.len() > 16384
        || input.secret.contains(['\r', '\n'])
    {
        return Err(Error::bad("Provide a valid protected provider credential."));
    }
    crate::execution::network_policy(&app, c.url.as_deref().ok_or_else(Error::internal)?).await?;
    let h = input.header_name.as_deref().unwrap_or("Authorization");
    if input.kind == "header"
        && (!h.to_ascii_lowercase().starts_with("x-")
            || h.len() > 100
            || h.contains(['\r', '\n'])
            || axum::http::HeaderName::from_bytes(h.as_bytes()).is_err())
    {
        return Err(Error::bad(
            "Custom credential headers must be an X- header.",
        ));
    }
    let owner = execution_account(&c, a.uuid()).to_owned();
    let key = credential_key(&c.id, &owner);
    let lock = app.lock(&format!("credential:{key}"));
    let _guard = lock.lock().await;
    let (c, _) = resolve(&app, &a.account, &c.id, c.auth_mode == "shared").await?;
    crate::oauth::bump_epoch(&app, &c, &owner)?;
    let g = ProviderGrant {
        connection_id: c.id.clone(),
        owner_uuid: owner.clone(),
        label: input.label.unwrap_or_else(|| a.account.label().to_owned()),
        kind: input.kind,
        secret: input.secret,
        header_name: input.header_name,
        oauth: None,
        other: Map::new(),
    };
    app.store.put(
        "credential",
        &key,
        ENV,
        &c.owner_uuid,
        &owner,
        None,
        &g,
        None,
    )?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    Ok(Json(json!({"data":account_status(&app, &c, &a.account)?})))
}
pub async fn account_remove(
    State(app): State<App>,
    Live(a): Live,
    Path(name): Path<String>,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>> {
    let (c, access) = resolve(&app, &a.account, &name, false).await?;
    if c.host_id.is_some() {
        return Err(Error::new(
            400,
            "configure_on_host",
            "Local credentials must be disconnected on their execution host.",
            "Run account disconnect on the registered host.",
        ));
    }
    if c.auth_mode == "shared" && !access.manages() {
        return Err(Error::denied());
    }
    let subject = subject(&app, &a, &query).await?;
    let owner = execution_account(&c, &subject.uuid).to_owned();
    let key = credential_key(&c.id, &owner);
    let lock = app.lock(&format!("credential:{key}"));
    let _guard = lock.lock().await;
    resolve(&app, &a.account, &c.id, false).await?;
    crate::oauth::bump_epoch(&app, &c, &owner)?;
    app.store.delete("credential", &key)?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    Ok(Json(json!({"data":{"disconnected":true}})))
}

#[cfg(test)]
mod health_tests {
    use super::*;
    #[test]
    fn status_requires_a_recent_protocol_probe() {
        assert_eq!(local_mcp_status(None, 100, 100), "checking");
        assert_eq!(
            local_mcp_status(Some(&json!({"age_seconds":null,"online":false})), 100, 100),
            "checking"
        );
        assert_eq!(
            local_mcp_status(Some(&json!({"age_seconds":3,"online":true})), 100, 105),
            "ready"
        );
        assert_eq!(
            local_mcp_status(Some(&json!({"age_seconds":3,"online":false})), 100, 105),
            "offline"
        );
        assert_eq!(
            local_mcp_status(Some(&json!({"age_seconds":89,"online":true})), 100, 105),
            "checking"
        );
        assert_eq!(
            local_mcp_status(
                Some(&json!({"age_seconds":u64::MAX,"online":true})),
                100,
                105
            ),
            "checking"
        );
    }
}

#[cfg(test)]
mod access_tests {
    use super::*;
    use crate::test_support::{Fixture, connection, fixture};
    use axum::http::StatusCode;

    /// Ada looks after Scout and Pilot; Bob looks after Rover; Cy is on its own.
    async fn people() -> Fixture {
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        f.silicon("Scout", "si:scout", "Ada");
        f.silicon("Pilot", "si:pilot", "Ada");
        f.carbon("Bob", "c:bob");
        f.silicon("Rover", "si:rover", "Bob");
        f.carbon("Cy", "c:cy");
        f
    }
    async fn ids(f: &Fixture, who: &str) -> Vec<(String, String)> {
        let (status, body) = f.as_(who, "GET", "/api/v1/connections", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let mut out: Vec<(String, String)> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["name"].as_str().unwrap().into(),
                    c["access"].as_str().unwrap().into(),
                )
            })
            .collect();
        out.sort();
        out
    }

    #[tokio::test]
    async fn owners_custodians_circles_and_invitations_decide_access() {
        let f = people().await;
        let (status, body) = f
            .as_(
                "Scout",
                "POST",
                "/api/v1/connections",
                Some(connection("scout-private", "none", "")),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let scout_private = body["data"]["id"].as_str().unwrap().to_owned();
        assert_eq!(body["data"]["visibility"], "invited");
        assert_eq!(body["data"]["owner"]["id"], "si:scout");
        assert_eq!(body["data"]["access"], "owner");
        f.as_(
            "Scout",
            "POST",
            "/api/v1/connections",
            Some(connection("scout-circle", "none", "circle")),
        )
        .await;
        f.as_(
            "Ada",
            "POST",
            "/api/v1/connections",
            Some(connection("ada-circle", "none", "circle")),
        )
        .await;
        f.as_(
            "Bob",
            "POST",
            "/api/v1/connections",
            Some(connection("bob-circle", "none", "circle")),
        )
        .await;
        // Ada looks after Scout: she sees and manages everything Scout owns.
        assert_eq!(
            ids(&f, "Ada").await,
            vec![
                ("ada-circle".into(), "owner".into()),
                ("scout-circle".into(), "custodian".into()),
                ("scout-private".into(), "custodian".into()),
            ]
        );
        // Pilot shares Ada's circle: circle connections only, never private ones.
        assert_eq!(
            ids(&f, "Pilot").await,
            vec![
                ("ada-circle".into(), "circle".into()),
                ("scout-circle".into(), "circle".into()),
            ]
        );
        // Bob, Rover and Cy are outside it.
        assert_eq!(
            ids(&f, "Bob").await,
            vec![("bob-circle".into(), "owner".into())]
        );
        assert_eq!(
            ids(&f, "Rover").await,
            vec![("bob-circle".into(), "circle".into())]
        );
        assert!(ids(&f, "Cy").await.is_empty());
        let (status, body) = f
            .as_(
                "Cy",
                "GET",
                &format!("/api/v1/connections/{scout_private}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        // The custodian manages: rename, then share with a Carbon outside the circle.
        let (status, body) = f
            .as_(
                "Ada",
                "PATCH",
                &format!("/api/v1/connections/{scout_private}"),
                Some(json!({"name":"scout-tools","version":1})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["can_manage"], true);
        let (status, body) = f
            .as_(
                "Ada",
                "POST",
                &format!("/api/v1/connections/{scout_private}/access"),
                Some(json!({"account":"c:cy"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["account"]["uuid"], "Cy");
        assert_eq!(body["data"]["account"]["id"], "c:cy");
        assert_eq!(body["data"]["created_by"]["id"], "c:ada");
        assert_eq!(
            ids(&f, "Cy").await,
            vec![("scout-tools".into(), "invited".into())]
        );
        // Use does not grant management.
        let (status, body) = f
            .as_(
                "Cy",
                "DELETE",
                &format!("/api/v1/connections/{scout_private}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let (status, _) = f
            .as_(
                "Pilot",
                "PATCH",
                "/api/v1/connections/scout-circle",
                Some(json!({"name":"taken"})),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // Revoking by the account's id removes access.
        let (status, _) = f
            .as_(
                "Scout",
                "DELETE",
                &format!("/api/v1/connections/{scout_private}/access/c:cy"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(ids(&f, "Cy").await.is_empty());
    }

    #[tokio::test]
    async fn silicons_outside_the_circle_must_allow_a_sharer_first() {
        let f = people().await;
        let (_, body) = f
            .as_(
                "Cy",
                "POST",
                "/api/v1/connections",
                Some(connection("cy-tools", "none", "")),
            )
            .await;
        let id = body["data"]["id"].as_str().unwrap().to_owned();
        let share = |who: &'static str| json!({"account": who});
        let (status, body) = f
            .as_(
                "Cy",
                "POST",
                &format!("/api/v1/connections/{id}/access"),
                Some(share("si:rover")),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"]["code"], "silicon_not_reachable");
        assert!(
            body["error"]["recovery"]
                .as_str()
                .unwrap()
                .contains("mcport allow add c:cy")
        );
        // Carbons are reachable by anyone signed in.
        let (status, _) = f
            .as_(
                "Cy",
                "POST",
                &format!("/api/v1/connections/{id}/access"),
                Some(share("c:bob")),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        // Rover's custodian allows Cy; then the share goes through.
        let (status, body) = f
            .as_(
                "Bob",
                "POST",
                "/api/v1/allow",
                Some(json!({"account":"c:cy","silicon":"si:rover"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["silicon"]["id"], "si:rover");
        assert_eq!(body["data"]["account"]["id"], "c:cy");
        let (status, body) = f.as_("Rover", "GET", "/api/v1/allow", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
        let (status, body) = f
            .as_(
                "Cy",
                "POST",
                &format!("/api/v1/connections/{id}/access"),
                Some(share("si:rover")),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // Nobody else manages Rover's list; Carbons have none.
        let (status, _) = f
            .as_(
                "Ada",
                "POST",
                "/api/v1/allow",
                Some(json!({"account":"c:ada","silicon":"si:rover"})),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, body) = f.as_("Cy", "GET", "/api/v1/allow", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "not_a_silicon");
        // Within a circle no allowance is needed.
        let (_, body) = f
            .as_(
                "Ada",
                "POST",
                "/api/v1/connections",
                Some(connection("ada-tools", "none", "")),
            )
            .await;
        let ada = body["data"]["id"].as_str().unwrap().to_owned();
        let (status, _) = f
            .as_(
                "Ada",
                "POST",
                &format!("/api/v1/connections/{ada}/access"),
                Some(share("si:pilot")),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        // Removing the allowance blocks new shares (existing grants stay).
        let (status, _) = f.as_("Rover", "DELETE", "/api/v1/allow/c:cy", None).await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = f
            .as_(
                "Cy",
                "POST",
                "/api/v1/connections",
                Some(connection("cy-more", "none", "")),
            )
            .await;
        let more = body["data"]["id"].as_str().unwrap().to_owned();
        let (status, _) = f
            .as_(
                "Cy",
                "POST",
                &format!("/api/v1/connections/{more}/access"),
                Some(share("si:rover")),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn ids_resolve_through_accounts_and_removed_values_are_refused() {
        let f = people().await;
        let (status, body) = f
            .as_(
                "Ada",
                "POST",
                "/api/v1/connections",
                Some(connection("docs", "none", "org")),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "visibility_removed");
        let (_, body) = f
            .as_(
                "Ada",
                "POST",
                "/api/v1/connections",
                Some(connection("docs", "none", "private")),
            )
            .await;
        assert_eq!(body["data"]["visibility"], "invited");
        let id = body["data"]["id"].as_str().unwrap().to_owned();
        let (status, body) = f
            .as_(
                "Ada",
                "POST",
                &format!("/api/v1/connections/{id}/access"),
                Some(json!({"account":"c:nobody"})),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "unknown_account");
        // Older clients send principal_id; it is the same field.
        let (status, body) = f
            .as_(
                "Ada",
                "POST",
                &format!("/api/v1/connections/{id}/access"),
                Some(json!({"principal_id":"C:BOB"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["account"]["uuid"], "Bob");
        let (status, body) = f
            .as_(
                "Ada",
                "PUT",
                &format!("/api/v1/connections/{id}/policies"),
                Some(json!({"tool":"write","account":"c:bob","enabled":false})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["account"]["id"], "c:bob");
        let (_, body) = f
            .as_(
                "Bob",
                "GET",
                &format!("/api/v1/connections/{id}/policies"),
                None,
            )
            .await;
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
        let (_, body) = f
            .as_(
                "Ada",
                "GET",
                &format!("/api/v1/connections/{id}/access"),
                None,
            )
            .await;
        assert_eq!(body["data"][0]["account"]["id"], "c:bob");
        let (status, _) = f
            .as_(
                "Bob",
                "GET",
                &format!("/api/v1/connections/{id}/access"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn names_prefer_the_callers_own_connection_and_ambiguity_lists_ids() {
        let f = people().await;
        for who in ["Ada", "Bob", "Cy"] {
            let (status, body) = f
                .as_(
                    who,
                    "POST",
                    "/api/v1/connections",
                    Some(connection("docs", "none", "")),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            if who != "Ada" {
                let id = body["data"]["id"].as_str().unwrap().to_owned();
                f.as_(
                    who,
                    "POST",
                    &format!("/api/v1/connections/{id}/access"),
                    Some(json!({"account":"c:ada"})),
                )
                .await;
            }
        }
        // Names are unique per owner, not globally.
        let (status, _) = f
            .as_(
                "Ada",
                "POST",
                "/api/v1/connections",
                Some(connection("docs", "none", "")),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, body) = f.as_("Ada", "GET", "/api/v1/connections/docs", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["owner"]["id"], "c:ada");
        let (status, body) = f.as_("Bob", "GET", "/api/v1/connections/docs", None).await;
        assert_eq!(body["data"]["owner"]["id"], "c:bob", "{status}");
        // Scout sees none named docs; Cy sees only its own.
        let (status, _) = f
            .as_("Scout", "GET", "/api/v1/connections/docs", None)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, body) = f.as_("Ada", "GET", "/api/v1/connections", None).await;
        let mine = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["owner"]["id"] == "c:ada")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        f.as_(
            "Ada",
            "DELETE",
            &format!("/api/v1/connections/{mine}"),
            None,
        )
        .await;
        let (status, body) = f.as_("Ada", "GET", "/api/v1/connections/docs", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "ambiguous_name");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("c:bob") && message.contains("c:cy"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn accessible_exact_ids_win_and_hidden_ids_do_not_shadow_authorized_names() {
        let f = people().await;
        let ada = f.auth("Ada").await;
        let template = ConnectionRecord {
            name: "template".into(),
            owner_uuid: "Ada".into(),
            transport: "http".into(),
            url: Some("https://mcp.example/mcp".into()),
            auth_mode: "none".into(),
            visibility: "invited".into(),
            version: 1,
            ..Default::default()
        };
        let put = |c: &ConnectionRecord| {
            f.app
                .store
                .put(
                    "connection",
                    &c.id,
                    ENV,
                    &c.owner_uuid,
                    &c.owner_uuid,
                    Some(&c.name),
                    c,
                    Some(0),
                )
                .unwrap();
            let host = crate::hosts::HostRecord {
                host: crate::hosts::HostData {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    owner_uuid: c.owner_uuid.clone(),
                    ..Default::default()
                },
                token_hash: "fixture".into(),
                ..Default::default()
            };
            f.app
                .store
                .put(
                    "host",
                    &c.id,
                    ENV,
                    &c.owner_uuid,
                    &c.owner_uuid,
                    Some(&c.name),
                    &host,
                    Some(0),
                )
                .unwrap();
        };
        // Records Ada cannot use: another owner's, and one never linked to an account.
        for (index, owner) in ["Bob", ""].into_iter().enumerate() {
            let selector = format!("aB{index}");
            put(&ConnectionRecord {
                id: selector.clone(),
                name: format!("hidden-{index}"),
                owner_uuid: owner.into(),
                ..template.clone()
            });
            let authorized = ConnectionRecord {
                // A legacy UUID remains directly addressable after the upgrade.
                id: uuid::Uuid::new_v4().to_string(),
                name: selector.clone(),
                ..template.clone()
            };
            put(&authorized);
            assert_eq!(
                resolve(&f.app, &ada.account, &selector, false)
                    .await
                    .unwrap()
                    .0
                    .id,
                authorized.id
            );
            assert_eq!(
                resolve(&f.app, &ada.account, &authorized.id, true)
                    .await
                    .unwrap()
                    .0
                    .id,
                authorized.id
            );
            assert_eq!(
                crate::hosts::resolve(&f.app, &ada.account, &selector)
                    .await
                    .unwrap()
                    .0
                    .host
                    .id,
                authorized.id
            );
        }
        let exact = ConnectionRecord {
            id: "c9Z".into(),
            name: "exact-id".into(),
            ..template.clone()
        };
        let named = ConnectionRecord {
            id: "b8Y".into(),
            name: exact.id.clone(),
            ..template
        };
        put(&exact);
        put(&named);
        assert_eq!(
            resolve(&f.app, &ada.account, "c9Z", true)
                .await
                .unwrap()
                .0
                .id,
            exact.id
        );
        assert_eq!(
            resolve(&f.app, &ada.account, "b8Y", true)
                .await
                .unwrap()
                .0
                .id,
            named.id
        );
        assert_eq!(
            crate::hosts::resolve(&f.app, &ada.account, "c9Z")
                .await
                .unwrap()
                .0
                .host
                .id,
            exact.id
        );
        assert_eq!(
            crate::hosts::resolve(&f.app, &ada.account, "b8Y")
                .await
                .unwrap()
                .0
                .host
                .id,
            named.id
        );
    }

    #[tokio::test]
    async fn records_not_linked_to_an_account_are_visible_to_nobody() {
        let f = people().await;
        let legacy = json!({"id":"aB0","name":"legacy","description":"","org_id":"tos","owner_id":"c:ada","environment":"production","transport":"http","url":"https://example.com/mcp","host_id":null,"command":null,"args":[],"auth_mode":"none","visibility":"org","status":"ready","can_manage":true,"account":null,"created_at":1,"updated_at":1,"version":1});
        f.app
            .store
            .put(
                "connection",
                "aB0",
                "production",
                "tos",
                "c:ada",
                Some("legacy"),
                &legacy,
                Some(0),
            )
            .unwrap();
        for who in ["Ada", "Scout", "Bob"] {
            assert!(ids(&f, who).await.is_empty());
            let (status, _) = f.as_(who, "GET", "/api/v1/connections/aB0", None).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }
        // A typed write keeps every field of the older shape.
        let mut record = f
            .app
            .store
            .get::<ConnectionRecord>("connection", "aB0")
            .unwrap()
            .unwrap();
        record.description = "edited".into();
        f.app
            .store
            .put(
                "connection",
                "aB0",
                "production",
                "tos",
                "c:ada",
                Some("legacy"),
                &record,
                None,
            )
            .unwrap();
        let raw: Value = f.app.store.get("connection", "aB0").unwrap().unwrap();
        assert_eq!(raw["owner_id"], "c:ada");
        assert_eq!(raw["org_id"], "tos");
        assert_eq!(raw["description"], "edited");
    }
}
