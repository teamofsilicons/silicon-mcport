//! Test fixtures: a local Silicon Accounts stub (JWKS, account lookups,
//! introspection) and access tokens signed with a real Ed25519 key.
use crate::{
    auth::{self, Auth},
    state::{App, Config, now},
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;

pub const WEBHOOK_SECRET: &str = "whsec_fixture_webhook_secret_0123456789";
pub const UPSTREAM: &str = "http://127.0.0.1:9";

/// An Ed25519 signing key published in the stub's JWKS under `kid`.
#[derive(Clone)]
pub struct TestKey {
    pub kid: String,
    key: SigningKey,
}
impl TestKey {
    pub fn new(kid: &str, seed: u8) -> Self {
        Self {
            kid: kid.into(),
            key: SigningKey::from_bytes(&[seed; 32]),
        }
    }
    pub fn jwk(&self) -> Value {
        json!({"kty":"OKP","crv":"Ed25519","alg":"EdDSA","use":"sig","kid":self.kid,
            "x":URL_SAFE_NO_PAD.encode(self.key.verifying_key().as_bytes())})
    }
    pub fn sign(&self, claims: &Value) -> String {
        let header = json!({"alg":"EdDSA","typ":"JWT","kid":self.kid});
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = self.key.sign(signed.as_bytes());
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    }
}

/// What the stub answers, and how often it was asked.
#[derive(Default)]
pub struct Stub {
    pub keys: Mutex<Vec<Value>>,
    pub accounts: Mutex<BTreeMap<String, Value>>,
    pub inactive: Mutex<HashSet<String>>,
    pub jwks_requests: AtomicUsize,
    pub lookups: AtomicUsize,
    pub introspections: AtomicUsize,
}
async fn jwks(State(stub): State<Arc<Stub>>) -> Json<Value> {
    stub.jwks_requests.fetch_add(1, Ordering::SeqCst);
    Json(json!({"keys": *stub.keys.lock().unwrap()}))
}
fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error":{"code":"account_not_found","message":"No such account."}})),
    )
        .into_response()
}
async fn lookup(
    State(stub): State<Arc<Stub>>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> Response {
    stub.lookups.fetch_add(1, Ordering::SeqCst);
    assert!(
        headers[header::AUTHORIZATION]
            .to_str()
            .unwrap()
            .starts_with("Basic ")
    );
    match stub.accounts.lock().unwrap().get(&uuid) {
        Some(account) => Json(account.clone()).into_response(),
        None => not_found(),
    }
}
async fn lookup_by_id(State(stub): State<Arc<Stub>>, Path(id): Path<String>) -> Response {
    stub.lookups.fetch_add(1, Ordering::SeqCst);
    let found = stub
        .accounts
        .lock()
        .unwrap()
        .values()
        .find(|a| {
            a["id"]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(&id))
        })
        .cloned();
    match found {
        Some(account) => Json(account).into_response(),
        None => not_found(),
    }
}
async fn introspect(State(stub): State<Arc<Stub>>, body: String) -> Json<Value> {
    stub.introspections.fetch_add(1, Ordering::SeqCst);
    let token = url::form_urlencoded::parse(body.as_bytes())
        .find(|(key, _)| key == "token")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default();
    let claims: Value = token
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    if stub.inactive.lock().unwrap().contains(&token) || claims.is_null() {
        return Json(json!({"active":false}));
    }
    Json(
        json!({"active":true,"sub":claims["sub"],"aud":claims["aud"],"exp":claims["exp"],"iat":claims["iat"],"kind":claims["kind"],"id":claims["id"]}),
    )
}

pub struct Fixture {
    pub app: App,
    pub stub: Arc<Stub>,
    pub key: TestKey,
    pub url: String,
    _dir: tempfile::TempDir,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
pub async fn fixture() -> Fixture {
    fixture_with(|_| {}).await
}
/// A fixture whose configuration `change` adjusts before the app starts.
pub async fn fixture_with(change: impl FnOnce(&mut Config)) -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let stub = Arc::new(Stub::default());
    let key = TestKey::new("k1", 7);
    stub.keys.lock().unwrap().push(key.jwk());
    let router = Router::new()
        .route("/.well-known/jwks.json", get(jwks))
        .route("/v1/accounts/by-id/{id}", get(lookup_by_id))
        .route("/v1/accounts/{uuid}", get(lookup))
        .route("/v1/oauth/introspect", post(introspect))
        .with_state(stub.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::local(dir.path(), &url);
    config.accounts_webhook_secret = Some(WEBHOOK_SECRET.into());
    // Connections in tests point here; nothing listens, nothing is resolved.
    config.upstream_origins.insert(UPSTREAM.into());
    change(&mut config);
    Fixture {
        app: App::new(config).unwrap(),
        stub,
        key,
        url,
        _dir: dir,
        server,
    }
}
impl Fixture {
    /// Make Accounts know a Carbon.
    pub fn carbon(&self, uuid: &str, id: &str) {
        self.stub.accounts.lock().unwrap().insert(
            uuid.into(),
            json!({"uuid":uuid,"kind":"carbon","id":id,"display_name":id.trim_start_matches("c:"),"pfp_url":format!("https://accounts.example/pfp/{uuid}.png"),"status":"active"}),
        );
    }
    /// Make Accounts know a Silicon looked after by `custodian`.
    pub fn silicon(&self, uuid: &str, id: &str, custodian: &str) {
        let custodian_id = self.stub.accounts.lock().unwrap()[custodian]["id"].clone();
        self.stub.accounts.lock().unwrap().insert(
            uuid.into(),
            json!({"uuid":uuid,"kind":"silicon","id":id,"display_name":id.trim_start_matches("si:"),"pfp_url":"","status":"active","custodian":{"uuid":custodian,"id":custodian_id}}),
        );
    }
    pub fn claims(&self, uuid: &str) -> Value {
        let account = self
            .stub
            .accounts
            .lock()
            .unwrap()
            .get(uuid)
            .cloned()
            .unwrap_or(json!({"kind":"carbon","id":""}));
        json!({"iss":self.url,"sub":uuid,"aud":"mcport","exp":now()+1800,"iat":now(),"nbf":now(),"jti":format!("jti-{uuid}-{}", rand::random::<u32>()),
            "kind":account["kind"],"id":account["id"],"mid":format!("mcport:{uuid}"),"fid":format!("family-{uuid}"),"scope":"profile"})
    }
    pub fn token(&self, uuid: &str) -> String {
        self.key.sign(&self.claims(uuid))
    }
    pub fn token_with(&self, uuid: &str, change: impl FnOnce(&mut Value)) -> String {
        let mut claims = self.claims(uuid);
        change(&mut claims);
        self.key.sign(&claims)
    }
    /// Authenticate `uuid` through the real bearer path.
    pub async fn auth(&self, uuid: &str) -> Auth {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", self.token(uuid)).parse().unwrap(),
        );
        auth::authenticate(&self.app, &headers).await.unwrap()
    }
    /// One request through the full router.
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = crate::router(self.app.clone())
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    /// `call` as `uuid`, with a fresh token.
    pub async fn as_(
        &self,
        uuid: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let token = self.token(uuid);
        self.call(method, path, Some(&token), body).await
    }
    /// Deliver a webhook event signed like Silicon Accounts does.
    pub async fn webhook(&self, event: &Value) -> (StatusCode, Value) {
        let body = event.to_string();
        let timestamp = now();
        let signature =
            silicon_accounts_client::sign_webhook(WEBHOOK_SECRET, timestamp, body.as_bytes());
        let response = crate::router(self.app.clone())
            .oneshot(
                Request::post("/webhooks/accounts")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-Accounts-Timestamp", timestamp.to_string())
                    .header("X-Accounts-Signature", signature)
                    .header(
                        "X-Accounts-Event-Id",
                        event["event_id"].as_str().unwrap_or(""),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}
/// A connection body for `POST /api/v1/connections`.
pub fn connection(name: &str, auth_mode: &str, visibility: &str) -> Value {
    json!({"name":name,"transport":"http","url":format!("{UPSTREAM}/mcp"),"auth_mode":auth_mode,"visibility":visibility})
}
/// An Accounts event body.
pub fn event(id: &str, kind: &str, occurred_at: &str, data: Value) -> Value {
    json!({"event_id":id,"type":kind,"occurred_at":occurred_at,"app_id":"mcport","silicon":null,"data":data})
}
/// RFC 3339 for `seconds` from now (negative: in the past).
pub fn at(seconds: i64) -> String {
    let t = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_secs((now() + seconds) as u64);
    httpdate_rfc3339(t)
}
fn httpdate_rfc3339(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
