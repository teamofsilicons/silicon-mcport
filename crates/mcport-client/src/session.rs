//! Keep one sign-in in one explicit file (the `session` feature).
//!
//! The caller chooses the path; nothing is discovered. The file is owner-only (0600 in
//! a 0700 directory on Unix), written atomically (a temporary file renamed over it),
//! and never a symbolic link. [`SessionFile::fresh`] refreshes single-flight: it takes
//! an exclusive lock on `<file>.lock`, re-reads the file (another process may have just
//! refreshed), rotates the refresh token only if the access token still has less than
//! the requested validity, and stores the new pair before returning it, because
//! presenting an already-used refresh token ends the whole sign-in.

use crate::accounts::{Secret, SignIn, SignInError, SignedInAccount, Tokens};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The format this release writes.
pub const SESSION_FORMAT: u32 = 1;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// A stored sign-in. `Debug` never prints the tokens.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSignIn {
    pub format: u32,
    /// The Silicon Accounts that issued the tokens; refresh and sign-out go there.
    pub accounts_url: String,
    /// The app the tokens are for (`mcport`).
    pub app_id: String,
    /// The MCPort backend this sign-in was made for (informational).
    #[serde(default)]
    pub backend_url: String,
    /// `device` (a Carbon approved a code) or `slt` (a short-lived token).
    pub method: String,
    pub account: SignedInAccount,
    pub access_token: Secret,
    pub refresh_token: Secret,
    /// When the access token expires (Unix seconds).
    pub expires_at: i64,
    /// When the sign-in ends (Unix seconds), if it has an end.
    #[serde(default)]
    pub refresh_expires_at: Option<i64>,
    #[serde(default)]
    pub scope: String,
    pub signed_in_at: i64,
    pub refreshed_at: i64,
}

impl std::fmt::Debug for StoredSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredSignIn")
            .field("accounts_url", &self.accounts_url)
            .field("app_id", &self.app_id)
            .field("method", &self.method)
            .field("account", &self.account)
            .field("expires_at", &self.expires_at)
            .field("refresh_expires_at", &self.refresh_expires_at)
            .finish_non_exhaustive()
    }
}

impl StoredSignIn {
    /// A new sign-in from `tokens`, made at `accounts` for `backend_url`.
    pub fn new(sign_in: &SignIn, backend_url: &str, method: &str, tokens: Tokens) -> Self {
        let at = now();
        Self {
            format: SESSION_FORMAT,
            accounts_url: sign_in.accounts_url(),
            app_id: sign_in.app_id().into(),
            backend_url: backend_url.into(),
            method: method.into(),
            account: tokens.account,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at: tokens.expires_at,
            refresh_expires_at: tokens.refresh_expires_at,
            scope: tokens.scope,
            signed_in_at: at,
            refreshed_at: at,
        }
    }
    /// The same sign-in after a refresh (the account details may have changed).
    fn refreshed(&self, tokens: Tokens) -> Self {
        Self {
            account: tokens.account,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at: tokens.expires_at,
            refresh_expires_at: tokens.refresh_expires_at.or(self.refresh_expires_at),
            scope: tokens.scope,
            refreshed_at: now(),
            ..self.clone()
        }
    }
    /// Seconds the access token stays valid (negative once expired).
    pub fn seconds_left(&self) -> i64 {
        self.expires_at - now()
    }
    /// Whether the sign-in itself has reached its end.
    pub fn ended(&self) -> bool {
        self.refresh_expires_at.is_some_and(|end| end <= now())
    }
}

/// Session storage failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// No sign-in is stored at this path.
    #[error("Not signed in.")]
    NotSignedIn,
    /// The file exists but is not a sign-in this release can read.
    #[error("The stored sign-in at {path} cannot be read: {reason}.")]
    Unreadable { path: PathBuf, reason: String },
    /// Another process held the refresh lock too long.
    #[error("Another mcport process has been refreshing this sign-in for {seconds} seconds.")]
    LockTimeout { seconds: u64 },
    /// Refreshing failed. `SignInError::SignInEnded` means the file was removed.
    #[error(transparent)]
    SignIn(#[from] SignInError),
    #[error("Could not use the stored sign-in: {0}")]
    Io(#[from] io::Error),
}

/// One sign-in at one path. Cheap to create; holds no open files.
#[derive(Clone, Debug)]
pub struct SessionFile {
    path: PathBuf,
}

impl SessionFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn lock_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        self.path.with_file_name(name)
    }

    /// The stored sign-in, `None` when there is none. Reads only: no lock, no writes.
    pub fn load(&self) -> Result<Option<StoredSignIn>, SessionError> {
        reject_symlink(&self.path)?;
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let unreadable = |reason: String| SessionError::Unreadable {
            path: self.path.clone(),
            reason,
        };
        let stored: StoredSignIn = serde_json::from_slice(&bytes)
            .map_err(|e| unreadable(format!("it is not a sign-in this mcport writes ({e})")))?;
        if stored.format != SESSION_FORMAT {
            return Err(unreadable(format!(
                "it uses format {} and this mcport reads format {SESSION_FORMAT}",
                stored.format
            )));
        }
        Ok(Some(stored))
    }

    /// Store a sign-in atomically (owner-only).
    pub fn save(&self, stored: &StoredSignIn) -> Result<(), SessionError> {
        let body = serde_json::to_vec_pretty(stored).map_err(io::Error::other)?;
        write_private(&self.path, &body)?;
        Ok(())
    }

    /// Forget the stored sign-in. Returns whether there was one.
    pub fn remove(&self) -> Result<bool, SessionError> {
        reject_symlink(&self.path)?;
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Take the exclusive refresh lock, waiting at most `timeout`.
    pub async fn lock(&self, timeout: Duration) -> Result<SessionLock, SessionError> {
        let parent = self.path.parent().unwrap_or(Path::new("."));
        private_directory(parent)?;
        let path = self.lock_path();
        reject_symlink(&path)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        let started = tokio::time::Instant::now();
        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(SessionLock { file }),
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
                {
                    if started.elapsed() >= timeout {
                        return Err(SessionError::LockTimeout {
                            seconds: timeout.as_secs(),
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// The stored sign-in with an access token valid for at least `min_valid`,
    /// refreshing single-flight when needed. When the refresh token no longer works,
    /// the file is removed and `SignInError::SignInEnded` says why.
    pub async fn fresh(&self, min_valid: Duration) -> Result<StoredSignIn, SessionError> {
        let wanted = i64::try_from(min_valid.as_secs()).unwrap_or(i64::MAX);
        let current = self.load()?.ok_or(SessionError::NotSignedIn)?;
        if current.seconds_left() >= wanted {
            return Ok(current);
        }
        let _lock = self.lock(Duration::from_secs(30)).await?;
        // Another process may have refreshed while we waited for the lock.
        let current = self.load()?.ok_or(SessionError::NotSignedIn)?;
        if current.seconds_left() >= wanted {
            return Ok(current);
        }
        let sign_in = SignIn::new(&current.accounts_url, &current.app_id)?;
        match sign_in.refresh(current.refresh_token.expose()).await {
            Ok(tokens) => {
                let next = current.refreshed(tokens);
                // The old refresh token is spent: the new pair must reach the disk
                // before anything uses it.
                self.save(&next)?;
                Ok(next)
            }
            Err(error @ SignInError::SignInEnded { .. }) => {
                self.remove()?;
                Err(error.into())
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// Holds the refresh lock until dropped.
pub struct SessionLock {
    file: File,
}
impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is a symbolic link; sign-ins are only kept in ordinary files",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn private_directory(path: &Path) -> io::Result<()> {
    reject_symlink(path)?;
    if !path.exists() {
        fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn write_private(path: &Path, body: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    private_directory(parent)?;
    reject_symlink(path)?;
    let temporary = parent.join(format!(
        ".{}.{}-{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let outcome = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(body)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}
