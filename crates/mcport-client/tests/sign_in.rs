//! Sign-in and session storage against a stub Silicon Accounts (the token, device
//! and revoke endpoints MCPort's public client uses).
#![cfg(all(feature = "accounts", feature = "session"))]

use axum::{Form, Json, Router, extract::State, http::StatusCode, routing::post};
use mcport_client::accounts::{DeviceProgress, SignIn, SignInError, SltRefusal};
use mcport_client::session::{SessionError, SessionFile, StoredSignIn};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct Stub {
    /// Scripted answers to device polls: pending, slow_down, denied, expired, tokens.
    device: Mutex<VecDeque<&'static str>>,
    /// The refresh token that currently works; every refresh rotates it.
    refresh: Mutex<String>,
    rotations: Mutex<u32>,
    /// Every form posted to the token and revoke endpoints.
    forms: Mutex<Vec<BTreeMap<String, String>>>,
    /// Answer refreshes with 503 (a passing failure).
    unavailable: Mutex<bool>,
}
type Shared = Arc<Stub>;

fn tokens(n: u32) -> Value {
    json!({"access_token":format!("access-{n}"),"token_type":"Bearer","expires_in":1800,"refresh_token":format!("sar_{n}"),"refresh_token_expires_at":"2029-03-25T02:31:52.745Z","scope":"profile","membership_id":"mcport:zQo","account":{"uuid":"zQo","membership_id":"mcport:zQo","kind":"carbon","id":"c:ada","display_name":"Ada","pfp_url":"https://pfp.example/zQo","updated_at":"2026-10-07T02:31:16.356Z","version":2}})
}
fn oauth(error: &str, description: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":error,"error_description":description})),
    )
}

async fn authorize(Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["client_id"], "mcport");
    Json(
        json!({"device_code":"sad_secret-device-code","user_code":"MVHB-KQAW","verification_uri":"http://127.0.0.1/device","verification_uri_complete":"http://127.0.0.1/device?code=MVHB-KQAW","expires_in":600,"interval":5}),
    )
}

async fn token(
    State(stub): State<Shared>,
    Form(form): Form<BTreeMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    stub.forms.lock().unwrap().push(form.clone());
    assert_eq!(form["client_id"], "mcport");
    assert!(!form.contains_key("client_secret"));
    match form["grant_type"].as_str() {
        "urn:ietf:params:oauth:grant-type:device_code" => {
            assert_eq!(form["device_code"], "sad_secret-device-code");
            match stub.device.lock().unwrap().pop_front().unwrap_or("pending") {
                "pending" => oauth("authorization_pending", "Not approved yet."),
                "slow_down" => oauth("slow_down", "Poll every 5 seconds."),
                "denied" => oauth("access_denied", "The Carbon denied the request."),
                "expired" => oauth("expired_token", "The code expired."),
                _ => (StatusCode::OK, Json(tokens(1))),
            }
        }
        "urn:silicon:params:oauth:grant-type:slt" => match form["slt"].as_str() {
            "slt_ok" => (StatusCode::OK, Json(tokens(1))),
            "slt_used" => oauth(
                "invalid_grant",
                "The short-lived token was already used at 2026-10-10T01:00:00Z.",
            ),
            "slt_late" => oauth(
                "invalid_grant",
                "The short-lived token expired at 2026-10-10T01:00:00Z (they last 120 seconds).",
            ),
            "slt_remind" => oauth(
                "invalid_grant",
                "The short-lived token was issued for the app 'remind', not for 'mcport'.",
            ),
            "slt_typo" => oauth(
                "invalid_grant",
                "The short-lived token is not known to Silicon Accounts.",
            ),
            "slt_rotated" => oauth(
                "invalid_grant",
                "The short-lived token was issued at 2026-10-10T01:00:00Z by a sign-in of si:rusty that ended when its custodian rotated its STK at 2026-10-10T01:01:00Z.",
            ),
            "slt_public_off" => (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({"error":"unauthorized_client","error_description":"mcport has not turned on public_client."}),
                ),
            ),
            other => panic!("unexpected slt {other}"),
        },
        "refresh_token" => {
            if *stub.unavailable.lock().unwrap() {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(
                        json!({"error":{"code":"unavailable","message":"Down for a moment.","hint":"Retry."}}),
                    ),
                );
            }
            // Widen the window in which a second refresher could race.
            tokio::time::sleep(Duration::from_millis(150)).await;
            let mut current = stub.refresh.lock().unwrap();
            if form["refresh_token"] != *current {
                return oauth(
                    "invalid_grant",
                    "The refresh token was already used, so this sign-in was revoked.",
                );
            }
            let mut rotations = stub.rotations.lock().unwrap();
            *rotations += 1;
            *current = format!("sar_{}", *rotations + 1);
            (StatusCode::OK, Json(tokens(*rotations + 1)))
        }
        other => panic!("unexpected grant {other}"),
    }
}

async fn revoke(
    State(stub): State<Shared>,
    Form(form): Form<BTreeMap<String, String>>,
) -> StatusCode {
    stub.forms.lock().unwrap().push(form);
    StatusCode::OK
}

async fn serve(stub: Shared) -> String {
    let app = Router::new()
        .route("/v1/device/authorize", post(authorize))
        .route("/v1/oauth/token", post(token))
        .route("/v1/oauth/revoke", post(revoke))
        .with_state(stub);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn sign_in(url: &str) -> SignIn {
    SignIn::new(url, "mcport")
        .unwrap()
        .with_poll_unit(Duration::from_millis(10))
}

#[tokio::test]
async fn device_flow_waits_through_pending_and_slow_down_then_returns_tokens() {
    let stub = Shared::default();
    *stub.device.lock().unwrap() = VecDeque::from(["pending", "slow_down", "pending", "tokens"]);
    let url = serve(stub.clone()).await;
    let client = sign_in(&url);
    let device = client
        .start_device(Some("mcport CLI on test"))
        .await
        .unwrap();
    assert_eq!(device.user_code, "MVHB-KQAW");
    assert_eq!(
        device.browser_url(),
        "http://127.0.0.1/device?code=MVHB-KQAW"
    );
    assert!(!format!("{device:?}").contains("sad_secret"));
    let mut events = Vec::new();
    let tokens = client
        .wait_for_device(&device, |event| events.push(event))
        .await
        .unwrap();
    assert_eq!(
        events,
        [
            DeviceProgress::Pending,
            DeviceProgress::SlowDown { interval: 10 },
            DeviceProgress::Pending
        ]
    );
    assert_eq!(tokens.account.uuid, "zQo");
    assert_eq!(tokens.account.kind, "carbon");
    assert_eq!(tokens.refresh_token.expose(), "sar_1");
    assert!(tokens.refresh_expires_at.is_some());
    assert!(!format!("{tokens:?}").contains("sar_1"));
    assert_eq!(stub.forms.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn device_flow_reports_denial_and_expiry() {
    for (script, code) in [
        ("denied", "device_denied"),
        ("expired", "device_code_expired"),
    ] {
        let stub = Shared::default();
        *stub.device.lock().unwrap() = VecDeque::from(["pending", script]);
        let url = serve(stub).await;
        let client = sign_in(&url);
        let device = client.start_device(None).await.unwrap();
        let error = client.wait_for_device(&device, |_| {}).await.unwrap_err();
        assert_eq!(error.code(), code);
        assert!(error.message().contains("MVHB-KQAW"), "{}", error.message());
    }
}

#[tokio::test]
async fn short_lived_tokens_are_exchanged_with_the_client_id_alone_and_refusals_are_exact() {
    let stub = Shared::default();
    let url = serve(stub.clone()).await;
    let client = sign_in(&url);
    let tokens = client.exchange_slt("  slt_ok\n").await.unwrap();
    assert_eq!(tokens.account.id, "c:ada");
    let form = stub.forms.lock().unwrap()[0].clone();
    assert_eq!(
        form["grant_type"],
        "urn:silicon:params:oauth:grant-type:slt"
    );
    assert_eq!(form["slt"], "slt_ok");
    for (slt, reason, code) in [
        ("slt_used", SltRefusal::AlreadyUsed, "slt_already_used"),
        ("slt_late", SltRefusal::Expired, "slt_expired"),
        (
            "slt_remind",
            SltRefusal::WrongApp {
                app: Some("remind".into()),
            },
            "slt_wrong_app",
        ),
        ("slt_typo", SltRefusal::Unknown, "slt_unknown"),
        ("slt_rotated", SltRefusal::SignInEnded, "slt_sign_in_ended"),
    ] {
        let error = client.exchange_slt(slt).await.unwrap_err();
        let SignInError::SltRefused { reason: got, .. } = &error else {
            panic!("{error:?}")
        };
        assert_eq!(*got, reason);
        assert_eq!(error.code(), code);
        assert!(
            error
                .hint()
                .contains("silicon-accounts login --app mcport -q")
        );
        assert!(!error.message().contains(slt) && !error.hint().contains(slt));
    }
    let error = client.exchange_slt("slt_public_off").await.unwrap_err();
    assert_eq!(error.code(), "sign_in_not_enabled");
    // Not a short-lived token: refused before anything is sent.
    let before = stub.forms.lock().unwrap().len();
    let error = client.exchange_slt("sar_refresh").await.unwrap_err();
    assert_eq!(error.code(), "sign_in_configuration");
    assert_eq!(stub.forms.lock().unwrap().len(), before);
}

#[tokio::test]
async fn refresh_rotates_and_a_spent_token_ends_the_sign_in_and_revoke_is_public() {
    let stub = Shared::default();
    *stub.refresh.lock().unwrap() = "sar_1".into();
    let url = serve(stub.clone()).await;
    let client = sign_in(&url);
    let next = client.refresh("sar_1").await.unwrap();
    assert_eq!(next.refresh_token.expose(), "sar_2");
    let error = client.refresh("sar_1").await.unwrap_err();
    assert!(matches!(error, SignInError::SignInEnded { .. }));
    assert!(error.hint().contains("mcport login"));
    client.revoke("sar_2").await.unwrap();
    let forms = stub.forms.lock().unwrap();
    let revoke = forms.last().unwrap();
    assert_eq!(revoke["token"], "sar_2");
    assert_eq!(revoke["token_type_hint"], "refresh_token");
    assert_eq!(revoke["client_id"], "mcport");
}

#[test]
fn only_https_or_this_machine_is_accepted() {
    assert!(SignIn::new("https://accounts.teamofsilicons.com", "mcport").is_ok());
    assert!(SignIn::new("http://localhost:9590", "mcport").is_ok());
    assert!(SignIn::new("http://127.0.0.1:9589", "mcport").is_ok());
    for bad in [
        "http://accounts.example",
        "https://user:pw@accounts.example",
        "ftp://accounts.example",
        "not a url",
    ] {
        let error = SignIn::new(bad, "mcport").err().unwrap();
        assert_eq!(error.code(), "sign_in_configuration", "{bad}");
    }
}

async fn stored(dir: &std::path::Path, url: &str, expires_in: i64) -> SessionFile {
    let file = SessionFile::new(dir.join("accounts/backend.json"));
    let mut tokens = sign_in(url).exchange_slt("slt_ok").await.unwrap();
    tokens.expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + expires_in;
    file.save(&StoredSignIn::new(
        &sign_in(url),
        "http://127.0.0.1:4241",
        "slt",
        tokens,
    ))
    .unwrap();
    file
}

#[tokio::test]
async fn sessions_are_private_and_concurrent_refreshes_rotate_once() {
    let stub = Shared::default();
    *stub.refresh.lock().unwrap() = "sar_1".into();
    let url = serve(stub.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let file = stored(dir.path(), &url, 10).await;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let parent = std::fs::metadata(file.path().parent().unwrap()).unwrap();
        assert_eq!(parent.permissions().mode() & 0o777, 0o700);
    }
    // A fresh enough token is returned without touching Accounts.
    assert_eq!(
        file.fresh(Duration::from_secs(5))
            .await
            .unwrap()
            .refresh_token
            .expose(),
        "sar_1"
    );
    // Eight processes' worth of concurrent refreshes: one rotation, one shared result.
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let file = file.clone();
        tasks.push(tokio::spawn(async move {
            file.fresh(Duration::from_secs(60)).await.unwrap()
        }));
    }
    for task in tasks {
        let session = task.await.unwrap();
        assert_eq!(session.refresh_token.expose(), "sar_2");
        assert_eq!(session.access_token.expose(), "access-2");
    }
    assert_eq!(*stub.rotations.lock().unwrap(), 1);
    let on_disk = file.load().unwrap().unwrap();
    assert_eq!(on_disk.refresh_token.expose(), "sar_2");
    assert!(on_disk.seconds_left() > 1700);
    assert!(!format!("{on_disk:?}").contains("sar_2"));
}

#[tokio::test]
async fn a_spent_refresh_token_removes_the_session_but_a_passing_failure_keeps_it() {
    let stub = Shared::default();
    *stub.refresh.lock().unwrap() = "sar_elsewhere".into();
    let url = serve(stub.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let file = stored(dir.path(), &url, 0).await;
    *stub.unavailable.lock().unwrap() = true;
    let error = file.fresh(Duration::from_secs(60)).await.unwrap_err();
    assert!(
        matches!(&error, SessionError::SignIn(e) if e.is_transient()),
        "{error:?}"
    );
    assert!(file.load().unwrap().is_some());
    *stub.unavailable.lock().unwrap() = false;
    let error = file.fresh(Duration::from_secs(60)).await.unwrap_err();
    assert!(
        matches!(
            &error,
            SessionError::SignIn(SignInError::SignInEnded { .. })
        ),
        "{error:?}"
    );
    assert!(file.load().unwrap().is_none());
    assert!(matches!(
        file.fresh(Duration::from_secs(60)).await,
        Err(SessionError::NotSignedIn)
    ));
}

#[test]
fn unreadable_and_linked_session_files_are_refused_not_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.json");
    std::fs::write(&path, b"{\"principal_id\":\"c:old\",\"org_id\":\"tos\"}").unwrap();
    let error = SessionFile::new(&path).load().unwrap_err();
    assert!(
        matches!(error, SessionError::Unreadable { .. }),
        "{error:?}"
    );
    std::fs::write(&path, b"not json").unwrap();
    assert!(SessionFile::new(&path).load().is_err());
    #[cfg(unix)]
    {
        let link = dir.path().join("linked.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(SessionFile::new(&link).load().is_err());
        assert!(SessionFile::new(&link).remove().is_err());
    }
    assert!(
        SessionFile::new(dir.path().join("absent.json"))
            .load()
            .unwrap()
            .is_none()
    );
}
