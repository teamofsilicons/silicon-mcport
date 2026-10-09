mod accounts;
mod accounts_webhook;
mod allowances;
mod assets;
mod auth;
mod connections;
mod directory;
mod error;
mod execution;
mod hosts;
mod identity;
mod identity_store;
mod oauth;
mod operations;
mod public_ids;
mod state;
mod store;
#[cfg(test)]
mod test_support;
mod tls;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderValue, header},
    middleware,
    routing::{delete, get, post},
};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use state::{App, Config};
use std::path::PathBuf;

const REPOSITORY: &str = "https://github.com/teamofsilicons/silicon-mcport";

/// Silicon MCPort service: the API behind the mcport CLI, the Rust package and the website.
#[derive(Parser)]
#[command(name = "mcport-server", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Serve the API (the default when no command is given).
    Serve,
    /// Print the pre-0.3.0 principal ids stored records belong to, as JSON (input
    /// for a link-identities mapping file). Changes nothing.
    LegacyPrincipals,
    /// Re-key stored records from pre-0.3.0 principal ids to Silicon Accounts uuids.
    /// Run with the service stopped, after a backup. Idempotent; re-running with
    /// another mapping recomputes everything from the preserved original ids.
    LinkIdentities {
        /// CSV with the header `iam_principal_id,accounts_uuid` (optional third
        /// column `iam_public_id`). Lines starting with # are ignored.
        #[arg(long)]
        file: PathBuf,
        /// Report what would change, then roll back.
        #[arg(long)]
        dry_run: bool,
        /// Do not look the uuids up in Silicon Accounts (kinds come from the ids'
        /// c:/si: prefixes and custodians are unknown, so every principal with
        /// evidence of use keeps explicit access).
        #[arg(long)]
        offline: bool,
    },
}

async fn discovery(State(app): State<App>) -> Json<Value> {
    Json(json!({"data": mcport_core::Discovery {
        app_id: app.config.app_id.clone(),
        accounts_url: app.config.accounts_url.clone(),
        client_id: app.config.app_id.clone(),
        backend_url: app.config.public_url.clone(),
        website_url: app.config.web_url.clone(),
        repository_url: REPOSITORY.into(),
        docs_url: format!("{REPOSITORY}/tree/main/docs"),
        package_url: "https://crates.io/crates/mcport-client".into(),
        install_url: format!("https://apps.teamofsilicons.com/apps/{}", app.config.app_id),
        version: env!("CARGO_PKG_VERSION").into(),
    }}))
}
async fn me(State(app): State<App>, a: auth::Auth) -> error::Result<Json<Value>> {
    let custodian = match a
        .account
        .custodian_uuid
        .as_deref()
        .filter(|_| a.account.is_silicon())
    {
        Some(uuid) => {
            let mut reference = accounts::reference(&app, uuid);
            if reference.id.is_empty() {
                reference.id = a.account.custodian_id.clone().unwrap_or_default();
            }
            Some(reference)
        }
        None => None,
    };
    Ok(Json(json!({"data": mcport_core::Me {
        account: a.account.reference(),
        custodian,
        expires_at: a.expires_at,
    }})))
}
/// Routes of releases before 0.3.0, kept for one release to tell old clients
/// what to do instead.
async fn gone() -> error::Error {
    error::Error::new(
        410,
        "client_update_required",
        "This MCPort service signs Carbons and Silicons in with Silicon Accounts; this route no longer exists.",
        "Install the current mcport (silicon-apps install mcport), then run mcport login (Carbons) or silicon-accounts login --app mcport -q | mcport login --slt-stdin (Silicons).",
    )
}
async fn webhook_moved() -> error::Error {
    error::Error::new(
        410,
        "webhook_moved",
        "MCPort receives account events at /webhooks/accounts.",
        "Point the webhook at /webhooks/accounts.",
    )
}
async fn secure_headers(
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if api
        && response.status().is_client_error()
        && !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"))
    {
        let status = response.status();
        response = error::Error::new(
            status.as_u16(),
            "invalid_request",
            "The request body, method or route is invalid.",
            "Use JSON with the documented API path and supported fields.",
        )
        .into_response();
    }
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        // JSON, two small provider-consent pages with inline styles, and downloads.
        h.insert(header::CONTENT_SECURITY_POLICY,HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"));
    }
    response
}
async fn not_found() -> error::Error {
    error::Error::missing()
}
fn router(app: App) -> Router {
    Router::new()
    .route("/health",get(||async{Json(json!({"status":"ok","version":env!("CARGO_PKG_VERSION"),"source_revision":option_env!("MCPORT_BUILD_REVISION")}))}))
    .route("/api/v1/discovery",get(discovery))
    .route("/api/v1/me",get(me))
    .route("/api/v1/iam",get(gone))
    .route("/api/v1/auth/{*removed}",get(gone).post(gone))
    .route("/api/{*unknown}",get(not_found).post(not_found))
    .route("/api/v1/settings",get(operations::get_settings).patch(operations::update_settings))
    .route("/api/v1/reports",post(operations::report))
    .route("/api/v1/telemetry",post(operations::telemetry))
    .route("/api/v1/allow",get(allowances::list).post(allowances::add))
    .route("/api/v1/allow/{account}",delete(allowances::remove))
    .route("/api/v1/directory",get(directory::list).post(directory::create))
    .route("/api/v1/directory/{entry}",get(directory::get).put(directory::update).delete(directory::remove))
    .route("/api/v1/directory/{entry}/access",get(directory::access_list).post(directory::share))
    .route("/api/v1/directory/{entry}/access/{account}",delete(directory::unshare))
    .route("/api/v1/connections",get(connections::list).post(connections::create))
    .route("/api/v1/connections/{connection}",get(connections::get).patch(connections::update).delete(connections::remove))
    .route("/api/v1/connections/{connection}/access",get(connections::access_list).post(connections::invite))
    .route("/api/v1/connections/{connection}/access/{account}",delete(connections::uninvite))
    .route("/api/v1/connections/{connection}/policies",get(connections::policies).put(connections::set_policy))
    .route("/api/v1/connections/{connection}/account",get(connections::account_get).post(connections::account_set).delete(connections::account_remove))
    .route("/api/v1/connections/{connection}/account/authorize",post(oauth::authorize))
    .route("/api/v1/connections/{connection}/mcp",post(execution::execute))
    .route("/api/v1/calls",get(execution::list))
    .route("/api/v1/calls/{call}",get(execution::get))
    .route("/api/v1/calls/{call}/assets",get(assets::list))
    .route("/api/v1/calls/{call}/assets/{index}",get(assets::download))
    .route("/api/v1/calls/{call}/assets/{index}/ticket",post(assets::ticket))
    .route("/api/v1/calls/{call}/cancel",post(execution::cancel))
    .route("/api/v1/downloads/{ticket}",get(assets::redeem))
    .route("/api/v1/hosts",get(hosts::list).post(hosts::create))
    .route("/api/v1/hosts/{host}",get(hosts::get).delete(hosts::remove))
    .route("/api/v1/hosts/{host}/poll",post(hosts::poll))
    .route("/api/v1/hosts/{host}/jobs/{job}/result",post(hosts::complete))
    .route("/api/v1/hosts/{host}/jobs/{job}/progress",post(hosts::progress))
    .route("/webhooks/accounts",post(accounts_webhook::receive))
    .route("/webhooks/iam",post(webhook_moved))
    .route("/oauth/start",get(oauth::start))
    .route("/oauth/client-metadata.json",get(oauth::client_metadata))
    .route("/oauth/callback",get(oauth::callback))
    .fallback(not_found)
    .layer(DefaultBodyLimit::max(20*1024*1024))
    .layer(middleware::from_fn(secure_headers))
    .with_state(app)
}
async fn serve(config: Config) -> anyhow::Result<()> {
    let bind = config.bind.clone();
    let app = App::new(config).map_err(|e| anyhow::anyhow!(e.1.message))?;
    app.store
        .migrate_private_visibility()
        .map_err(|e| anyhow::anyhow!(e.1.message))?;
    directory::seed(&app).map_err(|e| anyhow::anyhow!(e.1.message))?;
    execution::recover(&app).map_err(|e| anyhow::anyhow!(e.1.message))?;
    operations::init_telemetry(&app);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let worker = tokio::spawn(operations::report_worker(app.clone(), shutdown.clone()));
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(address=%bind,accounts=%app.config.accounts_url,"Silicon MCPort listening");
    let shutdown_app = app.clone();
    let worker_shutdown = shutdown.clone();
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("SIGTERM handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
            worker_shutdown.cancel();
            if let Ok(connections) = shutdown_app
                .store
                .list::<connections::ConnectionRecord>("connection", None)
            {
                for connection in connections {
                    let _ = execution::invalidate_connection(&shutdown_app, &connection.id);
                }
            }
            for cancel in shutdown_app
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
            {
                cancel.cancel();
            }
            shutdown_app.jobs.notify_waiters();
        })
        .await?;
    shutdown.cancel();
    let _ = worker.await;
    Ok(())
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tls::initialize()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mcport_server=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let (config, warnings) = Config::from_env().map_err(|message| anyhow::anyhow!(message))?;
    for warning in warnings {
        tracing::warn!("{warning}");
    }
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config).await,
        Command::LegacyPrincipals => identity::print_principals(config),
        Command::LinkIdentities {
            file,
            dry_run,
            offline,
        } => identity::run(config, &file, dry_run, offline).await,
    }
}
