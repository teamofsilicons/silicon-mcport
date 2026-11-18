//! The caller's Silicon Contacts address book and McPort sharing suggestions.
use crate::{
    accounts,
    auth::Auth,
    connections::GrantRecord,
    directory::EntryGrant,
    error::{Error, Result},
    state::{App, id},
    store::ENV,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::header,
    response::IntoResponse,
};
use mcport_core::AccountRef;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use silicon_accounts_client::types::IssueUserVerification;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Default, Serialize)]
pub struct Person {
    #[serde(flatten)]
    account: AccountRef,
    trust: Option<u8>,
    photo_id: Option<Uuid>,
}
impl Person {
    fn saved(value: &Value) -> Result<Self> {
        let account = &value["account"];
        let uuid = account["uuid"]
            .as_str()
            .filter(|u| Some(*u) == value["uuid"].as_str())
            .ok_or_else(unavailable)?;
        let photo = value["photo_id"]
            .as_str()
            .map(Uuid::parse_str)
            .transpose()
            .map_err(|_| unavailable())?;
        let trust = value["trust"]
            .as_u64()
            .and_then(|t| u8::try_from(t).ok())
            .filter(|t| *t > 0)
            .ok_or_else(unavailable)?;
        Ok(Self {
            account: AccountRef {
                uuid: uuid.into(),
                id: account["id"].as_str().unwrap_or_default().into(),
                kind: account["kind"].as_str().unwrap_or_default().into(),
                display_name: value["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| account["display_name"].as_str())
                    .unwrap_or_default()
                    .into(),
                pfp_url: photo
                    .map(|id| format!("/api/v1/contacts/photos/{id}"))
                    .or_else(|| {
                        account["pfp_url"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                    }),
            },
            trust: Some(trust),
            photo_id: photo,
        })
    }
}
fn unavailable() -> Error {
    Error::new(
        503,
        "contacts_unavailable",
        "Silicon Contacts could not complete the request.",
        "Try again when Silicon Contacts is available.",
    )
}
fn connected(app: &App, who: &str) -> Result<bool> {
    Ok(app
        .store
        .get::<bool>("contacts_connection", who)?
        .unwrap_or(false))
}
pub(crate) async fn user_proof(
    app: &App,
    a: &Auth,
    receiver: &str,
    scopes: &[&str],
) -> Result<String> {
    let proof = app
        .accounts
        .app()
        .issue_user_verification(
            &IssueUserVerification {
                subject_token: a.token().into(),
                receiving_app: receiver.into(),
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
                access_ttl_seconds: Some(180),
            },
            Some(&id()),
        )
        .await
        .map_err(|e| {
            Error::new(
                if matches!(e.status(), Some(401 | 403)) {
                    403
                } else {
                    503
                },
                "verification_unavailable",
                "Accounts could not authorize this integration.",
                "Check the app's access and try again.",
            )
        })?;
    Ok(proof.proof_token.expose().to_owned())
}
async fn contact_request(
    app: &App,
    proof: &str,
    method: Method,
    segments: &[&str],
    query: &[(&str, String)],
    body: Option<&Value>,
) -> Result<reqwest::Response> {
    let mut url = url::Url::parse(&app.config.contacts_url).map_err(|_| unavailable())?;
    url.path_segments_mut()
        .map_err(|_| unavailable())?
        .pop_if_empty()
        .push("v1")
        .extend(segments.iter().copied());
    url.query_pairs_mut()
        .extend_pairs(query.iter().map(|(key, value)| (*key, value)));
    let mut req = app
        .http
        .request(method, url)
        .header("authorization", format!("Proof {proof}"));
    if let Some(body) = body {
        req = req.json(body);
    }
    let reply = req.send().await.map_err(|_| unavailable())?;
    if reply.status().is_success() {
        return Ok(reply);
    }
    let status = reply.status().as_u16();
    if status == 404 {
        return Err(Error::missing());
    }
    if (400..500).contains(&status) {
        let data = reply.json::<Value>().await.unwrap_or_default();
        return Err(Error::new(
            status,
            "contacts_request_failed",
            data["error"]["message"]
                .as_str()
                .unwrap_or("Contacts refused the request."),
            "Check the contact details and your connection to Silicon Contacts.",
        ));
    }
    Err(unavailable())
}
async fn read(app: &App, proof: &str, path: &[&str], query: &[(&str, String)]) -> Result<Value> {
    contact_request(app, proof, Method::GET, path, query, None)
        .await?
        .json()
        .await
        .map_err(|_| unavailable())
}
async fn proof(app: &App, a: &Auth, scopes: &[&str]) -> Result<String> {
    if !connected(app, a.uuid())? {
        return Err(Error::new(
            403,
            "contacts_not_connected",
            "Connect Silicon Contacts to use your address book.",
            "Open Contacts in McPort settings.",
        ));
    }
    user_proof(app, a, "silicon-contacts", scopes).await
}
#[derive(Deserialize)]
pub struct Consent {
    enabled: bool,
}
pub async fn connect(
    State(app): State<App>,
    a: Auth,
    Json(input): Json<Consent>,
) -> Result<Json<Value>> {
    if input.enabled {
        let proof = user_proof(&app, &a, "silicon-contacts", &["contacts.read"]).await?;
        let me = read(&app, &proof, &["me"], &[]).await?;
        if me["uuid"]
            .as_str()
            .or_else(|| me.pointer("/account/uuid").and_then(Value::as_str))
            != Some(a.uuid())
        {
            return Err(unavailable());
        }
        app.store.put(
            "contacts_connection",
            a.uuid(),
            ENV,
            a.uuid(),
            a.uuid(),
            None,
            &true,
            None,
        )?;
    } else {
        app.store.delete("contacts_connection", a.uuid())?;
    }
    Ok(Json(json!({"data":{"connected":input.enabled}})))
}
async fn saved_people(app: &App, a: &Auth) -> Result<(BTreeMap<String, Person>, BTreeSet<String>)> {
    let token = proof(app, a, &["contacts.read"]).await?;
    let mut people = BTreeMap::new();
    let mut blocked = BTreeSet::new();
    let mut offset = 0;
    loop {
        let page = read(
            app,
            &token,
            &["contacts"],
            &[("offset", offset.to_string()), ("limit", "200".into())],
        )
        .await?;
        let rows = page["items"].as_array().ok_or_else(unavailable)?;
        for row in rows {
            if row["blocked"] == true
                || row.pointer("/account/status").and_then(Value::as_str) == Some("deleted")
            {
                if let Some(uuid) = row["uuid"].as_str() {
                    blocked.insert(uuid.to_owned());
                }
            } else {
                let person = Person::saved(row)?;
                people.insert(person.account.uuid.clone(), person);
            }
        }
        offset += rows.len();
        if rows.is_empty() || offset >= page["total"].as_u64().ok_or_else(unavailable)? as usize {
            break;
        }
    }
    Ok((people, blocked))
}
fn peers(app: &App, a: &Auth) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    if let Some(circle) = a.account.household() {
        ids.insert(circle.into());
        ids.extend(
            app.store
                .silicons_of(circle)?
                .into_iter()
                .filter(|s| s.active())
                .map(|s| s.uuid),
        );
    }
    let mut add_grant = |from: Option<&str>, to: &str| {
        if from == Some(a.uuid()) {
            ids.insert(to.to_owned());
        }
        if to == a.uuid()
            && let Some(from) = from
        {
            ids.insert(from.to_owned());
        }
    };
    for g in app.store.list::<GrantRecord>("grant", None)? {
        add_grant(g.created_by.as_deref(), &g.account_uuid);
    }
    for g in app.store.list::<EntryGrant>("directory_grant", None)? {
        add_grant(Some(&g.created_by), &g.account_uuid);
    }
    for (who, _, _) in app.store.allowances(a.uuid())? {
        ids.insert(who);
    }
    ids.remove(a.uuid());
    Ok(ids)
}
#[derive(Deserialize, Default)]
pub struct Search {
    #[serde(default)]
    q: String,
    kind: Option<String>,
    #[serde(default)]
    saved: bool,
}
fn fuzzy(needle: &str, haystack: &str) -> bool {
    let mut letters = haystack.chars();
    needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|next| letters.any(|c| c == next))
}
pub async fn list(
    State(app): State<App>,
    a: Auth,
    Query(query): Query<Search>,
) -> Result<Json<Value>> {
    let connected = connected(&app, a.uuid())?;
    let (mut people, blocked) = if connected {
        saved_people(&app, &a).await?
    } else {
        Default::default()
    };
    if !query.saved {
        for uuid in peers(&app, &a)? {
            if !blocked.contains(&uuid)
                && !people.contains_key(&uuid)
                && let Some(row) = app.store.account(&uuid)?.filter(|r| r.active())
            {
                people.insert(
                    uuid,
                    Person {
                        account: row.reference(),
                        ..Default::default()
                    },
                );
            }
        }
    }
    let q = query.q.trim().to_lowercase();
    let mut items: Vec<_> = people
        .into_values()
        .filter(|p| {
            query.kind.as_ref().is_none_or(|k| k == &p.account.kind)
                && (fuzzy(&q, &p.account.display_name.to_lowercase())
                    || fuzzy(&q, &p.account.id.to_lowercase()))
        })
        .collect();
    items.sort_by_cached_key(|p| {
        (
            std::cmp::Reverse(p.trust.unwrap_or(0)),
            p.account.display_name.to_lowercase(),
            p.account.id.clone(),
            p.account.uuid.clone(),
        )
    });
    Ok(Json(json!({"data":{"connected":connected,"items":items}})))
}
pub async fn resolve(
    State(app): State<App>,
    a: Auth,
    Query(q): Query<Search>,
) -> Result<Json<Value>> {
    let row = accounts::resolve(&app, a.uuid(), q.q.trim()).await?;
    if !row.active() {
        return Err(Error::missing());
    }
    if connected(&app, a.uuid())? {
        let proof = proof(&app, &a, &["contacts.read"]).await?;
        match read(&app, &proof, &["contacts", &row.uuid], &[]).await {
            Ok(value) if value["blocked"] == true => return Err(Error::missing()),
            Ok(value) => return Ok(Json(json!({"data":Person::saved(&value)?}))),
            Err(err) if err.0.as_u16() == 404 => {}
            Err(err) => return Err(err),
        }
    }
    let account = if row.display_name.is_empty() && row.pfp_url.is_none() {
        accounts::apply_profile(&app, row).await
    } else {
        row
    };
    Ok(Json(
        json!({"data":Person{account:account.reference(),..Default::default()}}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    id: String,
    name: String,
    trust: u8,
}
pub async fn save(State(app): State<App>, a: Auth, Json(input): Json<Save>) -> Result<Json<Value>> {
    if input.trust == 0 || input.name.trim().is_empty() || input.name.len() > 200 {
        return Err(Error::bad(
            "Enter a name of up to 200 bytes and trust from 1 to 255.",
        ));
    }
    let token = proof(&app, &a, &["contacts.read", "contacts.write"]).await?;
    let account = accounts::resolve(&app, a.uuid(), &input.id).await?;
    if account.uuid == a.uuid() {
        return Err(Error::bad("Choose another account."));
    }
    let lock = app.lock(&format!("contact:{}/{}", a.uuid(), account.uuid));
    let _guard = lock.lock().await;
    let mut body = json!({"id":account.uuid,"name":input.name.trim(),"trust":input.trust});
    match read(&app, &token, &["contacts", &account.uuid], &[]).await {
        Ok(old) => {
            for key in [
                "note",
                "phone",
                "email",
                "photo_id",
                "birthday_month",
                "birthday_day",
                "birthday_year",
            ] {
                if let Some(v) = old.get(key) {
                    body[key] = v.clone();
                }
            }
        }
        Err(e) if e.0.as_u16() == 404 => {}
        Err(e) => return Err(e),
    }
    let response = contact_request(&app, &token, Method::POST, &["contacts"], &[], Some(&body))
        .await?
        .json()
        .await
        .map_err(|_| unavailable())?;
    Ok(Json(json!({"data":Person::saved(&response)?})))
}
pub async fn photo(
    State(app): State<App>,
    a: Auth,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse> {
    let token = proof(&app, &a, &["contacts.read"]).await?;
    let response = contact_request(
        &app,
        &token,
        Method::GET,
        &["photos", &id.to_string()],
        &[],
        None,
    )
    .await?;
    let bytes = response.bytes().await.map_err(|_| unavailable())?;
    Ok(([(header::CONTENT_TYPE, "image/png")], bytes))
}
