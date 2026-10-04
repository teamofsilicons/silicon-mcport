mod args;
mod local;
mod store;

use args::*;
use clap::Parser;
use mcport_client::{
    ApiError, Client, ConnectionInput, ConnectionUpdate, RequestContext, Session, ToolPolicy,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};
use store::{Settings, Store, StoredSession};

const DEFAULT_BACKEND: &str = "https://backend.mcport.teamofsilicons.com";

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error(transparent)]
    Client(#[from] mcport_client::Error),
    #[error(transparent)]
    Store(#[from] store::StoreError),
    #[error(transparent)]
    Daemon(#[from] mcport_client::local::DaemonError),
    #[error(transparent)]
    Local(#[from] mcport_client::local::Error),
    #[error("{0}")]
    Input(String),
    #[error("Local I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("JSON could not be processed: {0}")]
    Json(#[from] serde_json::Error),
}
impl CliError {
    fn public(&self) -> ApiError {
        match self {
            Self::Client(error) => error.public(),
            _ => ApiError {
                code: "cli_error".into(),
                message: self.to_string(),
                recovery: Some(
                    "Use mcport <service> --help to inspect requirements and commands.".into(),
                ),
                outcome_unknown: false,
            },
        }
    }
}

type Result<T> = std::result::Result<T, CliError>;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let machine = cli.json;
    if let Command::Docs { topic } = &cli.command
        && !machine
    {
        println!("{}", topic.content());
        return;
    }
    match run(cli).await {
        Ok(value) => {
            let failed = value
                .pointer("/result/isError")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || value.get("status").and_then(Value::as_str) == Some("delivery_failed");
            if let Err(error) = print_value(&value, machine)
                && !matches!(&error, CliError::Io(e) if e.kind() == io::ErrorKind::BrokenPipe)
            {
                eprintln!("{error}");
                std::process::exit(1);
            }
            if failed {
                std::process::exit(1);
            }
        }
        Err(error) => {
            let public = error.public();
            if machine {
                // stdout remains valid machine-readable output even on failure.
                let _ = print_value(&json!({"error":public}), true);
            } else {
                eprintln!("Error [{}]: {}", public.code, public.message);
                if let Some(recovery) = public.recovery {
                    eprintln!("Recovery: {recovery}");
                }
                if public.outcome_unknown {
                    eprintln!(
                        "Outcome unknown: do not repeat a mutating action until its outcome is checked."
                    );
                }
            }
            std::process::exit(1);
        }
    }
}

fn print_value(value: &Value, compact: bool) -> Result<()> {
    let output = if compact {
        serde_json::to_string(value)?
    } else {
        serde_json::to_string_pretty(value)?
    };
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{output}")?;
    Ok(())
}

fn value<T: Serialize>(item: T) -> Result<Value> {
    Ok(serde_json::to_value(item)?)
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn stored(session: Session) -> Result<StoredSession> {
    Ok(StoredSession {
        principal_id: session.actor.principal_id.clone(),
        org_id: session.actor.org_id.clone(),
        access_token: session.access_token,
        refresh_token: Some(session.refresh_token),
        expires_at: Some(session.expires_at),
        identity: value(session.actor)?,
    })
}

fn public_context(test_id: Option<String>, settings: &Settings) -> RequestContext {
    RequestContext {
        access_token: None,
        test_id,
        isi: std::env::var("ISI").ok().filter(|v| !v.is_empty()),
        telemetry: Some(settings.telemetry),
    }
}

async fn authenticated(
    client: &Client,
    store: &Store,
    base: &RequestContext,
) -> Result<(RequestContext, StoredSession)> {
    let mut guard = store.sessions(&client.backend_url(), base.test_id.as_deref())?;
    let mut session = guard.active().cloned().ok_or_else(|| CliError::Input("Not authenticated in this backend and testing context. Run mcport iam --json, obtain an app-bound SLT, then run mcport login <slt>.".into()))?;
    if session
        .expires_at
        .is_some_and(|expiry| expiry <= now() + 30)
    {
        let token = session.refresh_token.as_deref().ok_or_else(|| {
            CliError::Input(
                "Session expired and no refresh token is stored. Run mcport login <slt>.".into(),
            )
        })?;
        let refreshed = client.refresh(base, token).await?;
        if refreshed.actor.principal_id != session.principal_id
            || refreshed.actor.org_id != session.org_id
        {
            return Err(CliError::Input("Refresh returned a different identity or organization. The stored session was preserved; sign in again and report this server error.".into()));
        }
        let expected = base.test_id.as_deref().unwrap_or("production");
        if refreshed.environment != expected {
            return Err(CliError::Input("Refresh returned a different testing environment. The stored session was preserved.".into()));
        }
        session = stored(refreshed)?;
        guard.save(session.clone())?;
    }
    let mut context = base.clone();
    context.access_token = Some(session.access_token.clone());
    Ok((context, session))
}

async fn run(cli: Cli) -> Result<Value> {
    if let Command::Docs { topic } = &cli.command {
        return Ok(json!({"topic":topic.name(),"content":topic.content()}));
    }
    if let Command::Daemon(DaemonCommand::Run { registry }) = &cli.command {
        let cancellation = tokio_util::sync::CancellationToken::new();
        let signal = cancellation.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal.cancel();
            }
        });
        mcport_client::local::run(registry, cancellation).await?;
        return Ok(json!({"stopped":true}));
    }
    let store = Store::discover()?;
    let mut settings = store.settings()?;
    if let Command::Config(ConfigCommand::Home { location }) = &cli.command {
        let home = store.change_home(location)?;
        return Ok(
            json!({"home":home,"directory":home.join(".mcport/dir"),"credentials_copied":false}),
        );
    }
    let backend = cli
        .url
        .as_deref()
        .or(settings.backend_url.as_deref())
        .unwrap_or(DEFAULT_BACKEND);
    if let Command::Config(ConfigCommand::Show) = &cli.command {
        return Ok(
            json!({"home":store.home,"directory":store.directory,"telemetry":settings.telemetry,"backend_url":backend,"test_id":cli.test_id}),
        );
    }
    let client = Client::new(backend)?;
    let base = public_context(cli.test_id.clone(), &settings);

    match cli.command {
        Command::Iam => return Ok(client.iam(&base).await?),
        Command::Login(LoginArgs { slt: Some(slt), action: None }) => {
            let session = client.login(&base, &slt).await?;
            if session.environment != base.test_id.as_deref().unwrap_or("production") {
                return Err(CliError::Input("Login returned a session for a different testing environment. No credentials were stored.".into()));
            }
            let output = json!({"authenticated":true,"actor":session.actor,"environment":session.environment,"expires_at":session.expires_at});
            store.sessions(&client.backend_url(), base.test_id.as_deref())?.save(stored(session)?)?;
            Ok(output)
        }
        Command::Login(LoginArgs { action: Some(LoginAction::Status), .. }) => {
            let current = store.sessions(&client.backend_url(), base.test_id.as_deref())?.active().cloned();
            if current.is_none() { return Ok(json!({"authenticated":false,"environment":base.test_id.as_deref().unwrap_or("production")})); }
            let (context, _) = authenticated(&client, &store, &base).await?;
            return Ok(client.status(&context).await?);
        }
        Command::Login(_) => Err(CliError::Input("A short-lived token is required: mcport login <slt>. To inspect a session, run mcport login status --json.".into())),
        Command::Session(SessionCommand::Ls) => Ok(store.sessions(&client.backend_url(), base.test_id.as_deref())?.list()),
        Command::Session(SessionCommand::Use { principal, org }) => Ok(store.sessions(&client.backend_url(), base.test_id.as_deref())?.select(&principal, org.as_deref())?),
        Command::Daemon(command) => return local::daemon_command(&store, &client.backend_url(), base.test_id.as_deref(), command).await,
        Command::Config(ConfigCommand::Set { key, value: setting }) => {
            match key.as_str() {
                "telemetry" => {
                    settings.telemetry = setting.parse::<bool>().map_err(|_| CliError::Input("Telemetry must be true or false: mcport config set telemetry false".into()))?;
                    store.save_settings(&settings)?;
                    let has_session = store.sessions(&client.backend_url(), base.test_id.as_deref())?.active().is_some();
                    if has_session {
                        let (mut ctx, _) = authenticated(&client, &store, &base).await?;
                        ctx.telemetry = Some(settings.telemetry);
                        client.set_telemetry(&ctx, settings.telemetry).await?;
                    }
                    Ok(json!({"telemetry":settings.telemetry,"server_updated":has_session}))
                }
                "url" | "backend" | "backend_url" => {
                    let validated = Client::new(&setting)?;
                    settings.backend_url = Some(validated.backend_url());
                    store.save_settings(&settings)?;
                    Ok(json!({"backend_url":settings.backend_url,"note":"Sessions are separate for each backend."}))
                }
                _ => Err(CliError::Input(format!("Unknown setting {key}. Supported settings: telemetry, backend. Use mcport config home <location> to change the storage home."))),
            }
        }
        command => {
            let (context, session) = authenticated(&client, &store, &base).await?;
            dispatch(&client, &context, &store, &session, command).await
        }
    }
}

async fn dispatch(
    client: &Client,
    ctx: &RequestContext,
    store: &Store,
    session: &StoredSession,
    command: Command,
) -> Result<Value> {
    let operation = match &command {
        Command::Logout => "logout",
        Command::Connection(ConnectionCommand::New { .. } | ConnectionCommand::Register { .. }) => {
            "connection.create"
        }
        Command::Connection(ConnectionCommand::Ls) => "connection.list",
        Command::Connection(ConnectionCommand::Show { .. }) => "connection.read",
        Command::Connection(ConnectionCommand::Set { .. }) => "connection.update",
        Command::Connection(ConnectionCommand::Rm { .. }) => "connection.delete",
        Command::Tool(ToolCommand::Call { .. }) => "tool.call",
        Command::Tool(ToolCommand::Set { .. }) => "tool.policy",
        Command::Tool(_) => "tool.list",
        Command::Account(AccountCommand::Disconnect { .. }) => "account.disconnect",
        Command::Account(_) => "account.connect",
        Command::Access(AccessCommand::Rm { .. }) => "access.revoke",
        Command::Access(_) => "access.grant",
        Command::Host(_) => "host.register",
        Command::Resource(ResourceCommand::Read { .. }) => "resource.read",
        Command::Resource(_) => "resource.list",
        Command::Prompt(PromptCommand::Get { .. }) => "prompt.get",
        Command::Prompt(_) => "prompt.list",
        Command::Completion(_) => "completion.complete",
        Command::Activity(ActivityCommand::Cancel { .. }) => "activity.cancel",
        Command::Activity(_) | Command::Asset(_) => "activity.read",
        Command::Report { .. } => "report.submit",
        _ => "navigation",
    };
    let started = std::time::Instant::now();
    let result = dispatch_inner(client, ctx, store, session, command).await;
    if ctx.telemetry != Some(false) {
        let failed = result.is_err()
            || result.as_ref().is_ok_and(|value| {
                value.pointer("/result/isError").and_then(Value::as_bool) == Some(true)
            });
        let event = json!({"source":"cli","operation":operation,"step":"complete","outcome":if failed {"failure"} else {"success"},"progress":1.0,"duration_ms":started.elapsed().as_millis().min(86_400_000) as u64,"correlation_id":uuid::Uuid::new_v4().to_string()});
        // Telemetry cannot alter the operation's result or delay a short-lived CLI indefinitely.
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            client.telemetry(ctx, &event),
        )
        .await;
    }
    result
}

async fn dispatch_inner(
    client: &Client,
    ctx: &RequestContext,
    store: &Store,
    session: &StoredSession,
    command: Command,
) -> Result<Value> {
    match command {
        Command::Logout => {
            let response = client.logout(ctx).await?;
            store
                .sessions(&client.backend_url(), ctx.test_id.as_deref())?
                .remove_active()?;
            Ok(response)
        }
        Command::Connection(command) => match command {
            ConnectionCommand::Register {
                connection,
                environment,
            } => {
                let connection = client.connection(ctx, &connection).await?;
                if connection.owner_id != session.principal_id
                    || connection.org_id != session.org_id
                    || connection.environment != ctx.test_id.as_deref().unwrap_or("production")
                {
                    return Err(CliError::Input("Only the connection owner in its organization and testing environment can approve local execution.".into()));
                }
                let host_id = connection.host_id.as_deref().ok_or_else(|| CliError::Input("This is a cloud connection. Only connections assigned to a local host need registration.".into()))?;
                let host = client.host(ctx, host_id).await?;
                if host.owner_id != session.principal_id || host.org_id != session.org_id {
                    return Err(CliError::Input("The connection's execution host is not owned by this account and organization.".into()));
                }
                let path = local::require_registry(
                    store,
                    &client.backend_url(),
                    ctx.test_id.as_deref(),
                    host_id,
                    session,
                )?;
                let _registry_lock = local::registry_lock(&path)?;
                let mut registry = mcport_client::local::Registry::load(&path)?;
                if registry.host.owner_id != session.principal_id {
                    return Err(CliError::Input(
                        "The local host registry belongs to another account.".into(),
                    ));
                }
                let added = mcport_client::local::register_connection(
                    &mut registry,
                    &connection,
                    &local::scope(&client.backend_url(), ctx.test_id.as_deref(), session),
                    parse_environment(environment)?,
                )?;
                if !added {
                    return Ok(
                        json!({"registered":true,"already_registered":true,"connection_id":connection.id,"host_id":host_id,"note":"Existing local credentials and process configuration were preserved."}),
                    );
                }
                registry.save(&path)?;
                Ok(
                    json!({"registered":true,"connection_id":connection.id,"host_id":host_id,"connection":connection}),
                )
            }
            ConnectionCommand::Ls => value(client.connections(ctx).await?),
            ConnectionCommand::Show { connection } => {
                value(client.connection(ctx, &connection).await?)
            }
            ConnectionCommand::New {
                name,
                description,
                transport,
                endpoint,
                host,
                command,
                arguments,
                environment,
                auth,
                visibility,
            } => {
                match transport {
                    Transport::Http if endpoint.is_none() => return Err(CliError::Input("HTTP connections require --url <mcp-url>. Use --host <host> for local-only endpoints.".into())),
                    Transport::Stdio if host.is_none() || command.is_none() => return Err(CliError::Input("stdio connections require --host <registered-host> and --command <absolute-executable-path>. First run mcport host new <name> on that machine.".into())),
                    _ => {}
                }
                if matches!(transport, Transport::Http)
                    && (command.is_some() || !arguments.is_empty() || !environment.is_empty())
                {
                    return Err(CliError::Input(
                        "--command, --arg and --env apply only to stdio connections.".into(),
                    ));
                }
                let env = parse_environment(environment)?;
                let host_record = match &host {
                    Some(id) => Some(client.host(ctx, id).await?),
                    None => None,
                };
                let local_path = if let Some(host) = &host_record {
                    Some(local::require_registry(
                        store,
                        &client.backend_url(),
                        ctx.test_id.as_deref(),
                        &host.id,
                        session,
                    )?)
                } else {
                    None
                };
                if let Some(command) = &command
                    && !std::path::Path::new(command).is_absolute()
                {
                    return Err(CliError::Input("--command must be an absolute executable path; MCPort never runs it through a shell.".into()));
                }
                let input = ConnectionInput {
                    name,
                    description,
                    transport: transport.value().into(),
                    url: endpoint.clone(),
                    host_id: host_record.map(|h| h.id),
                    command: command.clone(),
                    args: arguments.clone(),
                    auth_mode: auth.value().into(),
                    visibility: visibility.value().into(),
                };
                let created = client.create_connection(ctx, &input).await?;
                if let Some(path) = local_path {
                    let _registry_lock = local::registry_lock(&path)?;
                    let mut registry = mcport_client::local::Registry::load(&path)?;
                    mcport_client::local::register_connection(
                        &mut registry,
                        &created,
                        &local::scope(&client.backend_url(), ctx.test_id.as_deref(), session),
                        env,
                    )?;
                    if let Err(error) = registry.save(&path) {
                        return Err(CliError::Input(format!(
                            "Connection {} was saved centrally, but its local registration failed: {error}. It cannot execute. Delete it with mcport connection rm {} and retry after fixing local storage.",
                            created.id, created.id
                        )));
                    }
                }
                value(created)
            }
            ConnectionCommand::Set {
                connection,
                name,
                description,
                visibility,
            } => {
                if name.is_none() && description.is_none() && visibility.is_none() {
                    return Err(CliError::Input(
                        "Choose at least one setting: --name, --description or --visibility."
                            .into(),
                    ));
                }
                let existing = client.connection(ctx, &connection).await?;
                value(
                    client
                        .update_connection(
                            ctx,
                            &existing.id,
                            &ConnectionUpdate {
                                name,
                                description,
                                visibility: visibility.map(|v| v.value().into()),
                                version: Some(existing.version),
                            },
                        )
                        .await?,
                )
            }
            ConnectionCommand::Rm { connection } => {
                let existing = client.connection(ctx, &connection).await?;
                let response = client.delete_connection(ctx, &existing.id).await?;
                if let Some(host_id) = &existing.host_id {
                    let path = local::registry_path(
                        store,
                        &client.backend_url(),
                        ctx.test_id.as_deref(),
                        host_id,
                    );
                    if path.exists() {
                        let _registry_lock = local::registry_lock(&path)?;
                        let mut registry = mcport_client::local::Registry::load(&path)?;
                        mcport_client::local::unregister_connection(
                            &mut registry,
                            &existing,
                            &local::scope(&client.backend_url(), ctx.test_id.as_deref(), session),
                        )?;
                        registry.save(&path)?;
                    }
                }
                Ok(response)
            }
        },
        Command::Tool(command) => match command {
            ToolCommand::Ls { connection, cursor } => {
                value(client.tools(ctx, &connection, cursor.as_deref()).await?)
            }
            ToolCommand::Show { connection, tool } => {
                Ok(client.tool(ctx, &connection, &tool).await?)
            }
            ToolCommand::Call {
                connection,
                tool,
                input,
                idempotency_key,
                timeout_ms,
            } => {
                let arguments = input_object(&input)?;
                let request = mcport_client::RpcInput {
                    method: "tools/call".into(),
                    params: json!({"name":tool,"arguments":arguments}),
                    idempotency_key,
                    timeout_ms,
                };
                let future = client.rpc(ctx, &connection, &request);
                tokio::select! {
                    output = future => value(output?),
                    _ = tokio::signal::ctrl_c() => Err(CliError::Client(mcport_client::Error::Transport { message: "Stopped waiting for the invocation. The server may still be executing it; inspect mcport activity ls and use mcport activity cancel <id>.".into(), outcome_unknown: true })),
                }
            }
            ToolCommand::Set {
                connection,
                tool,
                enabled,
                principal,
            } => value(
                client
                    .set_tool_policy(
                        ctx,
                        &connection,
                        &ToolPolicy {
                            tool,
                            principal_id: principal,
                            enabled,
                        },
                    )
                    .await?,
            ),
        },
        Command::Account(command) => {
            let connection_name = match &command {
                AccountCommand::Connect { connection, .. }
                | AccountCommand::Disconnect { connection }
                | AccountCommand::Show { connection } => connection,
            };
            let connection = client.connection(ctx, connection_name).await?;
            if let Some(host_id) = &connection.host_id {
                if matches!(command, AccountCommand::Show { .. })
                    && !local::registry_path(
                        store,
                        &client.backend_url(),
                        ctx.test_id.as_deref(),
                        host_id,
                    )
                    .is_file()
                {
                    return value(client.account(ctx, &connection.id).await?);
                }
                return local::account(
                    store,
                    &client.backend_url(),
                    ctx.test_id.as_deref(),
                    session,
                    &connection,
                    command,
                );
            }
            match command {
                AccountCommand::Show { .. } => value(client.account(ctx, &connection.id).await?),
                AccountCommand::Disconnect { .. } => {
                    Ok(client.disconnect_account(ctx, &connection.id).await?)
                }
                AccountCommand::Connect {
                    input,
                    token,
                    client_id,
                    ..
                } => {
                    if token && input.is_some() {
                        return Err(CliError::Input(
                            "Use either --token or --input, not both.".into(),
                        ));
                    }
                    if token {
                        let secret =
                            rpassword::prompt_password("Provider bearer token (hidden): ")?;
                        if secret.trim().is_empty() {
                            return Err(CliError::Input(
                                "Provider token must not be empty.".into(),
                            ));
                        }
                        value(
                            client
                                .connect_account(
                                    ctx,
                                    &connection.id,
                                    &json!({"kind":"bearer","secret":secret}),
                                )
                                .await?,
                        )
                    } else if let Some(input) = input {
                        value(
                            client
                                .connect_account(ctx, &connection.id, &input_object(&input)?)
                                .await?,
                        )
                    } else {
                        let mut flow = client
                            .authorize_account(ctx, &connection.id, client_id.as_deref())
                            .await?;
                        if let Some(map) = flow.as_object_mut() {
                            map.insert("next_step".into(), json!("Open authorization_url to authorize the upstream provider. After consent, run mcport account show <connection> to verify the linked account."));
                        }
                        Ok(flow)
                    }
                }
            }
        }
        Command::Access(command) => match command {
            AccessCommand::New {
                connection,
                principal,
            } => value(client.grant_access(ctx, &connection, &principal).await?),
            AccessCommand::Ls { connection } => value(client.access(ctx, &connection).await?),
            AccessCommand::Rm {
                connection,
                principal,
            } => Ok(client.revoke_access(ctx, &connection, &principal).await?),
        },
        Command::Host(command) => match command {
            HostCommand::Ls => value(client.hosts(ctx).await?),
            HostCommand::Show { host } => value(client.host(ctx, &host).await?),
            HostCommand::New { name } => {
                let registration = client.create_host(ctx, &name).await?;
                let path = local::registry_path(
                    store,
                    &client.backend_url(),
                    ctx.test_id.as_deref(),
                    &registration.host.id,
                );
                let host = registration.host.clone();
                let registry = mcport_client::local::registry_for_host(
                    registration,
                    &local::scope(&client.backend_url(), ctx.test_id.as_deref(), session),
                    ctx.isi.clone(),
                )?;
                registry.save(&path)?;
                let daemon = local::start(&path).await?;
                Ok(json!({"host":host,"daemon":daemon}))
            }
            HostCommand::Rm { host } => {
                let host = client.host(ctx, &host).await?;
                let response = client.delete_host(ctx, &host.id).await?;
                let path = local::registry_path(
                    store,
                    &client.backend_url(),
                    ctx.test_id.as_deref(),
                    &host.id,
                );
                if path.exists() {
                    local::stop(&path).await?;
                    std::fs::remove_file(&path)?;
                }
                Ok(response)
            }
        },
        Command::Resource(command) => match command {
            ResourceCommand::Ls { connection, cursor } => value(
                client
                    .resources(ctx, &connection, cursor.as_deref())
                    .await?,
            ),
            ResourceCommand::Templates { connection, cursor } => value(
                client
                    .resource_templates(ctx, &connection, cursor.as_deref())
                    .await?,
            ),
            ResourceCommand::Read { connection, uri } => {
                value(client.read_resource(ctx, &connection, &uri).await?)
            }
        },
        Command::Prompt(command) => match command {
            PromptCommand::Ls { connection, cursor } => {
                value(client.prompts(ctx, &connection, cursor.as_deref()).await?)
            }
            PromptCommand::Get {
                connection,
                prompt,
                input,
            } => value(
                client
                    .get_prompt(ctx, &connection, &prompt, input_object(&input)?)
                    .await?,
            ),
        },
        Command::Completion(CompletionCommand::Get { connection, input }) => value(
            client
                .complete(ctx, &connection, input_object(&input)?)
                .await?,
        ),
        Command::Activity(command) => match command {
            ActivityCommand::Ls { connection } => {
                let id = match connection {
                    Some(connection) => Some(client.connection(ctx, &connection).await?.id),
                    None => None,
                };
                value(client.activity(ctx, id.as_deref()).await?)
            }
            ActivityCommand::Show { id } => value(client.invocation(ctx, &id).await?),
            ActivityCommand::Cancel { id } => value(client.cancel(ctx, &id).await?),
        },
        Command::Asset(command) => match command {
            AssetCommand::Ls { call } => value(client.assets(ctx, &call).await?),
            AssetCommand::Get {
                call,
                index,
                output,
            } => {
                let bytes = client.download_asset(ctx, &call, index).await?;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&output)?;
                if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
                    drop(file);
                    let _ = std::fs::remove_file(&output);
                    return Err(error.into());
                }
                Ok(
                    json!({"saved":true,"output":std::fs::canonicalize(output)?,"size":bytes.len(),"call_id":call,"index":index}),
                )
            }
        },
        Command::Report { message, pr } => {
            let mut output = client.report(ctx, &message, pr.as_deref()).await?;
            if pr.is_none() {
                let repository = output
                    .get("repository_url")
                    .and_then(Value::as_str)
                    .unwrap_or("https://github.com/teamofsilicons/silicon-mcport")
                    .to_owned();
                if let Some(map) = output.as_object_mut() {
                    map.insert(
                        "contribute".into(),
                        json!(format!(
                            "You can also reproduce the issue and submit a PR at {repository}."
                        )),
                    );
                }
            }
            Ok(output)
        }
        _ => Err(CliError::Input(
            "This command is not an authenticated application operation.".into(),
        )),
    }
}

pub(crate) fn input_object(input: &str) -> Result<Value> {
    let text = if input == "-" {
        let mut text = String::new();
        io::stdin().read_to_string(&mut text)?;
        text
    } else if let Some(path) = input.strip_prefix('@') {
        if path.is_empty() {
            return Err(CliError::Input(
                "@ input requires a file path, for example --input @request.json.".into(),
            ));
        }
        std::fs::read_to_string(path)
            .map_err(|e| CliError::Input(format!("Cannot read JSON input file {path}: {e}")))?
    } else {
        input.to_owned()
    };
    let value: Value = serde_json::from_str(&text).map_err(|error| CliError::Input(format!("Input is not valid JSON: {error}. Use --input '{{\"field\":\"value\"}}', @file.json, or - for stdin.")))?;
    if !value.is_object() {
        return Err(CliError::Input("MCP arguments must be a JSON object. Use mcport tool show <connection> <tool> for its schema.".into()));
    }
    Ok(value)
}

fn parse_environment(entries: Vec<String>) -> Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for entry in entries {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| CliError::Input("Each --env entry must have KEY=VALUE form.".into()))?;
        if key.is_empty() || key.contains('\0') || value.contains('\0') {
            return Err(CliError::Input(
                "Environment keys must be nonempty and entries cannot contain NUL.".into(),
            ));
        }
        env.insert(key.to_owned(), value.to_owned());
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_documented_grammars_parse() {
        let cases = [
            vec!["mcport", "login", "status", "--json"],
            vec!["mcport", "login", "slt-secret"],
            vec!["mcport", "docs"],
            vec!["mcport", "docs", "development", "--json"],
            vec!["mcport", "completion", "get", "notes", "--input", "{}"],
            vec![
                "mcport",
                "connection",
                "new",
                "docs",
                "--transport",
                "http",
                "--url",
                "https://example.test/mcp",
                "--auth",
                "none",
                "--visibility",
                "private",
            ],
            vec![
                "mcport",
                "tool",
                "set",
                "docs",
                "search",
                "--enabled",
                "false",
            ],
            vec![
                "mcport", "--test", "test-1", "tool", "call", "docs", "search", "--input", "-",
                "--json",
            ],
            vec!["mcport", "config", "home", "/tmp"],
            vec!["mcport", "config", "set", "telemetry", "false"],
        ];
        for case in cases {
            assert!(Cli::try_parse_from(case.clone()).is_ok(), "{case:?}");
        }
    }
    #[test]
    fn json_arguments_require_object_and_support_file() {
        assert!(input_object("[]").is_err());
        assert!(input_object("invalid").is_err());
        assert_eq!(
            input_object("{\"nested\":{\"x\":true}}").unwrap()["nested"]["x"],
            true
        );
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("input.json");
        std::fs::write(&file, "{\"x\":42}").unwrap();
        assert_eq!(
            input_object(&format!("@{}", file.display())).unwrap()["x"],
            42
        );
    }
}
