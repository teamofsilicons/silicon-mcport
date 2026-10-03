use crate::{
    auth::{self, Auth},
    connections::{self, ProviderGrant},
    error::{Error, Result},
    state::{App, hash, now, secret},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, header},
    response::{Html, IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use mcport_core::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub key: String,
    pub connection_id: String,
    pub environment: String,
    pub generation: i64,
    pub family: String,
    pub principal_id: String,
    pub org_id: String,
    pub verifier: String,
    pub client_id: String,
    pub token_url: String,
    #[serde(default)]
    pub issuer: String,
    #[serde(default)]
    pub require_iss: bool,
    pub resource: String,
    pub authorization_url: String,
    pub expires_at: i64,
    pub cookie: String,
    pub account_epoch: i64,
}
pub fn epoch(app: &App, c: &Connection, a: &Auth) -> Result<i64> {
    Ok(app
        .store
        .get::<i64>("account_epoch", &connections::credential_key(c, a))?
        .unwrap_or(0))
}
pub fn bump_epoch(app: &App, c: &Connection, a: &Auth) -> Result<()> {
    app.store.put(
        "account_epoch",
        &connections::credential_key(c, a),
        a.env(),
        &c.org_id,
        &a.actor().principal_id,
        None,
        &(epoch(app, c, a)? + 1),
        None,
    )
}
fn failure(code: &str, message: &str) -> Error {
    Error::new(
        400,
        code,
        message,
        "Inspect the provider's OAuth configuration or connect a protected bearer credential instead.",
    )
}
async fn client(app: &App, url: &str) -> Result<reqwest::Client> {
    let policy = crate::execution::network_policy(app, url).await?;
    Ok(mcport_mcp::validated_http_client(
        url,
        policy,
        Duration::from_secs(10),
        Duration::from_secs(25),
    )
    .await?)
}
async fn body(response: reqwest::Response) -> Result<Value> {
    if !response.status().is_success() {
        return Err(failure(
            "provider_oauth_rejected",
            "The provider rejected this OAuth operation.",
        ));
    }
    if response.content_length().is_some_and(|s| s > 1024 * 1024) {
        return Err(failure(
            "provider_oauth_invalid",
            "OAuth metadata was too large.",
        ));
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        failure(
            "provider_oauth_unavailable",
            "The OAuth response was interrupted.",
        )
    })? {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(failure(
                "provider_oauth_invalid",
                "OAuth response was too large.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        failure(
            "provider_oauth_invalid",
            "The OAuth response was not valid JSON.",
        )
    })
}
async fn fetch(app: &App, url: &str) -> Result<Value> {
    body(client(app, url).await?.get(url).send().await.map_err(|_| {
        failure(
            "provider_oauth_unavailable",
            "The OAuth provider could not be reached.",
        )
    })?)
    .await
}
fn required(v: &Value, key: &str) -> Result<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            failure(
                "provider_oauth_invalid",
                "The provider omitted required OAuth metadata.",
            )
        })
}
fn metadata_url(issuer: &url::Url, name: &str) -> String {
    let path = issuer.path().trim_end_matches('/');
    format!(
        "{}/.well-known/{name}{path}",
        issuer.origin().ascii_serialization()
    )
}
fn issuer_url(issuer: &str) -> Result<url::Url> {
    let url = url::Url::parse(issuer).map_err(|_| {
        failure(
            "provider_oauth_invalid",
            "The authorization server URL is invalid.",
        )
    })?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(failure(
            "provider_oauth_invalid",
            "The authorization server issuer must be an absolute URL without credentials, a query or a fragment.",
        ));
    }
    Ok(url)
}
fn validate_issuer(expected: &str, actual: Option<&str>, required: bool) -> Result<()> {
    // Issuer identity is an exact OAuth identifier, not a normalized URL. This
    // check also applies to authorization error responses before consuming code.
    if expected.is_empty()
        || actual.is_some_and(|issuer| issuer != expected)
        || required && actual.is_none()
    {
        return Err(failure(
            "issuer_mismatch",
            "The OAuth authorization response did not identify the expected issuer. Start a new authorization attempt.",
        ));
    }
    Ok(())
}
#[derive(Deserialize)]
pub struct AuthorizeInput {
    pub client_id: Option<String>,
}
pub async fn authorize(
    State(app): State<App>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(input): Json<AuthorizeInput>,
) -> Result<Response> {
    auth::csrf(&app, &headers)?;
    let a = auth::authenticate(&app, &headers).await?;
    let c = connections::resolve(&app, &a, &name, false)?;
    if c.host_id.is_some() {
        return Err(failure(
            "configure_on_host",
            "Authorize local MCP accounts on their registered host.",
        ));
    }
    if c.auth_mode == "none" || c.auth_mode == "shared" && !connections::can_manage(&c, &a) {
        return Err(Error::denied());
    }
    let account_epoch = epoch(&app, &c, &a)?;
    let endpoint = url::Url::parse(c.url.as_deref().ok_or_else(Error::internal)?)
        .map_err(|_| Error::bad("Invalid MCP URL."))?;
    let mut resource_url = metadata_url(&endpoint, "oauth-protected-resource");
    // Prefer a provider's explicit protected-resource metadata challenge.
    let probe = client(&app, endpoint.as_str())
        .await?
        .get(endpoint.as_str())
        .send()
        .await
        .map_err(|_| {
            failure(
                "provider_oauth_unavailable",
                "The MCP provider could not be reached.",
            )
        })?;
    if let Some(challenge) = probe
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        && let Some(value) = challenge
            .split("resource_metadata=\"")
            .nth(1)
            .and_then(|v| v.split('"').next())
    {
        resource_url = value.to_owned()
    }
    let resource = fetch(&app, &resource_url).await?;
    let resource_id = required(&resource, "resource")?;
    let resource_parsed = url::Url::parse(&resource_id).map_err(|_| {
        failure(
            "provider_oauth_invalid",
            "The provider resource identifier is invalid.",
        )
    })?;
    if resource_parsed.origin() != endpoint.origin()
        || !(endpoint.path() == resource_parsed.path()
            || endpoint.path().starts_with(&format!(
                "{}/",
                resource_parsed.path().trim_end_matches('/')
            )))
        || resource_parsed.query().is_some()
        || resource_parsed.fragment().is_some()
        || !resource_parsed.username().is_empty()
        || resource_parsed.password().is_some()
    {
        return Err(failure(
            "resource_mismatch",
            "The provider's OAuth resource does not match this MCP connection.",
        ));
    }
    let issuer = resource
        .get("authorization_servers")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .and_then(Value::as_str)
        .ok_or_else(|| {
            failure(
                "oauth_not_supported",
                "This MCP did not advertise an OAuth authorization server.",
            )
        })?;
    let issuer_url = issuer_url(issuer)?;
    let metadata = fetch(
        &app,
        &metadata_url(&issuer_url, "oauth-authorization-server"),
    )
    .await?;
    if required(&metadata, "issuer")? != issuer {
        return Err(failure(
            "issuer_mismatch",
            "The OAuth metadata issuer does not match its discovery source.",
        ));
    }
    if !metadata
        .get("code_challenge_methods_supported")
        .and_then(Value::as_array)
        .is_some_and(|v| v.iter().any(|x| x == "S256"))
    {
        return Err(failure(
            "pkce_required",
            "This provider must support OAuth PKCE with S256.",
        ));
    }
    let authorize_url = required(&metadata, "authorization_endpoint")?;
    let token_url = required(&metadata, "token_endpoint")?;
    let _ = client(&app, &authorize_url).await?;
    let _ = client(&app, &token_url).await?;
    let redirect = format!(
        "{}/oauth/callback",
        app.config.public_url.trim_end_matches('/')
    );
    let client_id = if let Some(client_id) = input.client_id {
        if client_id.is_empty() || client_id.len() > 2048 {
            return Err(Error::bad("Invalid provider client ID."));
        }
        client_id
    } else if let Some(registration) = metadata
        .get("registration_endpoint")
        .and_then(Value::as_str)
    {
        let response=client(&app,registration).await?.post(registration).json(&json!({"client_name":"Silicon MCPort","redirect_uris":[redirect],"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"token_endpoint_auth_method":"none"})).send().await.map_err(|_|failure("provider_registration_failed","OAuth client registration could not be completed."))?;
        let registration = body(response).await?;
        if registration
            .get("token_endpoint_auth_method")
            .and_then(Value::as_str)
            .is_some_and(|x| x != "none")
        {
            return Err(failure(
                "registration_required",
                "The provider requires a confidential OAuth client. Configure a public PKCE client with the provider.",
            ));
        }
        required(&registration, "client_id")?
    } else {
        return Err(failure(
            "registration_required",
            "Register a public PKCE OAuth client with this provider, then retry with its client ID.",
        ));
    };
    let verifier = secret("");
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = secret("mpo_");
    let key = hash(&state);
    let nonce = secret("mpb_");
    let mut destination = url::Url::parse(&authorize_url)
        .map_err(|_| Error::bad("Invalid authorization endpoint."))?;
    destination
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("resource", &resource_id);
    if let Some(scopes) = resource.get("scopes_supported").and_then(Value::as_array) {
        let scopes = scopes
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        if !scopes.is_empty() {
            destination.query_pairs_mut().append_pair("scope", &scopes);
        }
    }
    let attempt = Attempt {
        key: key.clone(),
        connection_id: c.id,
        environment: a.env().into(),
        generation: a.session.generation,
        family: a.session.family.clone(),
        principal_id: a.actor().principal_id.clone(),
        org_id: a.actor().org_id.clone(),
        verifier,
        client_id,
        token_url,
        issuer: issuer.to_owned(),
        require_iss: metadata.get("authorization_response_iss_parameter_supported")
            == Some(&Value::Bool(true)),
        resource: resource_id,
        authorization_url: destination.to_string(),
        expires_at: now() + 600,
        cookie: nonce,
        account_epoch,
    };
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    let current = connections::resolve(&app, &a, &attempt.connection_id, false)?;
    if current.version != c.version || epoch(&app, &current, &a)? != account_epoch {
        return Err(Error::denied());
    }
    app.store.put(
        "oauth_attempt",
        &key,
        &attempt.environment,
        &attempt.org_id,
        &attempt.principal_id,
        None,
        &attempt,
        Some(0),
    )?;
    Ok(Json(json!({"data":{"authorization_url":format!("{}/oauth/start?state={state}",app.config.public_url.trim_end_matches('/')),"state":state}})).into_response())
}
#[derive(Deserialize)]
pub struct OAuthQuery {
    pub state: String,
    pub code: Option<String>,
    pub error: Option<String>,
    pub iss: Option<String>,
}
fn escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn cookie_header(app: &App, name: &str, value: &str, age: i64) -> String {
    format!(
        "{name}={value}; Path=/oauth; HttpOnly; SameSite=Lax; Max-Age={age}{}",
        if app.config.public_url.starts_with("https://") {
            "; Secure"
        } else {
            ""
        }
    )
}
pub async fn start(State(app): State<App>, Query(q): Query<OAuthQuery>) -> Result<Response> {
    let attempt = app
        .store
        .get::<Attempt>("oauth_attempt", &hash(&q.state))?
        .ok_or_else(Error::expired)?;
    if attempt.expires_at <= now() {
        return Err(Error::expired());
    }
    let a = auth::authorize_family(&app, &attempt.family, &attempt.environment).await?;
    let c = connections::resolve(&app, &a, &attempt.connection_id, false)?;
    let mut response=Html(format!("<!doctype html><html><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>Authorize MCP account · Silicon MCPort</title><body style='font:18px system-ui;max-width:560px;margin:10vh auto;padding:24px'><h1>Connect {}</h1><p>This will save the provider account for <strong>{}</strong> in organization <strong>{}</strong>.</p><p>{}</p><a href='{}'>Continue to provider authorization</a><p>You can close this window to cancel.</p></body></html>",escape(&c.name),escape(&attempt.principal_id),escape(&attempt.org_id),if c.auth_mode=="shared"{"People with access to this connection will be able to act through this provider account."}else{"This provider account will be used only for your own calls."},escape(&attempt.authorization_url))).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie_header(
            &app,
            &format!("mcport_oauth_{}", &attempt.key[..16]),
            &attempt.cookie,
            600,
        )
        .parse()
        .map_err(|_| Error::internal())?,
    );
    Ok(response)
}
pub async fn callback(
    State(app): State<App>,
    headers: HeaderMap,
    Query(q): Query<OAuthQuery>,
) -> Result<Response> {
    let key = hash(&q.state);
    let lock = app.lock(&format!("oauth:{key}"));
    let _guard = lock.lock().await;
    let attempt = app
        .store
        .get::<Attempt>("oauth_attempt", &key)?
        .ok_or_else(Error::expired)?;
    if attempt.expires_at <= now()
        || auth::cookie(&headers, &format!("mcport_oauth_{}", &key[..16])).as_deref()
            != Some(&attempt.cookie)
    {
        return Err(Error::expired());
    }
    validate_issuer(&attempt.issuer, q.iss.as_deref(), attempt.require_iss)?;
    app.assert_generation(&attempt.environment, attempt.generation)?;
    let a = auth::authorize_family(&app, &attempt.family, &attempt.environment).await?;
    let c = connections::resolve(&app, &a, &attempt.connection_id, false)?;
    if a.actor().principal_id != attempt.principal_id
        || a.actor().org_id != attempt.org_id
        || c.auth_mode == "shared" && !connections::can_manage(&c, &a)
    {
        return Err(Error::denied());
    }
    let credential_key = connections::credential_key(&c, &a);
    let account_lock = app.lock(&format!("credential:{credential_key}"));
    let _account_guard = account_lock.lock().await;
    if epoch(&app, &c, &a)? != attempt.account_epoch {
        return Err(Error::new(
            409,
            "account_changed",
            "This account changed after authorization started.",
            "Start a fresh provider authorization attempt.",
        ));
    }
    {
        let _environment_guard = auth::mutation_guard(&app, &a).await?;
        let current = connections::resolve(&app, &a, &c.id, false)?;
        if current.version != c.version {
            return Err(Error::denied());
        }
        app.store.delete("oauth_attempt", &key)?;
    }
    if q.error.is_some() {
        return Err(failure(
            "provider_consent_denied",
            "Provider authorization was declined. Your existing account was not changed.",
        ));
    }
    let code = q
        .code
        .filter(|s| !s.is_empty() && s.len() <= 16384)
        .ok_or_else(|| Error::bad("OAuth callback omitted its authorization code."))?;
    let response = client(&app, &attempt.token_url)
        .await?
        .post(&attempt.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", &attempt.client_id),
            (
                "redirect_uri",
                &format!(
                    "{}/oauth/callback",
                    app.config.public_url.trim_end_matches('/')
                ),
            ),
            ("code_verifier", &attempt.verifier),
            ("resource", &attempt.resource),
        ])
        .send()
        .await
        .map_err(|_| {
            failure(
                "provider_exchange_failed",
                "The authorization code could not be exchanged. Start authorization again.",
            )
        })?;
    let tokens = body(response).await?;
    validate_token(&tokens)?;
    let access = required(&tokens, "access_token")?;
    app.assert_generation(&attempt.environment, attempt.generation)?;
    let a = auth::authorize_family(&app, &attempt.family, &attempt.environment).await?;
    let _environment_guard = auth::mutation_guard(&app, &a).await?;
    let current = connections::resolve(&app, &a, &c.id, false)?;
    if current.version != c.version {
        return Err(Error::denied());
    }
    let g = ProviderGrant {
        connection_id: c.id.clone(),
        owner_id: a.actor().principal_id.clone(),
        owner_org: a.actor().org_id.clone(),
        label: a.actor().display_name.clone(),
        kind: "oauth".into(),
        secret: access,
        header_name: None,
        oauth: Some(
            json!({"token_url":attempt.token_url,"client_id":attempt.client_id,"resource":attempt.resource,"refresh_token":tokens.get("refresh_token"),"expires_at":now()+tokens.get("expires_in").and_then(Value::as_i64).unwrap_or(3600).clamp(1,31536000)}),
        ),
    };
    // Every successful account replacement advances the epoch, including OAuth
    // callbacks. Older authorization windows cannot replace the new account.
    bump_epoch(&app, &c, &a)?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    app.store.put(
        "credential",
        &connections::credential_key(&c, &a),
        a.env(),
        &c.org_id,
        &g.owner_id,
        None,
        &g,
        None,
    )?;
    Ok(Html("<!doctype html><html><meta charset=utf-8><title>Account connected · MCPort</title><body style='font:18px system-ui;padding:40px'><h1>Account connected</h1><p>Return to MCPort or your terminal. You can close this window.</p></body></html>").into_response())
}
fn validate_token(tokens: &Value) -> Result<()> {
    let token = required(tokens, "access_token")?;
    if !tokens
        .get("token_type")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        || token.len() > 16384
        || token.contains(['\r', '\n'])
    {
        return Err(failure(
            "provider_oauth_invalid",
            "The provider returned an unsupported token type or invalid credential.",
        ));
    }
    Ok(())
}
fn replace_existing_grant(
    app: &App,
    c: &Connection,
    a: &Auth,
    expected: &ProviderGrant,
    replacement: &ProviderGrant,
) -> Result<()> {
    // The caller holds the credential and environment locks. Connection deletion
    // uses only the latter, so a grant loaded before waiting must never upsert.
    let current = connections::resolve(app, a, &c.id, false)?;
    if current.version != c.version {
        return Err(Error::denied());
    }
    let expected = serde_json::to_value(expected)?;
    let mut replaced = false;
    app.store.update::<ProviderGrant>(
        "credential",
        &connections::credential_key(c, a),
        |grant| {
            if serde_json::to_value(&*grant).is_ok_and(|value| value == expected) {
                *grant = replacement.clone();
                replaced = true;
                true
            } else {
                false
            }
        },
    )?;
    if !replaced {
        return Err(Error::denied());
    }
    Ok(())
}
pub async fn execution_grant(app: &App, c: &Connection, a: &Auth) -> Result<ProviderGrant> {
    let key = connections::credential_key(c, a);
    let lock = app.lock(&format!("credential:{key}"));
    let _guard = lock.lock().await;
    let mut g = app
        .store
        .get::<ProviderGrant>("credential", &key)?
        .ok_or_else(|| {
            failure(
                "provider_authentication_required",
                "This provider account is not connected.",
            )
        })?;
    if g.kind != "oauth" {
        return Ok(g);
    }
    let mut oauth = g.oauth.clone().ok_or_else(Error::internal)?;
    if oauth.get("refresh_uncertain") == Some(&Value::Bool(true)) {
        return Err(failure(
            "provider_reconnect_required",
            "A previous provider refresh had an uncertain outcome. Reconnect the account.",
        ));
    }
    if oauth.get("expires_at").and_then(Value::as_i64).unwrap_or(0) > now() + 30 {
        return Ok(g);
    }
    let refresh = required(&oauth, "refresh_token").map_err(|_| {
        failure(
            "provider_reconnect_required",
            "The provider account expired and did not supply a refresh token.",
        )
    })?;
    let token_url = required(&oauth, "token_url")?;
    let client_id = required(&oauth, "client_id")?;
    let resource = required(&oauth, "resource")?;
    let previous = g.clone();
    oauth["refresh_uncertain"] = json!(true);
    g.oauth = Some(oauth.clone());
    {
        let _environment_guard = auth::mutation_guard(app, a).await?;
        replace_existing_grant(app, c, a, &previous, &g)?;
    }
    let uncertain = g.clone();
    let response=client(app,&token_url).await?.post(&token_url).form(&[("grant_type","refresh_token"),("refresh_token",&refresh),("client_id",&client_id),("resource",&resource)]).send().await.map_err(|_|failure("provider_refresh_uncertain","The provider refresh response was lost. Reconnect this account before another call."))?;
    let tokens = body(response).await?;
    validate_token(&tokens)?;
    g.secret = required(&tokens, "access_token")?;
    if let Some(token) = tokens.get("refresh_token") {
        oauth["refresh_token"] = token.clone();
    }
    oauth["expires_at"] = json!(
        now()
            + tokens
                .get("expires_in")
                .and_then(Value::as_i64)
                .unwrap_or(3600)
                .clamp(1, 31536000)
    );
    oauth["refresh_uncertain"] = json!(false);
    g.oauth = Some(oauth);
    let _environment_guard = auth::mutation_guard(app, a).await?;
    replace_existing_grant(app, c, a, &uncertain, &g)?;
    Ok(g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::StoredSession, execution::CallRecord, state::Config};
    use axum::{Router, http::Uri};
    use mcport_core::{Actor, Invocation};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct Provider {
        exchanges: AtomicUsize,
    }
    async fn request(State(provider): State<Arc<Provider>>, uri: Uri, body: String) -> Json<Value> {
        match uri.path() {
            "/api/v1/oauth/introspect" => Json(
                json!({"active":true,"public_id":"c:alice","actor_type":"carbon",
                "client_id":"mcport","audience":"mcport","org_id":"tos","membership_id":"c:alice[tos]","expires_at":now()+3600,
                "authorization":{"actor_type":"carbon","public_id":"c:alice","organization_id":"00000000-0000-0000-0000-000000000001",
                    "org_id":"tos","membership_id":"c:alice[tos]","membership_version":1,"authorization_epoch":1,
                    "audience":"mcport","testing_environment_id":null,"scopes":[],"org_role":null,"tags":null}}),
            ),
            "/provider/token" => {
                provider.exchanges.fetch_add(1, Ordering::SeqCst);
                let form: std::collections::HashMap<_, _> =
                    url::form_urlencoded::parse(body.as_bytes())
                        .into_owned()
                        .collect();
                assert_eq!(form["resource"], "https://mcp.example/mcp");
                assert_eq!(form["code_verifier"], "verifier");
                Json(json!({"access_token":form["code"],"token_type":"Bearer","expires_in":3600}))
            }
            _ => panic!("Unexpected fixture route {}", uri.path()),
        }
    }
    async fn fixture() -> (
        App,
        tempfile::TempDir,
        Auth,
        Connection,
        Arc<Provider>,
        tokio::task::JoinHandle<()>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let provider = Arc::new(Provider::default());
        let router = Router::new().fallback(request).with_state(provider.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut config = Config::from_env();
        config.data_dir = directory.path().into();
        config.app_id = "mcport".into();
        config.app_secret = "app-secret".into();
        config.iam_url = origin.clone();
        config.upstream_origins.insert(origin);
        let app = App::new(config).unwrap();
        let a = Auth {
            session: StoredSession {
                key: "session".into(),
                family: "family".into(),
                actor: Actor {
                    principal_id: "c:alice".into(),
                    identity_kind: "carbon".into(),
                    org_id: "tos".into(),
                    display_name: "Alice".into(),
                },
                environment: "production".into(),
                generation: 0,
                control_revision: 0,
                iam_access: "iam-access".into(),
                iam_refresh: "iam-refresh".into(),
                iam_expires: now() + 3600,
                expires_at: now() + 3600,
                refresh_key: None,
            },
        };
        app.store
            .put(
                "session",
                &a.session.key,
                a.env(),
                "tos",
                "c:alice",
                None,
                &a.session,
                None,
            )
            .unwrap();
        let c = Connection {
            id: "connection".into(),
            name: "fixture".into(),
            description: "".into(),
            org_id: "tos".into(),
            owner_id: "c:alice".into(),
            environment: "production".into(),
            transport: "http".into(),
            url: Some("https://mcp.example/mcp".into()),
            host_id: None,
            command: None,
            args: vec![],
            auth_mode: "per-user".into(),
            visibility: "private".into(),
            status: "active".into(),
            can_manage: true,
            account: None,
            created_at: now(),
            updated_at: now(),
            version: 1,
        };
        app.store
            .put(
                "connection",
                &c.id,
                a.env(),
                "tos",
                "c:alice",
                Some(&c.name),
                &c,
                None,
            )
            .unwrap();
        (app, directory, a, c, provider, task)
    }
    fn attempt(
        app: &App,
        a: &Auth,
        c: &Connection,
        state: &str,
        require_iss: bool,
    ) -> (HeaderMap, OAuthQuery) {
        let key = hash(state);
        let attempt = Attempt {
            key: key.clone(),
            connection_id: c.id.clone(),
            environment: a.env().into(),
            generation: 0,
            family: a.session.family.clone(),
            principal_id: a.actor().principal_id.clone(),
            org_id: a.actor().org_id.clone(),
            verifier: "verifier".into(),
            client_id: "fixture".into(),
            token_url: format!("{}/provider/token", app.config.iam_url),
            issuer: "https://issuer.example".into(),
            require_iss,
            resource: "https://mcp.example/mcp".into(),
            authorization_url: "https://issuer.example/authorize".into(),
            expires_at: now() + 600,
            cookie: "nonce".into(),
            account_epoch: epoch(app, c, a).unwrap(),
        };
        app.store
            .put(
                "oauth_attempt",
                &key,
                a.env(),
                "tos",
                "c:alice",
                None,
                &attempt,
                None,
            )
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("mcport_oauth_{}=nonce", &key[..16])
                .parse()
                .unwrap(),
        );
        (
            headers,
            OAuthQuery {
                state: state.into(),
                code: Some(format!("account-{state}")),
                error: None,
                iss: Some(attempt.issuer),
            },
        )
    }
    #[test]
    fn issuer_identity_uses_exact_string_and_rejects_ambiguous_urls() {
        assert!(
            validate_issuer(
                "https://issuer.example",
                Some("https://issuer.example"),
                true
            )
            .is_ok()
        );
        for other in [
            "https://ISSUER.example",
            "https://issuer.example/",
            "https://issuer.example:443",
            "https://other.example",
        ] {
            assert!(validate_issuer("https://issuer.example", Some(other), false).is_err());
        }
        assert!(validate_issuer("https://issuer.example", None, false).is_ok());
        assert!(validate_issuer("https://issuer.example", None, true).is_err());
        for invalid in [
            "https://user@issuer.example",
            "https://issuer.example?x=1",
            "https://issuer.example/#part",
            "data:text/plain,issuer",
        ] {
            assert!(issuer_url(invalid).is_err());
        }
    }
    #[tokio::test]
    async fn missing_or_wrong_issuer_never_sends_code_or_verifier_to_token_endpoint() {
        let (app, _dir, a, c, provider, task) = fixture().await;
        for (state, iss, error) in [
            ("missing", None, None),
            ("wrong", Some("https://wrong.example"), None),
            (
                "error",
                Some("https://wrong.example"),
                Some("access_denied"),
            ),
        ] {
            let (headers, mut q) = attempt(&app, &a, &c, state, true);
            q.iss = iss.map(str::to_owned);
            q.error = error.map(str::to_owned);
            assert_eq!(
                callback(State(app.clone()), headers, Query(q))
                    .await
                    .unwrap_err()
                    .1
                    .code,
                "issuer_mismatch"
            );
        }
        assert_eq!(provider.exchanges.load(Ordering::SeqCst), 0);
        assert!(
            app.store
                .get::<ProviderGrant>("credential", &connections::credential_key(&c, &a))
                .unwrap()
                .is_none()
        );
        // Older issuers which do not advertise response-iss support remain usable.
        let (headers, mut q) = attempt(&app, &a, &c, "legacy", false);
        q.iss = None;
        callback(State(app.clone()), headers, Query(q))
            .await
            .unwrap();
        assert_eq!(provider.exchanges.load(Ordering::SeqCst), 1);
        task.abort();
    }
    #[tokio::test]
    async fn new_oauth_account_invalidates_work_and_prevents_older_attempt_replacement() {
        let (app, _dir, a, c, provider, task) = fixture().await;
        let (old_headers, old) = attempt(&app, &a, &c, "old", true);
        let (new_headers, new) = attempt(&app, &a, &c, "new", true);
        let call = CallRecord {
            invocation: Invocation {
                id: "call".into(),
                connection_id: c.id.clone(),
                connection_name: c.name.clone(),
                actor_id: a.actor().principal_id.clone(),
                execution_account_id: a.actor().principal_id.clone(),
                method: "tools/call".into(),
                tool_name: Some("mutate".into()),
                status: "running".into(),
                created_at: now(),
                completed_at: None,
                result: None,
                error: None,
            },
            environment: a.env().into(),
            org_id: "tos".into(),
            family: a.session.family.clone(),
            generation: 0,
            host_id: None,
            params: json!({"name":"mutate","arguments":{}}),
            timeout_ms: 30000,
            expires_at: now() + 30,
            connection_version: 1,
            fingerprint: "call".into(),
            progress: None,
            telemetry_enabled: false,
        };
        crate::execution::save(&app, &call).unwrap();
        let cancellation = tokio_util::sync::CancellationToken::new();
        app.active
            .lock()
            .unwrap()
            .insert("call".into(), cancellation.clone());
        callback(State(app.clone()), new_headers, Query(new))
            .await
            .unwrap();
        assert_eq!(epoch(&app, &c, &a).unwrap(), 1);
        assert!(cancellation.is_cancelled());
        let cancelled = app
            .store
            .get::<CallRecord>("call", "call")
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.invocation.status, "cancelled");
        assert!(cancelled.invocation.error.unwrap().outcome_unknown);
        assert_eq!(
            callback(State(app.clone()), old_headers, Query(old))
                .await
                .unwrap_err()
                .1
                .code,
            "account_changed"
        );
        assert_eq!(provider.exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(
            app.store
                .get::<ProviderGrant>("credential", &connections::credential_key(&c, &a))
                .unwrap()
                .unwrap()
                .secret,
            "account-new"
        );
        task.abort();
    }

    #[tokio::test]
    async fn refresh_waiting_for_environment_cannot_recreate_a_deleted_connection_grant() {
        let (app, _dir, a, c, provider, server) = fixture().await;
        let grant = ProviderGrant {
            connection_id: c.id.clone(),
            owner_id: a.actor().principal_id.clone(),
            owner_org: "tos".into(),
            label: "Alice".into(),
            kind: "oauth".into(),
            secret: "expired-token".into(),
            header_name: None,
            oauth: Some(
                json!({"token_url":format!("{}/provider/token",app.config.iam_url),"client_id":"fixture",
                "resource":"https://mcp.example/mcp","refresh_token":"rotating-refresh","expires_at":0}),
            ),
        };
        let key = connections::credential_key(&c, &a);
        app.store
            .put(
                "credential",
                &key,
                a.env(),
                "tos",
                "c:alice",
                None,
                &grant,
                None,
            )
            .unwrap();
        let environment_lock = app.lock("environment:production");
        let guard = environment_lock.lock().await;
        let task_app = app.clone();
        let task_c = c.clone();
        let task_a = a.clone();
        let mut task =
            tokio::spawn(async move { execution_grant(&task_app, &task_c, &task_a).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut task)
                .await
                .is_err()
        );
        app.store.delete("connection", &c.id).unwrap();
        app.store.delete("credential", &key).unwrap();
        drop(guard);
        assert!(task.await.unwrap().is_err());
        assert!(
            app.store
                .get::<ProviderGrant>("credential", &key)
                .unwrap()
                .is_none()
        );
        assert_eq!(provider.exchanges.load(Ordering::SeqCst), 0);
        server.abort();
    }
}
