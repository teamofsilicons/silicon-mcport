use mcport_mcp::{Endpoint, ExecutionOptions, McpSession, NetworkPolicy, execute};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

fn local_options() -> ExecutionOptions {
    ExecutionOptions {
        network_policy: NetworkPolicy::LocalHost,
        timeout: Duration::from_secs(3),
        ..Default::default()
    }
}
fn python() -> String {
    if let Some(path) = std::env::var_os("MCPORT_TEST_PYTHON") {
        let path = PathBuf::from(path);
        assert!(
            path.is_absolute() && path.is_file(),
            "MCPORT_TEST_PYTHON must identify an existing absolute Python executable"
        );
        return path.to_string_lossy().into_owned();
    }
    for path in [
        "/usr/bin/python3",
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
    ] {
        if std::path::Path::new(path).is_file() {
            return path.into();
        }
    }
    for command in ["python3", "python"] {
        if let Ok(output) = std::process::Command::new(command)
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            && output.status.success()
        {
            let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
            if path.is_absolute() && path.is_file() {
                return path.to_string_lossy().into_owned();
            }
        }
    }
    panic!(
        "The stdio protocol fixture requires Python 3; set MCPORT_TEST_PYTHON to its absolute executable path"
    )
}
fn stdio(legacy: bool) -> Endpoint {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio.py");
    let mut args = vec![fixture.to_string_lossy().into_owned()];
    if legacy {
        args.push("--legacy".into());
    }
    Endpoint::stdio(python(), args)
}

#[tokio::test]
async fn stdio_modern_and_legacy_discovery_and_mixed_content() {
    for legacy in [false, true] {
        let endpoint = stdio(legacy);
        let tools = execute(&endpoint, "tools/list", json!({}), local_options())
            .await
            .unwrap();
        assert_eq!(tools["tools"][0]["name"], "echo");
        let result = execute(
            &endpoint,
            "tools/call",
            json!({"name":"echo","arguments":{"text":"preserved"}}),
            local_options(),
        )
        .await
        .unwrap();
        assert_eq!(result["content"][0]["text"], "preserved");
        assert_eq!(result["content"][1]["type"], "image");
        assert_eq!(result["structuredContent"]["echoed"], true);
        let resource = execute(
            &endpoint,
            "resources/read",
            json!({"uri":"fixture://hello"}),
            local_options(),
        )
        .await
        .unwrap();
        assert_eq!(resource["contents"][0]["text"], "fixture text");
        let prompt = execute(
            &endpoint,
            "prompts/get",
            json!({"name":"review"}),
            local_options(),
        )
        .await
        .unwrap();
        assert_eq!(prompt["messages"][0]["role"], "user");
    }
}

#[tokio::test]
async fn validates_input_before_dispatch_and_reports_unknown_timeout() {
    let mut options = local_options();
    options.input_schema = Some(json!({"type":"object","required":["text"]}));
    let error = execute(
        &stdio(false),
        "tools/call",
        json!({"name":"echo","arguments":{}}),
        options,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(!error.outcome_unknown);
    let mut options = local_options();
    options.timeout = Duration::from_millis(100);
    let error = execute(
        &stdio(false),
        "tools/call",
        json!({"name":"slow","arguments":{}}),
        options,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "timeout");
    assert!(error.outcome_unknown);
}

#[tokio::test]
async fn emits_progress_and_cancels_without_replay() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let mut options = local_options();
    options.progress = Some(tx);
    execute(
        &stdio(false),
        "tools/call",
        json!({"name":"echo","arguments":{}}),
        options,
    )
    .await
    .unwrap();
    assert!(rx.recv().await.is_some());
    let options = local_options();
    let cancellation = options.cancellation.clone();
    let task = tokio::spawn(async move {
        execute(
            &stdio(false),
            "tools/call",
            json!({"name":"slow","arguments":{}}),
            options,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancellation.cancel();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.code, "cancelled");
}

#[tokio::test]
async fn bounds_stdio_frames() {
    let mut options = local_options();
    options.max_response_bytes = 1024;
    assert!(
        execute(
            &stdio(false),
            "tools/call",
            json!({"name":"large","arguments":{}}),
            options
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn explicit_session_preserves_stdio_state() {
    let mut session = McpSession::connect(&stdio(false), &local_options())
        .await
        .unwrap();
    for expected in [1, 2] {
        let result = session
            .request(
                "tools/call",
                json!({"name":"echo","arguments":{}}),
                local_options(),
            )
            .await
            .unwrap();
        assert_eq!(result["structuredContent"]["sequence"], expected);
    }
    session.close().await;
    assert!(session.is_closed());
}

async fn http_fixture(
    axum::extract::State(legacy): axum::extract::State<bool>,
    headers: axum::http::HeaderMap,
    axum::Json(request): axum::Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer fixture-token") {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    let method = request["method"].as_str().unwrap();
    if method.starts_with("notifications/") {
        return axum::http::StatusCode::ACCEPTED.into_response();
    }
    let id = request["id"].clone();
    if method == "server/discover" && legacy {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "legacy endpoint requires initialize",
        )
            .into_response();
    }
    let result = match method {
        "server/discover" => {
            json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"})
        }
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
        }
        "tools/list" => {
            json!({"resultType":"complete","tools":[{"name":"echo","inputSchema":{"type":"object"}}]})
        }
        _ => {
            json!({"resultType":"complete","content":[{"type":"text","text":"HTTP works"}],"isError":false})
        }
    };
    if !legacy && method != "server/discover" {
        assert_eq!(headers.get("mcp-method").unwrap(), method);
        assert_eq!(headers.get("mcp-protocol-version").unwrap(), "2026-07-28");
    }
    axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
}

#[tokio::test]
async fn http_auth_and_modern_legacy_compatibility() {
    for legacy in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route("/mcp", axum::routing::post(http_fixture))
            .with_state(legacy);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let endpoint = Endpoint::Http {
            url: format!("http://{address}/mcp"),
            headers: BTreeMap::new(),
            bearer_token: Some("fixture-token".into()),
        };
        let result = execute(
            &endpoint,
            "tools/call",
            json!({"name":"echo","arguments":{}}),
            local_options(),
        )
        .await
        .unwrap();
        assert_eq!(result["content"][0]["text"], "HTTP works");
        let missing_auth = execute(
            &Endpoint::http(format!("http://{address}/mcp")),
            "tools/list",
            json!({}),
            local_options(),
        )
        .await
        .unwrap_err();
        assert_eq!(missing_auth.code, "provider_authentication_required");
        server.abort();
    }
}

#[tokio::test]
async fn http_sse_is_bounded_and_redirects_are_not_followed() {
    use axum::response::IntoResponse;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let redirects = Arc::new(AtomicUsize::new(0));
    let count = redirects.clone();
    let app=axum::Router::new()
        .route("/redirect",axum::routing::post(||async{(axum::http::StatusCode::TEMPORARY_REDIRECT,[("location","/target")])}))
        .route("/target",axum::routing::post(move||{let count=count.clone();async move{count.fetch_add(1,Ordering::SeqCst);axum::http::StatusCode::OK}}))
        .route("/mcp",axum::routing::post(|axum::Json(input):axum::Json<Value>|async move{
            if input["method"]=="server/discover" {
                return axum::Json(json!({"jsonrpc":"2.0","id":input["id"],"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"}})).into_response();
            }
            let text=if input["params"]["name"]=="large" {"x".repeat(8192)} else {"stream works".into()};
            let response=json!({"jsonrpc":"2.0","id":input["id"],"result":{"resultType":"complete","content":[{"type":"text","text":text}],"isError":false}});
            ([("content-type","text/event-stream")],format!("event: message\ndata: {response}\n\n")).into_response()
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let endpoint = Endpoint::http(format!("http://{address}/mcp"));
    let result = execute(
        &endpoint,
        "tools/call",
        json!({"name":"echo","arguments":{}}),
        local_options(),
    )
    .await
    .unwrap();
    assert_eq!(result["content"][0]["text"], "stream works");
    let mut options = local_options();
    options.max_response_bytes = 1024;
    assert!(
        execute(
            &endpoint,
            "tools/call",
            json!({"name":"large","arguments":{}}),
            options
        )
        .await
        .is_err()
    );
    assert!(
        execute(
            &Endpoint::http(format!("http://{address}/redirect")),
            "tools/list",
            json!({}),
            local_options()
        )
        .await
        .is_err()
    );
    assert_eq!(redirects.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn discovers_account_schema_through_pages_and_rejects_cursor_loops() {
    let mut endpoint = stdio(false);
    if let Endpoint::Stdio { args, .. } = &mut endpoint {
        args.push("--paged".into());
    }
    let mut session = McpSession::connect(&endpoint, &local_options())
        .await
        .unwrap();
    let schemas = session
        .tool_schemas("echo", &local_options())
        .await
        .unwrap();
    let mut options = local_options();
    options.input_schema = Some(schemas.input_schema);
    options.output_schema = schemas.output_schema;
    let error = session
        .request(
            "tools/call",
            json!({"name":"echo","arguments":{"text":7}}),
            options.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(!error.outcome_unknown);
    let result = session
        .request(
            "tools/call",
            json!({"name":"echo","arguments":{"text":"valid"}}),
            options,
        )
        .await
        .unwrap();
    assert_eq!(result["structuredContent"]["sequence"], 1);
    assert_eq!(
        session
            .tool_schemas("missing", &local_options())
            .await
            .err()
            .unwrap()
            .code,
        "tool_not_found"
    );
    session.close().await;
    let mut endpoint = stdio(false);
    if let Endpoint::Stdio { args, .. } = &mut endpoint {
        args.push("--cursor-loop".into());
    }
    let mut session = McpSession::connect(&endpoint, &local_options())
        .await
        .unwrap();
    let error = session
        .tool_schemas("echo", &local_options())
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "invalid_catalog");
    assert!(!error.outcome_unknown);
    session.close().await;
}

#[tokio::test]
async fn unsupported_reverse_requests_report_actionable_error() {
    let error = execute(
        &stdio(true),
        "tools/call",
        json!({"name":"needs_roots","arguments":{}}),
        local_options(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "unsupported_client_capability");
    assert!(error.message.contains("filesystem roots"));
    assert!(error.outcome_unknown);
}
