//! Persistent CLI context. The public Rust API client does not read or write this state.
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Invalid(String),
    #[error("Could not access protected MCPort state: {0}")]
    Io(#[from] io::Error),
    #[error(
        "MCPort state contains invalid JSON: {0}; restore the state file from a trusted backup or log in from a new home"
    )]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct Store {
    pub original_home: PathBuf,
    pub home: PathBuf,
    pub directory: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_telemetry")]
    pub telemetry: bool,
    pub backend_url: Option<String>,
}

fn default_telemetry() -> bool {
    true
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            telemetry: true,
            backend_url: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct HomeOverride {
    home: PathBuf,
}

/// Credentials are retained only in a protected state file. Never serialize this in CLI output.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub principal_id: String,
    pub org_id: String,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub identity: Value,
}

impl StoredSession {
    pub fn key(&self) -> String {
        format!("{}\n{}", self.principal_id, self.org_id)
    }
    pub fn public(&self) -> Value {
        json!({"principal_id":self.principal_id,"org_id":self.org_id,"expires_at":self.expires_at,"identity":self.identity})
    }
}

#[derive(Default, Serialize, Deserialize)]
struct ContextSessions {
    active: Option<String>,
    sessions: BTreeMap<String, StoredSession>,
}

pub struct SessionGuard {
    _lock: File,
    path: PathBuf,
    data: ContextSessions,
}

impl Store {
    pub fn discover() -> Result<Self, StoreError> {
        let original_home = std::env::var_os("SILICON_HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .ok_or_else(|| {
                StoreError::Invalid(
                    "Cannot find a home directory. Set SILICON_HOME to an existing directory."
                        .into(),
                )
            })?;
        Self::at(original_home)
    }

    pub fn at(original_home: PathBuf) -> Result<Self, StoreError> {
        require_directory(&original_home)?;
        reject_symlink(&original_home.join(".mcport"))?;
        let pointer = original_home.join(".mcport/home.json");
        let home = match read_optional::<HomeOverride>(&pointer)? {
            Some(saved) => {
                require_directory(&saved.home)?;
                saved.home
            }
            None => original_home.clone(),
        };
        let directory = home.join(".mcport/dir");
        reject_symlink(&home.join(".mcport"))?;
        reject_symlink(&directory)?;
        Ok(Self {
            original_home,
            home,
            directory,
        })
    }

    pub fn change_home(&self, location: &Path) -> Result<PathBuf, StoreError> {
        require_directory(location)?;
        let home = fs::canonicalize(location)?;
        write_protected(
            &self.original_home.join(".mcport/home.json"),
            &HomeOverride { home: home.clone() },
        )?;
        Ok(home)
    }

    pub fn settings(&self) -> Result<Settings, StoreError> {
        Ok(read_optional(&self.directory.join("settings.json"))?.unwrap_or_default())
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), StoreError> {
        write_protected(&self.directory.join("settings.json"), settings)
    }

    /// Exclusive lock is intentionally held across token refresh and rotation persistence.
    /// Each backend/test context has its own lock; another context remains independent.
    pub fn sessions(
        &self,
        backend: &str,
        test_id: Option<&str>,
    ) -> Result<SessionGuard, StoreError> {
        let scope = context_key(backend, test_id);
        let parent = self.directory.join("sessions");
        secure_directory(&parent)?;
        let lock = protected_open(&parent.join(format!("{scope}.lock")), false)?;
        lock.lock_exclusive()?;
        let path = parent.join(format!("{scope}.json"));
        let data = read_optional(&path)?.unwrap_or_default();
        Ok(SessionGuard {
            _lock: lock,
            path,
            data,
        })
    }

    pub fn context_path(&self, backend: &str, test_id: Option<&str>, suffix: &str) -> PathBuf {
        self.directory
            .join("contexts")
            .join(context_key(backend, test_id))
            .join(suffix)
    }
}

impl SessionGuard {
    pub fn active(&self) -> Option<&StoredSession> {
        self.data
            .active
            .as_ref()
            .and_then(|key| self.data.sessions.get(key))
    }
    pub fn list(&self) -> Value {
        let sessions: Vec<Value> = self
            .data
            .sessions
            .iter()
            .map(|(key, session)| {
                let mut value = session.public();
                value["selected"] = json!(self.data.active.as_ref() == Some(key));
                value
            })
            .collect();
        json!({"sessions":sessions})
    }
    pub fn save(&mut self, session: StoredSession) -> Result<(), StoreError> {
        let key = session.key();
        self.data.sessions.insert(key.clone(), session);
        self.data.active = Some(key);
        self.persist()
    }
    pub fn remove_active(&mut self) -> Result<(), StoreError> {
        if let Some(key) = self.data.active.take() {
            self.data.sessions.remove(&key);
        }
        // Do not implicitly switch identity when logging out.
        self.persist()
    }
    pub fn select(&mut self, principal: &str, org: Option<&str>) -> Result<Value, StoreError> {
        let matching: Vec<String> = self
            .data
            .sessions
            .iter()
            .filter(|(_, session)| {
                session.principal_id == principal && org.is_none_or(|o| o == session.org_id)
            })
            .map(|(key, _)| key.clone())
            .collect();
        if matching.is_empty() {
            return Err(StoreError::Invalid(format!(
                "No stored session for {principal}. Run mcport login <slt> in this backend/test context first."
            )));
        }
        if matching.len() > 1 {
            return Err(StoreError::Invalid(format!(
                "{principal} has multiple organization sessions. Pass --org <org-id>; use mcport session ls to inspect them."
            )));
        }
        self.data.active = Some(matching[0].clone());
        self.persist()?;
        Ok(self.active().expect("selected existing session").public())
    }
    fn persist(&self) -> Result<(), StoreError> {
        write_protected(&self.path, &self.data)
    }
}

fn require_directory(path: &Path) -> Result<(), StoreError> {
    if !path.is_dir() {
        return Err(StoreError::Invalid(format!(
            "{} is not a directory. Create the directory first, then run mcport config home <location>.",
            path.display()
        )));
    }
    Ok(())
}

pub fn context_key(backend: &str, test_id: Option<&str>) -> String {
    let material = serde_json::to_vec(&(backend.trim_end_matches('/'), test_id))
        .expect("serializable strings");
    format!("{:x}", Sha256::digest(material))
}

pub fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, StoreError> {
    reject_symlink(path)?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn reject_symlink(path: &Path) -> Result<(), StoreError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(StoreError::Invalid(format!(
            "Refusing symbolic link at {} for protected state. Choose a home with ordinary MCPort state files.",
            path.display()
        )));
    }
    Ok(())
}

pub fn secure_directory(path: &Path) -> Result<(), StoreError> {
    // Only change MCPort-owned directory permissions, never the user's home.
    if path.components().any(|part| part.as_os_str() == ".mcport")
        && path.file_name().is_some_and(|name| name != ".mcport")
        && let Some(parent) = path.parent()
    {
        secure_directory(parent)?;
    }
    if !path.exists() {
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            secure_directory(parent)?;
        }
        fs::create_dir(path)?;
    }
    reject_symlink(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn exclusive_lock(path: &Path) -> Result<File, StoreError> {
    if let Some(parent) = path.parent() {
        secure_directory(parent)?;
    }
    let file = protected_open(path, false)?;
    file.lock_exclusive()?;
    Ok(file)
}

fn protected_open(path: &Path, exclusive: bool) -> Result<File, StoreError> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if exclusive {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

pub fn write_protected<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::Invalid("State path has no parent".into()))?;
    secure_directory(parent)?;
    reject_symlink(path)?;
    let tmp = parent.join(format!(".write-{}", uuid::Uuid::new_v4()));
    let outcome = (|| {
        let mut file = protected_open(&tmp, true)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        #[cfg(unix)]
        {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(principal: &str, org: &str, token: &str) -> StoredSession {
        StoredSession {
            principal_id: principal.into(),
            org_id: org.into(),
            access_token: token.into(),
            refresh_token: None,
            expires_at: None,
            identity: json!({}),
        }
    }
    #[test]
    fn home_switch_does_not_copy_credentials() {
        let original = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let store = Store::at(original.path().into()).unwrap();
        store
            .sessions("http://localhost:4380", None)
            .unwrap()
            .save(session("si:one", "tos", "secret"))
            .unwrap();
        store.change_home(other.path()).unwrap();
        let switched = Store::at(original.path().into()).unwrap();
        assert_eq!(switched.home, fs::canonicalize(other.path()).unwrap());
        assert!(
            switched
                .sessions("http://localhost:4380", None)
                .unwrap()
                .active()
                .is_none()
        );
        assert!(store.change_home(&other.path().join("missing")).is_err());
    }
    #[test]
    fn sessions_are_backend_test_and_identity_scoped() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::at(home.path().into()).unwrap();
        {
            let mut slots = store.sessions("https://one.test", None).unwrap();
            slots.save(session("c:one", "tos", "a")).unwrap();
            slots.save(session("si:two", "tos", "b")).unwrap();
            slots.select("c:one", Some("tos")).unwrap();
            assert_eq!(slots.active().unwrap().access_token, "a");
            assert!(!slots.list().to_string().contains("access_token"));
            slots.remove_active().unwrap();
            assert!(slots.active().is_none());
        }
        assert!(
            store
                .sessions("https://two.test", None)
                .unwrap()
                .active()
                .is_none()
        );
        assert!(
            store
                .sessions("https://one.test", Some("test-1"))
                .unwrap()
                .active()
                .is_none()
        );
    }
    #[cfg(unix)]
    #[test]
    fn credentials_are_owner_only_and_symlinks_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("state/session.json");
        write_protected(&path, &json!({"token":"secret"})).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = home.path().join("linked.json");
        symlink(&path, &link).unwrap();
        assert!(read_optional::<Value>(&link).is_err());
        assert!(write_protected(&link, &json!({})).is_err());
    }
}
