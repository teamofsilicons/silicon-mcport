use crate::{
    error::{Error, Result},
    store::Store,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
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

#[derive(Clone)]
pub struct Config {
    pub bind: String,
    pub app_id: String,
    pub app_secret: String,
    pub iam_url: String,
    pub iam_web_url: String,
    pub public_url: String,
    pub web_url: String,
    pub data_dir: PathBuf,
    pub upstream_origins: HashSet<String>,
    pub webhook_secret: Option<String>,
    pub lifecycle_secret: Option<String>,
    pub test_app_secrets: String,
    pub postmark_token: Option<String>,
    pub postmark_from: String,
}
impl Config {
    pub fn from_env() -> Self {
        let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.into());
        Self {
            bind: env("MCPORT_BIND", "127.0.0.1:4380"),
            app_id: env("MCPORT_APP_ID", "mcport"),
            app_secret: env("MCPORT_APP_SECRET", ""),
            iam_url: env("MCPORT_IAM_URL", "https://backend.iam.teamofsilicons.com"),
            iam_web_url: env("MCPORT_IAM_WEB_URL", "https://iam.teamofsilicons.com"),
            public_url: env("MCPORT_PUBLIC_URL", "http://127.0.0.1:4380"),
            web_url: env("MCPORT_WEB_URL", "http://127.0.0.1:4381"),
            data_dir: PathBuf::from(env("MCPORT_DATA_DIR", ".local/server")),
            upstream_origins: env("MCPORT_ALLOWED_UPSTREAM_ORIGINS", "")
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            webhook_secret: std::env::var("MCPORT_WEBHOOK_SECRET").ok(),
            lifecycle_secret: std::env::var("MCPORT_LIFECYCLE_SECRET").ok(),
            test_app_secrets: env("MCPORT_TEST_APP_SECRETS", "{}"),
            postmark_token: std::env::var("POSTMARK_SERVER_TOKEN").ok(),
            postmark_from: env("MCPORT_REPORT_FROM", "mcport@teamofsilicons.com"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Environment {
    pub id: String,
    pub state: String,
    pub generation: i64,
    #[serde(default)]
    pub control_revision: i64,
    pub app_secret: String,
    pub iam_key: Option<String>,
}

#[derive(Clone)]
pub struct App {
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub locks: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
    pub active: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pub jobs: Arc<Notify>,
    test_app_secrets: Arc<HashMap<String, String>>,
}
impl App {
    pub fn new(config: Config) -> Result<Self> {
        let test_app_secrets = parse_test_app_secrets(&config)?;
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
        Ok(Self {
            config: Arc::new(config),
            store: Arc::new(store),
            locks: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(HashMap::new())),
            jobs: Arc::new(Notify::new()),
            test_app_secrets: Arc::new(test_app_secrets),
        })
    }
    pub fn configured_test_secret(&self, environment: &str) -> Option<&String> {
        self.test_app_secrets.get(environment)
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
    pub fn environment(&self, name: &str) -> Result<Environment> {
        if name == "production" {
            return Ok(Environment {
                id: name.into(),
                state: "active".into(),
                generation: 0,
                control_revision: 0,
                app_secret: self.config.app_secret.clone(),
                iam_key: None,
            });
        }
        let mut e = self
            .store
            .get::<Environment>("environment", name)?
            .ok_or_else(|| {
                Error::new(
                    403,
                    "test_environment_unavailable",
                    "The selected test environment has not been provisioned.",
                    "Import MCPort into this Honeycomb testing environment first.",
                )
            })?;
        if e.state != "active" {
            return Err(Error::new(
                403,
                "test_environment_disabled",
                "This testing environment is not active.",
                "Wait for Honeycomb to complete its lifecycle operation.",
            ));
        }
        // Honeycomb returns the app-owned test credential after participants finish
        // importing. Configuration can therefore arrive after the durable receipt.
        // It changes credentials only: provisioning and lifecycle fences stay intact,
        // and auth::iam verifies the selected credential against IAM before use.
        if let Some(secret) = self.configured_test_secret(name) {
            e.app_secret.clone_from(secret);
        }
        Ok(e)
    }
    pub fn assert_environment(&self, expected: &Environment) -> Result<()> {
        let current = self.environment(&expected.id)?;
        if current.generation != expected.generation
            || current.control_revision != expected.control_revision
        {
            return Err(Error::new(
                409,
                "environment_changed",
                "The environment changed while this request was running.",
                "Start a new request in the current environment.",
            ));
        }
        Ok(())
    }
    pub fn assert_generation(&self, env: &str, generation: i64) -> Result<()> {
        if self.environment(env)?.generation != generation {
            return Err(Error::new(
                409,
                "environment_changed",
                "The environment changed while this request was running.",
                "Start a new request in the current environment.",
            ));
        }
        Ok(())
    }
}

fn parse_test_app_secrets(config: &Config) -> Result<HashMap<String, String>> {
    let invalid = || {
        Error::new(
            500,
            "invalid_test_credentials_configuration",
            "MCPORT_TEST_APP_SECRETS must map test environment UUIDs to their own nonempty credentials.",
            "Correct the protected runtime configuration and restart MCPort. Never use a production application credential.",
        )
    };
    if config.test_app_secrets.len() > 1024 * 1024 {
        return Err(invalid());
    }
    let secrets: HashMap<String, String> =
        serde_json::from_str(&config.test_app_secrets).map_err(|_| invalid())?;
    if secrets.len() > 1024
        || secrets.iter().any(|(environment, secret)| {
            uuid::Uuid::parse_str(environment).map_or(true, |id| {
                id.is_nil() || id.hyphenated().to_string() != *environment
            }) || secret.is_empty()
                || secret.len() > 2048
                || secret.trim() != secret
                || secret.chars().any(char::is_control)
                || secret == &config.app_secret
        })
    {
        return Err(invalid());
    }
    Ok(secrets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_credential_configuration_rejects_production_and_malformed_entries() {
        let mut config = Config::from_env();
        config.app_secret = "production-secret".into();
        const ENV: &str = "11111111-1111-4111-8111-111111111111";
        for raw in [
            json!({"production": "test-secret"}).to_string(),
            json!({ENV: "production-secret"}).to_string(),
            json!({ENV: ""}).to_string(),
            json!({ENV: "secret\n"}).to_string(),
            json!({"00000000-0000-0000-0000-000000000000": "test-secret"}).to_string(),
            "{private-credential-invalid-json".into(),
        ] {
            config.test_app_secrets = raw;
            let error = parse_test_app_secrets(&config).unwrap_err();
            assert_eq!(error.1.code, "invalid_test_credentials_configuration");
            assert!(!error.1.message.contains("private-credential"));
            assert!(!error.1.message.contains("production-secret"));
        }
        config.test_app_secrets = json!({ENV: "test-secret"}).to_string();
        assert_eq!(parse_test_app_secrets(&config).unwrap()[ENV], "test-secret");
    }
}
