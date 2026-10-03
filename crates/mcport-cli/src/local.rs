use crate::{
    CliError, Result,
    args::{AccountCommand, DaemonCommand},
    input_object,
    store::{self, Store, StoredSession},
};
use mcport_client::Connection;
use mcport_client::local::Registry;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
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
    scope(backend, test_id, session).validate_registry(&registry, host_id)?;
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

pub fn scope(
    backend: &str,
    test_id: Option<&str>,
    session: &StoredSession,
) -> mcport_client::local::Scope {
    mcport_client::local::Scope {
        backend_url: backend.into(),
        environment: test_id.unwrap_or("production").into(),
        principal_id: session.principal_id.clone(),
        org_id: session.org_id.clone(),
    }
}
pub fn registry_lock(path: &Path) -> Result<std::fs::File> {
    Ok(mcport_client::local::lock_registry(path)?)
}
pub async fn start(path: &Path) -> Result<Value> {
    Ok(mcport_client::local::start(path, &std::env::current_exe()?).await?)
}
pub async fn stop(path: &Path) -> Result<Value> {
    Ok(mcport_client::local::stop(path).await?)
}
fn status(path: &Path) -> Result<Value> {
    Ok(mcport_client::local::status(path)?)
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
    let scope = scope(backend, test_id, session);
    let output = match command {
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
                return Err(CliError::Input("Personal credentials for a local MCP must be configured on its trusted execution host. Use --token or --input @protected-config.json; credentials will not be uploaded.".into()));
            };
            mcport_client::local::connect_account(&mut registry, connection, &scope, input)?
        }
        AccountCommand::Disconnect { .. } => {
            mcport_client::local::disconnect_account(&mut registry, connection, &scope)?
        }
        AccountCommand::Show { .. } => {
            return Ok(mcport_client::local::account_status(
                &registry, connection, &scope,
            )?);
        }
    };
    registry.save(&path)?;
    Ok(output)
}
