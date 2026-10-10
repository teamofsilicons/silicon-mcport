use crate::{
    accounts::Accounts,
    error::{Error, Result},
    store::Store,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio_util::sync::CancellationToken;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn secret(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
    )
}
pub fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub const DEFAULT_ACCOUNTS_URL: &str = "https://accounts.teamofsilicons.com";
/// Variables earlier releases read. They are ignored; boot warns when one is set.
const REMOVED_VARIABLES: [&str; 8] = [
    "MCPORT_IAM_URL",
    "MCPORT_IAM_WEB_URL",
    "MCPORT_WEBHOOK_SECRET",
    "MCPORT_WEBHOOK_SECRET_VERSION",
    "MCPORT_LIFECYCLE_SECRET",
    "MCPORT_TEST_APP_SECRETS",
    "MCPORT_TEST_TELEMETRY_KEYS",
    "ACCOUNTS_ISSUER",
];

/// Service configuration. `Debug` redacts the secrets it holds (app secret, webhook
/// secret, Postmark token), so logging a configuration never prints them.
#[derive(Clone)]
pub struct Config {
    pub bind: String,
    pub app_id: String,
    pub app_secret: String,
    /// Silicon Accounts public URL: the issuer of access tokens.
    pub accounts_url: String,
    /// Where MCPort calls Silicon Accounts (defaults to `accounts_url`).
    pub accounts_api_url: String,
    /// `whsec_…` secret that signs Silicon Accounts webhook deliveries.
    pub accounts_webhook_secret: Option<String>,
    pub public_url: String,
    pub web_url: String,
    pub data_dir: PathBuf,
    pub upstream_origins: HashSet<String>,
    pub postmark_token: Option<String>,
    pub postmark_from: String,
}
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted = |set: bool| if set { "<redacted>" } else { "<not set>" };
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("app_id", &self.app_id)
            .field("app_secret", &redacted(!self.app_secret.is_empty()))
            .field("accounts_url", &self.accounts_url)
            .field("accounts_api_url", &self.accounts_api_url)
            .field(
                "accounts_webhook_secret",
                &redacted(self.accounts_webhook_secret.is_some()),
            )
            .field("public_url", &self.public_url)
            .field("web_url", &self.web_url)
            .field("data_dir", &self.data_dir)
            .field("upstream_origins", &self.upstream_origins)
            .field("postmark_token", &redacted(self.postmark_token.is_some()))
            .field("postmark_from", &self.postmark_from)
            .finish()
    }
}

/// Accept `https://` anywhere and `http://` only for this machine (local stacks).
pub fn checked_url(name: &str, value: &str, purpose: &str) -> std::result::Result<String, String> {
    let invalid = || {
        format!(
            "{name} must be {purpose} over https (http is allowed only for localhost and loopback addresses), without credentials, a query or a fragment; got `{value}`."
        )
    };
    let url = url::Url::parse(value.trim()).map_err(|_| invalid())?;
    let loopback = url.host_str().is_some_and(|host| {
        let host = host.trim_matches(['[', ']']);
        host.eq_ignore_ascii_case("localhost")
            || host.to_ascii_lowercase().ends_with(".localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(value.trim().trim_end_matches('/').to_owned())
}

impl Config {
    /// Read and validate the environment. Returns the configuration and warnings
    /// for the operator, or one exact message naming what is wrong.
    pub fn from_env() -> std::result::Result<(Self, Vec<String>), String> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let env = |k: &str, d: &str| var(k).unwrap_or_else(|| d.into());
        let accounts_url = checked_url(
            "ACCOUNTS_URL",
            &env("ACCOUNTS_URL", DEFAULT_ACCOUNTS_URL),
            "the Silicon Accounts public URL (the issuer of access tokens)",
        )?;
        let accounts_api_url = match var("ACCOUNTS_API_URL") {
            Some(value) => checked_url(
                "ACCOUNTS_API_URL",
                &value,
                "the URL MCPort uses to call Silicon Accounts",
            )?,
            None => accounts_url.clone(),
        };
        let app_id = env("MCPORT_APP_ID", "mcport");
        if app_id.len() > 64
            || !app_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!(
                "MCPORT_APP_ID must be MCPort's Silicon Accounts app id (lowercase letters, digits and hyphens); got `{app_id}`."
            ));
        }
        let app_secret = var("MCPORT_APP_SECRET").ok_or_else(|| {
            format!("MCPORT_APP_SECRET is required: set it to the secret of the `{app_id}` app in Silicon Accounts. MCPort uses it to look up accounts and to check that sign-ins are still active.")
        })?;
        let accounts_webhook_secret = var("MCPORT_ACCOUNTS_WEBHOOK_SECRET");
        if let Some(secret) = &accounts_webhook_secret
            && (!secret.starts_with("whsec_") || secret.len() < 16 || secret.trim() != secret)
        {
            return Err("MCPORT_ACCOUNTS_WEBHOOK_SECRET must be the whsec_… signing secret Silicon Accounts shows for MCPort's webhook.".into());
        }
        let public_url = env("MCPORT_PUBLIC_URL", "http://127.0.0.1:4380");
        let web_url = env("MCPORT_WEB_URL", "http://127.0.0.1:4381");
        for (name, value) in [
            ("MCPORT_PUBLIC_URL", &public_url),
            ("MCPORT_WEB_URL", &web_url),
        ] {
            if url::Url::parse(value)
                .ok()
                .is_none_or(|url| !matches!(url.scheme(), "http" | "https"))
            {
                return Err(format!(
                    "{name} must be an absolute http(s) URL; got `{value}`."
                ));
            }
        }
        let mut warnings = REMOVED_VARIABLES
            .iter()
            .filter(|name| var(name).is_some())
            .map(|name| format!("{name} is no longer used by MCPort and is ignored; remove it from the runtime environment."))
            .collect::<Vec<_>>();
        if accounts_webhook_secret.is_none() {
            warnings.push("MCPORT_ACCOUNTS_WEBHOOK_SECRET is not set: POST /webhooks/accounts answers 503, so sign-outs, removed access, id changes and custodian changes reach MCPort only through lookups and token expiry.".into());
        }
        Ok((
            Self {
                bind: env("MCPORT_BIND", "127.0.0.1:4380"),
                app_id,
                app_secret,
                accounts_url,
                accounts_api_url,
                accounts_webhook_secret,
                public_url,
                web_url,
                data_dir: PathBuf::from(env("MCPORT_DATA_DIR", ".local/server")),
                upstream_origins: env("MCPORT_ALLOWED_UPSTREAM_ORIGINS", "")
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
                postmark_token: var("POSTMARK_SERVER_TOKEN"),
                postmark_from: env("MCPORT_REPORT_FROM", "mcport@teamofsilicons.com"),
            },
            warnings,
        ))
    }
    /// A local configuration for tests: everything under `data_dir`, Silicon
    /// Accounts at `accounts_url`.
    #[cfg(test)]
    pub fn local(data_dir: impl Into<PathBuf>, accounts_url: &str) -> Self {
        Self {
            bind: "127.0.0.1:0".into(),
            app_id: "mcport".into(),
            app_secret: "fixture-app-secret".into(),
            accounts_url: accounts_url.trim_end_matches('/').into(),
            accounts_api_url: accounts_url.trim_end_matches('/').into(),
            accounts_webhook_secret: None,
            public_url: "http://127.0.0.1:4241".into(),
            web_url: "http://127.0.0.1:4240".into(),
            data_dir: data_dir.into(),
            upstream_origins: HashSet::new(),
            postmark_token: None,
            postmark_from: "mcport@teamofsilicons.com".into(),
        }
    }
}

#[derive(Clone)]
pub struct App {
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub accounts: Arc<Accounts>,
    pub locks: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
    pub active: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pub jobs: Arc<Notify>,
    pub tickets: Arc<Mutex<HashMap<String, crate::assets::Ticket>>>,
}
impl App {
    pub fn new(config: Config) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config.data_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let key_path = config.data_dir.join("master.key");
        let key = if let Ok(raw) = std::env::var("MCPORT_MASTER_KEY") {
            let bytes = hex::decode(raw)
                .map_err(|_| Error::bad("MCPORT_MASTER_KEY must be 64 hex characters."))?;
            bytes
                .try_into()
                .map_err(|_| Error::bad("MCPORT_MASTER_KEY must be 32 bytes."))?
        } else if key_path.exists() {
            std::fs::read(&key_path)?
                .try_into()
                .map_err(|_| Error::internal())?
        } else {
            let key = rand::random::<[u8; 32]>();
            let mut opts = std::fs::OpenOptions::new();
            opts.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            use std::io::Write;
            opts.open(&key_path)?.write_all(&key)?;
            key
        };
        let store = Store::open(&config.data_dir.join("mcport.sqlite"), &key)?;
        let client = silicon_accounts_client::AccountsClient::builder()
            .base_url(&config.accounts_api_url)
            .user_agent(format!("mcport-server/{}", env!("CARGO_PKG_VERSION")))
            .telemetry(false)
            .build()
            .map_err(|error| {
                Error::new(
                    500,
                    "invalid_accounts_configuration",
                    format!(
                        "The Silicon Accounts client could not be configured: {}",
                        error.message()
                    ),
                    "Correct ACCOUNTS_URL / ACCOUNTS_API_URL and restart MCPort.",
                )
            })?;
        let accounts = Accounts::new(
            client,
            &config.app_id,
            &config.app_secret,
            &config.accounts_url,
        );
        Ok(Self {
            config: Arc::new(config),
            store: Arc::new(store),
            accounts: Arc::new(accounts),
            locks: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(HashMap::new())),
            jobs: Arc::new(Notify::new()),
            tickets: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub fn lock(&self, key: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        if locks.len() > 1024 {
            locks.retain(|_, lock| lock.strong_count() > 0);
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key.into(), Arc::downgrade(&lock));
        lock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_the_secrets() {
        let mut config = Config::local("/tmp/mcport-debug", "http://127.0.0.1:9");
        config.app_secret = "sa_app_mcport_very_secret_value".into();
        config.accounts_webhook_secret = Some("whsec_also_very_secret_value".into());
        config.postmark_token = Some("postmark-token-secret-value".into());
        let printed = format!("{config:?}");
        for secret in [
            "sa_app_mcport_very_secret_value",
            "whsec_also_very_secret_value",
            "postmark-token-secret-value",
        ] {
            assert!(!printed.contains(secret), "{printed}");
        }
        assert!(printed.contains("app_secret: \"<redacted>\""), "{printed}");
        assert!(printed.contains("http://127.0.0.1:9"), "{printed}");
        config.postmark_token = None;
        assert!(format!("{config:?}").contains("postmark_token: \"<not set>\""));
    }

    #[test]
    fn accounts_urls_require_https_except_on_this_machine() {
        for ok in [
            "https://accounts.teamofsilicons.com",
            "https://accounts.teamofsilicons.com/",
            "http://localhost:9590",
            "http://127.0.0.1:9589",
            "http://[::1]:9589",
            "http://accounts.localhost:9590",
        ] {
            assert!(checked_url("ACCOUNTS_URL", ok, "x").is_ok(), "{ok}");
        }
        assert_eq!(
            checked_url("ACCOUNTS_URL", "https://a.example/", "x").unwrap(),
            "https://a.example"
        );
        for bad in [
            "http://accounts.teamofsilicons.com",
            "http://10.0.0.5:9590",
            "https://user:pass@accounts.example",
            "https://accounts.example/?x=1",
            "accounts.example",
            "ftp://localhost",
        ] {
            let error =
                checked_url("ACCOUNTS_URL", bad, "the Silicon Accounts public URL").unwrap_err();
            assert!(
                error
                    .starts_with("ACCOUNTS_URL must be the Silicon Accounts public URL over https"),
                "{error}"
            );
        }
    }
}
