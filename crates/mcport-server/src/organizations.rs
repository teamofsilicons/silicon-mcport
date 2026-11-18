//! Work accounts receive a fresh, UUID-bound membership view from Organizations.
use crate::{
    accounts::AccountRow,
    auth::Auth,
    connections::Access,
    error::{Error, Result},
    state::{App, hash, now},
};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Context {
    pub account_uuid: String,
    pub organizations: Vec<Organization>,
    pub groups: Vec<String>,
    pub share_targets: Vec<Target>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Organization {
    pub org_id: String,
    pub name: String,
    pub capabilities: Capabilities,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Capabilities {
    pub admin: bool,
    pub team_admins: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Target {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub org_id: String,
    pub name: String,
}

impl Context {
    pub fn target(&self, key: &str) -> Option<&Target> {
        self.share_targets
            .iter()
            .find(|t| key.split_once(':') == Some((t.kind.as_str(), t.id.as_str())))
    }
    pub fn access(&self, key: &str) -> Option<Access> {
        let target = self.target(key)?;
        let org = self
            .organizations
            .iter()
            .find(|org| org.org_id == target.org_id)?;
        if !self
            .groups
            .contains(&format!("organization:{}", org.org_id))
        {
            return None;
        }
        let manages_team = target.kind == "team"
            && target.id.split_once('/').is_some_and(|(owner, team)| {
                owner == org.org_id && org.capabilities.team_admins.iter().any(|id| id == team)
            });
        if org.capabilities.admin || manages_team {
            return Some(Access::GroupManager);
        }
        self.groups
            .iter()
            .any(|g| g == key)
            .then_some(Access::GroupMember)
    }
}
pub fn is_group(key: &str) -> bool {
    key.starts_with("team:") || key.starts_with("organization:")
}
pub async fn populate(app: &App, row: &mut AccountRow, token: &str) -> Result<()> {
    // Accounts reserves dotted handles for work accounts. Only the upstream UUID-bound
    // membership response can grant access. Personal sessions never need an Organizations call.
    if !row.id.contains('.') {
        return Ok(());
    }
    let unavailable = || {
        Error::new(
            503,
            "organizations_unavailable",
            "McPort could not verify this work account's membership.",
            "Try again when Silicon Organizations is available.",
        )
    };
    let response = app
        .http
        .get(format!(
            "{}/v1/integrations/{}/me",
            app.config.organizations_url, app.config.app_id
        ))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| unavailable())?;
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(Error::denied());
    }
    let context: Context = response
        .error_for_status()
        .map_err(|_| unavailable())?
        .json()
        .await
        .map_err(|_| unavailable())?;
    if context.account_uuid != row.uuid {
        return Err(unavailable());
    }
    row.organization = Some(context);
    Ok(())
}
pub async fn get(a: Auth) -> Json<Value> {
    Json(
        json!({"data":a.account.organization.as_ref().cloned().unwrap_or_else(||Context{account_uuid:a.uuid().into(),..Default::default()})}),
    )
}
// Short-lived continuations refer to the initiating access token by digest. The bearer never
// enters records, responses or logs. A restart or expiry requires a fresh operation.
#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, (Arc<str>, i64)>>);
impl Sessions {
    pub fn retain(&self, a: &Auth) -> String {
        let key = hash(a.token());
        let mut sessions = self.0.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|_, (_, expiry)| *expiry > now());
        sessions.insert(key.clone(), (Arc::from(a.token()), a.expires_at));
        key
    }
    fn token(&self, key: &str) -> Result<Arc<str>> {
        let mut sessions = self.0.lock().map_err(|_| Error::internal())?;
        sessions.retain(|_, (_, expiry)| *expiry > now());
        sessions
            .get(key)
            .map(|v| v.0.clone())
            .ok_or_else(Error::expired)
    }
}
pub async fn restore(app: &App, a: &mut AccountRow, key: Option<&str>) -> Result<()> {
    if let Some(key) = key {
        let token = app.work_sessions.token(key)?;
        populate(app, a, &token).await?;
    }
    Ok(())
}
