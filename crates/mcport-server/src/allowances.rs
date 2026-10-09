//! A Silicon's allow list. Silicons are not open to the world: an account
//! outside a Silicon's circle (its custodian and the custodian's other Silicons)
//! can share a connection or directory entry with it only after the Silicon, or
//! its custodian, allowed that account here. Carbons can be reached by anyone.
use crate::{
    accounts::{self, AccountRow},
    auth::{Auth, Live},
    error::{Error, Result},
    state::App,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use mcport_core::{Allowance, AllowanceInput};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, Default)]
pub struct SiliconQuery {
    /// The Silicon whose list to use (`si:` id or uuid). Default: the caller.
    pub silicon: Option<String>,
}

/// The Silicon whose list the caller manages: itself, or one it looks after.
async fn silicon(app: &App, a: &Auth, input: Option<&str>) -> Result<AccountRow> {
    let target = match input.filter(|s| !s.is_empty()) {
        Some(input) => accounts::resolve(app, input).await?,
        None => a.account.clone(),
    };
    if !target.is_silicon() {
        return Err(Error::new(
            400,
            "not_a_silicon",
            "Allow lists belong to Silicons: any Carbon or Silicon signed in to MCPort can already share with a Carbon.",
            "Name a Silicon you look after with silicon=si:….",
        ));
    }
    if target.uuid != a.uuid() && !accounts::looks_after(app, &a.account, &target).await {
        return Err(Error::new(
            403,
            "access_denied",
            "Only a Silicon and its custodian can change or read its allow list.",
            "Name a Silicon you look after, or leave silicon out to use your own list.",
        ));
    }
    Ok(target)
}
fn view(
    app: &App,
    silicon: &AccountRow,
    account: &str,
    created_by: &str,
    created_at: i64,
) -> Allowance {
    Allowance {
        silicon: silicon.reference(),
        account: accounts::reference(app, account),
        created_at,
        created_by: Some(accounts::reference(app, created_by)),
    }
}
pub async fn list(
    State(app): State<App>,
    a: Auth,
    Query(query): Query<SiliconQuery>,
) -> Result<Json<Value>> {
    let silicon = silicon(&app, &a, query.silicon.as_deref()).await?;
    let out = app
        .store
        .allowances(&silicon.uuid)?
        .into_iter()
        .map(|(account, created_by, created_at)| {
            view(&app, &silicon, &account, &created_by, created_at)
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"data": out})))
}
pub async fn add(
    State(app): State<App>,
    Live(a): Live,
    Json(input): Json<AllowanceInput>,
) -> Result<Json<Value>> {
    let silicon = silicon(&app, &a, input.silicon.as_deref()).await?;
    let account = accounts::resolve(&app, &input.account).await?;
    if account.uuid == silicon.uuid {
        return Err(Error::bad("A Silicon does not need to allow itself."));
    }
    let created_at = app.store.allow(&silicon.uuid, &account.uuid, a.uuid())?;
    let created_by = app
        .store
        .allowances(&silicon.uuid)?
        .into_iter()
        .find(|(uuid, _, _)| uuid == &account.uuid)
        .map(|(_, by, _)| by)
        .unwrap_or_else(|| a.uuid().to_owned());
    Ok(Json(
        json!({"data": view(&app, &silicon, &account.uuid, &created_by, created_at)}),
    ))
}
pub async fn remove(
    State(app): State<App>,
    Live(a): Live,
    Path(account): Path<String>,
    Query(query): Query<SiliconQuery>,
) -> Result<Json<Value>> {
    let silicon = silicon(&app, &a, query.silicon.as_deref()).await?;
    let uuid = if app.store.allows(&silicon.uuid, &account)? {
        account
    } else {
        accounts::resolve(&app, &account).await?.uuid
    };
    let removed = app.store.disallow(&silicon.uuid, &uuid)?;
    Ok(Json(json!({"data": {"deleted": removed}})))
}
