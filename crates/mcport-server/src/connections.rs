use crate::{
    auth::{self, Auth},
    error::{Error, Result},
    state::{App, hash, id, now},
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use mcport_core::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
pub struct Grant {
    pub connection_id: String,
    pub grant: AccessGrant,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Policy {
    pub connection_id: String,
    pub policy: ToolPolicy,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderGrant {
    pub connection_id: String,
    pub owner_id: String,
    pub owner_org: String,
    pub label: String,
    pub kind: String,
    pub secret: String,
    pub header_name: Option<String>,
    pub oauth: Option<Value>,
}

pub fn can_manage(c: &Connection, a: &Auth) -> bool {
    c.environment == a.env() && c.owner_id == a.actor().principal_id && c.org_id == a.actor().org_id
}
pub fn can_use(app: &App, c: &Connection, a: &Auth) -> Result<bool> {
    Ok(c.environment == a.env()
        && c.org_id == a.actor().org_id
        && (can_manage(c, a)
            || (c.visibility == "org" && c.org_id == a.actor().org_id)
            || (c.visibility == "invited"
                && app
                    .store
                    .get::<Grant>("grant", &grant_key(&c.id, &a.actor().principal_id))?
                    .is_some())))
}
pub fn grant_key(c: &str, principal: &str) -> String {
    hash(&json!([c, principal]).to_string())
}
pub fn credential_key(c: &Connection, a: &Auth) -> String {
    let (owner, org) = if c.auth_mode == "shared" {
        (&c.owner_id, &c.org_id)
    } else {
        (&a.actor().principal_id, &a.actor().org_id)
    };
    hash(&json!([c.id, owner, org]).to_string())
}
pub fn policy_key(c: &str, tool: &str, principal: Option<&str>) -> String {
    hash(&json!([c, tool, principal]).to_string())
}
pub fn allowed_tool(app: &App, c: &Connection, a: &Auth, name: &str) -> Result<bool> {
    let global = app
        .store
        .get::<Policy>("policy", &policy_key(&c.id, name, None))?;
    let personal = app.store.get::<Policy>(
        "policy",
        &policy_key(&c.id, name, Some(&a.actor().principal_id)),
    )?;
    Ok(!global.is_some_and(|p| !p.policy.enabled) && !personal.is_some_and(|p| !p.policy.enabled))
}
pub fn resolve(app: &App, a: &Auth, name: &str, manage: bool) -> Result<Connection> {
    let mut candidates = if let Some(c) = app.store.get::<Connection>("connection", name)? {
        vec![c]
    } else {
        app.store
            .list::<Connection>("connection", Some(a.env()))?
            .into_iter()
            .filter(|c| c.name == name)
            .collect()
    };
    candidates.retain(|c| can_use(app, c, a).unwrap_or(false));
    if candidates.len() > 1 {
        return Err(Error::new(
            409,
            "ambiguous_name",
            "More than one connection has that name.",
            "Use the connection ID shown by connection ls.",
        ));
    }
    let c = candidates.pop().ok_or_else(Error::missing)?;
    if manage && !can_manage(&c, a) {
        return Err(Error::denied());
    }
    Ok(c)
}
pub fn account_status(app: &App, c: &Connection, a: &Auth) -> Result<AccountStatus> {
    if c.auth_mode == "none" {
        return Ok(AccountStatus {
            connected: true,
            owner_id: "".into(),
            label: "No authentication".into(),
            kind: "none".into(),
        });
    }
    if let Some(hid) = &c.host_id {
        let owner = if c.auth_mode == "shared" {
            c.owner_id.clone()
        } else {
            a.actor().principal_id.clone()
        };
        let configured = if c.auth_mode == "shared" {
            !app.store
                .get::<crate::hosts::HostRecord>("host", hid)?
                .and_then(|h| h.capabilities.get(&c.id).cloned())
                .and_then(|v| {
                    v.get("shared_account_disconnected")
                        .and_then(Value::as_bool)
                })
                .unwrap_or(false)
        } else {
            app.store
                .get::<crate::hosts::HostRecord>("host", hid)?
                .and_then(|h| h.capabilities.get(&c.id).cloned())
                .and_then(|v| v.get("account_owners").cloned())
                .and_then(|v| v.as_array().cloned())
                .is_some_and(|xs| xs.iter().any(|v| v.as_str() == Some(&owner)))
        };
        return Ok(AccountStatus {
            connected: configured,
            owner_id: owner,
            label: if c.auth_mode == "shared" {
                "Host application account"
            } else {
                "Personal host account"
            }
            .into(),
            kind: "host".into(),
        });
    }
    let g = app
        .store
        .get::<ProviderGrant>("credential", &credential_key(c, a))?;
    Ok(g.map(|g| AccountStatus {
        connected: true,
        owner_id: g.owner_id,
        label: g.label,
        kind: g.kind,
    })
    .unwrap_or(AccountStatus {
        connected: false,
        owner_id: if c.auth_mode == "shared" {
            c.owner_id.clone()
        } else {
            a.actor().principal_id.clone()
        },
        label: "Not connected".into(),
        kind: c.auth_mode.clone(),
    }))
}
pub fn view(app: &App, mut c: Connection, a: &Auth) -> Result<Connection> {
    c.can_manage = can_manage(&c, a);
    c.account = Some(account_status(app, &c, a)?);
    c.status = if let Some(host) = &c.host_id {
        match app.store.get::<crate::hosts::HostRecord>("host", host)? {
            Some(h) if h.last_seen > now() - 35 && h.registered.contains(&c.id) => {
                if !c.account.as_ref().is_some_and(|x| x.connected) {
                    "authentication_required"
                } else {
                    let principal = if c.auth_mode == "per-user" {
                        a.actor().principal_id.as_str()
                    } else {
                        ""
                    };
                    local_mcp_status(
                        h.capabilities
                            .get(&c.id)
                            .and_then(|v| v.get("health"))
                            .and_then(|v| v.get(principal)),
                        h.last_seen,
                        now(),
                    )
                }
            }
            _ => "offline",
        }
    } else if !c.account.as_ref().is_some_and(|x| x.connected) {
        "authentication_required"
    } else {
        "ready"
    }
    .into();
    if !c.can_manage {
        c.command = None;
        c.args.clear();
        c.url = None;
    }
    Ok(c)
}
fn local_mcp_status(health: Option<&Value>, reported_at: i64, at: i64) -> &'static str {
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
pub async fn list(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let a = auth::authenticate(&app, &headers).await?;
    let mut out = Vec::new();
    for c in app.store.list::<Connection>("connection", Some(a.env()))? {
        if can_use(&app, &c, &a)? {
            out.push(view(&app, c, &a)?);
        }
    }
    Ok(Json(json!({"data":out})))
}
pub async fn get(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::authenticate(&app, &headers).await?;
    Ok(Json(
        json!({"data":view(&app,resolve(&app,&a,&name,false)?,&a)?}),
    ))
}
pub async fn create(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<ConnectionInput>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    validate_name(&input.name)?;
    if !matches!(input.transport.as_str(), "http" | "stdio")
        || !matches!(input.auth_mode.as_str(), "none" | "per-user" | "shared")
        || !matches!(input.visibility.as_str(), "private" | "org" | "invited")
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
        Some(crate::hosts::resolve(&app, &a, host)?.host.id)
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
                "stdio requires an owned registered host and an absolute executable path.",
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
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    if let Some(host_id) = &host {
        crate::hosts::resolve(&app, &a, host_id)?;
    }
    let c = Connection {
        id: id(),
        name: input.name,
        description: input.description,
        org_id: a.actor().org_id.clone(),
        owner_id: a.actor().principal_id.clone(),
        environment: a.env().into(),
        transport: input.transport,
        url: input.url,
        host_id: host,
        command: input.command,
        args: input.args,
        auth_mode: input.auth_mode,
        visibility: input.visibility,
        status: "ready".into(),
        can_manage: true,
        account: None,
        created_at: now(),
        updated_at: now(),
        version: 1,
    };
    app.store.put(
        "connection",
        &c.id,
        a.env(),
        &c.org_id,
        &c.owner_id,
        Some(&c.name),
        &c,
        Some(0),
    )?;
    Ok(Json(json!({"data":view(&app,c,&a)?})))
}
pub async fn update(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(input): Json<ConnectionUpdate>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    let mut c = resolve(&app, &a, &name, true)?;
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
    if let Some(v) = input.visibility {
        if !matches!(v.as_str(), "private" | "org" | "invited") {
            return Err(Error::bad("Invalid visibility."));
        }
        c.visibility = v
    }
    c.updated_at = now();
    c.version += 1;
    app.store.put(
        "connection",
        &c.id,
        a.env(),
        &c.org_id,
        &c.owner_id,
        Some(&c.name),
        &c,
        Some(previous),
    )?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    Ok(Json(json!({"data":view(&app,c,&a)?})))
}
pub async fn remove(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    let c = resolve(&app, &a, &name, true)?;
    app.store.delete_connection(&c.id, a.env())?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    Ok(Json(json!({"data":{"deleted":true}})))
}

pub async fn access_list(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::authenticate(&app, &headers).await?;
    let c = resolve(&app, &a, &name, true)?;
    let values = app
        .store
        .list::<Grant>("grant", Some(a.env()))?
        .into_iter()
        .filter(|g| g.connection_id == c.id)
        .map(|g| g.grant)
        .collect::<Vec<_>>();
    Ok(Json(json!({"data":values})))
}
#[derive(Deserialize)]
pub struct Invite {
    principal_id: String,
}
pub async fn invite(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(input): Json<Invite>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    let c = resolve(&app, &a, &name, true)?;
    if !input.principal_id.starts_with("c:") && !input.principal_id.starts_with("si:") {
        return Err(Error::bad(
            "Use a canonical Carbon (c:) or Silicon (si:) ID.",
        ));
    }
    if input.principal_id.len() > 100 {
        return Err(Error::bad("Principal ID is too long."));
    }
    let grant = AccessGrant {
        principal_id: input.principal_id,
        created_at: now(),
    };
    app.store.put(
        "grant",
        &grant_key(&c.id, &grant.principal_id),
        a.env(),
        &c.org_id,
        &c.owner_id,
        None,
        &Grant {
            connection_id: c.id.clone(),
            grant: grant.clone(),
        },
        None,
    )?;
    Ok(Json(json!({"data":grant})))
}
pub async fn uninvite(
    State(app): State<App>,
    headers: HeaderMap,
    Path((name, principal)): Path<(String, String)>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    let c = resolve(&app, &a, &name, true)?;
    app.store.delete("grant", &grant_key(&c.id, &principal))?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    Ok(Json(json!({"data":{"deleted":true}})))
}
pub async fn policies(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::authenticate(&app, &headers).await?;
    let c = resolve(&app, &a, &name, false)?;
    let out = app
        .store
        .list::<Policy>("policy", Some(a.env()))?
        .into_iter()
        .filter(|p| {
            p.connection_id == c.id
                && (can_manage(&c, &a)
                    || p.policy.principal_id.is_none()
                    || p.policy.principal_id.as_deref() == Some(&a.actor().principal_id))
        })
        .map(|p| p.policy)
        .collect::<Vec<_>>();
    Ok(Json(json!({"data":out})))
}
pub async fn set_policy(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(p): Json<ToolPolicy>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    let c = resolve(&app, &a, &name, true)?;
    if p.tool.is_empty() || p.tool.len() > 256 {
        return Err(Error::bad("A tool name is required."));
    }
    app.store.put(
        "policy",
        &policy_key(&c.id, &p.tool, p.principal_id.as_deref()),
        a.env(),
        &c.org_id,
        &c.owner_id,
        None,
        &Policy {
            connection_id: c.id.clone(),
            policy: p.clone(),
        },
        None,
    )?;
    if !p.enabled {
        crate::execution::invalidate_connection(&app, &c.id)?;
    }
    Ok(Json(json!({"data":p})))
}

pub async fn account_get(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::authenticate(&app, &headers).await?;
    let c = resolve(&app, &a, &name, false)?;
    Ok(Json(json!({"data":account_status(&app,&c,&a)?})))
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
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(input): Json<AccountInput>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let c = resolve(&app, &a, &name, false)?;
    if c.host_id.is_some() {
        return Err(Error::new(
            400,
            "configure_on_host",
            "Local connection credentials belong on their execution host.",
            "Run account connect on the registered host.",
        ));
    }
    if c.auth_mode == "none" || c.auth_mode == "shared" && !can_manage(&c, &a) {
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
    let a = auth::authorize_family(&app, &a.session.family, a.env()).await?;
    let c = resolve(&app, &a, &c.id, false)?;
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
    let key = credential_key(&c, &a);
    let lock = app.lock(&format!("credential:{key}"));
    let _guard = lock.lock().await;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    resolve(&app, &a, &c.id, false)?;
    crate::oauth::bump_epoch(&app, &c, &a)?;
    let g = ProviderGrant {
        connection_id: c.id.clone(),
        owner_id: a.actor().principal_id.clone(),
        owner_org: a.actor().org_id.clone(),
        label: input
            .label
            .unwrap_or_else(|| a.actor().display_name.clone()),
        kind: input.kind,
        secret: input.secret,
        header_name: input.header_name,
        oauth: None,
    };
    app.store.put(
        "credential",
        &credential_key(&c, &a),
        a.env(),
        &c.org_id,
        &g.owner_id,
        None,
        &g,
        None,
    )?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    Ok(Json(json!({"data":account_status(&app,&c,&a)?})))
}
pub async fn account_remove(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let c = resolve(&app, &a, &name, false)?;
    if c.host_id.is_some() {
        return Err(Error::new(
            400,
            "configure_on_host",
            "Local credentials must be disconnected on their execution host.",
            "Run account disconnect on the registered host.",
        ));
    }
    if c.auth_mode == "shared" && !can_manage(&c, &a) {
        return Err(Error::denied());
    }
    let key = credential_key(&c, &a);
    let lock = app.lock(&format!("credential:{key}"));
    let _guard = lock.lock().await;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    resolve(&app, &a, &c.id, false)?;
    crate::oauth::bump_epoch(&app, &c, &a)?;
    app.store.delete("credential", &key)?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    Ok(Json(json!({"data":{"disconnected":true}})))
}
