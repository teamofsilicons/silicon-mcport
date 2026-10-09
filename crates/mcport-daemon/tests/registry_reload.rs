use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use mcport_core::{Actor, HostJob};
use mcport_daemon::{HostConfig, Registry};
use mcport_mcp::Endpoint;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Fixture {
    polls: Mutex<Vec<Value>>,
    first_poll: Notify,
    following_poll: Notify,
    release_first: CancellationToken,
    release_following: CancellationToken,
    replay: AtomicBool,
    job: Option<HostJob>,
    results: Mutex<Vec<(String, Value)>>,
    result_received: Notify,
    tool_calls: AtomicUsize,
    tool_entered: Notify,
    hold_tool: bool,
    release_tool: CancellationToken,
}

async fn poll(State(fixture): State<Arc<Fixture>>, Json(body): Json<Value>) -> Json<Value> {
    let first = {
        let mut polls = fixture.polls.lock().unwrap();
        polls.push(body);
        polls.len() == 1
    };
    // The gateway has durably leased the job before writing this response. It
    // will not offer it to another poll just because this request disappears.
    if first {
        fixture.first_poll.notify_one();
        fixture.release_first.cancelled().await;
    } else {
        fixture.following_poll.notify_one();
        fixture.release_following.cancelled().await;
    }
    let jobs: Vec<_> = if first || fixture.replay.load(Ordering::SeqCst) {
        fixture.job.iter().cloned().collect()
    } else {
        vec![]
    };
    Json(json!({"data":{"jobs":jobs,"cancelled":[]}}))
}

async fn result(
    State(fixture): State<Arc<Fixture>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    fixture.results.lock().unwrap().push((id, body));
    fixture.result_received.notify_one();
    Json(json!({"data":{"accepted":true}}))
}

async fn mcp(
    State(fixture): State<Arc<Fixture>>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    if body.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let result = match body["method"].as_str().unwrap() {
        "server/discover" => {
            json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"})
        }
        "tools/list" => {
            json!({"resultType":"complete","tools":[{"name":"echo","inputSchema":{"type":"object","additionalProperties":false}}]})
        }
        "tools/call" => {
            fixture.tool_calls.fetch_add(1, Ordering::SeqCst);
            fixture.tool_entered.notify_one();
            if fixture.hold_tool {
                fixture.release_tool.cancelled().await;
            }
            json!({"resultType":"complete","content":[{"type":"text","text":"local result"}],"isError":false})
        }
        method => panic!("unexpected MCP method: {method}"),
    };
    Json(json!({"jsonrpc":"2.0","id":body["id"],"result":result})).into_response()
}

fn job() -> HostJob {
    HostJob {
        id: "leased-once".into(),
        connection_id: "registered".into(),
        method: "tools/call".into(),
        params: json!({"name":"echo","arguments":{}}),
        timeout_ms: 10_000,
        expires_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 30,
        actor: Actor {
            principal_id: "si:caller".into(),
            identity_kind: "silicon".into(),
            org_id: "tos".into(),
            display_name: "caller".into(),
            ..Default::default()
        },
    }
}

struct Harness {
    fixture: Arc<Fixture>,
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
    registry: Registry,
    endpoint: Endpoint,
    shutdown: CancellationToken,
    daemon: tokio::task::JoinHandle<Result<(), mcport_daemon::DaemonError>>,
    server: tokio::task::JoinHandle<()>,
}
impl Harness {
    async fn start(fixture: Fixture, registered: bool) -> Self {
        let fixture = Arc::new(fixture);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route("/api/v1/hosts/host/poll", axum::routing::post(poll))
            .route(
                "/api/v1/hosts/host/jobs/{id}/result",
                axum::routing::post(result),
            )
            .route("/mcp", axum::routing::post(mcp))
            .with_state(fixture.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("host/registry.json");
        let mut registry = Registry::new(HostConfig {
            backend_url: format!("http://{address}"),
            host_id: "host".into(),
            host_token: "token".into(),
            environment: "production".into(),
            org_id: "tos".into(),
            owner_id: "c:alice".into(),
            isi: None,
        });
        let endpoint = Endpoint::http(format!("http://{address}/mcp"));
        if registered {
            registry
                .register("registered", endpoint.clone(), "shared")
                .unwrap();
        }
        registry.save(&path).unwrap();
        let shutdown = CancellationToken::new();
        let cancellation = shutdown.clone();
        let registry_path = path.clone();
        let daemon =
            tokio::spawn(async move { mcport_daemon::run(registry_path, cancellation).await });
        notified(&fixture.first_poll).await;
        Self {
            fixture,
            _directory: directory,
            path,
            registry,
            endpoint,
            shutdown,
            daemon,
            server,
        }
    }

    fn save_changed(&self) {
        // Guarantee a changed timestamp even on filesystems with coarse mtime.
        let previous = std::fs::metadata(&self.path).unwrap().modified().unwrap();
        self.registry.save(&self.path).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&self.path)
            .unwrap()
            .set_modified(previous + Duration::from_secs(1))
            .unwrap();
    }

    async fn stop(self) -> Value {
        self.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(4), self.daemon)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        self.fixture.release_tool.cancel();
        self.fixture.release_first.cancel();
        self.fixture.release_following.cancel();
        self.server.abort();
        serde_json::from_slice(
            &std::fs::read(self.path.parent().unwrap().join("journal.json")).unwrap(),
        )
        .unwrap()
    }
}

async fn notified(notify: &Notify) {
    tokio::time::timeout(Duration::from_secs(3), notify.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn registration_announces_latest_registry_after_draining_the_pending_poll() {
    let mut harness = Harness::start(Fixture::default(), false).await;
    harness
        .registry
        .register("new", harness.endpoint.clone(), "none")
        .unwrap();
    harness.save_changed();
    // Cover several control ticks while the response is deliberately held.
    tokio::time::sleep(Duration::from_millis(750)).await;
    let requests_before_release = harness.fixture.polls.lock().unwrap().len();
    harness.fixture.release_first.cancel();
    notified(&harness.fixture.following_poll).await;
    let second = harness.fixture.polls.lock().unwrap()[1].clone();
    harness.stop().await;
    assert_eq!(
        requests_before_release, 1,
        "registry edits must not replace an in-flight poll"
    );
    assert_eq!(second["registered_connections"], json!(["new"]));
}

#[tokio::test]
async fn leased_response_survives_registration_and_is_journaled_exactly_once() {
    let mut harness = Harness::start(
        Fixture {
            job: Some(job()),
            ..Default::default()
        },
        true,
    )
    .await;
    harness
        .registry
        .register("new", harness.endpoint.clone(), "none")
        .unwrap();
    harness.save_changed();
    tokio::time::sleep(Duration::from_millis(750)).await;
    harness.fixture.release_first.cancel();
    let received = tokio::time::timeout(
        Duration::from_secs(3),
        harness.fixture.result_received.notified(),
    )
    .await;
    if received.is_ok() {
        // Even a subsequent duplicate lease must see the durable tombstone.
        harness.fixture.replay.store(true, Ordering::SeqCst);
        harness.fixture.release_following.cancel();
        notified(&harness.fixture.following_poll).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let fixture = harness.fixture.clone();
    let journal = harness.stop().await;
    assert!(
        received.is_ok(),
        "the only leased response was discarded after a registry edit"
    );
    let results = fixture.results.lock().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "leased-once");
    assert_eq!(results[0].1["result"]["content"][0]["text"], "local result");
    assert_eq!(fixture.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(journal["jobs"].as_object().unwrap().len(), 1);
    assert_eq!(journal["jobs"]["leased-once"]["delivered"], true);
    assert_eq!(journal["jobs"]["leased-once"]["result"], Value::Null);
}

#[tokio::test]
async fn disconnected_account_rejects_a_pending_lease_without_provider_execution() {
    let mut harness = Harness::start(
        Fixture {
            job: Some(job()),
            ..Default::default()
        },
        true,
    )
    .await;
    harness
        .registry
        .connections
        .get_mut("registered")
        .unwrap()
        .shared_account_disconnected = true;
    harness.save_changed();
    tokio::time::sleep(Duration::from_millis(750)).await;
    harness.fixture.release_first.cancel();
    let received = tokio::time::timeout(
        Duration::from_secs(3),
        harness.fixture.result_received.notified(),
    )
    .await;
    let fixture = harness.fixture.clone();
    let journal = harness.stop().await;
    assert!(
        received.is_ok(),
        "revocation must reject the leased job explicitly, not lose its response"
    );
    let results = fixture.results.lock().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].1["error"]["code"],
        "provider_authentication_required"
    );
    assert_eq!(results[0].1["error"]["outcome_unknown"], false);
    assert_eq!(fixture.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(journal["jobs"]["leased-once"]["delivered"], true);
}

#[tokio::test]
async fn account_disconnect_cancels_active_work_without_waiting_for_the_next_poll() {
    let mut harness = Harness::start(
        Fixture {
            job: Some(job()),
            hold_tool: true,
            ..Default::default()
        },
        true,
    )
    .await;
    harness.fixture.release_first.cancel();
    notified(&harness.fixture.tool_entered).await;
    // Keep the next long poll pending throughout cancellation.
    notified(&harness.fixture.following_poll).await;
    harness
        .registry
        .connections
        .get_mut("registered")
        .unwrap()
        .shared_account_disconnected = true;
    harness.save_changed();
    let received = tokio::time::timeout(
        Duration::from_secs(2),
        harness.fixture.result_received.notified(),
    )
    .await;
    let polls_before_release = harness.fixture.polls.lock().unwrap().len();
    let fixture = harness.fixture.clone();
    harness.stop().await;
    assert!(
        received.is_ok(),
        "local revocation must cancel active work while a long poll is pending"
    );
    assert_eq!(
        polls_before_release, 2,
        "revocation must preserve the pending poll"
    );
    assert_eq!(
        fixture.results.lock().unwrap()[0].1["error"]["code"],
        "cancelled"
    );
    assert_eq!(fixture.tool_calls.load(Ordering::SeqCst), 1);
}
