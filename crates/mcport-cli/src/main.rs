mod args;
mod local;
mod signin;
mod store;

use args::*;
use clap::Parser;
use mcport_client::accounts::{SILICON_SIGN_IN, SignInError};
use mcport_client::session::{SessionError, StoredSignIn};
use mcport_client::{
    AllowanceInput, ApiError, Client, ConnectionInput, ConnectionUpdate, DirectoryEntry,
    DirectoryInput, DirectoryUpdate, RequestContext, ToolPolicyInput,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use store::Store;

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
    #[error(transparent)]
    SignIn(#[from] SignInError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("Not signed in to MCPort at {backend}.")]
    NotSignedIn { backend: String, legacy: bool },
    #[error("{message}")]
    Coded {
        code: String,
        message: String,
        recovery: String,
    },
    #[error("{0}")]
    Input(String),
    #[error("Local I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("JSON could not be processed: {0}")]
    Json(#[from] serde_json::Error),
}

fn sign_in_recovery() -> String {
    format!("Carbons: mcport login. Silicons: {SILICON_SIGN_IN}")
}

impl CliError {
    fn public(&self) -> ApiError {
        let coded = |code: &str, message: String, recovery: String| ApiError {
            code: code.into(),
            message,
            recovery: Some(recovery),
            outcome_unknown: false,
        };
        match self {
            Self::Client(error) => error.public(),
            Self::SignIn(error) | Self::Session(SessionError::SignIn(error)) => {
                coded(&error.code(), error.message(), error.hint())
            }
            Self::Session(SessionError::NotSignedIn) => coded(
                "not_signed_in",
                "Not signed in to MCPort.".into(),
                sign_in_recovery(),
            ),
            Self::Session(error @ SessionError::Unreadable { .. }) => coded(
                "session_unreadable",
                error.to_string(),
                format!(
                    "Sign in again ({}); the new sign-in replaces the file.",
                    sign_in_recovery()
                ),
            ),
            Self::Session(error @ SessionError::LockTimeout { .. }) => coded(
                "session_busy",
                error.to_string(),
                "Wait for the other mcport command to finish, then retry.".into(),
            ),
            Self::NotSignedIn { backend, legacy } => coded(
                "not_signed_in",
                if *legacy {
                    format!(
                        "Not signed in to MCPort at {backend}: sign-ins from mcport 0.2 and earlier no longer work."
                    )
                } else {
                    format!("Not signed in to MCPort at {backend}.")
                },
                sign_in_recovery(),
            ),
            Self::Local(error @ mcport_client::local::Error::LegacyRegistry { host_id }) => coded(
                "registry_not_migrated",
                error.to_string(),
                format!(
                    "mcport host migrate {host_id} --dry-run, then mcport host migrate {host_id}"
                ),
            ),
            Self::Coded {
                code,
                message,
                recovery,
            } => coded(code, message.clone(), recovery.clone()),
            _ => coded(
                "cli_error",
                self.to_string(),
                "Run mcport <command> --help to see what the command needs.".into(),
            ),
        }
    }
}

type Result<T> = std::result::Result<T, CliError>;

fn main() {
    let cli = Cli::parse();
    let machine = cli.json;
    // Discovery and the bundled docs answer before any async runtime starts: no
    // threads, no network, no home needed and nothing written (Silicon Apps runs
    // these in a sandbox that allows none of that).
    match &cli.command {
        Command::Docs { topic } if !machine => {
            println!("{}", topic.content());
            return;
        }
        Command::Docs { topic } => {
            finish(
                Ok(json!({"topic":topic.name(),"content":topic.content()})),
                machine,
            );
        }
        Command::Accounts | Command::Iam => {
            finish(Ok(signin::accounts_json(&cli)), machine);
        }
        Command::Login(LoginArgs {
            action: Some(LoginAction::Status { offline }),
            ..
        }) => {
            let (value, signed_in) = signin::status(&cli, *offline);
            let _ = print_value(&value, machine);
            if !signed_in && !machine {
                eprintln!("Not signed in. {}", sign_in_recovery());
                std::process::exit(1);
            }
            return;
        }
        _ => {}
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => finish(Err(CliError::Io(error)), machine),
    };
    let result = runtime.block_on(run(cli));
    finish(result, machine);
}

fn finish(result: Result<Value>, machine: bool) -> ! {
    match result {
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
            std::process::exit(if failed { 1 } else { 0 });
        }
        Err(error) => {
            let public = error.public();
            if machine {
                // stdout stays valid machine-readable output even on failure.
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

fn request_context(session: &StoredSignIn, telemetry: bool) -> RequestContext {
    RequestContext {
        access_token: Some(session.access_token.expose().to_owned()),
        isi: std::env::var("ISI").ok().filter(|v| !v.is_empty()),
        telemetry: Some(telemetry),
    }
}

async fn run(cli: Cli) -> Result<Value> {
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
    match &cli.command {
        Command::Login(args) => return signin::login(&cli, args.clone()).await,
        Command::Logout => return signin::logout(&cli).await,
        _ => {}
    }
    let store = Store::discover()?;
    let mut settings = store.settings()?;
    if let Command::Config(ConfigCommand::Home { location }) = &cli.command {
        let home = store.change_home(location)?;
        return Ok(
            json!({"home":home,"directory":home.join(".mcport/dir"),"credentials_copied":false}),
        );
    }
    let target = signin::Target::resolve(&cli, &settings);
    let client = Client::new(&target.backend)?;
    let backend = client.backend_url();
    match cli.command {
        Command::Config(ConfigCommand::Show) => {
            let signed_in = signin::stored(&store, &backend)
                .map(|s| json!({"uuid":s.account.uuid,"id":s.account.id,"kind":s.account.kind}));
            Ok(
                json!({"home":store.home,"directory":store.directory,"telemetry":settings.telemetry,"backend_url":backend,"accounts_url":target.accounts_url,"app_id":target.app_id,"signed_in":signed_in}),
            )
        }
        Command::Config(ConfigCommand::Set {
            key,
            value: setting,
        }) => match key.as_str() {
            "telemetry" => {
                settings.telemetry = setting.parse::<bool>().map_err(|_| {
                    CliError::Input(
                        "Telemetry must be true or false: mcport config set telemetry false".into(),
                    )
                })?;
                store.save_settings(&settings)?;
                let signed_in = signin::stored(&store, &backend).is_some();
                if signed_in {
                    let session = signin::signed_in(&store, &backend, false).await?;
                    client
                        .set_telemetry(
                            &request_context(&session, settings.telemetry),
                            settings.telemetry,
                        )
                        .await?;
                }
                Ok(json!({"telemetry":settings.telemetry,"server_updated":signed_in}))
            }
            "url" | "backend" | "backend_url" => {
                let validated = Client::new(&setting)?;
                settings.backend_url = Some(validated.backend_url());
                store.save_settings(&settings)?;
                Ok(
                    json!({"backend_url":settings.backend_url,"note":"Sign-ins are kept per backend: sign in again for this one if needed."}),
                )
            }
            "accounts" | "accounts_url" => {
                let validated = mcport_client::accounts::SignIn::new(&setting, &target.app_id)?;
                settings.accounts_url = Some(validated.accounts_url());
                store.save_settings(&settings)?;
                Ok(
                    json!({"accounts_url":settings.accounts_url,"note":"Stored sign-ins keep using the Silicon Accounts that issued them; sign in again to use this one."}),
                )
            }
            _ => Err(CliError::Input(format!(
                "Unknown setting {key}. Supported settings: backend, accounts, telemetry. Use mcport config home <location> to change the storage home."
            ))),
        },
        Command::MigrateAccountUuids { file, apply } => {
            local::migrate_account_uuids(&store, &backend, &file, apply)
        }
        Command::Daemon(command) => local::daemon_command(&store, &backend, command).await,
        command => {
            let session = signin::signed_in(&store, &backend, false).await?;
            let context = request_context(&session, settings.telemetry);
            let first = dispatch(&client, &context, &store, &session, command.clone()).await;
            match first {
                // The service refused the token itself (expired early by clock skew, or a
                // sign-out it learned about): refresh once and run the command again. If
                // the sign-in really ended, the refresh says so and forgets it. A 401
                // means nothing was executed, so repeating is safe.
                Err(CliError::Client(error))
                    if error.status() == Some(401)
                        && signin::RETRY_CODES.iter().any(|code| error.is_code(code)) =>
                {
                    let session = signin::signed_in(&store, &backend, true).await?;
                    let context = request_context(&session, settings.telemetry);
                    dispatch(&client, &context, &store, &session, command).await
                }
                other => other,
            }
        }
    }
}

#[derive(Default)]
struct ConnectionDraft {
    name: String,
    description: Option<String>,
    transport: Option<Transport>,
    endpoint: Option<String>,
    host: Option<String>,
    command: Option<String>,
    arguments: Vec<String>,
    clear_args: bool,
    auth: Option<AuthMode>,
    visibility: Option<Visibility>,
}

/// `--visibility org` existed when MCPort grouped accounts into organizations.
fn visibility_value(visibility: Visibility) -> Result<&'static str> {
    match visibility {
        Visibility::Org => Err(CliError::Coded {
            code: "visibility_removed".into(),
            message: "--visibility org no longer exists: a connection is yours alone, shared with your own people (--visibility circle), or shared with chosen accounts.".into(),
            recovery: "Use --visibility circle (you and the Silicons you look after, or your custodian and its Silicons), or invite accounts with mcport access new <connection> --account <c:/si: id>.".into(),
        }),
        other => Ok(other.value()),
    }
}

impl ConnectionDraft {
    fn resolve(self, entry: Option<&DirectoryEntry>) -> Result<ConnectionInput> {
        let template = entry.and_then(|entry| entry.template.as_ref());
        let transport = self
            .transport
            .map(Transport::value)
            .or_else(|| template.map(|template| template.transport.as_str()))
            .ok_or_else(|| CliError::Input("This directory entry has no connection template. Supply --transport and an endpoint or local process configuration.".into()))?;
        if !matches!(transport, "http" | "stdio") {
            return Err(CliError::Input("Directory template has an unsupported transport. Choose --transport http or stdio explicitly.".into()));
        }
        let auth = self
            .auth
            .map(AuthMode::value)
            .or_else(|| template.map(|template| template.auth_mode.as_str()))
            .unwrap_or("none");
        if !matches!(auth, "none" | "per-user" | "shared") {
            return Err(CliError::Input("Directory template has an unsupported account mode. Choose --auth none, per-user or shared explicitly.".into()));
        }
        let visibility = match self.visibility {
            Some(visibility) => visibility_value(visibility)?,
            None => "invited",
        };
        let endpoint = self.endpoint.or_else(|| {
            template
                .filter(|template| template.transport == transport && transport == "http")
                .and_then(|template| template.url.clone())
        });
        let arguments = if self.clear_args {
            vec![]
        } else if !self.arguments.is_empty() {
            self.arguments
        } else {
            template
                .filter(|template| template.transport == transport && transport == "stdio")
                .map(|template| template.args.clone())
                .unwrap_or_default()
        };
        if transport == "http" {
            if endpoint.as_deref().is_none_or(|url| url.trim().is_empty()) {
                return Err(CliError::Input("HTTP connections require --url <mcp-url>. Use --host <host> for local-only endpoints.".into()));
            }
            if self.command.is_some() || !arguments.is_empty() {
                return Err(CliError::Input(
                    "--command and --arg apply only to stdio connections.".into(),
                ));
            }
        } else {
            if self.host.is_none() || self.command.is_none() {
                return Err(CliError::Input("stdio connections require --host <registered-host> and an explicit --command <absolute-executable-path>, including when using a directory template. First run mcport host new <name> on that machine.".into()));
            }
            if endpoint.is_some() {
                return Err(CliError::Input(
                    "--url applies only to HTTP connections.".into(),
                ));
            }
            if self
                .command
                .as_ref()
                .is_some_and(|command| !std::path::Path::new(command).is_absolute())
            {
                return Err(CliError::Input("--command must be an absolute executable path; MCPort never runs it through a shell.".into()));
            }
        }
        Ok(ConnectionInput {
            name: self.name,
            description: self.description.unwrap_or_else(|| {
                entry
                    .map(|entry| entry.description.clone())
                    .unwrap_or_default()
            }),
            transport: transport.into(),
            url: endpoint,
            host_id: self.host,
            command: self.command,
            args: arguments,
            auth_mode: auth.into(),
            visibility: visibility.into(),
        })
    }
}

fn operation(command: &Command) -> &'static str {
    match command {
        Command::Directory(DirectoryCommand::Ls { .. }) => "directory.list",
        Command::Directory(DirectoryCommand::Show { .. } | DirectoryCommand::Access { .. }) => {
            "directory.read"
        }
        Command::Directory(DirectoryCommand::New { .. }) => "directory.create",
        Command::Directory(
            DirectoryCommand::Set { .. }
            | DirectoryCommand::Share { .. }
            | DirectoryCommand::Unshare { .. },
        ) => "directory.update",
        Command::Directory(DirectoryCommand::Rm { .. }) => "directory.delete",
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
        Command::Access(AccessCommand::Rm { .. }) | Command::Allow(AllowCommand::Rm { .. }) => {
            "access.revoke"
        }
        Command::Access(_) | Command::Allow(_) => "access.grant",
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
    }
}

async fn dispatch(
    client: &Client,
    ctx: &RequestContext,
    store: &Store,
    session: &StoredSignIn,
    command: Command,
) -> Result<Value> {
    let operation = operation(&command);
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
    session: &StoredSignIn,
    command: Command,
) -> Result<Value> {
    let backend = client.backend_url();
    let me = session.account.uuid.as_str();
    match command {
        Command::Directory(command) => match command {
            DirectoryCommand::Ls { search } => {
                value(client.directory(ctx, search.as_deref()).await?)
            }
            DirectoryCommand::Show { entry } => value(client.directory_entry(ctx, &entry).await?),
            DirectoryCommand::New { input } => {
                let input: DirectoryInput = serde_json::from_value(input_object(&input)?)?;
                value(client.create_directory_entry(ctx, &input).await?)
            }
            DirectoryCommand::Set { entry, input } => {
                let input: DirectoryInput = serde_json::from_value(input_object(&input)?)?;
                let current = client.directory_entry(ctx, &entry).await?;
                value(
                    client
                        .update_directory_entry(
                            ctx,
                            &current.id,
                            &DirectoryUpdate {
                                input,
                                version: current.version,
                            },
                        )
                        .await?,
                )
            }
            DirectoryCommand::Rm { entry } => {
                Ok(client.delete_directory_entry(ctx, &entry).await?)
            }
            DirectoryCommand::Share { entry, account } => {
                value(client.share_directory_entry(ctx, &entry, &account).await?)
            }
            DirectoryCommand::Unshare { entry, account } => {
                Ok(client.unshare_directory_entry(ctx, &entry, &account).await?)
            }
            DirectoryCommand::Access { entry } => {
                value(client.directory_access(ctx, &entry).await?)
            }
        },
        Command::Connection(command) => match command {
            ConnectionCommand::Register {
                connection,
                environment,
            } => {
                let connection = client.connection(ctx, &connection).await?;
                if connection.owner.uuid != me {
                    return Err(CliError::Input(
                        "Only the connection's owner can approve local execution.".into(),
                    ));
                }
                let host_id = connection.host_id.as_deref().ok_or_else(|| CliError::Input("This is a cloud connection. Only connections assigned to a local host need registration.".into()))?;
                let host = client.host(ctx, host_id).await?;
                if host.owner.uuid != me {
                    return Err(CliError::Input(
                        "The connection's host is not registered by this account.".into(),
                    ));
                }
                let path = local::require_registry(store, &backend, host_id, me)?;
                let _registry_lock = local::registry_lock(&path)?;
                let mut registry = mcport_client::local::Registry::load(&path)?;
                let added = mcport_client::local::register_connection(
                    &mut registry,
                    &connection,
                    &local::scope(&backend, me),
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
                from,
                transport,
                endpoint,
                host,
                command,
                arguments,
                clear_args,
                environment,
                auth,
                visibility,
                dry_run,
            } => {
                let entry = match from {
                    Some(id) => Some(client.directory_entry(ctx, &id).await?),
                    None => None,
                };
                let draft = ConnectionDraft {
                    name,
                    description,
                    transport,
                    endpoint,
                    host,
                    command,
                    arguments,
                    clear_args,
                    auth,
                    visibility,
                };
                let mut input = draft.resolve(entry.as_ref())?;
                if input.transport != "stdio" && !environment.is_empty() {
                    return Err(CliError::Input(
                        "--env applies only to stdio connections.".into(),
                    ));
                }
                let env = parse_environment(environment)?;
                if dry_run {
                    return Ok(
                        json!({"dry_run":true,"connection":input,"directory":entry.as_ref().map(|entry|json!({"id":entry.id,"name":entry.name,"source":entry.source,"source_url":entry.source_url,"source_revision":entry.source_revision})),"local_environment_keys":env.keys().collect::<Vec<_>>() }),
                    );
                }
                let host_record = match &input.host_id {
                    Some(id) => Some(client.host(ctx, id).await?),
                    None => None,
                };
                let local_path = match &host_record {
                    Some(host) => Some(local::require_registry(store, &backend, &host.id, me)?),
                    None => None,
                };
                input.host_id = host_record.map(|h| h.id);
                let created = client.create_connection(ctx, &input).await?;
                if let Some(path) = local_path {
                    let _registry_lock = local::registry_lock(&path)?;
                    let mut registry = mcport_client::local::Registry::load(&path)?;
                    mcport_client::local::register_connection(
                        &mut registry,
                        &created,
                        &local::scope(&backend, me),
                        env,
                    )?;
                    if let Err(error) = registry.save(&path) {
                        return Err(CliError::Input(format!(
                            "Connection {} was saved, but its local registration failed: {error}. It cannot run. Delete it with mcport connection rm {} and retry after fixing local storage.",
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
                let visibility = visibility.map(visibility_value).transpose()?;
                let existing = client.connection(ctx, &connection).await?;
                value(
                    client
                        .update_connection(
                            ctx,
                            &existing.id,
                            &ConnectionUpdate {
                                name,
                                description,
                                visibility: visibility.map(Into::into),
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
                    let path = local::registry_path(store, &backend, host_id);
                    if path.exists() {
                        let _registry_lock = local::registry_lock(&path)?;
                        let mut registry = mcport_client::local::Registry::load(&path)?;
                        mcport_client::local::unregister_connection(
                            &mut registry,
                            &existing,
                            &local::scope(&backend, me),
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
                    _ = tokio::signal::ctrl_c() => Err(CliError::Client(mcport_client::Error::Transport { message: "Stopped waiting for the call. The service may still be running it; inspect mcport activity ls and use mcport activity cancel <id>.".into(), outcome_unknown: true })),
                }
            }
            ToolCommand::Set {
                connection,
                tool,
                enabled,
                account,
            } => value(
                client
                    .set_tool_policy(
                        ctx,
                        &connection,
                        &ToolPolicyInput {
                            tool,
                            account,
                            enabled,
                        },
                    )
                    .await?,
            ),
        },
        Command::Account(command) => {
            let (connection_name, account, host_home) = match &command {
                AccountCommand::Connect {
                    connection,
                    host_home,
                    ..
                } => (connection, None, host_home),
                AccountCommand::Disconnect {
                    connection,
                    account,
                    host_home,
                }
                | AccountCommand::Show {
                    connection,
                    account,
                    host_home,
                } => (connection, account.as_deref(), host_home),
            };
            let connection = client.connection(ctx, connection_name).await?;
            if let Some(account) = account {
                // A custodian asking about a Silicon it looks after: the service decides.
                return match (&command, connection.host_id.is_some()) {
                    (AccountCommand::Show { .. }, _) => {
                        value(client.account_for(ctx, &connection.id, account).await?)
                    }
                    (_, true) => Err(CliError::Input("A local provider account lives in its host's registry on that machine; only the account itself can disconnect it there (mcport account disconnect, signed in as that account).".into())),
                    _ => Ok(client
                        .disconnect_account_for(ctx, &connection.id, account)
                        .await?),
                };
            }
            if let Some(host_id) = &connection.host_id {
                let registry_store = match host_home {
                    Some(home) => Store::at(home.clone())?,
                    None => store.clone(),
                };
                if matches!(command, AccountCommand::Show { .. })
                    && !local::registry_path(&registry_store, &backend, host_id).is_file()
                {
                    return value(client.account(ctx, &connection.id).await?);
                }
                return local::account(&registry_store, &backend, me, &connection, command);
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
                            map.insert("next_step".into(), json!("Open authorization_url to authorize the provider. After consent, run mcport account show <connection> to check the linked account."));
                        }
                        Ok(flow)
                    }
                }
            }
        }
        Command::Access(command) => match command {
            AccessCommand::New {
                connection,
                account,
            } => value(client.grant_access(ctx, &connection, &account).await?),
            AccessCommand::Ls { connection } => value(client.access(ctx, &connection).await?),
            AccessCommand::Rm {
                connection,
                account,
            } => Ok(client.revoke_access(ctx, &connection, &account).await?),
        },
        Command::Allow(command) => match command {
            AllowCommand::Add { account, silicon } => value(
                client
                    .allow(ctx, &AllowanceInput { account, silicon })
                    .await?,
            ),
            AllowCommand::Ls { silicon } => {
                value(client.allowances(ctx, silicon.as_deref()).await?)
            }
            AllowCommand::Rm { account, silicon } => {
                Ok(client.disallow(ctx, &account, silicon.as_deref()).await?)
            }
        },
        Command::Host(command) => match command {
            HostCommand::Ls => value(client.hosts(ctx).await?),
            HostCommand::Show { host } => value(client.host(ctx, &host).await?),
            HostCommand::New { name } => {
                let registration = client.create_host(ctx, &name).await?;
                let path = local::registry_path(store, &backend, &registration.host.id);
                let host = registration.host.clone();
                let registry = mcport_client::local::registry_for_host(
                    registration,
                    &local::scope(&backend, me),
                    ctx.isi.clone(),
                )?;
                registry.save(&path)?;
                let daemon = local::start(&path).await?;
                Ok(json!({"host":host,"daemon":daemon}))
            }
            HostCommand::Rm { host } => {
                let host = client.host(ctx, &host).await?;
                let response = client.delete_host(ctx, &host.id).await?;
                let path = local::registry_path(store, &backend, &host.id);
                if path.exists() {
                    local::stop(&path).await?;
                    std::fs::remove_file(&path)?;
                    let backup = path.with_file_name("registry.v1.json");
                    if backup.exists() {
                        std::fs::remove_file(backup)?;
                    }
                }
                Ok(response)
            }
            HostCommand::Migrate {
                host,
                dry_run,
                drop_unmapped,
            } => {
                let host = client.host(ctx, &host).await?;
                local::migrate_host(client, ctx, store, me, &host, dry_run, drop_unmapped).await
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
            AssetCommand::Link { call, index } => {
                let ticket = client.asset_ticket(ctx, &call, index).await?;
                Ok(
                    json!({"url":ticket.url,"expires_at":ticket.expires_at,"call_id":call,"index":index,"note":"Works once, without signing in, until expires_at (60 seconds); your access is checked again when it is used."}),
                )
            }
        },
        Command::Report { message, pr } => {
            let mut output = client.report(ctx, &message, pr.as_deref()).await?;
            if pr.is_none() {
                let repository = output
                    .get("repository_url")
                    .and_then(Value::as_str)
                    .unwrap_or(signin::REPOSITORY)
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
            "This command does not need a sign-in; it was routed here by mistake. Report it with mcport report.".into(),
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

    fn directory_fixture() -> DirectoryEntry {
        serde_json::from_value(json!({"id":"abc","name":"Docs","description":"Suggested provider","category":"Documentation","source":"personal","source_url":"https://provider.example","source_revision":null,"owner":{"uuid":"Own","id":"c:owner","kind":"carbon","display_name":"Owner"},"can_manage":true,"template":{"transport":"http","url":"https://provider.example/mcp","command":null,"args":[],"auth_mode":"per-user"},"version":4,"created_at":1,"updated_at":2})).unwrap()
    }
    fn session_fixture() -> StoredSignIn {
        serde_json::from_value(json!({"format":1,"accounts_url":"http://127.0.0.1:9","app_id":"mcport","backend_url":"","method":"slt","account":{"uuid":"Own","id":"c:owner","kind":"carbon","display_name":"Owner"},"access_token":"fixture","refresh_token":"sar_fixture","expires_at":4102444800i64,"refresh_expires_at":null,"scope":"profile","signed_in_at":1,"refreshed_at":1})).unwrap()
    }

    #[test]
    fn template_overrides_control_endpoint_auth_visibility_and_process_arguments() {
        let mut entry = directory_fixture();
        let inherited = ConnectionDraft {
            name: "notes".into(),
            ..Default::default()
        }
        .resolve(Some(&entry))
        .unwrap();
        assert_eq!(
            inherited.url.as_deref(),
            Some("https://provider.example/mcp")
        );
        assert_eq!(inherited.auth_mode, "per-user");
        assert_eq!(inherited.visibility, "invited");
        assert_eq!(inherited.description, "Suggested provider");
        let explicit = ConnectionDraft {
            name: "docs".into(),
            description: Some(String::new()),
            endpoint: Some("https://alternate.example/mcp".into()),
            auth: Some(AuthMode::None),
            ..Default::default()
        }
        .resolve(Some(&entry))
        .unwrap();
        assert_eq!(
            explicit.url.as_deref(),
            Some("https://alternate.example/mcp")
        );
        // No organization-wide default any more: new connections are owner-only.
        assert_eq!(explicit.visibility, "invited");
        assert!(explicit.description.is_empty());
        let circle = ConnectionDraft {
            name: "shared".into(),
            visibility: Some(Visibility::Circle),
            ..Default::default()
        }
        .resolve(Some(&entry))
        .unwrap();
        assert_eq!(circle.visibility, "circle");
        let legacy = ConnectionDraft {
            name: "legacy".into(),
            visibility: Some(Visibility::Private),
            ..Default::default()
        }
        .resolve(Some(&entry))
        .unwrap();
        assert_eq!(legacy.visibility, "private"); // The service reads it as invited.
        let removed = ConnectionDraft {
            name: "org".into(),
            visibility: Some(Visibility::Org),
            ..Default::default()
        }
        .resolve(Some(&entry))
        .unwrap_err();
        assert_eq!(removed.public().code, "visibility_removed");
        assert!(
            removed
                .public()
                .recovery
                .unwrap()
                .contains("--visibility circle")
        );

        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        entry.template = Some(mcport_client::DirectoryTemplate {
            transport: "stdio".into(),
            url: None,
            command: Some(executable.clone()),
            args: vec!["suggested".into()],
            auth_mode: "shared".into(),
        });
        let missing = ConnectionDraft {
            name: "files".into(),
            host: Some("laptop".into()),
            ..Default::default()
        }
        .resolve(Some(&entry));
        assert!(
            missing
                .unwrap_err()
                .to_string()
                .contains("explicit --command")
        );
        let explicit = ConnectionDraft {
            name: "files".into(),
            host: Some("laptop".into()),
            command: Some(executable.clone()),
            arguments: vec!["chosen".into()],
            ..Default::default()
        }
        .resolve(Some(&entry))
        .unwrap();
        assert_eq!(explicit.args, ["chosen"]);
        let cleared = ConnectionDraft {
            name: "files".into(),
            host: Some("laptop".into()),
            command: Some(executable),
            clear_args: true,
            ..Default::default()
        }
        .resolve(Some(&entry))
        .unwrap();
        assert!(cleared.args.is_empty());
        let http_override = ConnectionDraft {
            name: "remote".into(),
            transport: Some(Transport::Http),
            endpoint: Some("https://other.example/mcp".into()),
            ..Default::default()
        }
        .resolve(Some(&entry))
        .unwrap();
        assert!(http_override.command.is_none() && http_override.args.is_empty());
    }

    #[test]
    fn setup_requires_complete_configuration_and_help_hides_removed_values() {
        let entry = directory_fixture();
        let manual = ConnectionDraft {
            name: "missing".into(),
            transport: Some(Transport::Http),
            ..Default::default()
        }
        .resolve(None);
        assert!(manual.unwrap_err().to_string().contains("--url"));
        let local = ConnectionDraft {
            name: "missing".into(),
            transport: Some(Transport::Stdio),
            command: Some("relative-server".into()),
            host: Some("laptop".into()),
            ..Default::default()
        }
        .resolve(None);
        assert!(
            local
                .unwrap_err()
                .to_string()
                .contains("absolute executable")
        );
        assert!(
            ConnectionDraft {
                name: "bad-http".into(),
                arguments: vec!["unexpected".into()],
                ..Default::default()
            }
            .resolve(Some(&entry))
            .is_err()
        );
        use clap::CommandFactory;
        let mut cli = Cli::command();
        let help = cli
            .find_subcommand_mut("connection")
            .unwrap()
            .find_subcommand_mut("new")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(!help.contains("private") && !help.contains("org"));
        assert!(help.contains("invited") && help.contains("circle") && help.contains("--dry-run"));
    }

    #[test]
    fn every_command_has_help_and_none_mentions_removed_concepts() {
        use clap::CommandFactory;
        fn visit(command: &mut clap::Command, path: &str, seen: &mut usize) {
            if command.is_hide_set() {
                return;
            }
            let help = command.render_long_help().to_string();
            for removed in [
                "IAM",
                "organization",
                "Honeycomb",
                "--test",
                "app-bound",
                "principal",
            ] {
                assert!(
                    !help.contains(removed),
                    "`{path} --help` mentions {removed}:\n{help}"
                );
            }
            if path != "mcport" {
                assert!(command.get_about().is_some(), "`{path}` has no description");
            }
            *seen += 1;
            let names: Vec<String> = command
                .get_subcommands()
                .map(|sub| sub.get_name().to_owned())
                .collect();
            for name in names {
                if name == "help" {
                    continue;
                }
                let sub = command.find_subcommand_mut(&name).unwrap();
                visit(sub, &format!("{path} {name}"), seen);
            }
        }
        let mut seen = 0;
        visit(&mut Cli::command(), "mcport", &mut seen);
        assert!(seen > 60, "only {seen} help pages");
    }

    #[tokio::test]
    async fn directory_edit_uses_current_version_and_template_preview_does_not_create() {
        use axum::{
            Json, Router,
            body::Bytes,
            extract::State,
            http::{Method, Uri},
        };
        use std::sync::{Arc, Mutex};
        type CapturedRequests = Arc<Mutex<Vec<(String, String, Value)>>>;
        async fn request(
            State(requests): State<CapturedRequests>,
            method: Method,
            uri: Uri,
            body: Bytes,
        ) -> Json<Value> {
            let body = if body.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&body).unwrap()
            };
            requests
                .lock()
                .unwrap()
                .push((method.to_string(), uri.path().into(), body));
            Json(json!({"data":directory_fixture()}))
        }
        let requests = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Client::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let app = Router::new().fallback(request).with_state(requests.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let store = Store::at(directory.path().into()).unwrap();
        let session = session_fixture();
        let ctx = RequestContext::authenticated("fixture");
        let file = directory.path().join("entry.json");
        std::fs::write(&file, r#"{"name":"Edited","category":"Docs"}"#).unwrap();
        dispatch_inner(
            &client,
            &ctx,
            &store,
            &session,
            Command::Directory(DirectoryCommand::Set {
                entry: "abc".into(),
                input: format!("@{}", file.display()),
            }),
        )
        .await
        .unwrap();
        let preview = Cli::try_parse_from([
            "mcport",
            "connection",
            "new",
            "suggested",
            "--from",
            "abc",
            "--auth",
            "none",
            "--dry-run",
        ])
        .unwrap();
        let output = dispatch_inner(&client, &ctx, &store, &session, preview.command)
            .await
            .unwrap();
        assert_eq!(output["dry_run"], true);
        assert_eq!(output["connection"]["visibility"], "invited");
        assert_eq!(output["connection"]["url"], "https://provider.example/mcp");
        assert_eq!(output["directory"]["id"], "abc");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1].0, "PUT");
        assert_eq!(requests[1].2["version"], 4);
        assert_eq!(requests[1].2["input"]["name"], "Edited");
        assert!(
            requests
                .iter()
                .all(|(_, path, _)| path == "/api/v1/directory/abc")
        );
        assert!(!store.directory.exists());
        server.abort();
    }

    #[test]
    fn documented_grammars_parse_and_removed_ones_do_not() {
        let cases = [
            vec!["mcport", "accounts", "--json"],
            vec!["mcport", "iam", "--json"],
            vec!["mcport", "login"],
            vec!["mcport", "login", "--open", "--label", "build box"],
            vec!["mcport", "login", "--slt-stdin", "--json"],
            vec!["mcport", "login", "--slt", "slt_x"],
            vec!["mcport", "login", "slt_x"],
            vec!["mcport", "login", "status", "--json"],
            vec!["mcport", "login", "status", "--offline", "--json"],
            vec!["mcport", "logout", "--json"],
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
                "circle",
            ],
            vec![
                "mcport",
                "connection",
                "set",
                "docs",
                "--visibility",
                "invited",
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
                "mcport",
                "tool",
                "set",
                "docs",
                "search",
                "--enabled",
                "false",
                "--account",
                "si:researcher",
            ],
            vec![
                "mcport",
                "tool",
                "set",
                "docs",
                "search",
                "--enabled",
                "true",
                "--principal",
                "si:researcher",
            ],
            vec!["mcport", "access", "new", "docs", "--account", "c:ada"],
            vec!["mcport", "access", "rm", "docs", "--principal", "c:ada"],
            vec!["mcport", "allow", "add", "c:ada"],
            vec![
                "mcport",
                "allow",
                "ls",
                "--silicon",
                "si:researcher",
                "--json",
            ],
            vec![
                "mcport",
                "allow",
                "rm",
                "c:ada",
                "--silicon",
                "si:researcher",
            ],
            vec![
                "mcport",
                "directory",
                "share",
                "abc",
                "--account",
                "si:researcher",
            ],
            vec![
                "mcport",
                "directory",
                "unshare",
                "abc",
                "--account",
                "si:researcher",
            ],
            vec!["mcport", "directory", "access", "abc"],
            vec![
                "mcport",
                "account",
                "show",
                "docs",
                "--account",
                "si:researcher",
            ],
            vec![
                "mcport",
                "account",
                "connect",
                "docs",
                "--token",
                "--host-home",
                "/tmp",
            ],
            vec!["mcport", "host", "migrate", "laptop", "--dry-run"],
            vec!["mcport", "host", "migrate", "laptop", "--drop-unmapped"],
            vec!["mcport", "asset", "link", "c1", "0"],
            vec![
                "mcport",
                "--backend",
                "http://127.0.0.1:4241",
                "--accounts-url",
                "http://localhost:9590",
                "connection",
                "ls",
            ],
            vec!["mcport", "config", "home", "/tmp"],
            vec![
                "mcport",
                "config",
                "set",
                "accounts",
                "https://accounts.example",
            ],
            vec!["mcport", "config", "set", "telemetry", "false"],
        ];
        for case in cases {
            assert!(Cli::try_parse_from(case.clone()).is_ok(), "{case:?}");
        }
        for removed in [
            vec!["mcport", "--test", "t-1", "connection", "ls"],
            vec!["mcport", "session", "ls"],
            vec!["mcport", "session", "use", "c:ada", "--org", "tos"],
            vec!["mcport", "login", "--slt", "slt_x", "--slt-stdin"],
            vec!["mcport", "login", "slt_x", "--open"],
        ] {
            assert!(Cli::try_parse_from(removed.clone()).is_err(), "{removed:?}");
        }
    }

    #[test]
    fn bundled_docs_describe_silicon_accounts_and_silicon_apps_only() {
        for topic in [DocsTopic::Usage, DocsTopic::Development] {
            let text = topic.content();
            let words: Vec<&str> = text
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .collect();
            for removed in [
                "IAM",
                "iam",
                "Honeycomb",
                "honeycomb",
                "org",
                "organization",
                "organizations",
                "principal",
            ] {
                assert!(
                    !words.contains(&removed),
                    "`mcport docs {}` mentions {removed}",
                    topic.name()
                );
            }
            for removed in ["--test", "app-bound", "session ls", "--principal"] {
                assert!(
                    !text.contains(removed),
                    "`mcport docs {}` mentions {removed}",
                    topic.name()
                );
            }
        }
        let usage = DocsTopic::Usage.content();
        for needed in [
            "mcport login",
            "silicon-accounts login --app mcport -q | mcport login --slt-stdin",
            "mcport login status --json",
            "mcport logout",
            "mcport accounts --json",
            "silicon-apps install mcport",
            "mcport host migrate",
            "mcport allow add",
        ] {
            assert!(usage.contains(needed), "usage guide lacks {needed}");
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
