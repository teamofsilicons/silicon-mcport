use axum::{Json, extract::State};
use mcport_daemon::{HostConfig, Registry};
use mcport_mcp::Endpoint;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Fixture {
    empty: Notify,
    registered: Notify,
}
async fn poll(State(fixture): State<Arc<Fixture>>, Json(body): Json<Value>) -> Json<Value> {
    if body["registered_connections"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        fixture.empty.notify_one();
        tokio::time::sleep(Duration::from_secs(20)).await;
    } else {
        fixture.registered.notify_one();
    }
    Json(json!({"data":{"jobs":[],"cancelled":[]}}))
}
#[tokio::test]
async fn local_registration_interrupts_a_pending_empty_long_poll() {
    let fixture = Arc::new(Fixture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new()
        .route("/api/v1/hosts/host/poll", axum::routing::post(poll))
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
    registry.save(&path).unwrap();
    let shutdown = CancellationToken::new();
    let cancellation = shutdown.clone();
    let registry_path = path.clone();
    let daemon = tokio::spawn(async move { mcport_daemon::run(registry_path, cancellation).await });
    tokio::time::timeout(Duration::from_secs(2), fixture.empty.notified())
        .await
        .unwrap();
    registry
        .register(
            "new",
            Endpoint::http(format!("http://{address}/mcp")),
            "none",
        )
        .unwrap();
    registry.save(&path).unwrap();
    tokio::time::timeout(Duration::from_secs(1), fixture.registered.notified())
        .await
        .unwrap();
    shutdown.cancel();
    daemon.await.unwrap().unwrap();
    server.abort();
}
