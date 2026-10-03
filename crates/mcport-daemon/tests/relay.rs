use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use mcport_core::{Actor, HostJob};
use mcport_daemon::{HostConfig, Registry};
use mcport_mcp::Endpoint;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Fixture {
    results: Arc<Mutex<BTreeMap<String, Value>>>,
    tool_calls: Arc<AtomicUsize>,
    discoveries: Arc<AtomicUsize>,
    jobs: Vec<HostJob>,
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
fn job(id: &str, connection: &str) -> HostJob {
    HostJob {
        id: id.into(),
        connection_id: connection.into(),
        method: "tools/call".into(),
        params: json!({"name":"echo","arguments":{}}),
        timeout_ms: 2000,
        expires_at: now() + 30,
        actor: Actor {
            principal_id: "si:caller".into(),
            identity_kind: "silicon".into(),
            org_id: "org".into(),
            display_name: "caller".into(),
        },
    }
}

async fn poll(
    State(fixture): State<Fixture>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> impl IntoResponse {
    assert_eq!(headers.get("authorization").unwrap(), "Bearer host-secret");
    assert_eq!(headers.get("x-mcport-test").unwrap(), "test-isolation");
    assert!(
        input["registered_connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == "registered")
    );
    // Deliberately duplicate leases: the connector must never repeat execution.
    Json(json!({"data":{"jobs":fixture.jobs,"cancelled":[]}}))
}
async fn result(
    State(fixture): State<Fixture>,
    Path(id): Path<String>,
    Json(input): Json<Value>,
) -> impl IntoResponse {
    fixture.results.lock().unwrap().insert(id, input);
    Json(json!({"data":{"accepted":true}}))
}
async fn mcp(State(fixture): State<Fixture>, Json(input): Json<Value>) -> axum::response::Response {
    if input.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let result = if input["method"] == "server/discover" {
        fixture.discoveries.fetch_add(1, Ordering::SeqCst);
        json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"})
    } else if input["method"] == "tools/list" {
        json!({"resultType":"complete","tools":[{"name":"echo","inputSchema":{"type":"object","additionalProperties":false},"outputSchema":{"type":"object","required":["local"],"properties":{"local":{"type":"boolean"}}}}]})
    } else {
        fixture.tool_calls.fetch_add(1, Ordering::SeqCst);
        json!({"resultType":"complete","content":[{"type":"text","text":"local result"}],"structuredContent":{"local":true},"isError":false})
    };
    Json(json!({"jsonrpc":"2.0","id":input["id"],"result":result})).into_response()
}

#[tokio::test]
async fn outbound_relay_rejects_unregistered_expired_cross_org_and_never_replays() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut expired = job("expired", "registered");
    expired.expires_at = now() - 1;
    let mut foreign = job("foreign", "registered");
    foreign.actor.org_id = "other-org".into();
    let mut second_actor = job("second-actor", "registered");
    second_actor.actor.principal_id = "another-caller".into();
    let mut invalid = job("invalid-input", "registered");
    invalid.params = json!({"name":"echo","arguments":{"unexpected":true}});
    let fixture = Fixture {
        results: Default::default(),
        tool_calls: Default::default(),
        discoveries: Default::default(),
        jobs: vec![
            job("valid", "registered"),
            job("same-actor-next-call", "registered"),
            second_actor,
            job("unregistered", "remote-command-is-not-accepted"),
            expired,
            foreign,
            invalid,
        ],
    };
    let app = axum::Router::new()
        .route("/api/v1/hosts/host/poll", axum::routing::post(poll))
        .route(
            "/api/v1/hosts/host/jobs/{id}/result",
            axum::routing::post(result),
        )
        .route("/mcp", axum::routing::post(mcp))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("host/registry.json");
    let mut registry = Registry::new(HostConfig {
        backend_url: format!("http://{address}"),
        host_id: "host".into(),
        host_token: "host-secret".into(),
        environment: "test-isolation".into(),
        org_id: "org".into(),
        owner_id: "owner".into(),
        isi: None,
    });
    registry
        .register(
            "registered",
            Endpoint::http(format!("http://{address}/mcp")),
            "none",
        )
        .unwrap();
    registry.save(&path).unwrap();
    let shutdown = CancellationToken::new();
    let daemon_path = path.clone();
    let daemon_shutdown = shutdown.clone();
    let daemon =
        tokio::spawn(async move { mcport_daemon::run(daemon_path, daemon_shutdown).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture.results.lock().unwrap().len() == 7 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    {
        let results = fixture.results.lock().unwrap();
        assert_eq!(
            results["valid"]["result"]["content"][0]["text"],
            "local result"
        );
        assert_eq!(
            results["unregistered"]["error"]["code"],
            "unregistered_connection"
        );
        assert_eq!(results["expired"]["error"]["code"], "expired");
        assert_eq!(results["foreign"]["error"]["code"], "wrong_organization");
        assert_eq!(
            results["invalid-input"]["error"]["code"],
            "invalid_arguments"
        );
        assert_eq!(results["invalid-input"]["error"]["outcome_unknown"], false);
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Stop through the local control marker, without unsafe PID signalling.
    std::fs::write(path.parent().unwrap().join("stop.request"), b"stop").unwrap();
    tokio::time::timeout(Duration::from_secs(3), daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(fixture.tool_calls.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.discoveries.load(Ordering::SeqCst), 2);
    // Restart and receive the same lease again: durable tombstone prevents replay.
    let shutdown = CancellationToken::new();
    let cancellation = shutdown.clone();
    let daemon = tokio::spawn(async move { mcport_daemon::run(path, cancellation).await });
    tokio::time::sleep(Duration::from_millis(600)).await;
    shutdown.cancel();
    daemon.await.unwrap().unwrap();
    assert_eq!(fixture.tool_calls.load(Ordering::SeqCst), 3);
    server.abort();
}
