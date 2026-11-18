//! Exercise the standalone bearer entry point with separate Accounts, Contacts,
//! Organizations and Ting HTTP boundaries. Every mutation uses disposable accounts.
use crate::test_support::{Fixture, Stub, UPSTREAM, fixture};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::sync::Arc;

fn subject(token: &str) -> String {
    let claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(token.split('.').nth(1).unwrap())
            .unwrap(),
    )
    .unwrap();
    claims["sub"].as_str().unwrap().into()
}
async fn issue(Json(v): Json<Value>) -> Json<Value> {
    let owner = v["subject_token"].as_str().map(subject).unwrap_or_default();
    Json(
        json!({"proof_token":format!("{}:{}:{}",v["receiving_app"].as_str().unwrap(),owner,v["scopes"].as_array().unwrap().iter().map(|s|s.as_str().unwrap()).collect::<Vec<_>>().join(","))}),
    )
}
fn grant(h: &HeaderMap, scope: &str) -> String {
    let pieces: Vec<_> = h["authorization"]
        .to_str()
        .unwrap()
        .strip_prefix("Proof silicon-contacts:")
        .unwrap()
        .split(':')
        .collect();
    assert!(pieces[1].split(',').any(|s| s == scope));
    pieces[0].to_owned()
}
async fn org(State(s): State<Arc<Stub>>, h: HeaderMap) -> Json<Value> {
    let uuid = subject(h["authorization"].to_str().unwrap());
    let role = s
        .roles
        .lock()
        .unwrap()
        .get(&uuid)
        .cloned()
        .unwrap_or_default();
    let mut groups = vec!["organization:studio"];
    if role == "member" || role == "admin" {
        groups.push("team:studio/design");
    }
    let targets = if groups.len() == 2 {
        json!([{"org_id":"studio","type":"team","id":"studio/design","name":"Design"}])
    } else {
        json!([])
    };
    Json(
        json!({"account_uuid":uuid,"organizations":[{"org_id":"studio","name":"Studio","capabilities":{"admin":role=="admin","team_admins":[]}}],"groups":groups,"share_targets":targets}),
    )
}
async fn one(State(s): State<Arc<Stub>>, h: HeaderMap, Path(id): Path<String>) -> Response {
    match s
        .contacts
        .lock()
        .unwrap()
        .get(&(grant(&h, "contacts.read"), id))
    {
        Some(v) => Json(v.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn save_contact(
    State(s): State<Arc<Stub>>,
    h: HeaderMap,
    Json(mut v): Json<Value>,
) -> Json<Value> {
    let owner = grant(&h, "contacts.write");
    let uuid = v["id"].as_str().unwrap().to_owned();
    v["account"] = s.accounts.lock().unwrap()[&uuid].clone();
    v["uuid"] = json!(uuid);
    s.contacts.lock().unwrap().insert((owner, uuid), v.clone());
    Json(v)
}
pub(crate) fn routes() -> Router<Arc<Stub>> {
    Router::new()
        .route("/v1/integrations/mcport/me", get(org))
        .route("/v1/proofs/user-verification", post(issue))
        .route("/v1/proofs/app-verification", post(issue))
        .route(
            "/v1/me",
            get(async |h: HeaderMap| Json(json!({"account":{"uuid":grant(&h,"contacts.read")}}))),
        )
        .route("/v1/contacts/{id}", get(one))
        .route(
            "/v1/contacts",
            get(async |State(s): State<Arc<Stub>>, h: HeaderMap| {
                let owner = grant(&h, "contacts.read");
                let items: Vec<_> = s
                    .contacts
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|((o, _), _)| o == &owner)
                    .map(|(_, v)| v.clone())
                    .collect();
                Json(json!({"total":items.len(),"items":items}))
            })
            .post(save_contact),
        )
        .route(
            "/v1/subscriptions",
            post(async |h: HeaderMap, Json(v): Json<Value>| {
                assert_eq!(
                    h["authorization"],
                    format!(
                        "Bearer ting:{}:subscriptions.register",
                        v["for"].as_str().unwrap()
                    )
                );
                Json(json!({"subscription":{"id":"local-sub"}}))
            }),
        )
        .route(
            "/v1/tings",
            post(
                async |State(s): State<Arc<Stub>>, h: HeaderMap, Json(v): Json<Value>| {
                    assert_eq!(h["authorization"], "Bearer ting::tings.send");
                    s.tings.lock().unwrap().push(v);
                    Json(json!({"status":"sent"}))
                },
            ),
        )
}
async fn ok(f: &Fixture, who: &str, method: &str, path: &str, body: Option<Value>) -> Value {
    let (status, body) = f.as_(who, method, path, body).await;
    assert_eq!(status, StatusCode::OK, "{method} {path} {body}");
    body["data"].clone()
}
#[tokio::test]
async fn standalone_contacts_ting_and_share_replay() {
    let f = fixture().await;
    f.carbon("Ada", "c:ada");
    f.carbon("Bob", "c:bob");
    f.silicon("Scout", "si:scout", "Ada");
    for (uuid, id) in [("Ada", "c:ada"), ("Bob", "c:bob"), ("Scout", "si:scout")] {
        f.member(
            uuid,
            id,
            "Public name",
            Some("https://accounts.example/photo"),
        );
    }
    ok(
        &f,
        "Ada",
        "PUT",
        "/api/v1/contacts/connection",
        Some(json!({"enabled":true})),
    )
    .await;
    for (id, name, trust) in [
        ("si:scout", "Science partner", 172),
        ("c:bob", "Science reader", 2),
    ] {
        ok(
            &f,
            "Ada",
            "POST",
            "/api/v1/contacts",
            Some(json!({"id":id,"name":name,"trust":trust})),
        )
        .await;
    }
    let people = ok(&f, "Ada", "GET", "/api/v1/people?q=scn", None).await;
    assert_eq!(people["items"][0]["trust"], 172);
    assert_eq!(people["items"][1]["trust"], 2);
    let p = ok(&f, "Ada", "GET", "/api/v1/people/resolve?q=si:scout", None).await;
    assert_eq!(p["display_name"], "Science partner");
    assert_ne!(
        ok(&f, "Bob", "GET", "/api/v1/people/resolve?q=si:scout", None).await["display_name"],
        "Science partner"
    );
    ok(
        &f,
        "Scout",
        "POST",
        "/api/v1/notifications/subscription",
        None,
    )
    .await;
    let entry = ok(
        &f,
        "Ada",
        "POST",
        "/api/v1/directory",
        Some(json!({"name":"Library"})),
    )
    .await;
    let access = format!("/api/v1/directory/{}/access", entry["id"].as_str().unwrap());
    for _ in 0..2 {
        ok(
            &f,
            "Ada",
            "POST",
            &access,
            Some(json!({"account":"si:scout"})),
        )
        .await;
    }
    crate::ting::deliver_due(&f.app).await.unwrap();
    crate::ting::deliver_due(&f.app).await.unwrap();
    let events = f.stub.tings.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["for"], "Scout");
    assert_eq!(events[0]["from"], "Ada");
    assert_eq!(events[0]["type"], "mcport.directoryEntryShared");
    ok(
        &f,
        "Ada",
        "PUT",
        "/api/v1/contacts/connection",
        Some(json!({"enabled":false})),
    )
    .await;
    assert_eq!(
        f.as_(
            "Ada",
            "POST",
            "/api/v1/contacts",
            Some(json!({"id":"c:bob","name":"B","trust":1}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn live_work_membership_owns_the_connection_and_revokes_team_access() {
    let f = fixture().await;
    for (uuid, id, role) in [
        ("Alice", "c:alice.studio", "admin"),
        ("Bob", "c:bob.studio", "member"),
        ("Personal", "c:alice", ""),
    ] {
        f.carbon(uuid, id);
        f.stub
            .roles
            .lock()
            .unwrap()
            .insert(uuid.into(), role.into());
    }
    let input = json!({"name":"design","visibility":"team:studio/design","transport":"http","url":UPSTREAM,"auth_mode":"none"});
    let c = ok(
        &f,
        "Alice",
        "POST",
        "/api/v1/connections",
        Some(input.clone()),
    )
    .await;
    let path = format!("/api/v1/connections/{}", c["id"].as_str().unwrap());
    assert_eq!(c["owner"]["uuid"], "team:studio/design");
    assert_eq!(ok(&f, "Bob", "GET", &path, None).await["can_manage"], false);
    assert_eq!(
        f.as_("Bob", "DELETE", &path, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.as_("Personal", "POST", "/api/v1/connections", Some(input))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.as_("Personal", "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        ok(&f, "Personal", "GET", "/api/v1/organizations", None).await["groups"],
        json!([])
    );
    f.stub.roles.lock().unwrap().remove("Bob");
    assert_eq!(
        f.as_("Bob", "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    for (field, value) in [
        ("test_token_type", "refresh_token"),
        ("test_client_id", "another-app"),
    ] {
        let token = f.token_with("Personal", |c| c[field] = json!(value));
        assert_eq!(
            f.call("GET", "/api/v1/me", Some(&token), None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
}
