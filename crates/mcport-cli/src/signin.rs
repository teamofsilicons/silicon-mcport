//! Signing in to MCPort with Silicon Accounts: `accounts`, `login`, `login status`,
//! `logout`, and the stored sign-in every other command uses.
use crate::{
    CliError, Result,
    args::{Cli, LoginArgs},
    store::{Settings, Store},
};
use mcport_client::accounts::{APP_ID, DeviceProgress, DeviceStart, SignIn, SignInError, Tokens};
use mcport_client::session::{SessionError, SessionFile, StoredSignIn};
use mcport_client::{Client, RequestContext};
use serde_json::{Value, json};
use std::io::{self, IsTerminal, Read, Write};
use std::time::Duration;

pub const DEFAULT_BACKEND: &str = "https://backend.mcport.teamofsilicons.com";
pub const WEBSITE: &str = "https://mcport.teamofsilicons.com";
pub const REPOSITORY: &str = "https://github.com/teamofsilicons/silicon-mcport";
/// Refresh when the access token has less than this left.
pub const MIN_VALID: Duration = Duration::from_secs(60);

/// Where this invocation signs in and which backend it talks to.
pub struct Target {
    pub backend: String,
    pub accounts_url: String,
    pub app_id: String,
}

impl Target {
    pub fn resolve(cli: &Cli, settings: &Settings) -> Self {
        let backend = cli
            .url
            .clone()
            .or_else(|| settings.backend_url.clone())
            .unwrap_or_else(|| DEFAULT_BACKEND.into());
        let accounts_url = cli
            .accounts_url
            .clone()
            .filter(|url| !url.trim().is_empty())
            .or_else(|| settings.accounts_url.clone())
            .unwrap_or_else(|| mcport_client::accounts::DEFAULT_ACCOUNTS_URL.into());
        let app_id = std::env::var("MCPORT_APP_ID")
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| APP_ID.into());
        Self {
            backend: backend.trim_end_matches('/').to_owned(),
            accounts_url: accounts_url.trim_end_matches('/').to_owned(),
            app_id,
        }
    }
    fn silicon_sign_in(&self) -> String {
        format!(
            "silicon-accounts login --app {} -q | mcport login --slt-stdin",
            self.app_id
        )
    }
}

/// The settings of this home when it can be read; defaults otherwise. Never writes.
pub fn peek_settings() -> Settings {
    Store::discover()
        .ok()
        .and_then(|store| store.settings().ok())
        .unwrap_or_default()
}

/// `mcport accounts --json`: offline, no home needed, nothing written.
pub fn accounts_json(cli: &Cli) -> Value {
    let target = Target::resolve(cli, &peek_settings());
    json!({
        "app_id": target.app_id,
        "client_id": target.app_id,
        "accounts_url": target.accounts_url,
        "api_url": target.backend,
        "backend_url": target.backend,
        "website_url": WEBSITE,
        "version": env!("CARGO_PKG_VERSION"),
        "device_flow": true,
        "public_client": true,
        "sign_in": {
            "carbon": "mcport login",
            "silicon": target.silicon_sign_in(),
        },
        "status": "mcport login status --json",
        "install": format!("silicon-apps install {}", target.app_id),
        "repository_url": REPOSITORY,
        "docs_url": format!("{REPOSITORY}/tree/main/docs"),
        "package_url": "https://crates.io/crates/mcport-client",
    })
}

/// The canonical form of a backend URL, as host registries and sign-ins are keyed.
/// Pure: no HTTP client, no network (discovery commands rely on it).
pub fn backend_key(backend: &str) -> Result<String> {
    Ok(mcport_client::canonical_backend_url(backend)?)
}

fn status_json(stored: &StoredSignIn, verified: bool) -> Value {
    let mut value = json!({
        "authenticated": true,
        "uuid": stored.account.uuid,
        "id": stored.account.id,
        "kind": stored.account.kind,
        "display_name": stored.account.display_name,
        "expires_at": stored.expires_at,
        "refresh_expires_at": stored.refresh_expires_at,
        "method": stored.method,
        "backend_url": stored.backend_url,
        "accounts_url": stored.accounts_url,
        "verified": verified,
    });
    if let Some(custodian) = &stored.account.custodian {
        value["custodian"] = json!({"uuid": custodian.uuid, "id": custodian.id});
    }
    value
}

fn signed_out(extra: Value) -> (Value, bool) {
    let mut value = json!({"authenticated": false});
    if let (Some(map), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        map.extend(extra.clone());
    }
    (value, false)
}

/// `login status`: `(json, signed_in)`. Never fails: problems become fields.
/// Signed out (or `offline`), it only reads files: no async runtime, no network and
/// no writes. Signed in, it refreshes when needed and asks the backend to confirm.
pub fn status(cli: &Cli, offline: bool) -> (Value, bool) {
    let Ok(store) = Store::discover() else {
        return signed_out(json!({}));
    };
    let target = Target::resolve(cli, &store.settings().unwrap_or_default());
    let Ok(backend) = backend_key(&target.backend) else {
        return signed_out(json!({}));
    };
    let file = SessionFile::new(store.sign_in_path(&backend));
    let stored = match file.load() {
        Ok(Some(stored)) => stored,
        Ok(None) if store.legacy_sessions_path(&backend).exists() => {
            return signed_out(json!({
                "reason": "signed_in_before_silicon_accounts",
                "message": "Sign-ins from mcport 0.2 and earlier no longer work; sign in again.",
            }));
        }
        Ok(None) => return signed_out(json!({})),
        Err(error) => {
            return signed_out(
                json!({"reason": "session_unreadable", "message": error.to_string()}),
            );
        }
    };
    if offline {
        if stored.ended() {
            return signed_out(
                json!({"reason": "sign_in_ended", "message": "This sign-in reached its end; sign in again."}),
            );
        }
        return (status_json(&stored, false), true);
    }
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(verify(&file, &backend, stored)),
        Err(error) => {
            let mut value = status_json(&stored, false);
            value["warning"] = json!(format!(
                "Could not check the sign-in ({error}); showing the stored one."
            ));
            (value, true)
        }
    }
}

/// Refresh the stored sign-in when needed and ask the backend who it belongs to.
async fn verify(file: &SessionFile, backend: &str, stored: StoredSignIn) -> (Value, bool) {
    let fresh = match file.fresh(MIN_VALID).await {
        Ok(fresh) => fresh,
        Err(SessionError::SignIn(error @ SignInError::SignInEnded { .. })) => {
            return signed_out(json!({"reason": "sign_in_ended", "message": error.message()}));
        }
        Err(SessionError::NotSignedIn) => return signed_out(json!({})),
        Err(error) => {
            let mut value = status_json(&stored, false);
            value["warning"] = json!(format!(
                "{error} Showing the stored sign-in without checking it."
            ));
            return (value, true);
        }
    };
    let me = match Client::new(backend) {
        Ok(client) => {
            let context = RequestContext::authenticated(fresh.access_token.expose());
            tokio::time::timeout(
                Duration::from_secs(15),
                async move { client.me(&context).await },
            )
            .await
        }
        Err(error) => Ok(Err(error)),
    };
    match me {
        Ok(Ok(me)) => {
            let mut value = status_json(&fresh, true);
            // The service's view is the most current; keep the stored one where it has none.
            if !me.account.id.is_empty() {
                value["id"] = json!(me.account.id);
            }
            if !me.account.display_name.is_empty() {
                value["display_name"] = json!(me.account.display_name);
            }
            if let Some(custodian) = me.custodian.filter(|c| !c.uuid.is_empty()) {
                value["custodian"] = json!({"uuid": custodian.uuid, "id": custodian.id});
            }
            (value, true)
        }
        Ok(Err(error)) if error.status() == Some(401) => signed_out(json!({
            "reason": error.code(),
            "message": error.message(),
            "uuid": fresh.account.uuid,
            "id": fresh.account.id,
        })),
        Ok(Err(error)) => {
            let mut value = status_json(&fresh, false);
            value["warning"] = json!(format!(
                "The backend could not confirm the sign-in: {}",
                error.message()
            ));
            (value, true)
        }
        Err(_) => {
            let mut value = status_json(&fresh, false);
            value["warning"] =
                json!("The backend did not answer within 15 seconds; showing the stored sign-in.");
            (value, true)
        }
    }
}

/// The short-lived token given to `login`, if any. Never echoed or logged.
fn short_lived_token(args: &LoginArgs) -> Result<Option<String>> {
    if args.slt_stdin {
        if io::stdin().is_terminal() {
            return Err(CliError::Coded {
                code: "slt_missing".into(),
                message:
                    "--slt-stdin reads the short-lived token from stdin, but stdin is a terminal."
                        .into(),
                recovery:
                    "Pipe it in: silicon-accounts login --app mcport -q | mcport login --slt-stdin"
                        .into(),
            });
        }
        let mut text = String::new();
        io::stdin().read_to_string(&mut text)?;
        return Ok(Some(text.trim().to_owned()));
    }
    Ok(args
        .slt
        .clone()
        .or_else(|| args.token.clone())
        .map(|token| token.trim().to_owned()))
}

fn normalized(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// Refuse to spend a code or a single-use token on a Silicon Accounts the backend
/// does not trust. Best effort: an unreachable or older backend does not block.
async fn check_backend(client: &Client, sign_in: &SignIn) -> Result<()> {
    let Ok(Ok(discovery)) = tokio::time::timeout(Duration::from_secs(5), client.discovery()).await
    else {
        return Ok(());
    };
    if !discovery.accounts_url.is_empty()
        && normalized(&discovery.accounts_url) != normalized(&sign_in.accounts_url())
    {
        return Err(CliError::Coded {
            code: "accounts_mismatch".into(),
            message: format!(
                "The MCPort backend at {} trusts Silicon Accounts at {}, but this CLI signs in at {}.",
                client.backend_url(),
                discovery.accounts_url,
                sign_in.accounts_url()
            ),
            recovery: format!(
                "Set ACCOUNTS_URL={0} (or mcport config set accounts {0}), or choose the backend that trusts {1} with --backend or MCPORT_URL.",
                discovery.accounts_url,
                sign_in.accounts_url()
            ),
        });
    }
    if !discovery.client_id.is_empty() && discovery.client_id != sign_in.app_id() {
        return Err(CliError::Coded {
            code: "app_id_mismatch".into(),
            message: format!(
                "The MCPort backend at {} signs in as the app {}, but this CLI uses {}.",
                client.backend_url(),
                discovery.client_id,
                sign_in.app_id()
            ),
            recovery: format!(
                "Set MCPORT_APP_ID={} for this backend.",
                discovery.client_id
            ),
        });
    }
    Ok(())
}

fn machine_label() -> String {
    let host = ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        });
    match host {
        Some(host) => format!("mcport CLI on {host}"),
        None => "mcport CLI".into(),
    }
}

fn emit(json_mode: bool, value: Value, text: impl FnOnce() -> String) {
    if json_mode {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    } else {
        let mut err = io::stderr().lock();
        let _ = writeln!(err, "{}", text());
        let _ = err.flush();
    }
}

fn open_in_browser(url: &str) -> io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

async fn device_flow(json_mode: bool, args: &LoginArgs, sign_in: &SignIn) -> Result<Tokens> {
    let label = args.label.clone().unwrap_or_else(machine_label);
    let device: DeviceStart = sign_in.start_device(Some(&label)).await?;
    let minutes = device.expires_in.div_ceil(60);
    emit(
        json_mode,
        json!({
            "event": "device_code",
            "user_code": device.user_code,
            "verification_uri": device.verification_uri,
            "verification_uri_complete": device.verification_uri_complete,
            "expires_at": device.expires_at,
            "interval": device.interval,
            "message": format!("Open {} and confirm the code {}.", device.browser_url(), device.user_code),
        }),
        || {
            format!(
                "To sign in to MCPort as a Carbon, open this page on any device:\n  {}\nand confirm the code {}. It expires in {minutes} minutes.\n(Silicons cannot approve codes: silicon-accounts login --app {} -q | mcport login --slt-stdin)\nWaiting for approval...",
                device.browser_url(),
                device.user_code,
                sign_in.app_id()
            )
        },
    );
    if args.open
        && let Err(error) = open_in_browser(device.browser_url())
    {
        emit(
            json_mode,
            json!({"event": "open_failed", "message": error.to_string()}),
            || format!("Could not open a browser ({error}); open the page yourself."),
        );
    }
    let wait = sign_in.wait_for_device(&device, |event| match event {
        DeviceProgress::Pending => {
            if json_mode {
                emit(true, json!({"event": "pending"}), String::new);
            }
        }
        DeviceProgress::SlowDown { interval } => emit(
            json_mode,
            json!({"event": "slow_down", "interval": interval}),
            || {
                format!(
                    "Silicon Accounts asked to poll less often; checking every {interval} seconds."
                )
            },
        ),
        DeviceProgress::Retrying { message } => emit(
            json_mode,
            json!({"event": "retrying", "message": message}),
            || format!("Still waiting ({message})"),
        ),
        _ => {}
    });
    tokio::select! {
        result = wait => Ok(result?),
        _ = tokio::signal::ctrl_c() => Err(CliError::Coded {
            code: "sign_in_cancelled".into(),
            message: format!("Sign-in cancelled before the code {} was approved.", device.user_code),
            recovery: "Run mcport login again when ready; the old code stops working by itself.".into(),
        }),
    }
}

/// `mcport login`: device flow, or a short-lived token. Replaces this home's sign-in
/// for the backend and signs the replaced one out.
pub async fn login(cli: &Cli, args: LoginArgs) -> Result<Value> {
    let slt = short_lived_token(&args)?;
    let store = Store::discover()?;
    let target = Target::resolve(cli, &store.settings()?);
    let client = Client::new(&target.backend)?;
    let backend = client.backend_url();
    let sign_in = SignIn::new(&target.accounts_url, &target.app_id)?;
    check_backend(&client, &sign_in).await?;
    let (tokens, method) = match slt {
        Some(slt) => (sign_in.exchange_slt(&slt).await?, "slt"),
        None => (device_flow(cli.json, &args, &sign_in).await?, "device"),
    };
    let file = SessionFile::new(store.prepare_sign_in(&backend)?);
    let lock = file.lock(Duration::from_secs(30)).await?;
    let previous = file.load().ok().flatten();
    let stored = StoredSignIn::new(&sign_in, &backend, method, tokens);
    file.save(&stored)?;
    drop(lock);
    let mut output = status_json(&stored, false);
    if let Some(map) = output.as_object_mut() {
        map.remove("verified");
    }
    if let Some(previous) = previous
        && previous.refresh_token != stored.refresh_token
    {
        // The replaced sign-in can never be used again from here: end it.
        let revoked = match SignIn::new(&previous.accounts_url, &previous.app_id) {
            Ok(old) => old.revoke(previous.refresh_token.expose()).await.is_ok(),
            Err(_) => false,
        };
        output["replaced"] =
            json!({"uuid": previous.account.uuid, "id": previous.account.id, "revoked": revoked});
    }
    Ok(output)
}

/// `mcport logout`: revoke this home's sign-in for the backend and forget it.
pub async fn logout(cli: &Cli) -> Result<Value> {
    let store = Store::discover()?;
    let target = Target::resolve(cli, &store.settings()?);
    let backend = backend_key(&target.backend)?;
    let legacy = store.legacy_sessions_path(&backend);
    let legacy_removed = legacy.is_file() && std::fs::remove_file(&legacy).is_ok();
    let path = store.sign_in_path(&backend);
    let file = SessionFile::new(&path);
    let _lock = if path.parent().is_some_and(|parent| parent.is_dir()) {
        Some(file.lock(Duration::from_secs(30)).await?)
    } else {
        None
    };
    let stored = match file.load() {
        Ok(Some(stored)) => stored,
        Ok(None) => {
            return Ok(
                json!({"signed_out": false, "reason": "not_signed_in", "legacy_sign_in_removed": legacy_removed}),
            );
        }
        Err(SessionError::Unreadable { .. }) => {
            file.remove()?;
            return Ok(
                json!({"signed_out": true, "revoked": false, "note": "The stored sign-in could not be read, so it was deleted without being revoked. To end it everywhere, sign out of MCPort in Silicon Accounts."}),
            );
        }
        Err(error) => return Err(error.into()),
    };
    let revoked = match SignIn::new(&stored.accounts_url, &stored.app_id) {
        Ok(sign_in) => sign_in.revoke(stored.refresh_token.expose()).await,
        Err(error) => Err(error),
    };
    file.remove()?;
    let mut output = json!({
        "signed_out": true,
        "uuid": stored.account.uuid,
        "id": stored.account.id,
        "kind": stored.account.kind,
        "revoked": revoked.is_ok(),
    });
    if let Err(error) = revoked {
        output["warning"] = json!(format!(
            "The sign-in was deleted here but could not be revoked at Silicon Accounts: {} It ends by itself when unused, or sign out of MCPort in Silicon Accounts.",
            error.message()
        ));
    }
    if legacy_removed {
        output["legacy_sign_in_removed"] = json!(true);
    }
    Ok(output)
}

/// The sign-in every authenticated command uses, refreshed single-flight when it has
/// less than [`MIN_VALID`] left (or always, with `force`).
pub async fn signed_in(store: &Store, backend: &str, force: bool) -> Result<StoredSignIn> {
    let file = SessionFile::new(store.sign_in_path(backend));
    let min_valid = if force {
        Duration::from_secs(u64::from(u32::MAX))
    } else {
        MIN_VALID
    };
    match file.fresh(min_valid).await {
        Ok(stored) => Ok(stored),
        Err(SessionError::NotSignedIn) => Err(CliError::NotSignedIn {
            backend: backend.into(),
            legacy: store.legacy_sessions_path(backend).exists(),
        }),
        // Silicon Accounts is briefly unreachable but the access token still works:
        // use it rather than fail a command that does not need a new one yet.
        Err(SessionError::SignIn(error)) if error.is_transient() && !force => match file.load() {
            Ok(Some(stored)) if stored.seconds_left() > USABLE_WITHOUT_REFRESH => Ok(stored),
            _ => Err(SessionError::SignIn(error).into()),
        },
        Err(other) => Err(other.into()),
    }
}

/// Seconds an access token must still have to be used when refreshing it failed for
/// a passing reason.
const USABLE_WITHOUT_REFRESH: i64 = 10;

/// The account signed in here for `backend`, read from the stored sign-in only.
pub fn stored(store: &Store, backend: &str) -> Option<StoredSignIn> {
    SessionFile::new(store.sign_in_path(backend))
        .load()
        .ok()
        .flatten()
}
