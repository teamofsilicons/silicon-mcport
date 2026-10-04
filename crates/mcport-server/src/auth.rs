use crate::{
    error::{Error, Result},
    state::{App, Environment, hash, id, now, secret},
};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use mcport_core::{Actor, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use silicon_iam_client::{Client, Credential, EnvironmentKey, IdempotencyKey, Mutation, models};

const SESSION_SECONDS: i64 = 3600;
const REFRESH_SECONDS: i64 = 30 * 86400;
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub key: String,
    pub family: String,
    pub actor: Actor,
    pub environment: String,
    pub generation: i64,
    #[serde(default)]
    pub control_revision: i64,
    pub iam_access: String,
    pub iam_refresh: String,
    pub iam_expires: i64,
    pub expires_at: i64,
    pub refresh_key: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Refresh {
    session_key: String,
    family: String,
    environment: String,
    generation: i64,
    expires_at: i64,
    successor: Option<Session>,
}
#[derive(Serialize, Deserialize)]
struct Revocation {
    revoked_at: i64,
}
#[derive(Serialize, Deserialize)]
struct LoginExchange {
    family: String,
    generation: i64,
    expires_at: i64,
    session: Session,
}
#[derive(Clone)]
pub struct Auth {
    pub session: StoredSession,
}
impl Auth {
    pub fn actor(&self) -> &Actor {
        &self.session.actor
    }
    pub fn env(&self) -> &str {
        &self.session.environment
    }
}

pub fn environment_header(headers: &HeaderMap) -> Result<String> {
    let s = headers
        .get("x-mcport-test")
        .map(|v| {
            v.to_str()
                .map(str::to_owned)
                .map_err(|_| Error::bad("Invalid testing environment header."))
        })
        .transpose()?
        .unwrap_or_else(|| "production".into());
    if s.is_empty() || s.len() > 100 || s.contains(['\r', '\n', '/']) {
        return Err(Error::bad("Invalid testing environment ID."));
    }
    Ok(s)
}
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|s| {
            s.trim()
                .strip_prefix(&format!("{name}="))
                .map(str::to_owned)
        })
}
pub fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_owned)
}
fn browser_csrf(app: &App, headers: &HeaderMap) -> Result<()> {
    let origin = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok());
    if origin != Some(&app.config.web_url) && origin != Some(&app.config.public_url) {
        return Err(Error::new(
            403,
            "origin_rejected",
            "This browser request did not originate from MCPort.",
            "Return to the MCPort website and try again.",
        ));
    }
    Ok(())
}
pub fn csrf(app: &App, headers: &HeaderMap) -> Result<()> {
    if bearer(headers).is_some() {
        Ok(())
    } else {
        browser_csrf(app, headers)
    }
}

pub async fn iam(app: &App, env: &Environment) -> Result<Client> {
    if env.app_secret.is_empty() {
        return Err(Error::new(
            503,
            "application_not_configured",
            "MCPort's IAM application credential is not configured.",
            "Register MCPort in Honeycomb and configure its application secret on the backend.",
        ));
    }
    let mut client = Client::builder(&app.config.iam_url)?
        .credential(Credential::application(&app.config.app_id, &env.app_secret))
        .telemetry(false)
        .build()?;
    if env.id != "production" {
        if let Some(key) = &env.iam_key {
            client = client.with_environment(EnvironmentKey::new(key)?)
        } else {
            client = client.with_testing_application(&app.config.app_id, &env.app_secret)?;
        }
        let context = client.applications().testing_context().await?;
        if context.environment_id.to_string() != env.id
            || context.application.app_id != app.config.app_id
        {
            return Err(Error::denied());
        }
    }
    Ok(client)
}
fn same_actor(left: &Actor, right: &Actor) -> bool {
    left.principal_id == right.principal_id
        && left.org_id == right.org_id
        && left.identity_kind == right.identity_kind
}
fn inspected_actor(
    app_id: &str,
    env: &Environment,
    t: models::TokenIntrospection,
    expected: Option<&Actor>,
) -> Result<Actor> {
    if !t.active
        || t.client_id.as_deref().or(t.audience.as_deref()) != Some(app_id)
        || t.client_id.as_deref().is_some_and(|s| s != app_id)
        || t.audience.as_deref().is_some_and(|s| s != app_id)
        || t.expires_at.is_some_and(|expires| expires <= now())
    {
        return Err(Error::expired());
    }
    let principal = t.public_id.ok_or_else(Error::denied)?;
    let kind = serde_json::to_value(t.actor_type)?
        .as_str()
        .unwrap_or("")
        .to_owned();
    if principal.is_empty() || !matches!(kind.as_str(), "carbon" | "silicon") {
        return Err(Error::new(
            403,
            "unsupported_actor",
            "IAM did not provide a supported user identity.",
            "Use an application session bound to a Carbon or Silicon.",
        ));
    }
    let authorization = t.authorization.ok_or_else(|| {
        Error::new(
            403,
            "organization_required",
            "This application session must select one organization.",
            "Obtain an SLT for the account and organization you want to use.",
        )
    })?;
    // Optional disclosures may be absent, but contradictory bindings never authorize.
    let authorization_kind = serde_json::to_value(authorization.actor_type)?;
    if authorization.audience != app_id
        || authorization.public_id.as_deref() != Some(&principal)
        || authorization_kind
            .as_str()
            .is_some_and(|value| value != kind)
        || authorization.org_id.is_empty()
        || authorization.membership_id.is_empty()
        || t.org_id
            .as_deref()
            .is_some_and(|org| org != authorization.org_id)
        || t.membership_id
            .as_deref()
            .is_some_and(|membership| membership != authorization.membership_id)
        || authorization
            .testing_environment_id
            .map(|x| x.to_string())
            .as_deref()
            != if env.id == "production" {
                None
            } else {
                Some(env.id.as_str())
            }
    {
        return Err(Error::denied());
    }
    let actor = Actor {
        display_name: principal.clone(),
        principal_id: principal,
        identity_kind: kind,
        org_id: authorization.org_id,
    };
    if expected.is_some_and(|expected| !same_actor(expected, &actor)) {
        return Err(Error::expired());
    }
    Ok(actor)
}
async fn inspect(
    app: &App,
    env: &Environment,
    access: &str,
    expected: Option<&Actor>,
) -> Result<Actor> {
    let t = iam(app, env)
        .await?
        .oauth()
        .introspect(
            &models::TokenIntrospectionRequest {
                token: access.into(),
                token_type_hint: None,
            },
            expected.map(|a| a.org_id.as_str()),
        )
        .await?;
    let actor = inspected_actor(&app.config.app_id, env, t, expected)?;
    app.assert_environment(env)?;
    Ok(actor)
}
fn assert_family(app: &App, family: &str, environment: &str, generation: i64) -> Result<()> {
    if app
        .store
        .get::<Revocation>("family_revoked", family)?
        .is_some()
    {
        return Err(Error::expired());
    }
    app.assert_generation(environment, generation)
}
fn assert_stored(app: &App, s: &StoredSession) -> Result<()> {
    if app.environment(&s.environment)?.control_revision != s.control_revision {
        return Err(Error::expired());
    }
    assert_family(app, &s.family, &s.environment, s.generation)?;
    let current = app
        .store
        .get::<StoredSession>("session", &s.key)?
        .ok_or_else(Error::expired)?;
    if current.family != s.family
        || current.environment != s.environment
        || current.generation != s.generation
        || !same_actor(&current.actor, &s.actor)
    {
        return Err(Error::expired());
    }
    Ok(())
}
/// Local authorization fence for the final, non-network part of a mutation.
/// Acquire after resource/family locks; lifecycle never waits on those locks.
pub fn check_current(app: &App, auth: &Auth) -> Result<()> {
    assert_stored(app, &auth.session)
}
pub async fn mutation_guard(app: &App, auth: &Auth) -> Result<tokio::sync::OwnedMutexGuard<()>> {
    let guard = app
        .lock(&format!("environment:{}", auth.env()))
        .lock_owned()
        .await;
    check_current(app, auth)?;
    Ok(guard)
}
fn save_session(app: &App, s: &StoredSession, create: bool) -> Result<()> {
    assert_family(app, &s.family, &s.environment, s.generation)?;
    app.store.put(
        "session",
        &s.key,
        &s.environment,
        &s.actor.org_id,
        &s.actor.principal_id,
        None,
        s,
        if create { Some(0) } else { None },
    )
}
// Hold the family lock across IAM requests, rotation and revocation. A refresh
// credential can renew an expired gateway access token; ordinary calls cannot.
async fn live_locked(
    app: &App,
    key: &str,
    environment: &str,
    refresh_authorized: bool,
) -> Result<Auth> {
    let mut s = app
        .store
        .get::<StoredSession>("session", key)?
        .ok_or_else(Error::expired)?;
    if s.environment != environment || (!refresh_authorized && s.expires_at <= now()) {
        return Err(Error::expired());
    }
    assert_stored(app, &s)?;
    let env = app.environment(environment)?;
    if s.iam_expires <= now() + 30 || s.refresh_key.is_some() {
        if s.refresh_key.is_none() {
            let _environment_guard = mutation_guard(app, &Auth { session: s.clone() }).await?;
            s.refresh_key = Some(id());
            save_session(app, &s, false)?;
        }
        let mutation = Mutation::with_key(IdempotencyKey::parse(
            s.refresh_key.clone().ok_or_else(Error::internal)?,
        )?);
        let tokens = iam(app, &env)
            .await?
            .oauth()
            .refresh(&app.config.app_id, &s.iam_refresh, &mutation)
            .await?;
        inspect(app, &env, &tokens.access_token, Some(&s.actor)).await?;
        let _environment_guard = mutation_guard(app, &Auth { session: s.clone() }).await?;
        s.iam_access = tokens.access_token;
        s.iam_refresh = tokens.refresh_token;
        s.iam_expires = now() + tokens.expires_in.clamp(1, 86400);
        s.refresh_key = None;
        save_session(app, &s, false)?;
    } else {
        inspect(app, &env, &s.iam_access, Some(&s.actor)).await?;
        assert_stored(app, &s)?;
    }
    Ok(Auth { session: s })
}
pub async fn live(app: &App, key: &str, environment: &str) -> Result<Auth> {
    let initial = app
        .store
        .get::<StoredSession>("session", key)?
        .ok_or_else(Error::expired)?;
    let lock = app.lock(&format!("session-family:{}", initial.family));
    let _guard = lock.lock().await;
    live_locked(app, key, environment, false).await
}
/// Revalidate an accepted job after access-token rotation. The caller must also
/// compare the returned actor and organization with the accepted job's identity.
pub async fn authorize_family(app: &App, family: &str, environment: &str) -> Result<Auth> {
    let lock = app.lock(&format!("session-family:{family}"));
    let _guard = lock.lock().await;
    let s = app
        .store
        .list::<StoredSession>("session", Some(environment))?
        .into_iter()
        .find(|s| s.family == family && s.expires_at > now())
        .ok_or_else(Error::expired)?;
    live_locked(app, &s.key, environment, false).await
}
pub async fn authenticate(app: &App, headers: &HeaderMap) -> Result<Auth> {
    let token = bearer(headers)
        .or_else(|| cookie(headers, "mcport_session"))
        .ok_or_else(Error::expired)?;
    live(app, &hash(&token), &environment_header(headers)?).await
}

#[derive(Deserialize)]
pub struct LoginInput {
    pub slt: String,
    pub identity_kind: Option<String>,
}
pub async fn login_inner(app: &App, headers: &HeaderMap, input: LoginInput) -> Result<Session> {
    // IAM's test plane also accepts actor selectors when its privileged client
    // submits a non-code value. Public callers possess neither that authority nor
    // the test root key: a world UUID and identity must never become a session.
    let issued_code = input.slt.strip_prefix("oac_").is_some_and(|code| {
        code.len() == 43
            && code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    });
    if !issued_code {
        return Err(Error::new(
            400,
            "invalid_login_token",
            "MCPort login requires an IAM-issued app-bound short-lived token.",
            "Obtain a new MCPort SLT from IAM for this account, organization and environment. Identity and test environment IDs are not login credentials.",
        ));
    }
    let env = app.environment(&environment_header(headers)?)?;
    let exchange_key = hash(&format!("{}:{}", env.id, input.slt));
    let lock = app.lock(&format!("login-exchange:{exchange_key}"));
    let _guard = lock.lock().await;
    if let Some(exchange) = app
        .store
        .get::<LoginExchange>("login_exchange", &exchange_key)?
    {
        if exchange.expires_at <= now()
            || exchange.session.environment != env.id
            || input
                .identity_kind
                .as_deref()
                .is_some_and(|kind| kind != exchange.session.actor.identity_kind)
        {
            return Err(Error::expired());
        }
        assert_family(app, &exchange.family, &env.id, exchange.generation)?;
        let auth = live(app, &hash(&exchange.session.access_token), &env.id).await?;
        if auth.session.family != exchange.family
            || !same_actor(auth.actor(), &exchange.session.actor)
        {
            return Err(Error::expired());
        }
        return Ok(exchange.session);
    }
    let tokens = iam(app, &env)
        .await?
        .oauth()
        .login(
            &app.config.app_id,
            &input.slt,
            &Mutation::with_key(IdempotencyKey::parse(exchange_key.clone())?),
        )
        .await?;
    let actor = inspect(app, &env, &tokens.access_token, None).await?;
    if input
        .identity_kind
        .as_deref()
        .is_some_and(|k| k != actor.identity_kind)
    {
        return Err(Error::new(
            403,
            "identity_kind_mismatch",
            "The selected account type does not match this login attempt.",
            "Start a new login using the correct Carbon or Silicon button.",
        ));
    }
    let _environment_guard = app
        .lock(&format!("environment:{}", env.id))
        .lock_owned()
        .await;
    app.assert_environment(&env)?;
    let access = secret("mpa_");
    let refresh = secret("mpr_");
    let key = hash(&access);
    let expires = now() + SESSION_SECONDS;
    let s = StoredSession {
        key,
        family: id(),
        actor: actor.clone(),
        environment: env.id.clone(),
        generation: env.generation,
        control_revision: env.control_revision,
        iam_access: tokens.access_token,
        iam_refresh: tokens.refresh_token,
        iam_expires: now() + tokens.expires_in.clamp(1, 86400),
        expires_at: expires,
        refresh_key: None,
    };
    save_session(app, &s, true)?;
    save_refresh(app, &refresh, &s)?;
    let session = Session {
        access_token: access,
        refresh_token: refresh,
        expires_at: expires,
        actor,
        environment: env.id.clone(),
    };
    app.store.put(
        "login_exchange",
        &exchange_key,
        &env.id,
        &s.actor.org_id,
        &s.actor.principal_id,
        None,
        &LoginExchange {
            family: s.family,
            generation: s.generation,
            expires_at: now() + 30,
            session: session.clone(),
        },
        Some(0),
    )?;
    Ok(session)
}
fn save_refresh(app: &App, token: &str, s: &StoredSession) -> Result<()> {
    assert_stored(app, s)?;
    let r = Refresh {
        session_key: s.key.clone(),
        family: s.family.clone(),
        environment: s.environment.clone(),
        generation: s.generation,
        expires_at: now() + REFRESH_SECONDS,
        successor: None,
    };
    app.store.put(
        "refresh",
        &hash(token),
        &s.environment,
        &s.actor.org_id,
        &s.actor.principal_id,
        None,
        &r,
        Some(0),
    )
}
pub async fn login(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<LoginInput>,
) -> Result<Json<Value>> {
    Ok(Json(
        json!({"data":login_inner(&app,&headers,input).await?}),
    ))
}
pub async fn status(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let a = authenticate(&app, &headers).await?;
    Ok(Json(
        json!({"data":{"authenticated":true,"actor":a.actor(),"environment":a.env(),"expires_at":a.session.expires_at}}),
    ))
}

#[derive(Deserialize)]
pub struct RefreshInput {
    refresh_token: String,
}
async fn refresh_inner(app: &App, token: &str, environment: &str) -> Result<Session> {
    if token.is_empty() || token.len() > 8192 {
        return Err(Error::expired());
    }
    let key = hash(token);
    let initial = app
        .store
        .get::<Refresh>("refresh", &key)?
        .ok_or_else(Error::expired)?;
    let lock = app.lock(&format!("session-family:{}", initial.family));
    let _guard = lock.lock().await;
    let mut r = app
        .store
        .get::<Refresh>("refresh", &key)?
        .ok_or_else(Error::expired)?;
    if r.expires_at <= now() || r.environment != environment {
        return Err(Error::expired());
    }
    assert_family(app, &r.family, environment, r.generation)?;
    if let Some(successor) = &r.successor {
        // A short retry window never restores logged-out, expired or rotated-away authority.
        let auth = live_locked(app, &hash(&successor.access_token), environment, false).await?;
        if auth.session.family != r.family || !same_actor(auth.actor(), &successor.actor) {
            return Err(Error::expired());
        }
        return Ok(successor.clone());
    }
    let auth = live_locked(app, &r.session_key, environment, true).await?;
    if auth.session.family != r.family || auth.session.generation != r.generation {
        return Err(Error::expired());
    }
    let _environment_guard = mutation_guard(app, &auth).await?;
    let access = secret("mpa_");
    let new_refresh = secret("mpr_");
    let mut next = auth.session;
    let old_key = next.key.clone();
    next.key = hash(&access);
    next.expires_at = now() + SESSION_SECONDS;
    let session = Session {
        access_token: access,
        refresh_token: new_refresh.clone(),
        expires_at: next.expires_at,
        actor: next.actor.clone(),
        environment: environment.into(),
    };
    save_session(app, &next, true)?;
    save_refresh(app, &new_refresh, &next)?;
    r.successor = Some(session.clone());
    r.expires_at = now() + 30;
    app.store.put(
        "refresh",
        &key,
        environment,
        &next.actor.org_id,
        &next.actor.principal_id,
        None,
        &r,
        None,
    )?;
    app.store.delete("session", &old_key)?;
    Ok(session)
}
pub async fn refresh(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<RefreshInput>,
) -> Result<Json<Value>> {
    Ok(Json(
        json!({"data":refresh_inner(&app,&input.refresh_token,&environment_header(&headers)?).await?}),
    ))
}
fn revoke_family(app: &App, s: &StoredSession) -> Result<()> {
    // Persist first: failed cleanup/restart cannot restore old refresh authority.
    app.store.put(
        "family_revoked",
        &s.family,
        &s.environment,
        &s.actor.org_id,
        &s.actor.principal_id,
        None,
        &Revocation { revoked_at: now() },
        None,
    )?;
    for member in app
        .store
        .list::<StoredSession>("session", Some(&s.environment))?
    {
        if member.family == s.family {
            app.store.delete("session", &member.key)?;
        }
    }
    Ok(())
}
pub async fn logout(State(app): State<App>, headers: HeaderMap) -> Result<Response> {
    csrf(&app, &headers)?;
    let environment = environment_header(&headers)?;
    let access = bearer(&headers).or_else(|| cookie(&headers, "mcport_session"));
    let mut session = access
        .as_ref()
        .map(|token| app.store.get::<StoredSession>("session", &hash(token)))
        .transpose()?
        .flatten();
    // Browsers remove an expired access cookie. Its HttpOnly refresh cookie
    // still identifies the family to revoke without consulting an unavailable IAM.
    // An explicit bearer token never falls back to ambient browser credentials.
    if session.is_none()
        && bearer(&headers).is_none()
        && let Some(token) = cookie(&headers, "mcport_refresh")
        && let Some(refresh) = app.store.get::<Refresh>("refresh", &hash(&token))?
        && refresh.environment == environment
    {
        session = app
            .store
            .list::<StoredSession>("session", Some(&environment))?
            .into_iter()
            .find(|session| session.family == refresh.family);
    }
    let session = session.ok_or_else(Error::expired)?;
    if session.environment != environment {
        return Err(Error::expired());
    }
    let lock = app.lock(&format!("session-family:{}", session.family));
    let _guard = lock.lock().await;
    let _environment_guard = app
        .lock(&format!("environment:{environment}"))
        .lock_owned()
        .await;
    app.assert_generation(&environment, session.generation)?;
    // Possession authorizes only revoking this token's family. IAM downtime or
    // an expired gateway deadline must not prevent sign-out.
    revoke_family(&app, &session)?;
    Ok(cookie_response(
        [
            "mcport_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0".into(),
            "mcport_refresh=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0".into(),
            "mcport_login_state=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0".into(),
        ],
        json!({"data":{"logged_out":true}}),
    ))
}

#[derive(Serialize, Deserialize)]
struct BrowserAttempt {
    state: String,
    kind: String,
    environment: String,
    generation: i64,
    #[serde(default)]
    control_revision: i64,
    expires_at: i64,
}
#[derive(Deserialize)]
pub struct BrowserStart {
    identity_kind: String,
}
fn login_url(app: &App, state: &str, kind: &str) -> Result<url::Url> {
    // Preserve state inside redirect_uri across both current and older IAM UIs.
    // Older IAM versions may ignore the identity/display preferences.
    let mut callback = url::Url::parse(&format!(
        "{}/auth/callback",
        app.config.web_url.trim_end_matches('/')
    ))
    .map_err(|_| Error::internal())?;
    callback.query_pairs_mut().append_pair("state", state);
    let mut url = url::Url::parse(&format!(
        "{}/login",
        app.config.iam_web_url.trim_end_matches('/')
    ))
    .map_err(|_| Error::internal())?;
    url.query_pairs_mut()
        .append_pair("app_id", &app.config.app_id)
        .append_pair("redirect_uri", callback.as_str())
        .append_pair("identity_kind", kind)
        .append_pair("display", "popup");
    Ok(url)
}
pub async fn browser_start(
    State(app): State<App>,
    headers: HeaderMap,
    Query(q): Query<BrowserStart>,
) -> Result<Response> {
    if !matches!(q.identity_kind.as_str(), "carbon" | "silicon") {
        return Err(Error::bad("Choose carbon or silicon."));
    }
    let state = secret("state_");
    let environment = environment_header(&headers)?;
    let _environment_guard = app
        .lock(&format!("environment:{environment}"))
        .lock_owned()
        .await;
    let env = app.environment(&environment)?;
    let attempt = BrowserAttempt {
        state: state.clone(),
        kind: q.identity_kind,
        environment: environment.clone(),
        generation: env.generation,
        control_revision: env.control_revision,
        expires_at: now() + 600,
    };
    app.store.put(
        "browser_attempt",
        &hash(&state),
        &environment,
        "",
        "",
        None,
        &attempt,
        Some(0),
    )?;
    let secure = if app.config.web_url.starts_with("https:") {
        "; Secure"
    } else {
        ""
    };
    Ok((
        [(
            header::SET_COOKIE,
            format!(
                "mcport_login_state={state}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600{secure}"
            ),
        )],
        Json(json!({"data":{"url":login_url(&app,&state,&attempt.kind)?.as_str(),"state":state}})),
    )
        .into_response())
}
#[derive(Deserialize)]
pub struct BrowserComplete {
    state: String,
    slt: String,
}
fn cookie_response(cookies: [String; 3], body: Value) -> Response {
    let mut response = Json(body).into_response();
    // Array response parts insert duplicate headers; Set-Cookie must append.
    for cookie in cookies {
        response.headers_mut().append(
            header::SET_COOKIE,
            cookie
                .parse()
                .expect("generated cookie contains only ASCII"),
        );
    }
    response
}
fn browser_session(app: &App, session: Session) -> Response {
    let secure = if app.config.web_url.starts_with("https:") {
        "; Secure"
    } else {
        ""
    };
    cookie_response(
        [
            format!(
                "mcport_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_SECONDS}{secure}",
                session.access_token
            ),
            format!(
                "mcport_refresh={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={REFRESH_SECONDS}{secure}",
                session.refresh_token
            ),
            format!("mcport_login_state=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}"),
        ],
        json!({"data":{"actor":session.actor,"environment":session.environment,"expires_at":session.expires_at}}),
    )
}
pub async fn browser_complete(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<BrowserComplete>,
) -> Result<Response> {
    browser_csrf(&app, &headers)?;
    if input.state.len() > 256
        || cookie(&headers, "mcport_login_state").as_deref() != Some(&input.state)
    {
        return Err(Error::denied());
    }
    let key = hash(&input.state);
    let lock = app.lock(&format!("browser-attempt:{key}"));
    let _guard = lock.lock().await;
    let attempt = app
        .store
        .get::<BrowserAttempt>("browser_attempt", &key)?
        .ok_or_else(Error::denied)?;
    if attempt.expires_at <= now() || attempt.environment != environment_header(&headers)? {
        return Err(Error::denied());
    }
    app.assert_generation(&attempt.environment, attempt.generation)?;
    if app.environment(&attempt.environment)?.control_revision != attempt.control_revision {
        return Err(Error::expired());
    }
    let session = login_inner(
        &app,
        &headers,
        LoginInput {
            slt: input.slt,
            identity_kind: Some(attempt.kind),
        },
    )
    .await?;
    let _environment_guard = app
        .lock(&format!("environment:{}", attempt.environment))
        .lock_owned()
        .await;
    app.assert_generation(&attempt.environment, attempt.generation)?;
    if app.environment(&attempt.environment)?.control_revision != attempt.control_revision {
        return Err(Error::expired());
    }
    app.store.delete("browser_attempt", &key)?;
    Ok(browser_session(&app, session))
}
pub async fn browser_refresh(State(app): State<App>, headers: HeaderMap) -> Result<Response> {
    browser_csrf(&app, &headers)?;
    let token = cookie(&headers, "mcport_refresh").ok_or_else(Error::expired)?;
    let session = refresh_inner(&app, &token, &environment_header(&headers)?).await?;
    Ok(browser_session(&app, session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::to_bytes, http::Uri};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::sync::Notify;

    #[derive(Default)]
    struct IamFixture {
        active: AtomicBool,
        requests: AtomicUsize,
        exchanges: AtomicUsize,
        block_refresh: AtomicBool,
        refresh_started: Notify,
        release_refresh: Notify,
    }
    fn introspection() -> Value {
        json!({"active":true,"public_id":"c:alice","actor_type":"carbon","client_id":"mcport","audience":"mcport","org_id":"tos","membership_id":"c:alice[tos]","expires_at":now()+3600,
            "authorization":{"actor_type":"carbon","public_id":"c:alice","organization_id":"00000000-0000-0000-0000-000000000001","org_id":"tos","membership_id":"c:alice[tos]","membership_version":1,"authorization_epoch":1,"audience":"mcport","testing_environment_id":null,"scopes":[],"org_role":null,"tags":null}})
    }
    async fn iam_request(
        State(fixture): State<Arc<IamFixture>>,
        uri: Uri,
        headers: HeaderMap,
        body: String,
    ) -> Json<Value> {
        fixture.requests.fetch_add(1, Ordering::SeqCst);
        assert!(
            headers
                .get(header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("Basic ")
        );
        let testing = headers.get("x-testing-environment-key").is_some();
        if uri.path().ends_with("/application/testing-context") {
            assert!(testing);
            Json(
                json!({"environment_id":TEST_ENV,"application":{"app_id":"mcport","base_url":"http://127.0.0.1:4380","app_scope":{"iam":["self.identity.read"],"external":[]},"webhook_scope":[],"testing_idle_days":30}}),
            )
        } else if uri.path().ends_with("/oauth/introspect") {
            let mut result = introspection();
            result["active"] = json!(fixture.active.load(Ordering::SeqCst));
            if testing {
                result["authorization"]["testing_environment_id"] = json!(TEST_ENV);
            }
            Json(result)
        } else if uri.path().ends_with("/app-auth/tokens") {
            fixture.exchanges.fetch_add(1, Ordering::SeqCst);
            if url::form_urlencoded::parse(body.as_bytes())
                .any(|(key, value)| key == "refresh_token" && !value.is_empty())
                && fixture.block_refresh.load(Ordering::SeqCst)
            {
                fixture.refresh_started.notify_one();
                fixture.release_refresh.notified().await;
            }
            Json(
                json!({"access_token":"iam-access","refresh_token":"iam-refresh","token_type":"Bearer","expires_in":3600,"scope":""}),
            )
        } else {
            panic!("Unexpected IAM fixture route {}", uri.path())
        }
    }
    async fn fixture() -> (
        App,
        tempfile::TempDir,
        Arc<IamFixture>,
        tokio::task::JoinHandle<()>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(IamFixture::default());
        state.active.store(true, Ordering::SeqCst);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = crate::state::Config::from_env();
        config.app_id = "mcport".into();
        config.app_secret = "fixture-app-secret".into();
        config.test_app_secrets = "{}".into();
        config.data_dir = directory.path().into();
        config.iam_url = format!("http://{}", listener.local_addr().unwrap());
        config.iam_web_url = "https://iam.example".into();
        config.web_url = "https://mcport.example".into();
        config.public_url = "https://api.mcport.example".into();
        let router = Router::new()
            .fallback(iam_request)
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (App::new(config).unwrap(), directory, state, task)
    }
    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }
    const TEST_ENV: &str = "11111111-1111-4111-8111-111111111111";
    fn issued_slt(label: &str) -> String {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        format!(
            "oac_{}",
            URL_SAFE_NO_PAD.encode(hex::decode(hash(label)).unwrap())
        )
    }
    fn provision_test_world(app: &App) {
        app.store
            .put(
                "environment",
                TEST_ENV,
                TEST_ENV,
                "tos",
                "",
                None,
                &Environment {
                    id: TEST_ENV.into(),
                    state: "active".into(),
                    generation: 1,
                    control_revision: 1,
                    app_secret: "fixture-test-secret".into(),
                    iam_key: Some("0123456789abcdefghijklmnopqrstuv".into()),
                },
                None,
            )
            .unwrap();
    }
    async fn sign_in(app: &App, slt: &str) -> Session {
        login_inner(
            app,
            &HeaderMap::new(),
            LoginInput {
                slt: issued_slt(slt),
                identity_kind: Some("carbon".into()),
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn public_login_rejects_actor_ids_before_iam_or_session_creation() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        let (app, _directory, iam, task) = fixture().await;
        provision_test_world(&app);
        for environment in [None, Some(TEST_ENV)] {
            for selector in [
                "c:alice",
                "si:worker",
                " c:alice",
                "c:alice[tos]",
                TEST_ENV,
                "oac_not-a-code",
            ] {
                let mut request = Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/json");
                if let Some(environment) = environment {
                    request = request.header("X-MCPort-Test", environment);
                }
                let response = crate::router(app.clone())
                    .oneshot(
                        request
                            .body(Body::from(json!({"slt":selector}).to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), 400);
                let body: Value =
                    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                        .unwrap();
                assert_eq!(body["error"]["code"], "invalid_login_token");
            }
        }
        assert_eq!(iam.requests.load(Ordering::SeqCst), 0);
        assert_eq!(iam.exchanges.load(Ordering::SeqCst), 0);
        assert!(
            app.store
                .list::<StoredSession>("session", None)
                .unwrap()
                .is_empty()
        );
        assert!(
            app.store
                .list::<LoginExchange>("login_exchange", None)
                .unwrap()
                .is_empty()
        );
        task.abort();
    }

    #[tokio::test]
    async fn issued_test_slt_still_uses_iam_and_creates_only_a_test_session() {
        let (app, _directory, iam, task) = fixture().await;
        provision_test_world(&app);
        let mut headers = HeaderMap::new();
        headers.insert("X-MCPort-Test", TEST_ENV.parse().unwrap());
        let session = login_inner(
            &app,
            &headers,
            LoginInput {
                slt: issued_slt("test-issued-code"),
                identity_kind: Some("carbon".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(session.environment, TEST_ENV);
        assert_eq!(session.actor.principal_id, "c:alice");
        assert_eq!(iam.exchanges.load(Ordering::SeqCst), 1);
        assert!(
            app.store
                .list::<StoredSession>("session", Some("production"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            app.store
                .list::<StoredSession>("session", Some(TEST_ENV))
                .unwrap()
                .len(),
            1
        );
        task.abort();
    }

    #[tokio::test]
    async fn logout_persistently_revokes_refresh_replays_and_slt_replays() {
        let (app, _directory, iam, task) = fixture().await;
        let initial = sign_in(&app, "single-use-code").await;
        let original = app
            .store
            .get::<StoredSession>("session", &hash(&initial.access_token))
            .unwrap()
            .unwrap();
        let next = refresh_inner(&app, &initial.refresh_token, "production")
            .await
            .unwrap();
        assert!(
            live(&app, &hash(&initial.access_token), "production")
                .await
                .is_err()
        );
        assert_eq!(
            authorize_family(&app, &original.family, "production")
                .await
                .unwrap()
                .actor()
                .principal_id,
            "c:alice"
        );
        assert_eq!(
            refresh_inner(&app, &initial.refresh_token, "production")
                .await
                .unwrap()
                .access_token,
            next.access_token
        );
        // Retry-cache hits must still consult current IAM authority.
        iam.active.store(false, Ordering::SeqCst);
        assert!(
            refresh_inner(&app, &initial.refresh_token, "production")
                .await
                .is_err()
        );
        iam.active.store(true, Ordering::SeqCst);
        logout(State(app.clone()), bearer_headers(&next.access_token))
            .await
            .unwrap();
        assert!(
            refresh_inner(&app, &initial.refresh_token, "production")
                .await
                .is_err()
        );
        assert!(
            refresh_inner(&app, &next.refresh_token, "production")
                .await
                .is_err()
        );
        assert!(
            login_inner(
                &app,
                &HeaderMap::new(),
                LoginInput {
                    slt: issued_slt("single-use-code"),
                    identity_kind: None
                }
            )
            .await
            .is_err()
        );
        let restarted = App::new((*app.config).clone()).unwrap();
        assert!(
            refresh_inner(&restarted, &next.refresh_token, "production")
                .await
                .is_err()
        );
        // Even a stale session row left after failed deletion has no authority.
        restarted
            .store
            .put(
                "session",
                &original.key,
                &original.environment,
                &original.actor.org_id,
                &original.actor.principal_id,
                None,
                &original,
                None,
            )
            .unwrap();
        assert!(live(&restarted, &original.key, "production").await.is_err());
        task.abort();
    }

    #[tokio::test]
    async fn refresh_in_flight_cannot_recreate_a_logged_out_family() {
        let (app, _directory, iam, task) = fixture().await;
        let initial = sign_in(&app, "concurrent-code").await;
        let mut stored = app
            .store
            .get::<StoredSession>("session", &hash(&initial.access_token))
            .unwrap()
            .unwrap();
        stored.iam_expires = 0;
        save_session(&app, &stored, false).unwrap();
        iam.block_refresh.store(true, Ordering::SeqCst);
        let refresh_app = app.clone();
        let token = initial.refresh_token.clone();
        let refresh_task =
            tokio::spawn(async move { refresh_inner(&refresh_app, &token, "production").await });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            iam.refresh_started.notified(),
        )
        .await
        .unwrap();
        let logout_app = app.clone();
        let headers = bearer_headers(&initial.access_token);
        let mut logout_task = tokio::spawn(async move { logout(State(logout_app), headers).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut logout_task)
                .await
                .is_err()
        );
        iam.release_refresh.notify_one();
        let next = refresh_task.await.unwrap().unwrap();
        logout_task.await.unwrap().unwrap();
        assert!(
            app.store
                .list::<StoredSession>("session", Some("production"))
                .unwrap()
                .is_empty()
        );
        assert!(
            refresh_inner(&app, &next.refresh_token, "production")
                .await
                .is_err()
        );
        assert!(
            authorize_family(&app, &stored.family, "production")
                .await
                .is_err()
        );
        task.abort();
    }

    #[tokio::test]
    async fn browser_callback_state_is_preserved_and_consumed_once_without_js_tokens() {
        let (app, _directory, iam, task) = fixture().await;
        let response = browser_start(
            State(app.clone()),
            HeaderMap::new(),
            Query(BrowserStart {
                identity_kind: "carbon".into(),
            }),
        )
        .await
        .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        let state = body["data"]["state"].as_str().unwrap().to_owned();
        let url = url::Url::parse(body["data"]["url"].as_str().unwrap()).unwrap();
        let callback = url
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .unwrap()
            .1
            .into_owned();
        let callback = url::Url::parse(&callback).unwrap();
        assert_eq!(callback.path(), "/auth/callback");
        assert_eq!(
            callback
                .query_pairs()
                .find(|(key, _)| key == "state")
                .unwrap()
                .1,
            state
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, app.config.web_url.parse().unwrap());
        headers.insert(
            header::COOKIE,
            format!("mcport_login_state={state}").parse().unwrap(),
        );
        let (first, second) = tokio::join!(
            browser_complete(
                State(app.clone()),
                headers.clone(),
                Json(BrowserComplete {
                    state: state.clone(),
                    slt: issued_slt("browser-code")
                })
            ),
            browser_complete(
                State(app.clone()),
                headers.clone(),
                Json(BrowserComplete {
                    state,
                    slt: issued_slt("browser-code")
                })
            )
        );
        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(iam.exchanges.load(Ordering::SeqCst), 1);
        let response = first.or(second).unwrap();
        let cookies: Vec<String> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|header| header.to_str().unwrap().into())
            .collect();
        assert_eq!(cookies.len(), 3);
        assert!(
            cookies
                .iter()
                .all(|cookie| cookie.contains("HttpOnly") && cookie.contains("Secure"))
        );
        let refresh_cookie = cookies
            .iter()
            .find(|cookie| cookie.starts_with("mcport_refresh="))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert!(body["data"].get("access_token").is_none());
        assert!(body["data"].get("refresh_token").is_none());
        headers.insert(header::COOKIE, refresh_cookie.parse().unwrap());
        let refreshed = browser_refresh(State(app.clone()), headers.clone())
            .await
            .unwrap();
        let logout_cookie = refreshed
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .find_map(|value| {
                value
                    .to_str()
                    .unwrap()
                    .strip_prefix("mcport_refresh=")
                    .map(|value| format!("mcport_refresh={}", value.split(';').next().unwrap()))
            })
            .unwrap();
        let mut logout_headers = headers.clone();
        logout_headers.insert(header::COOKIE, logout_cookie.parse().unwrap());
        iam.active.store(false, Ordering::SeqCst);
        let logout_response = logout(State(app.clone()), logout_headers).await.unwrap();
        assert_eq!(
            logout_response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .count(),
            3
        );
        headers.remove(header::ORIGIN);
        headers.insert(header::AUTHORIZATION, "Bearer ignored".parse().unwrap());
        assert!(browser_refresh(State(app.clone()), headers).await.is_err());
        task.abort();
    }

    #[tokio::test]
    async fn introspection_rejects_conflicting_identity_and_tenant_bindings() {
        let (app, _directory, _iam, task) = fixture().await;
        let env = app.environment("production").unwrap();
        for (pointer, value) in [
            ("/authorization/actor_type", json!("silicon")),
            ("/authorization/public_id", json!("c:bob")),
            ("/org_id", json!("elsewhere")),
            ("/membership_id", json!("wrong")),
            ("/audience", json!("other-app")),
            ("/expires_at", json!(now() - 1)),
        ] {
            let mut token = introspection();
            *token.pointer_mut(pointer).unwrap() = value;
            assert!(
                inspected_actor("mcport", &env, serde_json::from_value(token).unwrap(), None)
                    .is_err(),
                "accepted {pointer}"
            );
        }
        let actor = inspected_actor(
            "mcport",
            &env,
            serde_json::from_value(introspection()).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(actor.org_id, "tos");
        task.abort();
    }
}
