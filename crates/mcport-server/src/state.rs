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
        Ok(Self {
            config: Arc::new(config),
            store: Arc::new(store),
            locks: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(HashMap::new())),
            jobs: Arc::new(Notify::new()),
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
        let e = self
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
