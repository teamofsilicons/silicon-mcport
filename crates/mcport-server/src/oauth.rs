//! Provider (upstream MCP) OAuth: PKCE authorization of a provider account for a
//! connection. Unrelated to Silicon Accounts sign-in.
use crate::{
    auth::{self, Auth, Live},
    connections::{self, ConnectionRecord, ProviderGrant},
    error::{Error, Result},
    state::{App, hash, now, secret},
    store::ENV,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, header},
    response::{Html, IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};

/// One provider authorization in progress (expires after 10 minutes). `other`
/// keeps fields of earlier releases; such attempts can no longer complete.
#[derive(Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub key: String,
    pub connection_id: String,
    /// The account that started it.
    #[serde(default)]
    pub account_uuid: String,
    #[serde(default)]
    pub created_at: i64,
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
    #[serde(flatten)]
    pub other: Map<String, Value>,
}
/// The replacement counter of `account`'s provider account on `c`.
pub fn epoch(app: &App, c: &ConnectionRecord, account: &str) -> Result<i64> {
    Ok(app
        .store
        .get::<i64>(
            "account_epoch",
            &connections::credential_key(&c.id, account),
        )?
        .unwrap_or(0))
}
pub fn bump_epoch(app: &App, c: &ConnectionRecord, account: &str) -> Result<()> {
    app.store.put(
        "account_epoch",
        &connections::credential_key(&c.id, account),
        ENV,
        &c.owner_uuid,
        account,
        None,
        &(epoch(app, c, account)? + 1),
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
async fn discover(app: &App, urls: &[String]) -> Result<Value> {
    for url in urls {
        let response = client(app, url).await?.get(url).send().await.map_err(|_| {
            failure(
                "provider_oauth_unavailable",
                "The OAuth discovery endpoint could not be reached.",
            )
        })?;
        // Only an absent endpoint permits fallback. Do not hide an invalid
        // document, issuer mismatch, redirect or authorization failure.
        if matches!(response.status().as_u16(), 404 | 410) {
            continue;
        }
        return body(response).await;
    }
    Err(failure(
        "oauth_metadata_not_found",
        "The provider did not publish OAuth metadata at its supported discovery endpoints.",
    ))
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
fn authorization_metadata_urls(issuer: &url::Url) -> Vec<String> {
    let mut urls = vec![
        metadata_url(issuer, "oauth-authorization-server"),
        metadata_url(issuer, "openid-configuration"),
    ];
    if !issuer.path().trim_matches('/').is_empty() {
        urls.push(format!(
            "{}/.well-known/openid-configuration",
            issuer.as_str().trim_end_matches('/')
        ));
    }
    urls
}
async fn authorization_metadata(app: &App, issuer: &str) -> Result<Value> {
    let metadata = discover(app, &authorization_metadata_urls(&issuer_url(issuer)?)).await?;
    if required(&metadata, "issuer")? != issuer {
        return Err(failure(
            "issuer_mismatch",
            "The OAuth metadata issuer does not match its discovery source.",
        ));
    }
    Ok(metadata)
}

#[derive(Default, Debug, PartialEq, Eq)]
struct BearerChallenge {
    resource_metadata: Option<String>,
    scope: Option<String>,
}
fn challenge_error() -> Error {
    failure(
        "provider_oauth_invalid",
        "The provider returned an invalid or ambiguous Bearer challenge.",
    )
}
fn token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}
fn auth_value(value: &str) -> Result<String> {
    if let Some(quoted) = value.strip_prefix('"') {
        let mut escaped = false;
        let mut result = String::new();
        for (index, ch) in quoted.char_indices() {
            if escaped {
                if ch.is_control() {
                    return Err(challenge_error());
                }
                result.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                return if quoted[index + 1..].trim().is_empty() {
                    Ok(result)
                } else {
                    Err(challenge_error())
                };
            } else if ch.is_control() {
                return Err(challenge_error());
            } else {
                result.push(ch);
            }
        }
        Err(challenge_error())
    } else if !value.is_empty() && value.bytes().all(token_char) {
        Ok(value.into())
    } else {
        Err(challenge_error())
    }
}
fn bearer_challenge(headers: &HeaderMap) -> Result<BearerChallenge> {
    let mut bearer: Option<BTreeMap<String, String>> = None;
    let mut challenges = Vec::new();
    let mut bytes = 0;
    for header in headers.get_all(header::WWW_AUTHENTICATE) {
        let text = header.to_str().map_err(|_| challenge_error())?;
        bytes += text.len();
        if bytes > 16384 {
            return Err(challenge_error());
        }
        let mut quoted = false;
        let mut escaped = false;
        let mut start = 0;
        let mut parts = Vec::new();
        for (index, byte) in text.bytes().enumerate() {
            if escaped {
                escaped = false;
            } else if quoted && byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = !quoted;
            } else if !quoted && byte == b',' {
                parts.push(&text[start..index]);
                start = index + 1;
            }
        }
        if quoted || escaped {
            return Err(challenge_error());
        }
        parts.push(&text[start..]);
        for part in parts {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let token_end = part.bytes().take_while(|b| token_char(*b)).count();
            if token_end == 0 {
                return Err(challenge_error());
            }
            let mut name = &part[..token_end];
            let mut rest = part[token_end..].trim_start();
            if !rest.starts_with('=') {
                if let Some(previous) = bearer.take() {
                    challenges.push(previous);
                }
                if !name.eq_ignore_ascii_case("bearer") {
                    continue;
                }
                bearer = Some(BTreeMap::new());
                if rest.is_empty() {
                    continue;
                }
                let token_end = rest.bytes().take_while(|b| token_char(*b)).count();
                name = &rest[..token_end];
                rest = rest[token_end..].trim_start();
            }
            if let Some(params) = &mut bearer {
                let value = auth_value(rest.strip_prefix('=').ok_or_else(challenge_error)?.trim())?;
                if name.is_empty()
                    || params.len() >= 32
                    || params.insert(name.to_ascii_lowercase(), value).is_some()
                {
                    return Err(challenge_error());
                }
            }
        }
        if let Some(previous) = bearer.take() {
            challenges.push(previous);
        }
    }
    if challenges.len() > 1 {
        return Err(challenge_error());
    }
    let mut params = challenges.pop().unwrap_or_default();
    Ok(BearerChallenge {
        resource_metadata: params.remove("resource_metadata"),
        scope: params.remove("scope"),
    })
}
fn selected_scope(challenge: &BearerChallenge, resource: &Value) -> Result<Option<String>> {
    let scope = match &challenge.scope {
        Some(scope) => scope.clone(),
        None => match resource.get("scopes_supported") {
            None => return Ok(None),
            Some(Value::Array(scopes)) => scopes
                .iter()
                .map(|scope| scope.as_str().ok_or_else(challenge_error))
                .collect::<Result<Vec<_>>>()?
                .join(" "),
            _ => return Err(challenge_error()),
        },
    };
    if scope.is_empty() && challenge.scope.is_none() {
        return Ok(None);
    }
    if scope.is_empty()
        || scope.len() > 8192
        || !scope.bytes().all(|b| {
            b == b' ' || b == b'!' || (b'#'..=b'[').contains(&b) || (b']'..=b'~').contains(&b)
        })
    {
        return Err(challenge_error());
    }
    let scope = scope
        .split(' ')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if scope.is_empty() {
        return Err(challenge_error());
    }
    Ok(Some(scope))
}

fn public_endpoints(app: &App) -> Result<(url::Url, String, String)> {
    let base = app.config.public_url.trim_end_matches('/');
    let url = issuer_url(base)?;
    if base.len() > 2048
        || base.contains('\\')
        || base.split('/').any(|part| {
            matches!(
                part.to_ascii_lowercase().as_str(),
                "." | ".." | "%2e" | ".%2e" | "%2e." | "%2e%2e"
            )
        })
    {
        return Err(failure(
            "invalid_public_url",
            "Configure a public MCPort URL without dot path segments or a query.",
        ));
    }
    Ok((
        url,
        format!("{base}/oauth/client-metadata.json"),
        format!("{base}/oauth/callback"),
    ))
}
fn registration_metadata(app: &App) -> Result<Value> {
    let (url, _, redirect) = public_endpoints(app)?;
    let local = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    Ok(
        json!({"client_name":"Silicon MCPort","redirect_uris":[redirect],"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"token_endpoint_auth_method":"none","application_type":if local {"native"} else {"web"}}),
    )
}
pub async fn client_metadata(State(app): State<App>) -> Result<Json<Value>> {
    let (url, client_id, _) = public_endpoints(&app)?;
    if url.scheme() != "https" {
        return Err(Error::missing());
    }
    let mut metadata = registration_metadata(&app)?;
    metadata["client_id"] = json!(client_id);
    Ok(Json(metadata))
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
async fn client_identifier(
    app: &App,
    configured: Option<String>,
    metadata: &Value,
) -> Result<String> {
    if let Some(client_id) = configured {
        if client_id.is_empty() || client_id.len() > 2048 || client_id.chars().any(char::is_control)
        {
            return Err(Error::bad("Invalid provider client ID."));
        }
        return Ok(client_id);
    }
    let (public_url, client_id, _) = public_endpoints(app)?;
    if metadata.get("client_id_metadata_document_supported") == Some(&Value::Bool(true))
        && public_url.scheme() == "https"
    {
        return Ok(client_id);
    }
    if let Some(registration) = metadata
        .get("registration_endpoint")
        .and_then(Value::as_str)
    {
        let response = client(app, registration)
            .await?
            .post(registration)
            .json(&registration_metadata(app)?)
            .send()
            .await
            .map_err(|_| {
                failure(
                    "provider_registration_failed",
                    "OAuth client registration could not be completed.",
                )
            })?;
        if !response.status().is_success() {
            return Err(failure(
                "provider_registration_failed",
                "The provider rejected public PKCE client registration. Check its callback and application-type requirements, or supply a pre-registered public client ID.",
            ));
        }
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
        let client_id = required(&registration, "client_id")?;
        if client_id.len() > 2048 || client_id.chars().any(char::is_control) {
            return Err(failure(
                "provider_oauth_invalid",
                "The provider returned an invalid client identifier.",
            ));
        }
        return Ok(client_id);
    }
    Err(failure(
        "registration_required",
        "Register a public PKCE OAuth client with this provider, or configure a publicly reachable HTTPS MCPORT_PUBLIC_URL for providers supporting client metadata documents.",
    ))
}
#[derive(Deserialize)]
pub struct AuthorizeInput {
    pub client_id: Option<String>,
}
pub async fn authorize(
    State(app): State<App>,
    Live(a): Live,
    Path(name): Path<String>,
    Json(input): Json<AuthorizeInput>,
) -> Result<Response> {
    let (c, access) = connections::resolve(&app, &a.account, &name, false).await?;
    if c.host_id.is_some() {
        return Err(failure(
            "configure_on_host",
            "Authorize local MCP accounts on their registered host.",
        ));
    }
    if c.auth_mode == "none" || c.auth_mode == "shared" && !access.manages() {
        return Err(Error::denied());
    }
    let account = connections::execution_account(&c, a.uuid()).to_owned();
    let account_epoch = epoch(&app, &c, &account)?;
    let endpoint = url::Url::parse(c.url.as_deref().ok_or_else(Error::internal)?)
        .map_err(|_| Error::bad("Invalid MCP URL."))?;
    // Only the initial unauthorized Bearer challenge chooses requested scopes.
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
    let challenge = if probe.status() == reqwest::StatusCode::UNAUTHORIZED {
        bearer_challenge(probe.headers())?
    } else {
        BearerChallenge::default()
    };
    drop(probe);
    let resource = if let Some(resource_url) = &challenge.resource_metadata {
        fetch(&app, resource_url).await?
    } else {
        let path = metadata_url(&endpoint, "oauth-protected-resource");
        let root = format!(
            "{}/.well-known/oauth-protected-resource",
            endpoint.origin().ascii_serialization()
        );
        let urls = if path == root {
            vec![root]
        } else {
            vec![path, root]
        };
        discover(&app, &urls).await?
    };
    let scope = selected_scope(&challenge, &resource)?;
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
    let metadata = authorization_metadata(&app, issuer).await?;
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
    let (_, _, redirect) = public_endpoints(&app)?;
    let client_id = client_identifier(&app, input.client_id, &metadata).await?;
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
    if let Some(scope) = scope {
        destination.query_pairs_mut().append_pair("scope", &scope);
    }
    let attempt = Attempt {
        key: key.clone(),
        connection_id: c.id.clone(),
        account_uuid: a.uuid().into(),
        created_at: now(),
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
        other: Map::new(),
    };
    let (current, _) =
        connections::resolve(&app, &a.account, &attempt.connection_id, false).await?;
    if current.version != c.version || epoch(&app, &current, &account)? != account_epoch {
        return Err(Error::denied());
    }
    app.store.put(
        "oauth_attempt",
        &key,
        ENV,
        &c.owner_uuid,
        a.uuid(),
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
fn attempt_expired() -> Error {
    Error::new(
        401,
        "authorization_expired",
        "This provider authorization link expired or was already used.",
        "Start the provider account connection again from MCPort.",
    )
}
pub async fn start(State(app): State<App>, Query(q): Query<OAuthQuery>) -> Result<Response> {
    let attempt = app
        .store
        .get::<Attempt>("oauth_attempt", &hash(&q.state))?
        .filter(|attempt| !attempt.account_uuid.is_empty())
        .ok_or_else(attempt_expired)?;
    if attempt.expires_at <= now() {
        return Err(attempt_expired());
    }
    let a = auth::for_account(&app, &attempt.account_uuid, attempt.created_at)?;
    let (c, _) = connections::resolve(&app, &a.account, &attempt.connection_id, false).await?;
    let mut response=Html(format!("<!doctype html><html><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>Authorize MCP account · Silicon MCPort</title><body style='font:18px system-ui;max-width:560px;margin:10vh auto;padding:24px'><h1>Connect {}</h1><p>This will save the provider account for <strong>{}</strong>.</p><p>{}</p><a href='{}'>Continue to provider authorization</a><p>You can close this window to cancel.</p></body></html>",escape(&c.name),escape(a.account.label()),if c.auth_mode=="shared"{"Carbons and Silicons with access to this connection will be able to act through this provider account."}else{"This provider account will be used only for your own calls."},escape(&attempt.authorization_url))).into_response();
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
        .filter(|attempt| !attempt.account_uuid.is_empty())
        .ok_or_else(attempt_expired)?;
    if attempt.expires_at <= now()
        || auth::cookie(&headers, &format!("mcport_oauth_{}", &key[..16])).as_deref()
            != Some(&attempt.cookie)
    {
        return Err(attempt_expired());
    }
    validate_issuer(&attempt.issuer, q.iss.as_deref(), attempt.require_iss)?;
    let a = auth::for_account(&app, &attempt.account_uuid, attempt.created_at)?;
    let (c, access) = connections::resolve(&app, &a.account, &attempt.connection_id, false).await?;
    if c.auth_mode == "shared" && !access.manages() {
        return Err(Error::denied());
    }
    let account = connections::execution_account(&c, a.uuid()).to_owned();
    let credential_key = connections::credential_key(&c.id, &account);
    let account_lock = app.lock(&format!("credential:{credential_key}"));
    let _account_guard = account_lock.lock().await;
    if epoch(&app, &c, &account)? != attempt.account_epoch {
        return Err(Error::new(
            409,
            "account_changed",
            "This account changed after authorization started.",
            "Start a fresh provider authorization attempt.",
        ));
    }
    {
        let (current, _) = connections::resolve(&app, &a.account, &c.id, false).await?;
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
    let token = required(&tokens, "access_token")?;
    let a = auth::for_account(&app, &attempt.account_uuid, attempt.created_at)?;
    let (current, _) = connections::resolve(&app, &a.account, &c.id, false).await?;
    if current.version != c.version {
        return Err(Error::denied());
    }
    let g = ProviderGrant {
        connection_id: c.id.clone(),
        owner_uuid: account.clone(),
        label: a.account.label().to_owned(),
        kind: "oauth".into(),
        secret: token,
        header_name: None,
        oauth: Some(
            json!({"issuer":attempt.issuer,"token_url":attempt.token_url,"client_id":attempt.client_id,"resource":attempt.resource,"refresh_token":tokens.get("refresh_token"),"expires_at":now()+tokens.get("expires_in").and_then(Value::as_i64).unwrap_or(3600).clamp(1,31536000)}),
        ),
        other: Map::new(),
    };
    // Every successful account replacement advances the epoch, including OAuth
    // callbacks. Older authorization windows cannot replace the new account.
    bump_epoch(&app, &c, &account)?;
    crate::execution::invalidate_connection(&app, &c.id)?;
    app.store.put(
        "credential",
        &credential_key,
        ENV,
        &c.owner_uuid,
        &account,
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
async fn replace_existing_grant(
    app: &App,
    c: &ConnectionRecord,
    a: &Auth,
    key: &str,
    expected: &ProviderGrant,
    replacement: &ProviderGrant,
) -> Result<()> {
    // The caller holds the credential lock. Connection deletion does not, so a
    // grant loaded before waiting is only ever replaced in place, never upserted.
    let (current, _) = connections::resolve(app, &a.account, &c.id, false).await?;
    if current.version != c.version {
        return Err(Error::denied());
    }
    let expected = serde_json::to_value(expected)?;
    let mut replaced = false;
    app.store
        .update::<ProviderGrant>("credential", key, |grant| {
            if serde_json::to_value(&*grant).is_ok_and(|value| value == expected) {
                *grant = replacement.clone();
                replaced = true;
                true
            } else {
                false
            }
        })?;
    if !replaced {
        return Err(Error::denied());
    }
    Ok(())
}
pub async fn execution_grant(app: &App, c: &ConnectionRecord, a: &Auth) -> Result<ProviderGrant> {
    let key = connections::credential_key(&c.id, connections::execution_account(c, a.uuid()));
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
    replace_existing_grant(app, c, a, &key, &previous, &g).await?;
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
    replace_existing_grant(app, c, a, &key, &uncertain, &g).await?;
    Ok(g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        execution::{CallRecord, InvocationData},
        test_support::{Fixture, fixture_with},
    };
    use axum::{Router, http::Uri};
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
    /// Accounts stub + a provider token endpoint at `provider` + Alice's
    /// per-user connection.
    async fn fixture() -> (
        Fixture,
        Auth,
        ConnectionRecord,
        Arc<Provider>,
        String,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let provider = Arc::new(Provider::default());
        let router = Router::new().fallback(request).with_state(provider.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let allowed = origin.clone();
        let f = fixture_with(move |config| {
            config.upstream_origins.insert(allowed);
        })
        .await;
        f.carbon("Alice", "c:alice");
        let a = f.auth("Alice").await;
        let c = ConnectionRecord {
            id: "connection".into(),
            name: "fixture".into(),
            owner_uuid: "Alice".into(),
            transport: "http".into(),
            url: Some("https://mcp.example/mcp".into()),
            auth_mode: "per-user".into(),
            visibility: "invited".into(),
            created_at: now(),
            updated_at: now(),
            version: 1,
            ..Default::default()
        };
        f.app
            .store
            .put(
                "connection",
                &c.id,
                ENV,
                "Alice",
                "Alice",
                Some(&c.name),
                &c,
                None,
            )
            .unwrap();
        (f, a, c, provider, origin, task)
    }
    fn attempt(
        app: &App,
        a: &Auth,
        c: &ConnectionRecord,
        origin: &str,
        state: &str,
        require_iss: bool,
    ) -> (HeaderMap, OAuthQuery) {
        let key = hash(state);
        let attempt = Attempt {
            key: key.clone(),
            connection_id: c.id.clone(),
            account_uuid: a.uuid().into(),
            created_at: now(),
            verifier: "verifier".into(),
            client_id: "fixture".into(),
            token_url: format!("{origin}/provider/token"),
            issuer: "https://issuer.example".into(),
            require_iss,
            resource: "https://mcp.example/mcp".into(),
            authorization_url: "https://issuer.example/authorize".into(),
            expires_at: now() + 600,
            cookie: "nonce".into(),
            account_epoch: epoch(app, c, a.uuid()).unwrap(),
            other: Map::new(),
        };
        app.store
            .put(
                "oauth_attempt",
                &key,
                ENV,
                &c.owner_uuid,
                a.uuid(),
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
        let (f, a, c, provider, origin, task) = fixture().await;
        let app = f.app.clone();
        for (state, iss, error) in [
            ("missing", None, None),
            ("wrong", Some("https://wrong.example"), None),
            (
                "error",
                Some("https://wrong.example"),
                Some("access_denied"),
            ),
        ] {
            let (headers, mut q) = attempt(&app, &a, &c, &origin, state, true);
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
                .get::<ProviderGrant>("credential", &connections::credential_key(&c.id, a.uuid()))
                .unwrap()
                .is_none()
        );
        // Older issuers which do not advertise response-iss support remain usable.
        let (headers, mut q) = attempt(&app, &a, &c, &origin, "legacy", false);
        q.iss = None;
        callback(State(app.clone()), headers, Query(q))
            .await
            .unwrap();
        assert_eq!(provider.exchanges.load(Ordering::SeqCst), 1);
        task.abort();
    }
    #[tokio::test]
    async fn new_oauth_account_invalidates_work_and_prevents_older_attempt_replacement() {
        let (f, a, c, provider, origin, task) = fixture().await;
        let app = f.app.clone();
        let (old_headers, old) = attempt(&app, &a, &c, &origin, "old", true);
        let (new_headers, new) = attempt(&app, &a, &c, &origin, "new", true);
        let call = CallRecord {
            invocation: InvocationData {
                id: "call".into(),
                connection_id: c.id.clone(),
                connection_name: c.name.clone(),
                method: "tools/call".into(),
                tool_name: Some("mutate".into()),
                status: "running".into(),
                created_at: now(),
                ..Default::default()
            },
            caller_uuid: a.uuid().into(),
            execution_account_uuid: Some(a.uuid().into()),
            params: json!({"name":"mutate","arguments":{}}),
            timeout_ms: 30000,
            expires_at: now() + 30,
            connection_version: 1,
            fingerprint: "call".into(),
            ..Default::default()
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
        assert_eq!(epoch(&app, &c, a.uuid()).unwrap(), 1);
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
                .get::<ProviderGrant>("credential", &connections::credential_key(&c.id, a.uuid()))
                .unwrap()
                .unwrap()
                .secret,
            "account-new"
        );
        let stored = app
            .store
            .get::<ProviderGrant>("credential", &connections::credential_key(&c.id, a.uuid()))
            .unwrap()
            .unwrap();
        assert_eq!(stored.owner_uuid, "Alice");
        assert_eq!(stored.label, "c:alice");
        task.abort();
    }

    #[tokio::test]
    async fn refresh_waiting_for_the_credential_cannot_recreate_a_deleted_connection_grant() {
        let (f, a, c, provider, origin, server) = fixture().await;
        let app = f.app.clone();
        let key = connections::credential_key(&c.id, a.uuid());
        let grant = ProviderGrant {
            connection_id: c.id.clone(),
            owner_uuid: a.uuid().into(),
            label: "Alice".into(),
            kind: "oauth".into(),
            secret: "expired-token".into(),
            header_name: None,
            oauth: Some(
                json!({"token_url":format!("{origin}/provider/token"),"client_id":"fixture",
                "resource":"https://mcp.example/mcp","refresh_token":"rotating-refresh","expires_at":0}),
            ),
            other: Map::new(),
        };
        app.store
            .put(
                "credential",
                &key,
                ENV,
                "Alice",
                "Alice",
                None,
                &grant,
                None,
            )
            .unwrap();
        let credential_lock = app.lock(&format!("credential:{key}"));
        let guard = credential_lock.lock().await;
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
        app.store.delete_connection(&c.id).unwrap();
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
    #[derive(Clone)]
    struct Reply {
        status: u16,
        headers: Vec<(String, String)>,
        json: Value,
    }
    #[derive(Default)]
    struct Discovery {
        replies: std::sync::Mutex<BTreeMap<String, Reply>>,
        requests: std::sync::Mutex<Vec<String>>,
        registrations: std::sync::Mutex<Vec<Value>>,
    }
    impl Discovery {
        fn reply(&self, path: &str, status: u16, json: Value) {
            self.replies.lock().unwrap().insert(
                path.into(),
                Reply {
                    status,
                    headers: vec![],
                    json,
                },
            );
        }
        fn challenge(&self, value: &str) {
            self.replies.lock().unwrap().insert(
                "/mcp".into(),
                Reply {
                    status: 401,
                    headers: vec![("www-authenticate".into(), value.into())],
                    json: json!({}),
                },
            );
        }
    }
    async fn discovery_request(
        State(state): State<Arc<Discovery>>,
        uri: Uri,
        body: String,
    ) -> Response {
        state.requests.lock().unwrap().push(uri.path().into());
        if uri.path() == "/register" {
            state
                .registrations
                .lock()
                .unwrap()
                .push(serde_json::from_str(&body).unwrap());
        }
        let reply = state
            .replies
            .lock()
            .unwrap()
            .get(uri.path())
            .cloned()
            .unwrap_or(Reply {
                status: 404,
                headers: vec![],
                json: json!({"error":"not_found"}),
            });
        let mut response = (
            axum::http::StatusCode::from_u16(reply.status).unwrap(),
            Json(reply.json),
        )
            .into_response();
        for (name, value) in reply.headers {
            response.headers_mut().append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        response
    }
    async fn discovery_fixture(
        app: &App,
        c: &ConnectionRecord,
    ) -> (App, Arc<Discovery>, String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Discovery::default());
        let router = Router::new()
            .fallback(discovery_request)
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut app = app.clone();
        let mut config = (*app.config).clone();
        config.upstream_origins.insert(origin.clone());
        config.public_url = "https://configured.mcport.example".into();
        app.config = Arc::new(config);
        let mut c = c.clone();
        c.url = Some(format!("{origin}/mcp"));
        app.store
            .put(
                "connection",
                &c.id,
                ENV,
                &c.owner_uuid,
                &c.owner_uuid,
                Some(&c.name),
                &c,
                None,
            )
            .unwrap();
        (app, state, origin, task)
    }
    async fn authorize_fixture(app: &App, a: &Auth, configured: Option<&str>) -> Result<Attempt> {
        let response = authorize(
            State(app.clone()),
            Live(a.clone()),
            Path("connection".into()),
            Json(AuthorizeInput {
                client_id: configured.map(str::to_owned),
            }),
        )
        .await?;
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        Ok(app
            .store
            .get(
                "oauth_attempt",
                &hash(body["data"]["state"].as_str().unwrap()),
            )?
            .unwrap())
    }
    fn metadata(origin: &str, issuer: &str) -> Value {
        json!({"issuer":issuer,"authorization_endpoint":format!("{origin}/authorize"),"token_endpoint":format!("{origin}/token"),
            "code_challenge_methods_supported":["S256"],"authorization_response_iss_parameter_supported":true,
            "client_id_metadata_document_supported":true,"registration_endpoint":format!("{origin}/register")})
    }
    #[test]
    fn challenge_parser_handles_multiple_schemes_quoted_commas_and_rejects_ambiguity() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::WWW_AUTHENTICATE,
            r#"Basic realm="ignore, this""#.parse().unwrap(),
        );
        headers.append(header::WWW_AUTHENTICATE, r#"bEaReR realm="a\"b", Resource_Metadata = "https://provider.example/meta?x=1,2", scope = "read write""#.parse().unwrap());
        assert_eq!(
            bearer_challenge(&headers).unwrap(),
            BearerChallenge {
                resource_metadata: Some("https://provider.example/meta?x=1,2".into()),
                scope: Some("read write".into())
            }
        );
        for invalid in [
            r#"Bearer scope="read", scope="write""#,
            r#"Bearer scope="read", Bearer scope="write""#,
            r#"Bearer scope="unterminated"#,
            r#"Bearer scope="read"trailing"#,
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::WWW_AUTHENTICATE, invalid.parse().unwrap());
            assert!(bearer_challenge(&headers).is_err(), "{invalid}");
        }
        let challenge = BearerChallenge {
            scope: Some("read".into()),
            resource_metadata: None,
        };
        assert_eq!(
            selected_scope(&challenge, &json!({"scopes_supported":["admin"]})).unwrap(),
            Some("read".into())
        );
        assert_eq!(
            selected_scope(
                &BearerChallenge::default(),
                &json!({"scopes_supported":["read","write"]})
            )
            .unwrap(),
            Some("read write".into())
        );
        assert_eq!(
            selected_scope(&BearerChallenge::default(), &json!({})).unwrap(),
            None
        );
    }
    #[tokio::test]
    async fn oidc_appended_discovery_and_root_resource_fallback_use_challenge_scope_and_cimd() {
        let (f, a, c, _provider, _origin, provider_server) = fixture().await;
        let (app, discovery, origin, server) = discovery_fixture(&f.app, &c).await;
        let issuer = format!("{origin}/tenant");
        discovery.challenge(r#"Basic realm="ignore, this", Bearer scope="read:one read:two""#);
        discovery.reply("/.well-known/oauth-protected-resource",200,json!({"resource":format!("{origin}/mcp"),"authorization_servers":[issuer],"scopes_supported":["admin"]}));
        discovery.reply(
            "/tenant/.well-known/openid-configuration",
            200,
            metadata(&origin, &issuer),
        );
        let attempt = authorize_fixture(&app, &a, None).await.unwrap();
        assert_eq!(attempt.issuer, issuer);
        assert_eq!(
            attempt.client_id,
            "https://configured.mcport.example/oauth/client-metadata.json"
        );
        let url = url::Url::parse(&attempt.authorization_url).unwrap();
        let pairs: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["scope"], "read:one read:two");
        assert_eq!(pairs["resource"], format!("{origin}/mcp"));
        assert_eq!(
            pairs["redirect_uri"],
            "https://configured.mcport.example/oauth/callback"
        );
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(
            pairs["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(attempt.verifier.as_bytes()))
        );
        assert_eq!(
            *discovery.requests.lock().unwrap(),
            vec![
                "/mcp",
                "/.well-known/oauth-protected-resource/mcp",
                "/.well-known/oauth-protected-resource",
                "/.well-known/oauth-authorization-server/tenant",
                "/.well-known/openid-configuration/tenant",
                "/tenant/.well-known/openid-configuration"
            ]
        );
        assert!(discovery.registrations.lock().unwrap().is_empty());
        server.abort();
        provider_server.abort();
    }
    #[tokio::test]
    async fn discovery_does_not_fallback_after_issuer_mismatch_or_redirect() {
        let (f, _a, c, _provider, _origin, provider_server) = fixture().await;
        let (app, discovery, origin, server) = discovery_fixture(&f.app, &c).await;
        let issuer = format!("{origin}/tenant");
        discovery.reply(
            "/.well-known/oauth-authorization-server/tenant",
            200,
            metadata(&origin, "https://wrong.example"),
        );
        discovery.reply(
            "/.well-known/openid-configuration/tenant",
            200,
            metadata(&origin, &issuer),
        );
        assert_eq!(
            authorization_metadata(&app, &issuer)
                .await
                .unwrap_err()
                .1
                .code,
            "issuer_mismatch"
        );
        assert_eq!(discovery.requests.lock().unwrap().len(), 1);
        discovery.replies.lock().unwrap().insert(
            "/.well-known/oauth-authorization-server/tenant".into(),
            Reply {
                status: 302,
                headers: vec![(
                    "location".into(),
                    format!("{origin}/.well-known/openid-configuration/tenant"),
                )],
                json: json!({}),
            },
        );
        assert_eq!(
            authorization_metadata(&app, &issuer)
                .await
                .unwrap_err()
                .1
                .code,
            "provider_oauth_rejected"
        );
        assert_eq!(discovery.requests.lock().unwrap().len(), 2);
        server.abort();
        provider_server.abort();
    }
    #[tokio::test]
    async fn registration_order_and_application_type_respect_configured_public_url() {
        let (f, _a, c, _provider, _origin, provider_server) = fixture().await;
        let (mut app, discovery, origin, server) = discovery_fixture(&f.app, &c).await;
        let metadata = metadata(&origin, &origin);
        discovery.reply(
            "/register",
            201,
            json!({"client_id":"dynamic-id","token_endpoint_auth_method":"none"}),
        );
        assert_eq!(
            client_identifier(&app, Some("explicit-id".into()), &metadata)
                .await
                .unwrap(),
            "explicit-id"
        );
        assert_eq!(
            client_identifier(&app, None, &metadata).await.unwrap(),
            "https://configured.mcport.example/oauth/client-metadata.json"
        );
        assert!(discovery.registrations.lock().unwrap().is_empty());
        let mut config = (*app.config).clone();
        config.public_url = "http://127.0.0.1:4380".into();
        app.config = Arc::new(config);
        assert_eq!(
            client_identifier(&app, None, &metadata).await.unwrap(),
            "dynamic-id"
        );
        assert_eq!(
            discovery.registrations.lock().unwrap()[0]["application_type"],
            "native"
        );
        assert_eq!(
            discovery.registrations.lock().unwrap()[0]["redirect_uris"],
            json!(["http://127.0.0.1:4380/oauth/callback"])
        );
        assert!(client_metadata(State(app.clone())).await.is_err());
        let mut config = (*app.config).clone();
        config.public_url = "https://configured.mcport.example".into();
        app.config = Arc::new(config);
        let mut legacy = metadata.clone();
        legacy["client_id_metadata_document_supported"] = json!(false);
        assert_eq!(
            client_identifier(&app, None, &legacy).await.unwrap(),
            "dynamic-id"
        );
        assert_eq!(
            discovery.registrations.lock().unwrap()[1]["application_type"],
            "web"
        );
        server.abort();
        provider_server.abort();
    }
    #[tokio::test]
    async fn client_metadata_route_is_public_and_never_uses_request_host() {
        use tower::ServiceExt;
        let (f, _a, c, _provider, _origin, provider_server) = fixture().await;
        let (app, _discovery, _origin, server) = discovery_fixture(&f.app, &c).await;
        let response = crate::router(app)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/oauth/client-metadata.json")
                    .header("host", "attacker.example")
                    .header("x-forwarded-host", "attacker.example")
                    .header("x-forwarded-proto", "http")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let data = axum::body::to_bytes(response.into_body(), 5120)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(
            json["client_id"],
            "https://configured.mcport.example/oauth/client-metadata.json"
        );
        assert_eq!(
            json["redirect_uris"],
            json!(["https://configured.mcport.example/oauth/callback"])
        );
        assert_eq!(json["token_endpoint_auth_method"], "none");
        assert!(json.get("client_secret").is_none());
        server.abort();
        provider_server.abort();
    }
}
