//! The real `mcport` binary against one stub server playing both Silicon Accounts
//! (device flow, short-lived token exchange, refresh, revoke) and an MCPort backend.
use axum::{
    Form, Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

#[derive(Default)]
struct Stub {
    origin: Mutex<String>,
    /// The Accounts URL the backend's discovery names (default: this stub).
    discovery_accounts_url: Mutex<Option<String>>,
    /// Scripted device polls: pending, slow_down, denied, expired, tokens.
    device: Mutex<VecDeque<&'static str>>,
    issued: AtomicU32,
    rotations: AtomicU32,
    /// Seconds the first issued access token lives (refreshed ones live 1800).
    first_expires_in: AtomicU32,
    current_refresh: Mutex<String>,
    refuse_refresh: AtomicBool,
    /// Answer refreshes with 503 (Silicon Accounts briefly unreachable).
    refresh_unavailable: AtomicBool,
    slts: Mutex<Vec<String>>,
    revoked: Mutex<Vec<BTreeMap<String, String>>>,
    bearers: Mutex<Vec<String>>,
    /// An access token the backend answers with 401 token_expired.
    expired_access: Mutex<Option<String>>,
}
type Shared = Arc<Stub>;

fn issue(stub: &Stub, kind: &str) -> Value {
    let n = stub.issued.fetch_add(1, Ordering::SeqCst) + 1;
    *stub.current_refresh.lock().unwrap() = format!("sar_{n}");
    let expires_in = if n == 1 {
        stub.first_expires_in.load(Ordering::SeqCst).max(1)
    } else {
        1800
    };
    let (uuid, id) = if kind == "silicon" {
        ("Sc1", "si:scout")
    } else {
        ("Ada", "c:ada")
    };
    let mut account = json!({"uuid":uuid,"membership_id":format!("mcport:{uuid}"),"kind":kind,"id":id,"display_name":"Fixture","pfp_url":"","version":1});
    if kind == "silicon" {
        account["custodian"] = json!({"uuid":"Ada","id":"c:ada"});
    }
    json!({"access_token":format!("access-{n}"),"token_type":"Bearer","expires_in":expires_in,"refresh_token":format!("sar_{n}"),"refresh_token_expires_at":"2029-03-25T02:31:52.745Z","scope":"profile","membership_id":format!("mcport:{uuid}"),"account":account})
}
fn oauth(error: &str, description: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":error,"error_description":description})),
    )
}

async fn device_authorize(Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["client_id"], "mcport");
    assert!(body["client_label"].as_str().is_some_and(|l| !l.is_empty()));
    Json(
        json!({"device_code":"sad_device","user_code":"WDJB-MJHT","verification_uri":"http://127.0.0.1/device","verification_uri_complete":"http://127.0.0.1/device?code=WDJB-MJHT","expires_in":600,"interval":1}),
    )
}

async fn token(
    State(stub): State<Shared>,
    Form(form): Form<BTreeMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(form["client_id"], "mcport");
    assert!(!form.contains_key("client_secret"), "public client only");
    match form["grant_type"].as_str() {
        "urn:ietf:params:oauth:grant-type:device_code" => {
            match stub.device.lock().unwrap().pop_front().unwrap_or("pending") {
                "pending" => oauth("authorization_pending", "Waiting."),
                "slow_down" => oauth("slow_down", "Slower."),
                "denied" => oauth("access_denied", "Denied."),
                "expired" => oauth("expired_token", "Expired."),
                _ => (StatusCode::OK, Json(issue(&stub, "carbon"))),
            }
        }
        "urn:silicon:params:oauth:grant-type:slt" => {
            let slt = form["slt"].clone();
            stub.slts.lock().unwrap().push(slt.clone());
            match slt.as_str() {
                "slt_ok" => (StatusCode::OK, Json(issue(&stub, "silicon"))),
                "slt_used" => oauth(
                    "invalid_grant",
                    "The short-lived token was already used at 2026-10-10T01:00:00Z.",
                ),
                "slt_late" => oauth(
                    "invalid_grant",
                    "The short-lived token expired at 2026-10-10T01:00:00Z.",
                ),
                "slt_remind" => oauth(
                    "invalid_grant",
                    "The short-lived token was issued for the app 'remind', not for 'mcport'.",
                ),
                other => panic!("unexpected slt {other}"),
            }
        }
        "refresh_token" => {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if stub.refresh_unavailable.load(Ordering::SeqCst) {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(
                        json!({"error":{"code":"unavailable","message":"Down for a moment.","hint":"Retry."}}),
                    ),
                );
            }
            if stub.refuse_refresh.load(Ordering::SeqCst)
                || form["refresh_token"] != *stub.current_refresh.lock().unwrap()
            {
                return oauth(
                    "invalid_grant",
                    "The refresh token was already used, so this sign-in was revoked.",
                );
            }
            stub.rotations.fetch_add(1, Ordering::SeqCst);
            (StatusCode::OK, Json(issue(&stub, "silicon")))
        }
        other => panic!("unexpected grant {other}"),
    }
}

async fn revoke(
    State(stub): State<Shared>,
    Form(form): Form<BTreeMap<String, String>>,
) -> StatusCode {
    stub.revoked.lock().unwrap().push(form);
    StatusCode::OK
}

fn bearer(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned()
}

async fn discovery(State(stub): State<Shared>) -> Json<Value> {
    let origin = stub.origin.lock().unwrap().clone();
    let accounts = stub
        .discovery_accounts_url
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| origin.clone());
    Json(
        json!({"data":{"app_id":"mcport","accounts_url":accounts,"client_id":"mcport","backend_url":origin,"website_url":"","repository_url":"","docs_url":"","package_url":"","install_url":"","version":"0.3.0"}}),
    )
}

async fn me(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if !bearer(&headers).starts_with("access-") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(
                json!({"error":{"code":"authentication_required","message":"No token.","recovery":null,"outcome_unknown":false}}),
            ),
        );
    }
    (
        StatusCode::OK,
        Json(
            json!({"data":{"uuid":"Sc1","id":"si:scout-renamed","kind":"silicon","display_name":"Scout","custodian":{"uuid":"Ada","id":"c:ada","kind":"carbon","display_name":"Ada"},"expires_at":1}}),
        ),
    )
}

async fn connections(State(stub): State<Shared>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let token = bearer(&headers);
    stub.bearers.lock().unwrap().push(token.clone());
    if stub.expired_access.lock().unwrap().as_deref() == Some(token.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(
                json!({"error":{"code":"token_expired","message":"The access token expired.","recovery":"Refresh it.","outcome_unknown":false}}),
            ),
        );
    }
    (StatusCode::OK, Json(json!({"data":[]})))
}

async fn serve() -> (Shared, String) {
    let stub = Shared::default();
    stub.first_expires_in.store(1800, Ordering::SeqCst);
    let app = Router::new()
        .route("/v1/device/authorize", post(device_authorize))
        .route("/v1/oauth/token", post(token))
        .route("/v1/oauth/revoke", post(revoke))
        .route("/api/v1/discovery", get(discovery))
        .route("/api/v1/me", get(me))
        .route("/api/v1/connections", get(connections))
        .route(
            "/api/v1/telemetry",
            post(|| async { Json(json!({"data":{"recorded":false}})) }),
        )
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    *stub.origin.lock().unwrap() = origin.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (stub, origin)
}

/// The binary in a clean environment: only this home, this backend and this Accounts.
fn mcport(home: &Path, origin: &str, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcport"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("SILICON_HOME", home)
        .env("MCPORT_URL", origin)
        .env("ACCOUNTS_URL", origin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}
async fn run(command: Command) -> Output {
    let mut command = command;
    tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap()
}
async fn run_with_stdin(command: Command, stdin: &'static str) -> Output {
    let mut command = command;
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        command.stdin(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap()
}
/// Stdout as JSON: the whole of it (pretty output), or else its last line (a device
/// sign-in with --json prints one line per step).
fn json_of(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    if let Ok(value) = serde_json::from_str(&text) {
        return value;
    }
    let last = text.lines().last().unwrap_or_default();
    serde_json::from_str(last).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}): {text} / {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
fn sign_in_files(home: &Path) -> Vec<std::path::PathBuf> {
    let dir = home.join(".mcport/dir/accounts");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect()
}
fn everything_written(home: &Path) -> String {
    let mut text = String::new();
    for path in walk(home) {
        text.push_str(&String::from_utf8_lossy(&std::fs::read(path).unwrap()));
    }
    text
}
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for entry in std::fs::read_dir(dir).into_iter().flatten() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn discovery_answers_offline_in_an_empty_home_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    // A backend nobody listens on: discovery must not need the network.
    let nowhere = "http://127.0.0.1:9";
    let help = mcport(home.path(), nowhere, &["--help"]).output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    for command in [
        "accounts",
        "login",
        "logout",
        "allow",
        "host",
        "connection",
        "tool",
    ] {
        assert!(help.contains(command), "{command} missing from --help");
    }
    assert!(!help.contains("iam") && !help.contains("session") && !help.contains("--test"));
    let accounts = mcport(home.path(), nowhere, &["accounts", "--json"])
        .env_remove("ACCOUNTS_URL")
        .env_remove("MCPORT_URL")
        .output()
        .unwrap();
    assert!(accounts.status.success());
    let accounts: Value = serde_json::from_slice(&accounts.stdout).unwrap();
    assert_eq!(
        accounts,
        json!({
            "app_id": "mcport",
            "client_id": "mcport",
            "accounts_url": "https://accounts.teamofsilicons.com",
            "api_url": "https://backend.mcport.teamofsilicons.com",
            "backend_url": "https://backend.mcport.teamofsilicons.com",
            "website_url": "https://mcport.teamofsilicons.com",
            "version": env!("CARGO_PKG_VERSION"),
            "device_flow": true,
            "public_client": true,
            "sign_in": {
                "carbon": "mcport login",
                "silicon": "silicon-accounts login --app mcport -q | mcport login --slt-stdin"
            },
            "status": "mcport login status --json",
            "install": "silicon-apps install mcport",
            "repository_url": "https://github.com/teamofsilicons/silicon-mcport",
            "docs_url": "https://github.com/teamofsilicons/silicon-mcport/tree/main/docs",
            "package_url": "https://crates.io/crates/mcport-client"
        })
    );
    // The hidden transition alias prints exactly the same object.
    let iam = mcport(home.path(), nowhere, &["iam", "--json"])
        .env_remove("ACCOUNTS_URL")
        .env_remove("MCPORT_URL")
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&iam.stdout).unwrap(),
        accounts
    );
    let status = mcport(home.path(), nowhere, &["login", "status", "--json"])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap(),
        json!({"authenticated": false})
    );
    // Without --json, signed out exits 1 (as silicon-accounts does).
    let status = mcport(home.path(), nowhere, &["login", "status"])
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(1));
    // A missing home is just "signed out".
    let missing = mcport(home.path(), nowhere, &["login", "status", "--json"])
        .env("SILICON_HOME", home.path().join("missing"))
        .output()
        .unwrap();
    assert!(missing.status.success());
    assert!(walk(home.path()).is_empty(), "discovery wrote files");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_silicon_signs_in_with_a_short_lived_token_on_stdin() {
    let (stub, origin) = serve().await;
    let home = tempfile::tempdir().unwrap();
    let login = run_with_stdin(
        mcport(home.path(), &origin, &["login", "--slt-stdin", "--json"]),
        "slt_ok\n",
    )
    .await;
    assert!(
        login.status.success(),
        "{}",
        String::from_utf8_lossy(&login.stderr)
    );
    let login = json_of(&login);
    assert_eq!(login["authenticated"], true);
    assert_eq!(login["uuid"], "Sc1");
    assert_eq!(login["kind"], "silicon");
    assert_eq!(login["method"], "slt");
    assert_eq!(login["custodian"]["id"], "c:ada");
    assert_eq!(*stub.slts.lock().unwrap(), ["slt_ok"]);
    let files = sign_in_files(home.path());
    assert_eq!(files.len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&files[0]).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The token itself is never kept, printed or logged.
    assert!(!everything_written(home.path()).contains("slt_ok"));
    let status = json_of(&run(mcport(home.path(), &origin, &["login", "status", "--json"])).await);
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["verified"], true);
    assert_eq!(
        status["id"], "si:scout-renamed",
        "the service's current id wins"
    );
    assert!(status["refresh_expires_at"].as_i64().is_some());
    let offline = json_of(
        &run(mcport(
            home.path(),
            &origin,
            &["login", "status", "--offline", "--json"],
        ))
        .await,
    );
    assert_eq!(offline["verified"], false);
    assert_eq!(offline["id"], "si:scout");
    // --slt and the positional form reach the same exchange.
    for args in [
        vec!["login", "--slt", "slt_used", "--json"],
        vec!["login", "slt_used", "--json"],
    ] {
        let refused = run(mcport(home.path(), &origin, &args)).await;
        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(json_of(&refused)["error"]["code"], "slt_already_used");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_short_lived_tokens_say_why_and_how_to_get_another() {
    let (stub, origin) = serve().await;
    let home = tempfile::tempdir().unwrap();
    for (slt, code) in [
        ("slt_used", "slt_already_used"),
        ("slt_late", "slt_expired"),
        ("slt_remind", "slt_wrong_app"),
    ] {
        let output = run(mcport(
            home.path(),
            &origin,
            &["login", "--slt", slt, "--json"],
        ))
        .await;
        assert_eq!(output.status.code(), Some(1));
        let error = &json_of(&output)["error"];
        assert_eq!(error["code"], code);
        assert!(
            error["recovery"]
                .as_str()
                .unwrap()
                .contains("silicon-accounts login --app mcport -q"),
            "{error}"
        );
        assert!(!error.to_string().contains(slt), "the token was echoed");
    }
    // Not a short-lived token at all: refused locally, nothing sent.
    let before = stub.slts.lock().unwrap().len();
    let output = run(mcport(
        home.path(),
        &origin,
        &["login", "--slt", "oac_old-iam-code", "--json"],
    ))
    .await;
    assert_eq!(json_of(&output)["error"]["code"], "sign_in_configuration");
    assert_eq!(stub.slts.lock().unwrap().len(), before);
    assert!(sign_in_files(home.path()).is_empty());
    // Human output names the problem and the recovery on stderr.
    let output = run(mcport(
        home.path(),
        &origin,
        &["login", "--slt", "slt_late"],
    ))
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error [slt_expired]") && stderr.contains("Recovery:"),
        "{stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_carbon_signs_in_with_the_device_flow() {
    let (stub, origin) = serve().await;
    *stub.device.lock().unwrap() = VecDeque::from(["pending", "tokens"]);
    let home = tempfile::tempdir().unwrap();
    let output = run(mcport(home.path(), &origin, &["login", "--json"])).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[0]["event"], "device_code");
    assert_eq!(lines[0]["user_code"], "WDJB-MJHT");
    assert_eq!(
        lines[0]["verification_uri_complete"],
        "http://127.0.0.1/device?code=WDJB-MJHT"
    );
    assert!(lines.iter().any(|line| line["event"] == "pending"));
    let last = lines.last().unwrap();
    assert_eq!(last["authenticated"], true);
    assert_eq!(last["kind"], "carbon");
    assert_eq!(last["method"], "device");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("sad_device"));
    // Human mode explains where to go on stderr and keeps stdout for the result.
    *stub.device.lock().unwrap() = VecDeque::from(["tokens"]);
    let human = run(mcport(home.path(), &origin, &["login"])).await;
    let stderr = String::from_utf8_lossy(&human.stderr);
    assert!(
        stderr.contains("WDJB-MJHT") && stderr.contains("http://127.0.0.1/device"),
        "{stderr}"
    );
    assert!(stderr.contains("Silicons cannot approve codes"));
    // The second sign-in replaced the first, and the first was signed out.
    let replaced = json_of(&human);
    assert_eq!(replaced["replaced"]["revoked"], true);
    assert_eq!(stub.revoked.lock().unwrap()[0]["token"], "sar_1");
    for (script, code) in [
        ("denied", "device_denied"),
        ("expired", "device_code_expired"),
    ] {
        *stub.device.lock().unwrap() = VecDeque::from([script]);
        let output = run(mcport(home.path(), &origin, &["login", "--json"])).await;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(json_of(&output)["error"]["code"], code);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_commands_refresh_once_and_persist_the_rotation() {
    let (stub, origin) = serve().await;
    // The first access token is about to expire, so the next commands must refresh.
    stub.first_expires_in.store(30, Ordering::SeqCst);
    let home = tempfile::tempdir().unwrap();
    let login = run_with_stdin(
        mcport(home.path(), &origin, &["login", "--slt-stdin", "--json"]),
        "slt_ok",
    )
    .await;
    assert!(login.status.success());
    let first = mcport(home.path(), &origin, &["connection", "ls", "--json"]);
    let second = mcport(home.path(), &origin, &["connection", "ls", "--json"]);
    let (a, b) = tokio::join!(run(first), run(second));
    assert!(a.status.success(), "{}", String::from_utf8_lossy(&a.stderr));
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    assert_eq!(json_of(&a), json!([]));
    assert_eq!(
        stub.rotations.load(Ordering::SeqCst),
        1,
        "refresh must be single-flight"
    );
    assert!(
        stub.bearers
            .lock()
            .unwrap()
            .iter()
            .all(|token| token == "access-2")
    );
    // The rotated pair reached the disk: the stored token now lives ~30 minutes.
    let status = json_of(
        &run(mcport(
            home.path(),
            &origin,
            &["login", "status", "--offline", "--json"],
        ))
        .await,
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(
        status["expires_at"].as_i64().unwrap() > now + 1000,
        "{status}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_the_service_refuses_is_refreshed_once_and_the_command_repeated() {
    let (stub, origin) = serve().await;
    let home = tempfile::tempdir().unwrap();
    run_with_stdin(
        mcport(home.path(), &origin, &["login", "--slt-stdin"]),
        "slt_ok",
    )
    .await;
    *stub.expired_access.lock().unwrap() = Some("access-1".into());
    let output = run(mcport(
        home.path(),
        &origin,
        &["connection", "ls", "--json"],
    ))
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(*stub.bearers.lock().unwrap(), ["access-1", "access-2"]);
    assert_eq!(stub.rotations.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spent_refresh_token_ends_the_sign_in_with_a_clear_reason() {
    let (stub, origin) = serve().await;
    stub.first_expires_in.store(10, Ordering::SeqCst);
    let home = tempfile::tempdir().unwrap();
    run_with_stdin(
        mcport(home.path(), &origin, &["login", "--slt-stdin"]),
        "slt_ok",
    )
    .await;
    stub.refuse_refresh.store(true, Ordering::SeqCst);
    let output = run(mcport(
        home.path(),
        &origin,
        &["connection", "ls", "--json"],
    ))
    .await;
    assert_eq!(output.status.code(), Some(1));
    let error = &json_of(&output)["error"];
    assert_eq!(error["code"], "sign_in_ended");
    assert!(error["message"].as_str().unwrap().contains("already used"));
    assert!(error["recovery"].as_str().unwrap().contains("mcport login"));
    assert!(sign_in_files(home.path()).is_empty());
    let status = json_of(&run(mcport(home.path(), &origin, &["login", "status", "--json"])).await);
    assert_eq!(status, json!({"authenticated": false}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_accounts_does_not_fail_a_command_while_the_token_still_works() {
    let (stub, origin) = serve().await;
    // Under a minute left: the CLI tries to refresh first.
    stub.first_expires_in.store(40, Ordering::SeqCst);
    let home = tempfile::tempdir().unwrap();
    run_with_stdin(
        mcport(home.path(), &origin, &["login", "--slt-stdin"]),
        "slt_ok",
    )
    .await;
    stub.refresh_unavailable.store(true, Ordering::SeqCst);
    let output = run(mcport(
        home.path(),
        &origin,
        &["connection", "ls", "--json"],
    ))
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(*stub.bearers.lock().unwrap(), ["access-1"]);
    assert_eq!(stub.rotations.load(Ordering::SeqCst), 0);
    // The sign-in is kept for when Accounts answers again.
    assert_eq!(sign_in_files(home.path()).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logout_revokes_the_refresh_token_as_a_public_client_and_forgets_it() {
    let (stub, origin) = serve().await;
    let home = tempfile::tempdir().unwrap();
    run_with_stdin(
        mcport(home.path(), &origin, &["login", "--slt-stdin"]),
        "slt_ok",
    )
    .await;
    let output = run(mcport(home.path(), &origin, &["logout", "--json"])).await;
    assert!(output.status.success());
    let out = json_of(&output);
    assert_eq!(out["signed_out"], true);
    assert_eq!(out["revoked"], true);
    let revoked = stub.revoked.lock().unwrap().clone();
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0]["token"], "sar_1");
    assert_eq!(revoked[0]["token_type_hint"], "refresh_token");
    assert_eq!(revoked[0]["client_id"], "mcport");
    assert!(sign_in_files(home.path()).is_empty());
    let again = json_of(&run(mcport(home.path(), &origin, &["logout", "--json"])).await);
    assert_eq!(again["signed_out"], false);
    assert_eq!(again["reason"], "not_signed_in");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_ins_from_before_silicon_accounts_are_ignored_with_a_hint() {
    let (_stub, origin) = serve().await;
    let home = tempfile::tempdir().unwrap();
    // mcport 0.2 kept gateway sessions per [backend, test_id] under sessions/.
    let key = {
        use sha2::{Digest, Sha256};
        let material = serde_json::to_vec(&(origin.as_str(), None::<&str>)).unwrap();
        format!("{:x}", Sha256::digest(material))
    };
    let legacy = home.path().join(format!(".mcport/dir/sessions/{key}.json"));
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, r#"{"active":"c:owner\ntos","sessions":{"c:owner\ntos":{"principal_id":"c:owner","org_id":"tos","access_token":"mpa_old","refresh_token":"mpr_old"}}}"#).unwrap();
    let status = json_of(&run(mcport(home.path(), &origin, &["login", "status", "--json"])).await);
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "signed_in_before_silicon_accounts");
    let output = run(mcport(
        home.path(),
        &origin,
        &["connection", "ls", "--json"],
    ))
    .await;
    let error = &json_of(&output)["error"];
    assert_eq!(error["code"], "not_signed_in");
    assert!(error["message"].as_str().unwrap().contains("0.2"));
    let out = json_of(&run(mcport(home.path(), &origin, &["logout", "--json"])).await);
    assert_eq!(out["legacy_sign_in_removed"], true);
    assert!(!legacy.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backend_that_trusts_another_accounts_is_refused_before_the_token_is_spent() {
    let (stub, origin) = serve().await;
    *stub.discovery_accounts_url.lock().unwrap() = Some("https://accounts.example".into());
    let home = tempfile::tempdir().unwrap();
    let output = run_with_stdin(
        mcport(home.path(), &origin, &["login", "--slt-stdin", "--json"]),
        "slt_ok",
    )
    .await;
    assert_eq!(output.status.code(), Some(1));
    let error = &json_of(&output)["error"];
    assert_eq!(error["code"], "accounts_mismatch");
    assert!(
        error["recovery"]
            .as_str()
            .unwrap()
            .contains("ACCOUNTS_URL=https://accounts.example")
    );
    assert!(
        stub.slts.lock().unwrap().is_empty(),
        "the single-use token was spent"
    );
}

#[test]
fn backend_and_accounts_settings_keep_their_precedence() {
    let home = tempfile::tempdir().unwrap();
    let show = |extra: &[(&str, &str)], args: &[&str]| {
        let mut command = mcport(home.path(), "", args);
        command.env_remove("MCPORT_URL").env_remove("ACCOUNTS_URL");
        for (key, value) in extra {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let fresh = show(&[], &["config", "show", "--json"]);
    assert_eq!(
        fresh["backend_url"],
        "https://backend.mcport.teamofsilicons.com"
    );
    assert_eq!(fresh["accounts_url"], "https://accounts.teamofsilicons.com");
    assert_eq!(fresh["signed_in"], Value::Null);
    show(
        &[],
        &[
            "config",
            "set",
            "backend",
            "http://127.0.0.1:4241",
            "--json",
        ],
    );
    show(
        &[],
        &[
            "config",
            "set",
            "accounts",
            "http://localhost:9590/",
            "--json",
        ],
    );
    let saved = show(&[], &["config", "show", "--json"]);
    assert_eq!(saved["backend_url"], "http://127.0.0.1:4241");
    assert_eq!(saved["accounts_url"], "http://localhost:9590");
    let env = show(
        &[
            ("MCPORT_URL", "http://127.0.0.1:4250"),
            ("ACCOUNTS_URL", "https://accounts.example"),
        ],
        &["config", "show", "--json"],
    );
    assert_eq!(env["backend_url"], "http://127.0.0.1:4250");
    assert_eq!(env["accounts_url"], "https://accounts.example");
    let flags = show(
        &[("MCPORT_URL", "http://127.0.0.1:4250")],
        &[
            "--backend",
            "http://127.0.0.1:4251",
            "--accounts-url",
            "https://a.example",
            "config",
            "show",
            "--json",
        ],
    );
    assert_eq!(flags["backend_url"], "http://127.0.0.1:4251");
    assert_eq!(flags["accounts_url"], "https://a.example");
    let output = mcport(
        home.path(),
        "",
        &[
            "config",
            "set",
            "accounts",
            "http://accounts.example",
            "--json",
        ],
    )
    .env_remove("MCPORT_URL")
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let output = mcport(
        home.path(),
        "",
        &["config", "home", "/does-not-exist-mcport", "--json"],
    )
    .output()
    .unwrap();
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a directory")
    );
}
