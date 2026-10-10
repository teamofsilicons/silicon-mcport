//! Sign in to MCPort with Silicon Accounts, as MCPort's public client (no secret).
//!
//! - **Carbons** use the device flow: [`SignIn::start_device`] returns a code to show,
//!   the Carbon approves it at the verification page on any device, and
//!   [`SignIn::wait_for_device`] polls (honouring `interval` and `slow_down`) until the
//!   code is approved, denied or expires (10 minutes).
//! - **Silicons** never see a page: they mint a short-lived token with
//!   `silicon-accounts login --app mcport -q` and hand it over; [`SignIn::exchange_slt`]
//!   exchanges it (single use, 2 minutes, bound to MCPort).
//! - [`SignIn::refresh`] rotates the refresh token. A used refresh token ends the whole
//!   sign-in, so refresh one at a time and store the new pair before using it (the
//!   `session` feature's [`crate::session::SessionFile`] does both).
//! - [`SignIn::revoke`] signs this sign-in out at Silicon Accounts.
//!
//! [`Tokens::access_token`] is what [`crate::RequestContext::authenticated`] sends to
//! MCPort. Nothing here reads files or the environment, and no token is ever logged:
//! token fields are [`Secret`]s whose `Debug` output hides the value.
//!
//! ```no_run
//! # async fn example() -> Result<(), mcport_client::accounts::SignInError> {
//! use mcport_client::accounts::{DEFAULT_ACCOUNTS_URL, SignIn, APP_ID};
//! let sign_in = SignIn::new(DEFAULT_ACCOUNTS_URL, APP_ID)?;
//! let device = sign_in.start_device(Some("mcport CLI on build-box")).await?;
//! println!("Open {} and enter {}", device.verification_uri, device.user_code);
//! let tokens = sign_in.wait_for_device(&device, |_| {}).await?;
//! println!("Signed in as {}", tokens.account.id);
//! # Ok(()) }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use silicon_accounts_client::Secret;
use silicon_accounts_client::{AccountsClient, DevicePoll, SLT_GRANT_TYPE, TokenResponse};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

/// Production Silicon Accounts.
pub const DEFAULT_ACCOUNTS_URL: &str = "https://accounts.teamofsilicons.com";
/// MCPort's app id at Silicon Accounts; also its public `client_id`.
pub const APP_ID: &str = "mcport";
/// How a Silicon gets a short-lived token for MCPort.
pub const SLT_MINT_COMMAND: &str = "silicon-accounts login --app mcport -q";
/// The whole Silicon sign-in, for messages.
pub const SILICON_SIGN_IN: &str =
    "silicon-accounts login --app mcport -q | mcport login --slt-stdin";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// The signed-in Carbon or Silicon, as Silicon Accounts shares it with MCPort.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedInAccount {
    /// Permanent Silicon Accounts uuid (short, case-sensitive). Key on this.
    pub uuid: String,
    /// Current public id (`c:ada`, `si:scout`); it can change.
    pub id: String,
    /// `carbon` or `silicon`.
    pub kind: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp_url: Option<String>,
    /// A Silicon's custodian (the Carbon who looks after it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custodian: Option<CustodianRef>,
}

/// A Silicon's custodian.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodianRef {
    pub uuid: String,
    #[serde(default)]
    pub id: String,
}

/// A successful sign-in or refresh: MCPort's tokens for one account.
#[derive(Clone, Debug)]
pub struct Tokens {
    /// Silicon Accounts access token for `mcport` (EdDSA JWT, 30 minutes).
    pub access_token: Secret,
    /// Rotating refresh token (`sar_…`). Every refresh returns a new one.
    pub refresh_token: Secret,
    /// When the access token expires (Unix seconds, from this machine's clock).
    pub expires_at: i64,
    /// When the sign-in itself ends (Unix seconds), if it has an end.
    pub refresh_expires_at: Option<i64>,
    /// Granted scopes, space-separated (`profile` at least).
    pub scope: String,
    pub account: SignedInAccount,
}

impl Tokens {
    fn from_response(response: TokenResponse, received_at: i64) -> Result<Self, SignInError> {
        let unexpected = |what: &str| SignInError::Accounts {
            status: None,
            code: "unexpected_response".into(),
            message: format!("Silicon Accounts answered the sign-in without {what}."),
            hint: Some("Retry; if it keeps happening, report it with mcport report.".into()),
        };
        let refresh_token = response
            .refresh_token
            .clone()
            .ok_or_else(|| unexpected("a refresh token"))?;
        let account = response
            .account
            .clone()
            .ok_or_else(|| unexpected("the account"))?;
        Ok(Self {
            access_token: response.access_token.clone(),
            refresh_token,
            expires_at: received_at
                .saturating_add(i64::try_from(response.expires_in).unwrap_or(i64::MAX)),
            refresh_expires_at: response
                .refresh_token_expires_at
                .map(|at| at.unix_timestamp()),
            scope: response.scope.clone().unwrap_or_default(),
            account: SignedInAccount {
                uuid: account.uuid.clone(),
                id: account.id.clone(),
                kind: account.kind.as_str().into(),
                display_name: account.display_name.clone(),
                pfp_url: Some(account.pfp_url.clone()).filter(|url| !url.is_empty()),
                custodian: account.custodian.as_ref().map(|custodian| CustodianRef {
                    uuid: custodian.uuid.clone(),
                    id: custodian.id.clone(),
                }),
            },
        })
    }
}

/// A device sign-in in progress. Show [`DeviceStart::user_code`] and
/// [`DeviceStart::verification_uri`]; never show the device code.
#[derive(Clone)]
pub struct DeviceStart {
    /// The code the Carbon confirms, e.g. `WDJB-MJHT`.
    pub user_code: String,
    /// Where to approve, e.g. `https://accounts.teamofsilicons.com/device`.
    pub verification_uri: String,
    /// The same page with the code filled in.
    pub verification_uri_complete: Option<String>,
    /// Seconds the code stays valid (600).
    pub expires_in: u64,
    /// Seconds between polls (5).
    pub interval: u64,
    /// When the code stops working (Unix seconds).
    pub expires_at: i64,
    device_code: Secret,
    started: tokio::time::Instant,
}
impl std::fmt::Debug for DeviceStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceStart")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}
impl DeviceStart {
    /// The page to open in a browser: the one with the code filled in when available.
    pub fn browser_url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// What [`SignIn::wait_for_device`] reports after each poll that did not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceProgress {
    /// Not approved yet.
    Pending,
    /// Silicon Accounts asked to poll more slowly; the new interval in seconds.
    SlowDown { interval: u64 },
    /// A poll failed for a passing reason (network, 5xx, rate limit); polling continues.
    Retrying { message: String },
}

/// Why Silicon Accounts refused a short-lived token. Every refusal uses the token up.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SltRefusal {
    AlreadyUsed,
    Expired,
    /// Minted for another app (named when Silicon Accounts said which).
    WrongApp {
        app: Option<String>,
    },
    /// Mistyped, never issued, or issued by another Silicon Accounts deployment.
    Unknown,
    /// Not a short-lived token at all (for example a refresh token).
    NotAShortLivedToken,
    /// The Silicon's sign-in that minted it ended (STK rotated, CI sign-in over, trust removed).
    SignInEnded,
    Other,
}
impl SltRefusal {
    fn classify(description: &str) -> Self {
        let text = description.to_ascii_lowercase();
        if text.contains("already used") {
            Self::AlreadyUsed
        } else if text.contains("expired at") || text.contains("short-lived token expired") {
            Self::Expired
        } else if text.contains("was issued for the app") {
            let app = description
                .split('\'')
                .nth(1)
                .filter(|app| !app.is_empty())
                .map(str::to_owned);
            Self::WrongApp { app }
        } else if text.contains("is not known") {
            Self::Unknown
        } else if text.contains("must be a short-lived token") || text.contains("refresh token") {
            Self::NotAShortLivedToken
        } else if text.contains("rotated its stk")
            || text.contains("trusted outside token")
            || text.contains("removed that trust")
        {
            Self::SignInEnded
        } else {
            Self::Other
        }
    }
    fn code(&self) -> &'static str {
        match self {
            Self::AlreadyUsed => "slt_already_used",
            Self::Expired => "slt_expired",
            Self::WrongApp { .. } => "slt_wrong_app",
            Self::Unknown => "slt_unknown",
            Self::NotAShortLivedToken => "slt_not_a_short_lived_token",
            Self::SignInEnded => "slt_sign_in_ended",
            Self::Other => "slt_refused",
        }
    }
}

/// Everything that can go wrong signing in. Each error says what failed and why
/// ([`SignInError::message`]) and what to do next ([`SignInError::hint`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SignInError {
    /// A setting is wrong before anything was sent (URL, empty token…).
    #[error("{message}")]
    Configuration { message: String, hint: String },
    /// Silicon Accounts refused a short-lived token; it is used up either way.
    #[error("{description}")]
    SltRefused {
        reason: SltRefusal,
        description: String,
    },
    /// The refresh token no longer works: this sign-in ended (signed out elsewhere,
    /// MCPort's access removed, STK rotated, refresh token presented twice, or expired).
    #[error("{description}")]
    SignInEnded { description: String },
    /// The Carbon denied the device sign-in.
    #[error("The sign-in request {user_code} was denied on the account site.")]
    DeviceDenied { user_code: String },
    /// Nobody approved the code in time.
    #[error("The code {user_code} expired before anyone approved it.")]
    DeviceExpired { user_code: String },
    /// MCPort's sign-in setup at this Silicon Accounts does not allow this grant.
    #[error("{description}")]
    NotEnabled { grant: String, description: String },
    /// Any other answer from Silicon Accounts, with its code.
    #[error("{message}")]
    Accounts {
        status: Option<u16>,
        code: String,
        message: String,
        hint: Option<String>,
    },
    /// No answer: connection refused, DNS, TLS or timeout.
    #[error("{message}")]
    Transport { message: String },
}

impl SignInError {
    /// Stable machine-readable code.
    pub fn code(&self) -> String {
        match self {
            Self::Configuration { .. } => "sign_in_configuration".into(),
            Self::SltRefused { reason, .. } => reason.code().into(),
            Self::SignInEnded { .. } => "sign_in_ended".into(),
            Self::DeviceDenied { .. } => "device_denied".into(),
            Self::DeviceExpired { .. } => "device_code_expired".into(),
            Self::NotEnabled { .. } => "sign_in_not_enabled".into(),
            Self::Accounts { code, .. } => code.clone(),
            Self::Transport { .. } => "accounts_unreachable".into(),
        }
    }
    /// What failed and why.
    pub fn message(&self) -> String {
        match self {
            Self::SltRefused {
                reason: SltRefusal::Unknown,
                description,
            } => format!(
                "{description} (Short-lived tokens are only known to the Silicon Accounts that minted them.)"
            ),
            Self::SignInEnded { description } => {
                format!("Your MCPort sign-in ended: {description}")
            }
            _ => self.to_string(),
        }
    }
    /// What to do next.
    pub fn hint(&self) -> String {
        let fresh = format!("Mint a fresh one and use it at once: {SILICON_SIGN_IN}");
        match self {
            Self::Configuration { hint, .. } => hint.clone(),
            Self::SltRefused { reason, .. } => match reason {
                SltRefusal::AlreadyUsed => format!("Short-lived tokens work once. {fresh}"),
                SltRefusal::Expired => format!("Short-lived tokens last 2 minutes. {fresh}"),
                SltRefusal::WrongApp { .. } => format!(
                    "Mint one for MCPort (--app mcport): {SILICON_SIGN_IN}"
                ),
                SltRefusal::Unknown => format!(
                    "Check that the whole token was passed and that this CLI signs in at the Silicon Accounts that minted it (ACCOUNTS_URL, or mcport config set accounts <url>). {fresh}"
                ),
                SltRefusal::NotAShortLivedToken => format!(
                    "Pass a short-lived token (slt_…), not a refresh or access token. {fresh}"
                ),
                SltRefusal::SignInEnded => format!(
                    "Sign the Silicon in to Silicon Accounts again (silicon-accounts login --silicon si:<id> …), then mint a fresh token: {SILICON_SIGN_IN}"
                ),
                SltRefusal::Other => fresh,
            },
            Self::SignInEnded { .. } => format!(
                "Sign in again: Carbons run mcport login; Silicons run {SILICON_SIGN_IN}."
            ),
            Self::DeviceDenied { .. } => {
                "If that was a mistake, run mcport login again and approve the new code.".into()
            }
            Self::DeviceExpired { .. } => {
                "Run mcport login again and approve the new code within 10 minutes.".into()
            }
            Self::NotEnabled { grant, .. } => format!(
                "MCPort's sign-in setup at this Silicon Accounts must allow {grant} (device_flow and public_client). Check ACCOUNTS_URL, or ask the operator of this MCPort deployment."
            ),
            Self::Accounts { hint, .. } => hint
                .clone()
                .unwrap_or_else(|| "Retry; if it keeps failing, report it with mcport report.".into()),
            Self::Transport { .. } => {
                "Check the network and the Silicon Accounts URL (ACCOUNTS_URL or mcport config show), then retry.".into()
            }
        }
    }
    /// A passing failure worth retrying later (network, 5xx, rate limit).
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::Accounts {
                status: Some(status),
                ..
            } => *status >= 500 || *status == 429,
            _ => false,
        }
    }
}

/// Which request an error answers, to word `invalid_grant` precisely.
#[derive(Clone, Copy)]
enum Grant {
    Device,
    Slt,
    Refresh,
    Revoke,
}

fn from_accounts(error: silicon_accounts_client::Error, grant: Grant) -> SignInError {
    use silicon_accounts_client::Error as E;
    if let E::OAuth(oauth) = &error {
        let description = oauth.message();
        match (oauth.error.as_str(), grant) {
            ("invalid_grant", Grant::Slt) => {
                return SignInError::SltRefused {
                    reason: SltRefusal::classify(&description),
                    description,
                };
            }
            ("invalid_grant", Grant::Refresh) => {
                return SignInError::SignInEnded { description };
            }
            ("unauthorized_client" | "invalid_client", _) => {
                return SignInError::NotEnabled {
                    grant: match grant {
                        Grant::Device => "the device flow",
                        Grant::Slt => "short-lived token exchange as a public client",
                        Grant::Refresh | Grant::Revoke => "public clients",
                    }
                    .into(),
                    description,
                };
            }
            _ => {}
        }
    }
    if let E::Http { message, hint, .. } = &error {
        return SignInError::Transport {
            message: format!("Could not reach Silicon Accounts: {message} {hint}"),
        };
    }
    SignInError::Accounts {
        status: error.status(),
        code: error.code().to_owned(),
        message: error.message(),
        hint: error.hint(),
    }
}

/// An `application/x-www-form-urlencoded` answer of the token or revoke endpoint.
fn from_body(status: u16, body: &[u8], grant: Grant) -> SignInError {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        let description = value
            .get("error_description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        return from_accounts(
            silicon_accounts_client::Error::OAuth(Box::new(
                silicon_accounts_client::OAuthError::new(status, error, description),
            )),
            grant,
        );
    }
    if let Some(error) = value.get("error").filter(|e| e.is_object()) {
        let text = |key: &str| error.get(key).and_then(Value::as_str).map(str::to_owned);
        return SignInError::Accounts {
            status: Some(status),
            code: text("code").unwrap_or_else(|| "accounts_error".into()),
            message: text("message")
                .unwrap_or_else(|| format!("Silicon Accounts answered HTTP {status}.")),
            hint: text("hint"),
        };
    }
    SignInError::Accounts {
        status: Some(status),
        code: "unexpected_response".into(),
        message: format!(
            "Silicon Accounts answered HTTP {status} without an error body this client understands."
        ),
        hint: Some("Check ACCOUNTS_URL points at Silicon Accounts, then retry.".into()),
    }
}

/// Silicon Accounts as MCPort's public client: device flow, short-lived token
/// exchange, refresh and sign-out, with MCPort's app id as `client_id` and no secret.
#[derive(Clone)]
pub struct SignIn {
    client: AccountsClient,
    http: reqwest::Client,
    base: Url,
    app_id: String,
    poll_unit: Duration,
}

impl SignIn {
    /// `accounts_url` must be https, or http on this machine (`localhost`, `127.0.0.1`,
    /// `[::1]`) for local development.
    pub fn new(accounts_url: &str, app_id: &str) -> Result<Self, SignInError> {
        let configuration = |message: String| SignInError::Configuration {
            message,
            hint: format!(
                "Set ACCOUNTS_URL (or mcport config set accounts <url>) to an https URL, for example {DEFAULT_ACCOUNTS_URL}."
            ),
        };
        let base = Url::parse(accounts_url.trim()).map_err(|e| {
            configuration(format!(
                "The Silicon Accounts URL `{accounts_url}` is not a URL: {e}."
            ))
        })?;
        let loopback = match base.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if !(base.scheme() == "https" || base.scheme() == "http" && loopback)
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(configuration(format!(
                "The Silicon Accounts URL `{accounts_url}` must be https (http only on this machine) without credentials, query or fragment."
            )));
        }
        let app_id = app_id.trim();
        if app_id.is_empty() {
            return Err(SignInError::Configuration {
                message: "The app id is empty.".into(),
                hint: format!("Use {APP_ID}, or set MCPORT_APP_ID for another deployment."),
            });
        }
        let agent = concat!("mcport-client/", env!("CARGO_PKG_VERSION"));
        let client = AccountsClient::builder()
            .base_url(base.as_str())
            .user_agent(agent)
            // MCPort sends its own opt-out telemetry; nothing goes to Accounts' sink.
            .telemetry(false)
            .build()
            .map_err(|e| configuration(e.message()))?;
        let http = reqwest::Client::builder()
            .user_agent(agent)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| configuration(format!("Could not set up HTTPS: {e}")))?;
        Ok(Self {
            client,
            http,
            base,
            app_id: app_id.into(),
            poll_unit: Duration::from_secs(1),
        })
    }

    /// Scale the device flow's `interval` (seconds) for tests against a stub.
    #[doc(hidden)]
    pub fn with_poll_unit(mut self, unit: Duration) -> Self {
        self.poll_unit = unit;
        self
    }

    /// The Silicon Accounts URL, without a trailing slash.
    pub fn accounts_url(&self) -> String {
        self.base.as_str().trim_end_matches('/').to_owned()
    }
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// `POST /v1/device/authorize` with `client_id` = the app id: start a Carbon's sign-in.
    /// `label` names this machine on the approval page (e.g. `mcport CLI on build-box`).
    pub async fn start_device(&self, label: Option<&str>) -> Result<DeviceStart, SignInError> {
        let authorization = self
            .client
            .app_device_authorize(&self.app_id, None, label)
            .await
            .map_err(|e| from_accounts(e, Grant::Device))?;
        Ok(DeviceStart {
            user_code: authorization.user_code.clone(),
            verification_uri: authorization.verification_uri.clone(),
            verification_uri_complete: authorization.verification_uri_complete.clone(),
            expires_in: authorization.expires_in,
            interval: authorization.interval.max(1),
            expires_at: now()
                .saturating_add(i64::try_from(authorization.expires_in).unwrap_or(i64::MAX)),
            device_code: authorization.device_code.clone(),
            started: tokio::time::Instant::now(),
        })
    }

    /// Poll until the Carbon approves (the tokens), denies, or the code expires.
    /// Waits `interval` before each poll, adds 5 seconds on `slow_down`, and keeps
    /// polling through passing failures (each reported to `progress`).
    pub async fn wait_for_device(
        &self,
        device: &DeviceStart,
        mut progress: impl FnMut(DeviceProgress),
    ) -> Result<Tokens, SignInError> {
        let deadline = device.started
            + self
                .poll_unit
                .saturating_mul(u32::try_from(device.expires_in.max(1)).unwrap_or(u32::MAX));
        let mut interval = device.interval.max(1);
        loop {
            let next = tokio::time::Instant::now()
                + self
                    .poll_unit
                    .saturating_mul(u32::try_from(interval).unwrap_or(u32::MAX));
            if next > deadline {
                return Err(SignInError::DeviceExpired {
                    user_code: device.user_code.clone(),
                });
            }
            tokio::time::sleep_until(next).await;
            let received_at = now();
            match self
                .client
                .app_device_poll(&self.app_id, device.device_code.expose())
                .await
            {
                Ok(DevicePoll::Tokens(tokens)) => {
                    return Tokens::from_response(*tokens, received_at);
                }
                Ok(DevicePoll::Pending) => progress(DeviceProgress::Pending),
                Ok(DevicePoll::SlowDown) => {
                    interval += 5;
                    progress(DeviceProgress::SlowDown { interval });
                }
                Ok(DevicePoll::Denied) => {
                    return Err(SignInError::DeviceDenied {
                        user_code: device.user_code.clone(),
                    });
                }
                Ok(DevicePoll::Expired) => {
                    return Err(SignInError::DeviceExpired {
                        user_code: device.user_code.clone(),
                    });
                }
                Ok(_) => progress(DeviceProgress::Pending),
                Err(error) => {
                    let error = from_accounts(error, Grant::Device);
                    if !error.is_transient() {
                        return Err(error);
                    }
                    progress(DeviceProgress::Retrying {
                        message: error.message(),
                    });
                }
            }
        }
    }

    /// Exchange a Silicon's short-lived token (`slt_…`) with the app id alone.
    /// The token is never logged; a refused token is used up (see [`SltRefusal`]).
    pub async fn exchange_slt(&self, slt: &str) -> Result<Tokens, SignInError> {
        let slt = slt.trim();
        if slt.is_empty() {
            return Err(SignInError::Configuration {
                message: "No short-lived token was given.".into(),
                hint: format!("Pipe one in: {SILICON_SIGN_IN}"),
            });
        }
        if !slt.starts_with("slt_") {
            return Err(SignInError::Configuration {
                message: "This is not a Silicon Accounts short-lived token: those start with slt_."
                    .into(),
                hint: format!(
                    "Mint one for MCPort with {SLT_MINT_COMMAND}; Carbons can run mcport login instead."
                ),
            });
        }
        self.token_form(
            &[
                ("grant_type", SLT_GRANT_TYPE),
                ("slt", slt),
                ("client_id", &self.app_id),
            ],
            Grant::Slt,
        )
        .await
    }

    /// Rotate a refresh token with the app id alone. The old refresh token stops
    /// working at once: store the returned pair before using it.
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, SignInError> {
        let received_at = now();
        let response = self
            .client
            .refresh_app_public_client(&self.app_id, refresh_token)
            .await
            .map_err(|e| from_accounts(e, Grant::Refresh))?;
        Tokens::from_response(response, received_at)
    }

    /// Sign this sign-in out (RFC 7009 revoke of its refresh token, `client_id` only).
    /// Unknown or already revoked tokens succeed. Other sign-ins of the account stay.
    pub async fn revoke(&self, refresh_token: &str) -> Result<(), SignInError> {
        let response = self
            .http
            .post(self.endpoint("v1/oauth/revoke"))
            .form(&[
                ("token", refresh_token.trim()),
                ("token_type_hint", "refresh_token"),
                ("client_id", &self.app_id),
            ])
            .send()
            .await
            .map_err(transport)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(transport)?;
        if (200..300).contains(&status) {
            return Ok(());
        }
        Err(from_body(status, &body, Grant::Revoke))
    }

    async fn token_form(&self, form: &[(&str, &str)], grant: Grant) -> Result<Tokens, SignInError> {
        let received_at = now();
        let response = self
            .http
            .post(self.endpoint("v1/oauth/token"))
            .form(form)
            .send()
            .await
            .map_err(transport)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(transport)?;
        if !(200..300).contains(&status) {
            return Err(from_body(status, &body, grant));
        }
        let parsed: TokenResponse =
            serde_json::from_slice(&body).map_err(|e| SignInError::Accounts {
                status: Some(status),
                code: "unexpected_response".into(),
                message: format!("Silicon Accounts answered the sign-in with a body this client cannot read: {e}."),
                hint: Some("Check ACCOUNTS_URL points at Silicon Accounts, then retry.".into()),
            })?;
        Tokens::from_response(parsed, received_at)
    }

    fn endpoint(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{prefix}/{path}"));
        url
    }
}

fn transport(error: reqwest::Error) -> SignInError {
    SignInError::Transport {
        message: format!("Could not reach Silicon Accounts: {}", error.without_url()),
    }
}
