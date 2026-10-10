//! Host registries in this home: where they live, the daemon that serves them, local
//! provider accounts and the one-time migration of registries written before 0.3.0.
use crate::{
    CliError, Result,
    args::{AccountCommand, DaemonCommand},
    input_object,
    store::{self, Store},
};
use mcport_client::local::{Registry, Scope};
use mcport_client::{Client, Connection, Host, RequestContext};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub fn registry_path(store: &Store, backend: &str, host_id: &str) -> PathBuf {
    // Server-issued host IDs are opaque identifiers, never paths.
    let safe_id = store::context_key(host_id);
    store
        .context_path(backend, "hosts")
        .join(safe_id)
        .join("registry.json")
}

/// The registry of `host_id` in this home, checked to be for this backend and keyed
/// by uuid (registries written before 0.3.0 must be migrated first).
pub fn require_registry(
    store: &Store,
    backend: &str,
    host_id: &str,
    account_uuid: &str,
) -> Result<PathBuf> {
    let path = registry_path(store, backend, host_id);
    if !path.is_file() {
        return Err(CliError::Input(format!(
            "This host's local registry is not in this home ({}). Configure local execution on the host's own machine, in the home that ran mcport host new; the backend cannot authorize a local process remotely.",
            store.home.display()
        )));
    }
    let registry = Registry::load(&path)?;
    scope(backend, account_uuid).validate_registry(&registry, host_id)?;
    Ok(path)
}

fn registry_files(store: &Store, backend: &str) -> Result<Vec<PathBuf>> {
    let folder = store.context_path(backend, "hosts");
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

pub fn scope(backend: &str, account_uuid: &str) -> Scope {
    Scope {
        backend_url: backend.into(),
        account_uuid: account_uuid.into(),
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
    let mut value = mcport_client::local::status(path)?;
    let registry = Registry::load(path)?;
    value["registry_version"] = json!(registry.version);
    if registry.is_legacy() {
        value["migrate"] = json!(format!(
            "Registered before Silicon Accounts: run mcport host migrate {} on this machine.",
            registry.host.host_id
        ));
    }
    Ok(value)
}

pub async fn daemon_command(store: &Store, backend: &str, command: DaemonCommand) -> Result<Value> {
    let paths = registry_files(store, backend)?;
    if paths.is_empty() {
        return Ok(
            json!({"hosts":[],"note":"No hosts are registered in this home for this backend. Run mcport host new <name>."}),
        );
    }
    let mut hosts = Vec::new();
    for path in paths {
        hosts.push(match command {
            DaemonCommand::Start => {
                start(&path).await?;
                status(&path)?
            }
            DaemonCommand::Stop => {
                stop(&path).await?;
                status(&path)?
            }
            DaemonCommand::Status => status(&path)?,
            DaemonCommand::Run { .. } => unreachable!("handled before local context loading"),
        });
    }
    Ok(json!({"hosts":hosts}))
}

/// `account connect|disconnect|show` for a local connection: credentials stay in the
/// host's registry on this machine. `registry_store` is this home, or `--host-home`.
pub fn account(
    registry_store: &Store,
    backend: &str,
    account_uuid: &str,
    connection: &Connection,
    command: AccountCommand,
) -> Result<Value> {
    let host_id = connection.host_id.as_deref().expect("local connection");
    let path = require_registry(registry_store, backend, host_id, account_uuid)?;
    let _registry_lock = registry_lock(&path)?;
    let mut registry = Registry::load(&path)?;
    let scope = scope(backend, account_uuid);
    let output = match command {
        AccountCommand::Connect {
            input,
            token,
            client_id,
            ..
        } => {
            if client_id.is_some() {
                return Err(CliError::Input("Provider OAuth for a local MCP must run with the provider on its host. Use --token or --input @protected-config.json to save an existing provider credential locally.".into()));
            }
            if token && input.is_some() {
                return Err(CliError::Input(
                    "Use either --token or --input, not both.".into(),
                ));
            }
            let input = if token {
                let secret = rpassword::prompt_password(
                    "Provider bearer token (hidden; kept only on this host): ",
                )?;
                json!({"kind":"bearer","secret":secret})
            } else if let Some(input) = input {
                input_object(&input)?
            } else if connection.auth_mode == "shared" {
                json!({"kind":"host","label":"The host's existing application account"})
            } else {
                return Err(CliError::Input("Personal credentials for a local MCP are configured on its host. Use --token or --input @protected-config.json; they are never uploaded.".into()));
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

/// `host migrate`: rewrite a registry written before 0.3.0 so its personal provider
/// accounts are keyed by uuid, keeping a copy of the original.
pub async fn migrate_host(
    client: &Client,
    ctx: &RequestContext,
    store: &Store,
    account_uuid: &str,
    host: &Host,
    dry_run: bool,
    drop_unmapped: bool,
) -> Result<Value> {
    let backend = client.backend_url();
    if host.owner.uuid != account_uuid {
        return Err(CliError::Input(format!(
            "Only the host's owner ({}) can migrate its local registry. Run this on the host's machine, signed in as the owner.",
            if host.owner.id.is_empty() {
                &host.owner.uuid
            } else {
                &host.owner.id
            }
        )));
    }
    let path = registry_path(store, &backend, &host.id);
    if !path.is_file() {
        return Err(CliError::Input(format!(
            "Host {} has no local registry in this home ({}). Run host migrate on the host's machine, in the home that registered it (SILICON_HOME or mcport config home).",
            host.name,
            store.home.display()
        )));
    }
    let _registry_lock = registry_lock(&path)?;
    let registry = Registry::load(&path)?;
    let scope = scope(&backend, account_uuid);
    scope.validate_host(&registry, &host.id)?;
    if !registry.is_legacy() {
        return Ok(
            json!({"migrated":false,"already_migrated":true,"host_id":host.id,"registry_version":registry.version,"registry":path}),
        );
    }
    let linked = client.legacy_host_accounts(ctx, &host.id).await?;
    let mut mapping: BTreeMap<String, String> = linked
        .accounts
        .iter()
        .map(|entry| (entry.legacy_id.clone(), entry.account.uuid.clone()))
        .collect();
    // The registry's own owner key is the signed-in owner, whom the backend linked.
    mapping.insert(registry.host.owner_id.clone(), account_uuid.to_owned());
    let (migrated, report) =
        mcport_client::local::migrate_registry(&registry, account_uuid, &mapping)?;
    let ids: BTreeMap<&str, &str> = linked
        .accounts
        .iter()
        .map(|entry| (entry.account.uuid.as_str(), entry.account.id.as_str()))
        .collect();
    let accounts: Vec<Value> = report
        .moved
        .iter()
        .map(|(old, uuid)| json!({"old_key":old,"uuid":uuid,"id":ids.get(uuid.as_str()).copied().unwrap_or_default()}))
        .collect();
    let plan = json!({"host_id":host.id,"accounts":accounts,"unmapped":report.unmapped,"duplicates":report.duplicates,"registry":path});
    let refused = !report.unmapped.is_empty() && !drop_unmapped;
    if dry_run {
        let mut plan = plan;
        plan["dry_run"] = json!(true);
        plan["migrated"] = json!(false);
        if refused {
            plan["note"] = json!(
                "Without --drop-unmapped, the migration refuses because of the unmapped keys."
            );
        }
        return Ok(plan);
    }
    if refused {
        return Err(CliError::Coded {
            code: "unmapped_accounts".into(),
            message: format!(
                "{} local provider account(s) in this registry belong to old ids MCPort cannot link to an account: {}.",
                report.unmapped.len(),
                report.unmapped.join(", ")
            ),
            recovery: "If the host's daemon has not run since those accounts were added, start it (mcport daemon start), wait a minute and retry. Otherwise rerun with --drop-unmapped: those credentials are removed (the old registry is kept as registry.v1.json) and their accounts connect again with mcport account connect.".into(),
        });
    }
    let was_running = mcport_client::local::status(&path)?
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if was_running {
        let stopped = stop(&path).await?;
        if stopped.get("running").and_then(Value::as_bool) == Some(true) {
            return Err(CliError::Input("The host's daemon did not stop in time, so its registry was left unchanged. Retry when mcport daemon status shows it stopped.".into()));
        }
    }
    let backup = path.with_file_name("registry.v1.json");
    if !backup.exists() {
        store::write_bytes_protected(&backup, &fs::read(&path)?)?;
    }
    migrated.save(&path)?;
    let daemon = if was_running {
        Some(start(&path).await?)
    } else {
        None
    };
    let mut plan = plan;
    plan["migrated"] = json!(true);
    plan["registry_version"] = json!(migrated.version);
    plan["backup"] = json!(backup);
    plan["dropped"] = plan["unmapped"].clone();
    plan["daemon"] = json!(daemon);
    plan["next"] = json!(if was_running {
        "The daemon was restarted with this mcport."
    } else {
        "Start the daemon when this host should serve its connections: mcport daemon start."
    });
    Ok(plan)
}
