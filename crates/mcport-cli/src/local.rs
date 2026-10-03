use crate::{
    CliError, Result,
    args::{AccountCommand, DaemonCommand},
    input_object, now,
    store::{self, Store, StoredSession},
};
use fs2::FileExt;
use mcport_client::Connection;
use mcport_daemon::{LocalAccount, Registry};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

pub fn registry_path(
    store: &Store,
    backend: &str,
    test_id: Option<&str>,
    host_id: &str,
) -> PathBuf {
    // Server-issued host IDs are still treated as opaque identifiers, never paths.
    let safe_id = store::context_key(host_id, None);
    store
        .context_path(backend, test_id, "hosts")
        .join(safe_id)
        .join("registry.json")
}

pub fn require_registry(
    store: &Store,
    backend: &str,
    test_id: Option<&str>,
    host_id: &str,
    session: &StoredSession,
) -> Result<PathBuf> {
    let path = registry_path(store, backend, test_id, host_id);
    if !path.is_file() {
        return Err(CliError::Input("This host's local registry is not present in this home/backend/test context. Configure local execution on the registered host machine; central configuration cannot authorize a new local process.".into()));
    }
    let registry = Registry::load(&path)?;
    if registry.host.host_id != host_id
        || registry.host.org_id != session.org_id
        || registry.host.environment != test_id.unwrap_or("production")
        || registry.host.backend_url.trim_end_matches('/') != backend.trim_end_matches('/')
    {
        return Err(CliError::Input("Local host registry does not match the selected backend, organization and testing environment.".into()));
    }
    Ok(path)
}

fn registry_files(store: &Store, backend: &str, test_id: Option<&str>) -> Result<Vec<PathBuf>> {
    let folder = store.context_path(backend, test_id, "hosts");
    if !folder.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let file = entry.path().join("registry.json");
            if file.is_file() {
                result.push(file);
            }
        }
    }
    result.sort();
    Ok(result)
}

fn parent(path: &Path) -> Result<&Path> {
    path.parent()
        .ok_or_else(|| CliError::Input("Registry path has no parent directory.".into()))
}

pub fn registry_lock(path: &Path) -> Result<std::fs::File> {
    Ok(store::exclusive_lock(
        &parent(path)?.join("registry-edit.lock"),
    )?)
}

fn running(path: &Path) -> Result<bool> {
    let lock_path = parent(path)?.join("daemon.lock");
    if !lock_path.exists() {
        return Ok(false);
    }
    let lock = OpenOptions::new().read(true).write(true).open(lock_path)?;
    match lock.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&lock)?;
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn status(path: &Path) -> Result<Value> {
    let registry = Registry::load(path)?;
    let connected = running(path)?;
    let previous = store::read_optional::<Value>(&parent(path)?.join("daemon-status.json"))?
        .unwrap_or_else(|| json!({}));
    let last_poll_at = previous.get("last_poll_at").and_then(Value::as_i64);
    Ok(
        json!({"host_id":registry.host.host_id,"running":connected,"connected_recently":connected && last_poll_at.is_some_and(|stamp| stamp > now() - 60),"last_poll_at":last_poll_at,"registry":path}),
    )
}

pub async fn start(path: &Path) -> Result<Value> {
    Registry::load(path)?;
    if running(path)? {
        return status(path);
    }
    let directory = parent(path)?;
    store::secure_directory(directory)?;
    let log_path = directory.join("daemon.log");
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let log = options.open(log_path)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("daemon")
        .arg("run")
        .arg("--registry")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200);
    }
    let mut child = command.spawn()?;
    for _ in 0..30 {
        if running(path)? {
            return status(path);
        }
        if let Some(exit) = child.try_wait()? {
            return Err(CliError::Input(format!(
                "Local daemon exited during startup ({exit}). Inspect {} for the reason, then run mcport daemon start.",
                directory.join("daemon.log").display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(CliError::Input(format!(
        "The daemon has not confirmed startup. Inspect {} and run mcport daemon status before starting another copy.",
        directory.join("daemon.log").display()
    )))
}

pub async fn stop(path: &Path) -> Result<Value> {
    if !running(path)? {
        return status(path);
    }
    store::write_protected(
        &parent(path)?.join("stop.request"),
        &json!({"requested_at":now()}),
    )?;
    for _ in 0..80 {
        if !running(path)? {
            return status(path);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(
        json!({"stop_requested":true,"running":true,"note":"The daemon is finishing cancellation and durable result handling. Inspect mcport daemon status; do not kill an unrelated process by a stale PID."}),
    )
}

pub async fn daemon_command(
    store: &Store,
    backend: &str,
    test_id: Option<&str>,
    command: DaemonCommand,
) -> Result<Value> {
    let paths = registry_files(store, backend, test_id)?;
    if paths.is_empty() {
        return Ok(
            json!({"hosts":[],"note":"No hosts are registered in this home/backend/test context. Run mcport host new <name>."}),
        );
    }
    let mut hosts = Vec::new();
    for path in paths {
        hosts.push(match command {
            DaemonCommand::Start => start(&path).await?,
            DaemonCommand::Stop => stop(&path).await?,
            DaemonCommand::Status => status(&path)?,
            DaemonCommand::Run { .. } => unreachable!("handled before local context loading"),
        });
    }
    Ok(json!({"hosts":hosts}))
}

pub fn account(
    store: &Store,
    backend: &str,
    test_id: Option<&str>,
    session: &StoredSession,
    connection: &Connection,
    command: AccountCommand,
) -> Result<Value> {
    let host_id = connection.host_id.as_deref().expect("local connection");
    let path = require_registry(store, backend, test_id, host_id, session)?;
    let _registry_lock = registry_lock(&path)?;
    let mut registry = Registry::load(&path)?;
    let registered = registry
        .connections
        .get_mut(&connection.id)
        .ok_or_else(|| {
            CliError::Input(
                "Connection is not registered in this host's local execution allowlist.".into(),
            )
        })?;
    if connection.auth_mode == "shared"
        && connection.owner_id != session.principal_id
        && !matches!(command, AccountCommand::Show { .. })
    {
        return Err(CliError::Input(
            "Only the connection owner can change its shared upstream account.".into(),
        ));
    }
    if connection.auth_mode == "none" && matches!(command, AccountCommand::Connect { .. }) {
        return Err(CliError::Input("This connection uses --auth none. Create a shared or per-user connection to configure credentials.".into()));
    }
    match command {
        AccountCommand::Connect {
            input,
            token,
            client_id,
            ..
        } => {
            if client_id.is_some() {
                return Err(CliError::Input("Local provider OAuth setup must run with the provider on its execution host. Use --token or --input @protected-config.json to save an existing provider grant locally.".into()));
            }
            if token && input.is_some() {
                return Err(CliError::Input(
                    "Use either --token or --input, not both.".into(),
                ));
            }
            let input = if token {
                let secret = rpassword::prompt_password(
                    "Provider bearer token (hidden; stored only on this host): ",
                )?;
                json!({"kind":"bearer","secret":secret})
            } else if let Some(input) = input {
                input_object(&input)?
            } else if connection.auth_mode == "shared" {
                json!({"kind":"host","label":"Execution host's existing application account"})
            } else {
                return Err(CliError::Input("Personal credentials for a local MCP must be configured on its trusted execution host. Use mcport account connect <connection> --token, or --input @protected-config.json. They will not be uploaded to MCPort.".into()));
            };
            let configured = local_account(input, &connection.transport, &connection.auth_mode)?;
            let label = configured.label.clone();
            if connection.auth_mode == "shared" {
                registered.shared_account = Some(configured);
                registered.shared_account_disconnected = false;
            } else {
                registered
                    .personal_accounts
                    .insert(session.principal_id.clone(), configured);
            }
            registry.save(&path)?;
            Ok(
                json!({"connected":true,"owner_id":session.principal_id,"label":label,"kind":"local","credentials_uploaded":false,"provider_verified":false}),
            )
        }
        AccountCommand::Disconnect { .. } => {
            if connection.auth_mode == "shared" {
                registered.shared_account = None;
                registered.shared_account_disconnected = true;
            } else {
                registered.personal_accounts.remove(&session.principal_id);
            }
            registry.save(&path)?;
            Ok(
                json!({"disconnected":true,"note":"Saved MCPort credentials were removed. A desktop application's own session remains controlled by that application."}),
            )
        }
        AccountCommand::Show { .. } => {
            let account = if connection.auth_mode == "shared" {
                registered.shared_account.as_ref()
            } else {
                registered.personal_accounts.get(&session.principal_id)
            };
            Ok(
                json!({"connected":account.is_some(),"owner_id":if connection.auth_mode == "shared" { &connection.owner_id } else { &session.principal_id },"label":account.map(|a|a.label.as_str()),"kind":"local","uses_host_account":connection.auth_mode == "shared" && account.is_none() && !registered.shared_account_disconnected,"disconnected":registered.shared_account_disconnected,"provider_verified":false}),
            )
        }
    }
}

fn local_account(input: Value, transport: &str, auth_mode: &str) -> Result<LocalAccount> {
    let kind = input
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("bearer");
    if kind == "host" && auth_mode != "shared" {
        return Err(CliError::Input("The execution host's existing account can only be used by an explicitly shared connection.".into()));
    }
    if (matches!(kind, "bearer" | "header") && transport != "http")
        || (kind == "env" && transport != "stdio")
    {
        return Err(CliError::Input("HTTP MCPs accept bearer or header credentials; stdio MCPs accept environment credentials.".into()));
    }
    let mut account = LocalAccount {
        label: input
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("Configured local account")
            .to_owned(),
        ..LocalAccount::default()
    };
    match kind {
        "bearer" => {
            let secret = input
                .get("secret")
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| {
                    CliError::Input("Bearer configuration requires a nonempty secret field.".into())
                })?;
            account.bearer_token = Some(secret.into());
        }
        "header" => {
            let name = input
                .get("header_name")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    CliError::Input("Header configuration requires header_name.".into())
                })?;
            let secret = input
                .get("secret")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| CliError::Input("Header configuration requires secret.".into()))?;
            account.headers.insert(name.into(), secret.into());
        }
        "env" => {
            account.env = serde_json::from_value(input.get("env").cloned().ok_or_else(|| {
                CliError::Input(
                    "Environment configuration requires an env object of key/value strings.".into(),
                )
            })?)?;
        }
        "host" => {}
        _ => {
            return Err(CliError::Input(
                "Local account kind must be bearer, header, env or host.".into(),
            ));
        }
    }
    if kind == "env"
        && (account.env.is_empty()
            || account.env.iter().any(|(key, value)| {
                key.is_empty() || key.contains(['=', '\0']) || value.contains('\0')
            }))
    {
        return Err(CliError::Input(
            "Environment credentials need nonempty valid variable names and at least one value."
                .into(),
        ));
    }
    Ok(account)
}

#[cfg(test)]
mod credential_tests {
    use super::*;
    #[test]
    fn credential_kind_matches_transport_and_personal_grants_are_explicit() {
        assert!(local_account(json!({"kind":"host"}), "http", "per-user").is_err());
        assert!(
            local_account(
                json!({"kind":"bearer","secret":"fixture"}),
                "stdio",
                "shared"
            )
            .is_err()
        );
        assert!(
            local_account(
                json!({"kind":"env","env":{"TOKEN":"fixture"}}),
                "http",
                "shared"
            )
            .is_err()
        );
        assert!(local_account(json!({"kind":"env","env":{}}), "stdio", "per-user").is_err());
        assert!(
            local_account(
                json!({"kind":"env","env":{"TOKEN":"fixture"}}),
                "stdio",
                "per-user"
            )
            .is_ok()
        );
        assert!(local_account(json!({"kind":"host"}), "http", "shared").is_ok());
    }
}
