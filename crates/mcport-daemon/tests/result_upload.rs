use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use mcport_core::{Actor, HostJob};
use mcport_daemon::{HostConfig, Registry};
use mcport_mcp::Endpoint;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Fixture {
    polls: Mutex<Vec<Value>>,
    uploads: Mutex<Vec<(Instant, Value)>>,
    release_upload: CancellationToken,
    delay: Duration,
    fail_first: bool,
    reject_upload: bool,
    tool_calls: AtomicUsize,
}

fn completed_result() -> Value {
    json!({"result":{"content":[{"type":"text","text":"journaled provider result"}]},"error":null})
}

async fn poll(State(fixture): State<Arc<Fixture>>, Json(body): Json<Value>) -> Json<Value> {
    fixture.polls.lock().unwrap().push(body);
    // A duplicate lease must never execute, whether the upload is pending or acknowledged.
    let job = HostJob {
        id: "a".into(),
        connection_id: "registered".into(),
        method: "tools/call".into(),
        params: json!({"name":"echo","arguments":{}}),
        timeout_ms: 2_000,
        expires_at: i64::MAX,
        actor: Actor {
            uuid: "Caller".into(),
            id: "si:caller".into(),
            kind: "silicon".into(),
            display_name: "caller".into(),
            principal_id: "Caller".into(),
            identity_kind: "silicon".into(),
            org_id: String::new(),
        },
    };
    Json(json!({"data":{"jobs":[job],"cancelled":[]}}))
}

async fn result(State(fixture): State<Arc<Fixture>>, Json(body): Json<Value>) -> Response {
    let attempt = {
        let mut uploads = fixture.uploads.lock().unwrap();
        uploads.push((Instant::now(), body));
        uploads.len()
    };
    if fixture.reject_upload {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if fixture.fail_first && attempt == 1 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    fixture.release_upload.cancelled().await;
    tokio::time::sleep(fixture.delay).await;
    Json(json!({"data":{"accepted":true}})).into_response()
}

async fn mcp(State(fixture): State<Arc<Fixture>>, Json(body): Json<Value>) -> Response {
    if body.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let result = match body["method"].as_str().unwrap() {
        "server/discover" => {
            json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"})
        }
        "tools/list" => {
            json!({"resultType":"complete","tools":[{"name":"echo","inputSchema":{"type":"object"}}]})
        }
        "tools/call" => {
            fixture.tool_calls.fetch_add(1, Ordering::SeqCst);
            json!({"resultType":"complete","content":[]})
        }
        method => panic!("unexpected MCP method: {method}"),
    };
    Json(json!({"jsonrpc":"2.0","id":body["id"],"result":result})).into_response()
}

struct Harness {
    fixture: Arc<Fixture>,
    _directory: tempfile::TempDir,
    path: PathBuf,
    registry: Registry,
    daemon: Option<tokio::task::JoinHandle<Result<(), mcport_daemon::DaemonError>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn start(fixture: Fixture, pending: &[&str]) -> Self {
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
        let mut registry = Registry::new(HostConfig::new(
            format!("http://{address}"),
            "host",
            "fixture-host-token",
            "Owner",
            None,
        ));
        registry
            .register(
                "registered",
                Endpoint::http(format!("http://{address}/mcp")),
                "none",
            )
            .unwrap();
        registry.save(&path).unwrap();
        let jobs: serde_json::Map<String, Value> = pending
            .iter()
            .map(|id| {
                (
                    (*id).into(),
                    json!({"started_at":1,"result":completed_result(),"delivered":false}),
                )
            })
            .collect();
        let journal = path.parent().unwrap().join("journal.json");
        std::fs::write(&journal, json!({"jobs":jobs}).to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut harness = Self {
            fixture,
            _directory: directory,
            path,
            registry,
            daemon: None,
            server,
        };
        harness.restart();
        harness
    }

    fn restart(&mut self) {
        assert!(self.daemon.is_none());
        let path = self.path.clone();
        self.daemon = Some(tokio::spawn(async move {
            mcport_daemon::run(path, CancellationToken::new()).await
        }));
    }

    fn journal(&self) -> Value {
        serde_json::from_slice(
            &std::fs::read(self.path.parent().unwrap().join("journal.json")).unwrap(),
        )
        .unwrap()
    }

    async fn stop(&mut self) {
        std::fs::write(self.path.parent().unwrap().join("stop.request"), b"stop").unwrap();
        tokio::time::timeout(Duration::from_secs(1), self.daemon.take().unwrap())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(daemon) = &self.daemon {
            daemon.abort();
        }
        self.server.abort();
        self.fixture.release_upload.cancel();
    }
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(7), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn slow_ack_keeps_poll_and_registry_controls_live_and_reserves_outbox_capacity() {
    let fixture = Fixture {
        delay: Duration::from_secs(4),
        ..Default::default()
    };
    fixture.release_upload.cancel();
    let mut harness = Harness::start(fixture, &["a", "b", "c", "d"]).await;
    until(|| harness.fixture.uploads.lock().unwrap().len() == 1).await;
    let previous = std::fs::metadata(&harness.path)
        .unwrap()
        .modified()
        .unwrap();
    let endpoint = harness.registry.connections["registered"].endpoint.clone();
    harness.registry.register("new", endpoint, "none").unwrap();
    harness.registry.save(&harness.path).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&harness.path)
        .unwrap()
        .set_modified(previous + Duration::from_secs(1))
        .unwrap();
    until(|| {
        harness.fixture.polls.lock().unwrap().iter().any(|poll| {
            poll["registered_connections"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| id == "new")
        })
    })
    .await;
    assert_eq!(harness.journal()["jobs"]["a"]["delivered"], false);
    assert_eq!(harness.fixture.uploads.lock().unwrap().len(), 1);
    {
        let polls = harness.fixture.polls.lock().unwrap();
        assert!(polls.len() >= 2);
        for poll in polls.iter() {
            assert_eq!(poll["capabilities"]["active_jobs"], 4);
            assert_eq!(
                poll["capabilities"]["active_job_ids"],
                json!(["a", "b", "c", "d"])
            );
        }
    }
    // The real request must survive the former three-second timeout, with no retry.
    until(|| harness.journal()["jobs"]["a"]["delivered"] == true).await;
    assert_eq!(harness.journal()["jobs"]["a"]["result"], Value::Null);
    assert_eq!(
        harness.fixture.uploads.lock().unwrap()[0].1,
        completed_result()
    );
    harness.stop().await;
    assert_eq!(harness.fixture.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.journal()["jobs"]["b"]["result"], completed_result());
}

#[tokio::test]
async fn shutdown_retains_unacknowledged_result_and_restart_retries_without_execution() {
    let mut harness = Harness::start(Fixture::default(), &["a"]).await;
    until(|| harness.fixture.uploads.lock().unwrap().len() == 1).await;
    harness.stop().await;
    let saved = harness.journal();
    assert_eq!(saved["jobs"]["a"]["delivered"], false);
    assert_eq!(saved["jobs"]["a"]["result"], completed_result());
    harness.fixture.release_upload.cancel();
    harness.restart();
    until(|| harness.journal()["jobs"]["a"]["delivered"] == true).await;
    harness.stop().await;
    let uploads = harness.fixture.uploads.lock().unwrap();
    assert_eq!(uploads.len(), 2);
    assert!(
        uploads
            .iter()
            .all(|(_, result)| *result == completed_result())
    );
    assert_eq!(harness.fixture.tool_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn failed_upload_backs_off_without_losing_result_or_stalling_polls() {
    let fixture = Fixture {
        fail_first: true,
        ..Default::default()
    };
    fixture.release_upload.cancel();
    let mut harness = Harness::start(fixture, &["a"]).await;
    until(|| harness.fixture.uploads.lock().unwrap().len() == 1).await;
    tokio::time::sleep(Duration::from_millis(750)).await;
    assert_eq!(harness.fixture.uploads.lock().unwrap().len(), 1);
    assert!(harness.fixture.polls.lock().unwrap().len() >= 2);
    assert_eq!(harness.journal()["jobs"]["a"]["result"], completed_result());
    until(|| harness.journal()["jobs"]["a"]["delivered"] == true).await;
    harness.stop().await;
    let uploads = harness.fixture.uploads.lock().unwrap();
    assert_eq!(uploads.len(), 2);
    assert!(uploads[1].0.duration_since(uploads[0].0) >= Duration::from_secs(2));
    assert_eq!(harness.fixture.tool_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn revoked_upload_stops_connector_and_keeps_the_result() {
    let mut harness = Harness::start(
        Fixture {
            reject_upload: true,
            ..Default::default()
        },
        &["a"],
    )
    .await;
    let result = tokio::time::timeout(Duration::from_secs(1), harness.daemon.take().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        Err(mcport_daemon::DaemonError::Unauthorized)
    ));
    assert_eq!(harness.journal()["jobs"]["a"]["delivered"], false);
    assert_eq!(harness.journal()["jobs"]["a"]["result"], completed_result());
    assert_eq!(harness.fixture.uploads.lock().unwrap().len(), 1);
}
