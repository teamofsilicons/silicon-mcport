use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Default)]
struct Fixture {
    refreshes: Arc<AtomicUsize>,
}

fn session(environment: &str, expired: bool) -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    json!({"data":{"access_token":if expired {"first-access"} else {"rotated-access"},"refresh_token":if expired {"first-refresh"} else {"rotated-refresh"},"expires_at":now + if expired {-60} else {3600},"actor":{"principal_id":"si:test","identity_kind":"silicon","org_id":"tos","display_name":"Test"},"environment":environment}})
}

async fn login(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["slt"], "test-slt");
    let env = headers
        .get("X-MCPort-Test")
        .map(|v| v.to_str().unwrap())
        .unwrap_or("production");
    Json(session(env, true))
}
async fn refresh(
    State(state): State<Fixture>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let previous = state.refreshes.fetch_add(1, Ordering::SeqCst);
    if previous != 0 || body["refresh_token"] != "first-refresh" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(
                json!({"error":{"code":"refresh_replayed","message":"Refresh token already rotated","recovery":"Login again","outcome_unknown":false}}),
            ),
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let env = headers
        .get("X-MCPort-Test")
        .map(|v| v.to_str().unwrap())
        .unwrap_or("production");
    (StatusCode::OK, Json(session(env, false)))
}
async fn connections(headers: HeaderMap) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer rotated-access");
    Json(json!({"data":[]}))
}

fn command(home: &std::path::Path, backend: &str, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcport"));
    command
        .env("SILICON_HOME", home)
        .env("MCPORT_URL", backend)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}
fn output_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "CLI failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_cli_refresh_rotates_once_and_test_context_has_no_production_session() {
    let fixture = Fixture::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/refresh", post(refresh))
        .route("/api/v1/connections", get(connections))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let home = tempfile::tempdir().unwrap();
    let logged_in = output_json(
        command(home.path(), &backend, &["login", "test-slt", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(logged_in["authenticated"], true);
    assert!(!logged_in.to_string().contains("first-access"));
    assert!(!logged_in.to_string().contains("first-refresh"));
    let first = command(home.path(), &backend, &["connection", "ls", "--json"])
        .spawn()
        .unwrap();
    let second = command(home.path(), &backend, &["connection", "ls", "--json"])
        .spawn()
        .unwrap();
    assert_eq!(output_json(first.wait_with_output().unwrap()), json!([]));
    assert_eq!(output_json(second.wait_with_output().unwrap()), json!([]));
    assert_eq!(fixture.refreshes.load(Ordering::SeqCst), 1);
    let test_status = output_json(
        command(
            home.path(),
            &backend,
            &["--test", "isolated", "login", "status", "--json"],
        )
        .output()
        .unwrap(),
    );
    assert_eq!(test_status["authenticated"], false);
    let other_backend = output_json(
        command(
            home.path(),
            "http://127.0.0.1:1",
            &["login", "status", "--json"],
        )
        .output()
        .unwrap(),
    );
    assert_eq!(other_backend["authenticated"], false);
    server.abort();
}

#[test]
fn invalid_input_and_invalid_home_fail_with_machine_readable_errors() {
    let home = tempfile::tempdir().unwrap();
    let output = command(
        home.path(),
        "http://127.0.0.1:1",
        &["config", "home", "/does-not-exist-mcport", "--json"],
    )
    .output()
    .unwrap();
    assert!(!output.status.success());
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a directory")
    );
}
