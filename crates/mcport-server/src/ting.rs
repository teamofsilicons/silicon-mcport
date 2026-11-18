//! Share changes and their Ting outbox event commit together. Hooks are a recipient-side Ting option.
use crate::{
    auth::Auth,
    error::{Error, Result},
    people::user_proof,
    state::{App, id, now},
    store::ENV,
};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use silicon_accounts_client::types::IssueAppVerification;
use tokio_util::sync::CancellationToken;

const TING: &str = "ting";
const TYPES: [(&str, &str); 2] = [
    (
        "connectionShared",
        "A Carbon or Silicon shared a McPort connection with you.",
    ),
    (
        "directoryEntryShared",
        "A Carbon or Silicon shared a McPort directory entry with you.",
    ),
];
fn unavailable() -> Error {
    Error::new(
        503,
        "ting_unavailable",
        "Silicon Ting could not complete the request.",
        "Try again or check your subscription in Silicon Ting.",
    )
}
async fn app_proof(app: &App, scope: &str) -> Result<String> {
    let proof = app
        .accounts
        .app()
        .issue_app_verification(
            &IssueAppVerification {
                receiving_app: TING.into(),
                scopes: vec![scope.into()],
                access_ttl_seconds: Some(180),
            },
            Some(&id()),
        )
        .await
        .map_err(|_| unavailable())?;
    Ok(proof.proof_token.expose().into())
}
async fn post(app: &App, path: &str, proof: &str, body: &Value) -> Result<Value> {
    let reply = app
        .http
        .post(format!("{}{path}", app.config.ting_url))
        .bearer_auth(proof)
        .json(body)
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !reply.status().is_success() {
        return Err(unavailable());
    }
    reply.json().await.map_err(|_| unavailable())
}
pub async fn subscription(State(app): State<App>, a: Auth) -> Result<Json<Value>> {
    Ok(Json(
        json!({"data":{"subscribed":app.store.get::<String>("ting_subscription",a.uuid())?.is_some(),"manage_url":app.config.ting_url}}),
    ))
}
pub async fn enroll(State(app): State<App>, a: Auth) -> Result<Json<Value>> {
    let token = user_proof(&app, &a, TING, &["subscriptions.register"]).await?;
    let result = post(
        &app,
        "/v1/subscriptions",
        &token,
        &json!({"app_id":app.config.app_id,"for":a.uuid()}),
    )
    .await?;
    let sub = result["id"]
        .as_str()
        .or_else(|| result.pointer("/subscription/id").and_then(Value::as_str))
        .ok_or_else(unavailable)?;
    app.store.put(
        "ting_subscription",
        a.uuid(),
        ENV,
        a.uuid(),
        a.uuid(),
        None,
        &sub,
        None,
    )?;
    Ok(Json(
        json!({"data":{"subscribed":true,"manage_url":app.config.ting_url}}),
    ))
}

struct ShareKind {
    record: &'static str,
    event: &'static str,
    page: &'static str,
}
#[derive(Clone, Copy)]
pub enum Share {
    Connection,
    Directory,
}
impl Share {
    fn kind(self) -> ShareKind {
        match self {
            Self::Connection => ShareKind {
                record: "grant",
                event: "connectionShared",
                page: "connections",
            },
            Self::Directory => ShareKind {
                record: "directory_grant",
                event: "directoryEntryShared",
                page: "directory",
            },
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Event {
    id: String,
    kind: String,
    grant_key: String,
    recipient: String,
    created: i64,
    next: i64,
    attempts: u32,
    body: Value,
}

pub struct ShareDetails<'a> {
    pub kind: Share,
    pub owner: &'a str,
    pub recipient: &'a str,
    pub sender: &'a str,
    pub resource_id: &'a str,
    pub name: &'a str,
}
/// Returns the committed grant, including on a concurrent or repeated invitation.
pub fn grant<T: Serialize + serde::de::DeserializeOwned>(
    app: &App,
    key: &str,
    details: ShareDetails<'_>,
    grant: &T,
) -> Result<T> {
    let ShareDetails {
        kind,
        owner,
        recipient,
        sender,
        resource_id,
        name,
    } = details;
    let kind = kind.kind();
    let event_id = id();
    let url = if kind.page == "directory" {
        format!("{}/directory", app.config.web_url)
    } else {
        format!("{}/connections/{}", app.config.web_url, resource_id)
    };
    let body = json!({"type":format!("{}.{}",app.config.app_id,kind.event),"for":recipient,"from":sender,"key":event_id,"data":{"resource_id":resource_id,"name":name,"url":url},"metadata":{"url":url}});
    let event = Event {
        id: event_id.clone(),
        kind: kind.record.into(),
        grant_key: key.into(),
        recipient: recipient.into(),
        created: now(),
        next: now(),
        attempts: 0,
        body,
    };
    let mut value = serde_json::to_value(grant)?;
    value["notification_id"] = json!(event_id);
    app.store.raw_transaction(true, |tx| {
        if tx.insert(kind.record, key, owner, owner, &value)? {
            tx.insert(
                "ting_event",
                &event.id,
                recipient,
                recipient,
                &serde_json::to_value(&event)?,
            )?;
        }
        Ok(())
    })?;
    app.store.get(kind.record, key)?.ok_or_else(Error::missing)
}

async fn register(app: &App) -> Result<()> {
    let proof = app_proof(app, "types.manage").await?;
    for (name, description) in TYPES {
        let schema = json!({"type":"object","additionalProperties":false,"properties":{"resource_id":{"type":"string"},"name":{"type":"string"},"url":{"type":"string"}},"required":["resource_id","name","url"]});
        post(app,&format!("/v1/apps/{}/types",app.config.app_id),&proof,&json!({"type":format!("{}.{}",app.config.app_id,name),"description":description,"schema":schema})).await?;
    }
    Ok(())
}
pub(crate) async fn deliver_due(app: &App) -> Result<()> {
    let due: Vec<_> = app
        .store
        .list::<Event>("ting_event", None)?
        .into_iter()
        .filter(|e| e.next <= now())
        .take(32)
        .collect();
    if due.is_empty() {
        return Ok(());
    }
    let proof = app_proof(app, "tings.send").await?;
    for mut event in due {
        let current = app.store.get::<Value>(&event.kind, &event.grant_key)?;
        if current.as_ref().and_then(|g| g["notification_id"].as_str()) != Some(&event.id)
            || now() - event.created >= 13 * 86400
        {
            app.store.delete("ting_event", &event.id)?;
            continue;
        }
        if post(app, "/v1/tings", &proof, &event.body).await.is_ok() {
            app.store.delete("ting_event", &event.id)?;
        } else {
            event.attempts = event.attempts.saturating_add(1);
            event.next = now() + (5_i64 << event.attempts.min(9)).min(1800);
            app.store.update::<Event>("ting_event", &event.id, |e| {
                e.next = event.next;
                e.attempts = event.attempts;
                true
            })?;
        }
    }
    Ok(())
}
pub async fn worker(app: App, stop: CancellationToken) {
    let mut ready = false;
    loop {
        if !ready {
            ready = register(&app).await.is_ok();
        }
        if ready && let Err(e) = deliver_due(&app).await {
            tracing::warn!(code=%e.1.code,"Ting delivery will retry");
        }
        tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_secs(if ready {5} else {60}))=>{}}
    }
}
