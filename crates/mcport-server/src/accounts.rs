//! Silicon Accounts: access-token verification, the cached account rows MCPort
//! keeps (kind, current id, custodian, revocation), and the custodian circle.
//!
//! The account `uuid` (JWT `sub`) is permanent and is what every record stores.
//! The public id (`c:…`/`si:…`) can change and is only displayed.
use crate::{
    error::{Error, Result},
    state::{App, hash, now},
};
use mcport_core::AccountRef;
use silicon_accounts_client::{
    AccountSummary, AccountsClient, AppClient, Claims, Jwks, TokenError, VerifyOptions,
    verify_access_token,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

/// An unknown key id triggers a JWKS refetch at most this often.
const JWKS_REFETCH_INTERVAL: Duration = Duration::from_secs(30);
/// Cached signing keys are refetched after this long (kept if the refetch fails).
const JWKS_MAX_AGE: Duration = Duration::from_secs(3600);
/// A positive introspection is reused for at most this long.
const INTROSPECTION_TTL: Duration = Duration::from_secs(30);
/// A `c:`/`si:` id resolved through Accounts is reused for this long.
const RESOLVE_TTL: Duration = Duration::from_secs(60);
/// Account rows of active accounts are refreshed from Accounts this often.
pub const ACCOUNT_MAX_AGE: i64 = 3600;
/// Custodian data that grants someone access is re-checked after this long.
pub const CUSTODIAN_MAX_AGE: i64 = 600;
/// Optional lookups stop above this many per minute (Accounts allows 600 per app).
const OPTIONAL_LOOKUPS_PER_MINUTE: usize = 450;
/// Ids one account may have resolved per minute (each can cost an Accounts lookup).
const RESOLUTIONS_PER_ACCOUNT_PER_MINUTE: usize = 30;
/// How long a lookup may take before MCPort continues with what it knows.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

pub const SIGN_IN_AGAIN: &str = "Sign in again: Carbons run mcport login (the website signs in through Silicon Accounts); Silicons run silicon-accounts login --app mcport -q | mcport login --slt-stdin.";

/// What MCPort knows about one Carbon or Silicon.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountRow {
    pub uuid: String,
    /// `carbon` or `silicon` (empty only for rows created from a webhook about an
    /// account MCPort never saw).
    pub kind: String,
    pub id: String,
    pub display_name: String,
    pub pfp_url: Option<String>,
    /// `active`, `access_removed` (the account removed MCPort's access) or `deleted`.
    pub status: String,
    pub custodian_uuid: Option<String>,
    pub custodian_id: Option<String>,
    pub version: i64,
    /// Access tokens issued before this time (Unix seconds) are refused.
    pub revoked_before: i64,
    /// Accounts-side time (ms) of the newest change applied; older events are ignored.
    pub synced_at_ms: i64,
    /// When MCPort last confirmed this row with Accounts (lookup or webhook).
    pub looked_up_at: i64,
    pub last_fid: Option<String>,
    pub updated_at: i64,
}
impl AccountRow {
    pub fn new(uuid: &str, kind: &str) -> Self {
        Self {
            uuid: uuid.into(),
            kind: kind.into(),
            status: "active".into(),
            updated_at: now(),
            ..Default::default()
        }
    }
    pub fn reference(&self) -> AccountRef {
        AccountRef {
            uuid: self.uuid.clone(),
            id: self.id.clone(),
            kind: self.kind.clone(),
            display_name: self.display_name.clone(),
            pfp_url: self.pfp_url.clone(),
        }
    }
    pub fn is_silicon(&self) -> bool {
        self.kind == "silicon"
    }
    pub fn active(&self) -> bool {
        self.status == "active"
    }
    /// The Carbon at the root of this account's circle: a Carbon itself, or a
    /// Silicon's custodian. `None` for a Silicon whose custodian is unknown.
    pub fn household(&self) -> Option<&str> {
        match self.kind.as_str() {
            "carbon" => Some(&self.uuid),
            "silicon" => self.custodian_uuid.as_deref(),
            _ => None,
        }
    }
    /// The name to show: the current id, or the uuid while the id is unknown.
    pub fn label(&self) -> &str {
        if self.id.is_empty() {
            &self.uuid
        } else {
            &self.id
        }
    }
    /// Apply an Accounts lookup observed at `at_ms`.
    pub fn apply_summary(&mut self, summary: &AccountSummary, at_ms: i64) {
        self.kind = summary.kind.as_str().into();
        if !summary.id.is_empty() {
            self.id.clone_from(&summary.id);
        }
        if !summary.display_name.is_empty() {
            self.display_name.clone_from(&summary.display_name);
        }
        self.pfp_url = Some(summary.pfp_url.clone()).filter(|url| !url.is_empty());
        match &summary.custodian {
            Some(custodian) if self.is_silicon() => {
                self.custodian_uuid = Some(custodian.uuid.clone());
                self.custodian_id = Some(custodian.id.clone()).filter(|id| !id.is_empty());
            }
            _ if !self.is_silicon() => {
                self.custodian_uuid = None;
                self.custodian_id = None;
            }
            _ => {}
        }
        if summary.status == "deleted" {
            self.status = "deleted".into();
        }
        self.synced_at_ms = self.synced_at_ms.max(at_ms);
        self.looked_up_at = at_ms / 1000;
        self.updated_at = now();
    }
}

struct CachedJwks {
    keys: Jwks,
    fetched: Instant,
}

/// The Silicon Accounts client and MCPort's caches of its answers.
pub struct Accounts {
    pub client: AccountsClient,
    pub app_id: String,
    app_secret: String,
    /// The Accounts public URL: the `iss` of every access token.
    pub issuer: String,
    jwks: tokio::sync::RwLock<Option<CachedJwks>>,
    jwks_fetch: tokio::sync::Mutex<Option<Instant>>,
    introspected: Mutex<HashMap<String, Instant>>,
    resolved: Mutex<HashMap<String, (AccountSummary, Instant)>>,
    lookups: Mutex<VecDeque<Instant>>,
    resolutions: Mutex<HashMap<String, VecDeque<Instant>>>,
}
impl Accounts {
    pub fn new(client: AccountsClient, app_id: &str, app_secret: &str, issuer: &str) -> Self {
        Self {
            client,
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            issuer: issuer.into(),
            jwks: tokio::sync::RwLock::new(None),
            jwks_fetch: tokio::sync::Mutex::new(None),
            introspected: Mutex::new(HashMap::new()),
            resolved: Mutex::new(HashMap::new()),
            lookups: Mutex::new(VecDeque::new()),
            resolutions: Mutex::new(HashMap::new()),
        }
    }
    pub fn app(&self) -> AppClient<'_> {
        self.client.as_app(&self.app_id, &self.app_secret)
    }
    /// Verify an access token locally: EdDSA signature by a published key,
    /// `aud` = MCPort's app id, `iss` = the Accounts public URL, `exp`/`nbf`.
    pub async fn verify(&self, token: &str) -> Result<Claims> {
        let options = VerifyOptions::for_app(&self.app_id).with_issuer(&self.issuer);
        let keys = self.keys(false).await?;
        match verify_access_token(&keys, token, &options) {
            Err(silicon_accounts_client::Error::Token(TokenError::UnknownKey { .. })) => {
                let keys = self.keys(true).await?;
                verify_access_token(&keys, token, &options).map_err(token_error)
            }
            other => other.map_err(token_error),
        }
    }
    async fn keys(&self, unknown_kid: bool) -> Result<Jwks> {
        if let Some(cached) = &*self.jwks.read().await
            && !unknown_kid
            && cached.fetched.elapsed() < JWKS_MAX_AGE
        {
            return Ok(cached.keys.clone());
        }
        // Single flight. An unknown key id refetches at most every
        // JWKS_REFETCH_INTERVAL, so a stream of made-up kids cannot flood Accounts;
        // the first fetch and age-based refreshes do not count against it.
        let mut last_refetch = self.jwks_fetch.lock().await;
        if let Some(cached) = &*self.jwks.read().await {
            if unknown_kid && last_refetch.is_some_and(|at| at.elapsed() < JWKS_REFETCH_INTERVAL) {
                return Ok(cached.keys.clone());
            }
            if !unknown_kid && cached.fetched.elapsed() < JWKS_MAX_AGE {
                return Ok(cached.keys.clone());
            }
        }
        if unknown_kid {
            *last_refetch = Some(Instant::now());
        }
        match self.client.jwks().await {
            Ok(keys) => {
                *self.jwks.write().await = Some(CachedJwks {
                    keys: keys.clone(),
                    fetched: Instant::now(),
                });
                Ok(keys)
            }
            Err(error) => {
                tracing::warn!(code = %error.code(), "Silicon Accounts signing keys could not be fetched");
                match &*self.jwks.read().await {
                    Some(cached) => Ok(cached.keys.clone()),
                    None => Err(unavailable(
                        "MCPort could not fetch Silicon Accounts' signing keys, so it cannot check access tokens.",
                    )),
                }
            }
        }
    }

    /// Ask Accounts whether this sign-in is still active (sees revocation at once).
    /// Positive answers are reused for up to 30 seconds per token.
    pub async fn ensure_active(&self, token: &str, uuid: &str) -> Result<()> {
        let key = hash(token);
        if self
            .introspected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .is_some_and(|at| at.elapsed() < INTROSPECTION_TTL)
        {
            return Ok(());
        }
        // An inactive token is a normal `{active:false}` answer. Any error means MCPort
        // could not ask (network, or MCPort's own app credentials were refused).
        let result = self.app().introspect(token).await.map_err(|error| {
            tracing::warn!(code = %error.code(), "Silicon Accounts introspection failed");
            unavailable(
                "MCPort could not confirm with Silicon Accounts that this sign-in is still active.",
            )
        })?;
        if !result.active || result.sub.as_deref() != Some(uuid) {
            return Err(Error::new(
                401,
                "sign_in_revoked",
                "This sign-in was signed out or its access to MCPort was removed in Silicon Accounts.",
                SIGN_IN_AGAIN,
            ));
        }
        let mut cache = self.introspected.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 4096 {
            cache.retain(|_, at| at.elapsed() < INTROSPECTION_TTL);
        }
        cache.insert(key, Instant::now());
        Ok(())
    }
    /// Forget cached introspections (after a revocation webhook).
    pub fn forget_introspections(&self) {
        self.introspected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn take_lookup_budget(&self, required: bool) -> bool {
        let mut recent = self.lookups.lock().unwrap_or_else(|e| e.into_inner());
        while recent
            .front()
            .is_some_and(|at| at.elapsed() > Duration::from_secs(60))
        {
            recent.pop_front();
        }
        if !required && recent.len() >= OPTIONAL_LOOKUPS_PER_MINUTE {
            return false;
        }
        recent.push_back(Instant::now());
        true
    }
    /// `GET /v1/accounts/{uuid}`. `None` when the optional-lookup budget is spent.
    pub async fn lookup(&self, uuid: &str, required: bool) -> Result<Option<AccountSummary>> {
        if !self.take_lookup_budget(required) {
            return Ok(None);
        }
        match tokio::time::timeout(LOOKUP_TIMEOUT, self.app().lookup(uuid)).await {
            Ok(Ok(summary)) => Ok(Some(summary)),
            Ok(Err(error)) if error.is_not_found() => Err(unknown_account(uuid)),
            Ok(Err(error)) => {
                tracing::warn!(code = %error.code(), "Silicon Accounts account lookup failed");
                Err(unavailable(
                    "MCPort could not look up this account in Silicon Accounts.",
                ))
            }
            Err(_) => Err(unavailable(
                "Silicon Accounts did not answer an account lookup in time.",
            )),
        }
    }
    /// Count one id resolution for `caller`; refuse beyond the per-minute limit so
    /// one account cannot spend MCPort's Accounts lookup allowance for everyone.
    fn take_resolution(&self, caller: &str) -> Result<()> {
        let mut all = self.resolutions.lock().unwrap_or_else(|e| e.into_inner());
        if all.len() > 4096 {
            all.retain(|_, times| {
                times
                    .back()
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
            });
        }
        let times = all.entry(caller.to_owned()).or_default();
        while times
            .front()
            .is_some_and(|at| at.elapsed() > Duration::from_secs(60))
        {
            times.pop_front();
        }
        if times.len() >= RESOLUTIONS_PER_ACCOUNT_PER_MINUTE {
            return Err(Error::new(
                429,
                "too_many_lookups",
                "This account named too many accounts in the last minute.",
                "Wait a minute, then try again.",
            ));
        }
        times.push_back(Instant::now());
        Ok(())
    }
    /// Resolve a `c:`/`si:` id (current ids only) or a uuid through Accounts.
    pub async fn resolve(&self, caller: &str, input: &str) -> Result<AccountSummary> {
        let input = input.trim();
        if input.is_empty()
            || input.len() > 100
            || input
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '/')
        {
            return Err(Error::bad(
                "Name an account by its c: (Carbon) or si: (Silicon) id, or by its uuid.",
            ));
        }
        let by_id = silicon_accounts_client::AccountKind::of_id(input).is_some();
        let key = if by_id {
            input.to_ascii_lowercase()
        } else {
            input.to_owned()
        };
        if let Some((summary, at)) = self
            .resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            && at.elapsed() < RESOLVE_TTL
        {
            return Ok(summary.clone());
        }
        self.take_resolution(caller)?;
        let summary = if by_id {
            self.take_lookup_budget(true);
            match tokio::time::timeout(LOOKUP_TIMEOUT, self.app().lookup_by_id(input)).await {
                Ok(Ok(summary)) => summary,
                Ok(Err(error)) if error.is_not_found() => return Err(unknown_account(input)),
                Ok(Err(error)) if error.status() == Some(400) || error.status() == Some(422) => {
                    return Err(unknown_account(input));
                }
                Ok(Err(error)) => {
                    tracing::warn!(code = %error.code(), "Silicon Accounts id lookup failed");
                    return Err(unavailable(
                        "MCPort could not look up this id in Silicon Accounts.",
                    ));
                }
                Err(_) => {
                    return Err(unavailable(
                        "Silicon Accounts did not answer an id lookup in time.",
                    ));
                }
            }
        } else {
            self.lookup(input, true)
                .await?
                .ok_or_else(|| unknown_account(input))?
        };
        if summary.status == "deleted" || summary.uuid.is_empty() {
            return Err(unknown_account(input));
        }
        let mut cache = self.resolved.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 4096 {
            cache.retain(|_, (_, at)| at.elapsed() < RESOLVE_TTL);
        }
        cache.insert(key, (summary.clone(), Instant::now()));
        Ok(summary)
    }
    /// Drop a cached id resolution (its id changed).
    pub fn forget_resolution(&self, id: &str) {
        self.resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|key, (summary, _)| key != &id.to_ascii_lowercase() && summary.id != id);
    }
}

pub fn unavailable(message: &str) -> Error {
    Error::new(
        503,
        "accounts_unavailable",
        message,
        "Retry shortly. If it persists, check that MCPort can reach Silicon Accounts (ACCOUNTS_API_URL).",
    )
}
pub fn unknown_account(input: &str) -> Error {
    Error::new(
        404,
        "unknown_account",
        format!("No Carbon or Silicon has the id `{input}`."),
        "Check the id: Carbon ids start with c: and Silicon ids with si:. Ids can change, so use the current one (the account's uuid never changes).",
    )
}
fn token_error(error: silicon_accounts_client::Error) -> Error {
    let silicon_accounts_client::Error::Token(token) = &error else {
        return unavailable("MCPort could not check this access token with Silicon Accounts.");
    };
    let (code, message) = match token {
        TokenError::Expired { .. } => (
            "token_expired",
            "The Silicon Accounts access token has expired.".to_owned(),
        ),
        TokenError::WrongAudience { .. } => (
            "wrong_audience",
            "This access token was issued to another app, not to MCPort.".to_owned(),
        ),
        TokenError::WrongIssuer { .. } => (
            "wrong_issuer",
            "This access token was not issued by the Silicon Accounts service MCPort trusts."
                .to_owned(),
        ),
        TokenError::UnknownKey { .. } => (
            "unknown_signing_key",
            "This access token is signed with a key Silicon Accounts does not publish.".to_owned(),
        ),
        TokenError::NotYetValid => (
            "token_not_yet_valid",
            "This access token is not valid yet; check this machine's clock.".to_owned(),
        ),
        other => (
            "invalid_token",
            format!(
                "The access token is not a valid Silicon Accounts token: {}",
                other.message()
            ),
        ),
    };
    Error::new(401, code, message, SIGN_IN_AGAIN)
}

/// Apply a fresh Accounts lookup of `uuid` to its row (creating it).
pub async fn refresh(app: &App, uuid: &str, required: bool) -> Result<Option<AccountRow>> {
    let lock = app.lock(&format!("account-lookup:{uuid}"));
    let _guard = lock.lock().await;
    if let Some(row) = app.store.account(uuid)?
        && now() - row.looked_up_at < 5
    {
        return Ok(Some(row));
    }
    let Some(summary) = app.accounts.lookup(uuid, required).await? else {
        return Ok(None);
    };
    let at_ms = now() * 1000;
    let row = app.store.update_account(uuid, |row| {
        let mut row = row.unwrap_or_else(|| AccountRow::new(uuid, summary.kind.as_str()));
        row.apply_summary(&summary, at_ms);
        Some(row)
    })?;
    Ok(row)
}
/// The row for `uuid`, refreshed when older than `max_age` seconds. Lookup
/// failures keep the cached row (webhooks are the primary update path).
pub async fn fresh(app: &App, row: AccountRow, max_age: i64) -> AccountRow {
    if now() - row.looked_up_at <= max_age {
        return row;
    }
    match refresh(app, &row.uuid, false).await {
        Ok(Some(fresh)) => fresh,
        Ok(None) => row,
        Err(error) => {
            tracing::warn!(code = %error.1.code, "Keeping cached account data after a failed refresh");
            row
        }
    }
}
/// Resolve a `c:`/`si:` id or uuid that `caller` supplied to a stored account row.
pub async fn resolve(app: &App, caller: &str, input: &str) -> Result<AccountRow> {
    let summary = app.accounts.resolve(caller, input).await?;
    let at_ms = now() * 1000;
    app.store
        .update_account(&summary.uuid, |row| {
            let mut row =
                row.unwrap_or_else(|| AccountRow::new(&summary.uuid, summary.kind.as_str()));
            row.apply_summary(&summary, at_ms);
            Some(row)
        })?
        .ok_or_else(Error::internal)
}

/// Whether `caller` is the custodian of the Silicon `owner`. Custodian data that
/// grants access is confirmed with Accounts when older than CUSTODIAN_MAX_AGE.
pub async fn looks_after(app: &App, caller: &AccountRow, owner: &AccountRow) -> bool {
    if !owner.is_silicon() || owner.custodian_uuid.as_deref() != Some(caller.uuid.as_str()) {
        return false;
    }
    let owner = fresh(app, owner.clone(), CUSTODIAN_MAX_AGE).await;
    owner.custodian_uuid.as_deref() == Some(caller.uuid.as_str())
}
/// Whether two accounts share a circle: a Carbon and the Silicons it looks after.
pub async fn same_circle(app: &App, a: &AccountRow, b: &AccountRow) -> bool {
    if a.uuid == b.uuid {
        return true;
    }
    if a.household().is_none() || a.household() != b.household() {
        return false;
    }
    // A shared household that rests on custodian data is confirmed when stale.
    let a = if a.is_silicon() {
        fresh(app, a.clone(), CUSTODIAN_MAX_AGE).await
    } else {
        a.clone()
    };
    let b = if b.is_silicon() {
        fresh(app, b.clone(), CUSTODIAN_MAX_AGE).await
    } else {
        b.clone()
    };
    a.household().is_some() && a.household() == b.household()
}
/// The display reference for a stored uuid (uuid only when MCPort never saw it).
pub fn reference(app: &App, uuid: &str) -> AccountRef {
    match app.store.account(uuid) {
        Ok(Some(row)) => row.reference(),
        _ => AccountRef {
            uuid: uuid.into(),
            ..Default::default()
        },
    }
}
