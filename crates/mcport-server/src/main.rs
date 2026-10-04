mod assets;
mod auth;
mod connections;
mod directory;
mod error;
mod execution;
mod hosts;
mod lifecycle;
mod oauth;
mod operations;
mod public_ids;
mod state;
mod store;
mod tls;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderValue, header},
    middleware,
    routing::{get, post, put},
};
use serde_json::{Value, json};
use state::{App, Config};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};
async fn iam(State(app): State<App>) -> Json<Value> {
    Json(
        json!({"data":{"app_id":app.config.app_id,"iam_url":app.config.iam_url,"login_url":app.config.iam_web_url,"backend_url":app.config.public_url,"website_url":app.config.web_url,"repository_url":"https://github.com/teamofsilicons/silicon-mcport","docs_url":"https://github.com/teamofsilicons/silicon-mcport/tree/main/docs","package_url":"https://crates.io/crates/mcport-client"}}),
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
        h.insert(header::CONTENT_SECURITY_POLICY,HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src 'self' https://fonts.gstatic.com; img-src 'self' data: blob:; media-src 'self' data: blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    }
    response
}
fn router(app: App) -> Router {
    let allowed = vec![
        app.config
            .web_url
            .parse::<HeaderValue>()
            .expect("MCPORT_WEB_URL valid origin"),
        app.config
            .public_url
            .parse::<HeaderValue>()
            .expect("MCPORT_PUBLIC_URL valid origin"),
    ];
    Router::new()
    .route("/health",get(||async{Json(json!({"status":"ok","version":env!("CARGO_PKG_VERSION"),"source_revision":option_env!("MCPORT_BUILD_REVISION")}))}))
    .route("/api/v1/iam",get(iam))
    .route("/api/{*unknown}",get(||async{error::Error::missing()}).post(||async{error::Error::missing()}))
    .route("/api/v1/settings",get(operations::get_settings).patch(operations::update_settings))
    .route("/api/v1/reports",post(operations::report))
    .route("/api/v1/telemetry",post(operations::telemetry))
    .route("/api/v1/auth/login",post(auth::login))
    .route("/api/v1/auth/refresh",post(auth::refresh))
    .route("/api/v1/auth/status",get(auth::status))
    .route("/api/v1/auth/logout",post(auth::logout))
    .route("/api/v1/auth/browser/start",get(auth::browser_start))
    .route("/api/v1/auth/browser/complete",post(auth::browser_complete))
    .route("/api/v1/auth/browser/refresh",post(auth::browser_refresh))
    .route("/api/v1/directory",get(directory::list).post(directory::create))
    .route("/api/v1/directory/{entry}",get(directory::get).put(directory::update).delete(directory::remove))
    .route("/api/v1/connections",get(connections::list).post(connections::create))
    .route("/api/v1/connections/{connection}",get(connections::get).patch(connections::update).delete(connections::remove))
    .route("/api/v1/connections/{connection}/access",get(connections::access_list).post(connections::invite))
    .route("/api/v1/connections/{connection}/access/{principal}",axum::routing::delete(connections::uninvite))
    .route("/api/v1/connections/{connection}/policies",get(connections::policies).put(connections::set_policy))
    .route("/api/v1/connections/{connection}/account",get(connections::account_get).post(connections::account_set).delete(connections::account_remove))
    .route("/api/v1/connections/{connection}/account/authorize",post(oauth::authorize))
    .route("/api/v1/connections/{connection}/mcp",post(execution::execute))
    .route("/api/v1/calls",get(execution::list))
    .route("/api/v1/calls/{call}",get(execution::get))
    .route("/api/v1/calls/{call}/assets",get(assets::list))
    .route("/api/v1/calls/{call}/assets/{index}",get(assets::download))
    .route("/api/v1/calls/{call}/cancel",post(execution::cancel))
    .route("/api/v1/hosts",get(hosts::list).post(hosts::create))
    .route("/api/v1/hosts/{host}",get(hosts::get).delete(hosts::remove))
    .route("/api/v1/hosts/{host}/poll",post(hosts::poll))
    .route("/api/v1/hosts/{host}/jobs/{job}/result",post(hosts::complete))
    .route("/api/v1/hosts/{host}/jobs/{job}/progress",post(hosts::progress))
    .route("/internal/honeycomb/organizations/{org}/testing-environments/{environment}/operations/{operation}",put(lifecycle::apply).get(lifecycle::receipt))
    .route("/webhooks/iam",post(lifecycle::webhook))
    .route("/oauth/start",get(oauth::start))
    .route("/oauth/client-metadata.json",get(oauth::client_metadata))
    .route("/oauth/callback",get(oauth::callback))
    .fallback_service(ServeDir::new("web/dist").not_found_service(ServeFile::new("web/dist/index.html")))
    .layer(DefaultBodyLimit::max(20*1024*1024))
    .layer(CorsLayer::new().allow_origin(allowed).allow_credentials(true).allow_methods([axum::http::Method::GET,axum::http::Method::POST,axum::http::Method::PATCH,axum::http::Method::DELETE,axum::http::Method::PUT]).allow_headers([header::CONTENT_TYPE,header::AUTHORIZATION,axum::http::HeaderName::from_static("x-mcport-test"),axum::http::HeaderName::from_static("x-mcport-isi"),axum::http::HeaderName::from_static("x-mcport-telemetry")]))
    .layer(middleware::from_fn(secure_headers))
    .with_state(app)
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tls::initialize()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mcport_server=info".into()),
        )
        .init();
    let config = Config::from_env();
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
    tracing::info!(address=%bind,"Silicon MCPort listening");
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
                .list::<mcport_core::Connection>("connection", None)
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
