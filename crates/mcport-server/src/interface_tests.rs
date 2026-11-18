//! Exercise the standalone bearer entry point with separate Accounts, Contacts,
//! Organizations and Ting HTTP boundaries. Every mutation uses disposable accounts.
use crate::test_support::{Fixture, Stub, fixture, fixture_with};
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
    if role.is_empty() {
        return Json(
            json!({"account_uuid":uuid, "organizations":[], "groups":[], "share_targets":[]}),
        );
    }
    let mut groups = vec!["organization:studio"];
    let mut targets =
        vec![json!({"org_id":"studio","type":"organization","id":"studio","name":"Studio"})];
    if matches!(role.as_str(), "member" | "admin" | "team_admin") {
        groups.push("team:studio/design");
        targets.push(json!({"org_id":"studio","type":"team","id":"studio/design","name":"Design"}));
    }
    let managed: Vec<_> = if role == "team_admin" {
        vec!["design"]
    } else {
        vec![]
    };
    Json(
        json!({"account_uuid":uuid,"organizations":[{"org_id":"studio","name":"Studio","capabilities":{"admin":role=="admin","team_admins":managed}}],"groups":groups,"share_targets":targets}),
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
        .route("/team-tools", post(async |Json(v): Json<Value>| {
            let result = match v["method"].as_str() {
                Some("initialize") => json!({"protocolVersion":v["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"team-tools","version":"1"}}),
                Some("tools/list") => json!({"tools":[{"name":"schedule","inputSchema":{"type":"object"}}]}),
                Some("tools/call") => json!({"content":[{"type":"text","text":"team schedule"}]}),
                Some("notifications/initialized") => return StatusCode::ACCEPTED.into_response(),
                _ => return StatusCode::BAD_REQUEST.into_response(),
            };
            Json(json!({"jsonrpc":"2.0","id":v["id"],"result":result})).into_response()
        }))
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
            post(async |State(s): State<Arc<Stub>>, h: HeaderMap, Json(v): Json<Value>| {
                assert_eq!(
                    h["authorization"],
                    format!(
                        "Bearer ting:{}:subscriptions.register",
                        v["for"].as_str().unwrap()
                    )
                );
                let recipient = v["for"].as_str().unwrap();
                s.ting_subscribers.lock().unwrap().insert(recipient.to_string());
                Json(json!({"subscription":{"id":format!("sub-{recipient}")}}))
            }),
        )
        .route("/v1/subscriptions/query", post(async |State(s): State<Arc<Stub>>, h: HeaderMap, Json(v): Json<Value>| {
            assert_eq!(h["authorization"], "Bearer ting::subscriptions.read");
            let who = v["for"].as_str().unwrap();
            Json(json!({"items":[{"id":format!("sub-{who}"),"for":who,"app_id":"mcport","active":s.ting_subscribers.lock().unwrap().contains(who)}]}))
        }))
        .route("/v1/subscriptions/revoke", post(async |State(s): State<Arc<Stub>>, h: HeaderMap, Json(v): Json<Value>| {
            assert_eq!(h["authorization"], "Bearer ting::subscriptions.revoke");
            s.ting_subscribers.lock().unwrap().remove(v["id"].as_str().unwrap().strip_prefix("sub-").unwrap());
            Json(json!({"active":false}))
        }))
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
async fn ting_enrollment_tracks_recipient_revocation() {
    let f = fixture().await;
    f.carbon("Ada", "c:ada");
    let path = "/api/v1/notifications/subscription";
    assert_eq!(ok(&f, "Ada", "GET", path, None).await["subscribed"], false);
    ok(&f, "Ada", "POST", path, None).await;
    assert_eq!(ok(&f, "Ada", "GET", path, None).await["subscribed"], true);
    f.stub.ting_subscribers.lock().unwrap().remove("Ada");
    assert_eq!(ok(&f, "Ada", "GET", path, None).await["subscribed"], false);
    ok(&f, "Ada", "POST", path, None).await;
    for _ in 0..2 {
        assert_eq!(
            ok(&f, "Ada", "DELETE", path, None).await["subscribed"],
            false
        );
    }
    assert!(!f.stub.ting_subscribers.lock().unwrap().contains("Ada"));
}
#[tokio::test]
async fn live_work_membership_owns_the_connection_and_revokes_team_access() {
    let f = fixture_with(|c| {
        c.upstream_origins.insert(c.accounts_url.clone());
    })
    .await;
    for (uuid, id, role) in [
        ("Alice", "c:alice.studio", "admin"),
        ("Bob", "c:bob.studio", "member"),
        ("TeamAdmin", "c:lead.studio", "team_admin"),
        ("OtherTeam", "c:outside.studio", "other_team"),
        ("Personal", "c:alice", ""),
    ] {
        f.carbon(uuid, id);
        f.stub
            .roles
            .lock()
            .unwrap()
            .insert(uuid.into(), role.into());
    }
    let input = json!({"name":"design","visibility":"team:studio/design","transport":"http","url":format!("{}/team-tools", f.url),"auth_mode":"none"});
    let mut org_input = input.clone();
    org_input["visibility"] = json!("organization:studio");
    org_input["name"] = json!("company");
    assert_eq!(
        f.as_(
            "TeamAdmin",
            "POST",
            "/api/v1/connections",
            Some(org_input.clone())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let company = ok(&f, "Alice", "POST", "/api/v1/connections", Some(org_input)).await;
    let company_path = format!("/api/v1/connections/{}", company["id"].as_str().unwrap());
    assert_eq!(
        ok(&f, "OtherTeam", "GET", &company_path, None).await["access"],
        "group_member"
    );
    let c = ok(
        &f,
        "TeamAdmin",
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
    assert_eq!(
        f.as_("OtherTeam", "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    f.stub.roles.lock().unwrap().remove("TeamAdmin");
    assert_eq!(
        ok(&f, "Alice", "GET", &path, None).await["can_manage"],
        true
    );
    let result = ok(
        &f,
        "Bob",
        "POST",
        &format!("{path}/mcp"),
        Some(json!({"method":"tools/call","params":{"name":"schedule","arguments":{}}})),
    )
    .await;
    assert_eq!(result["result"]["content"][0]["text"], "team schedule");
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
