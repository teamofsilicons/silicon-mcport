//! Persistent CLI context: the home, settings, sign-ins and host registries under
//! `{home}/.mcport/dir`. The public Rust API client does not read or write this state.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
    /// Silicon Accounts for this home (development and test deployments).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts_url: Option<String>,
}

fn default_telemetry() -> bool {
    true
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            telemetry: true,
            backend_url: None,
            accounts_url: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct HomeOverride {
    home: PathBuf,
}

impl Store {
    pub fn discover() -> Result<Self, StoreError> {
        let original_home = std::env::var_os("SILICON_HOME")
            .filter(|home| !home.is_empty())
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

    /// Open the store under an existing home. Only reads (the optional home pointer);
    /// directories are created when something is written.
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

    /// The sign-in for one backend: `accounts/<key>.json` (+ `.lock`). One account per
    /// home and backend; several identities on one machine use separate homes.
    pub fn sign_in_path(&self, backend: &str) -> PathBuf {
        self.directory
            .join("accounts")
            .join(format!("{}.json", context_key(backend)))
    }
    /// Create the sign-in directory (and `.mcport/dir`) owner-only before a sign-in
    /// is written. Reading a sign-in never creates anything.
    pub fn prepare_sign_in(&self, backend: &str) -> Result<PathBuf, StoreError> {
        let path = self.sign_in_path(backend);
        if let Some(parent) = path.parent() {
            secure_directory(parent)?;
        }
        Ok(path)
    }
    /// Where mcport 0.2 and earlier kept sign-ins for this backend (no longer used).
    pub fn legacy_sessions_path(&self, backend: &str) -> PathBuf {
        self.directory
            .join("sessions")
            .join(format!("{}.json", context_key(backend)))
    }

    pub fn context_path(&self, backend: &str, suffix: &str) -> PathBuf {
        self.directory
            .join("contexts")
            .join(context_key(backend))
            .join(suffix)
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

/// The directory key of one backend. It hashes `[backend, null]` exactly as mcport
/// 0.2 did for production contexts, so existing host registries keep their paths.
pub fn context_key(backend: &str) -> String {
    let material = serde_json::to_vec(&(backend.trim_end_matches('/'), None::<&str>))
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
    let mut body = serde_json::to_vec_pretty(value)?;
    body.push(b'\n');
    write_bytes_protected(path, &body)
}

/// Write `body` to `path` atomically, owner-only, never through a symbolic link.
pub fn write_bytes_protected(path: &Path, body: &[u8]) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::Invalid("State path has no parent".into()))?;
    secure_directory(parent)?;
    reject_symlink(path)?;
    let tmp = parent.join(format!(".write-{}", uuid::Uuid::new_v4()));
    let outcome = (|| {
        let mut file = protected_open(&tmp, true)?;
        file.write_all(body)?;
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
    use serde_json::{Value, json};
    #[test]
    fn home_switch_does_not_copy_credentials() {
        let original = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let store = Store::at(original.path().into()).unwrap();
        let path = store.prepare_sign_in("http://127.0.0.1:4241").unwrap();
        write_protected(&path, &json!({"secret":"token"})).unwrap();
        store.change_home(other.path()).unwrap();
        let switched = Store::at(original.path().into()).unwrap();
        assert_eq!(switched.home, fs::canonicalize(other.path()).unwrap());
        assert!(!switched.sign_in_path("http://127.0.0.1:4241").exists());
        assert!(store.change_home(&other.path().join("missing")).is_err());
    }
    #[test]
    fn sign_ins_are_per_backend_and_registry_paths_do_not_move() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::at(home.path().into()).unwrap();
        assert_ne!(
            store.sign_in_path("https://one.test"),
            store.sign_in_path("https://two.test")
        );
        assert_eq!(
            store.sign_in_path("https://one.test/"),
            store.sign_in_path("https://one.test")
        );
        // mcport 0.2 hashed [backend, test_id]; production contexts used null.
        assert_eq!(
            context_key("https://backend.mcport.teamofsilicons.com"),
            format!(
                "{:x}",
                Sha256::digest(br#"["https://backend.mcport.teamofsilicons.com",null]"#)
            )
        );
        assert!(
            store
                .legacy_sessions_path("https://one.test")
                .ends_with(format!("sessions/{}.json", context_key("https://one.test")))
        );
    }
    #[test]
    fn opening_a_store_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::at(home.path().into()).unwrap();
        assert_eq!(store.settings().unwrap().backend_url, None);
        assert!(!home.path().join(".mcport").exists());
        assert!(Store::at(home.path().join("missing")).is_err());
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
