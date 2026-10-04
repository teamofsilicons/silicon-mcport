//! User settings, bounded diagnostic events and durable production bug-mail delivery.
use crate::{
    auth::{self, Auth},
    error::{Error, Result},
    state::{App, hash, now},
};
use axum::{Json, extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const REPOSITORY: &str = "https://github.com/teamofsilicons/silicon-mcport";
const RECIPIENTS: &str = "saketdev12@gmail.com,shubhastro2@gmail.com";

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub telemetry: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self { telemetry: true }
    }
}

fn settings_key(auth: &Auth) -> String {
    hash(&json!([auth.env(), auth.actor().org_id, auth.actor().principal_id]).to_string())
}
fn settings(app: &App, auth: &Auth) -> Result<Settings> {
    Ok(app
        .store
        .get("settings", &settings_key(auth))?
        .unwrap_or_default())
}

pub async fn get_settings(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let auth = auth::authenticate(&app, &headers).await?;
    Ok(Json(json!({"data":settings(&app, &auth)?})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsInput {
    pub telemetry: bool,
}
pub async fn update_settings(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<SettingsInput>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let auth = auth::authenticate(&app, &headers).await?;
    let _environment_guard = auth::mutation_guard(&app, &auth).await?;
    let value = Settings {
        telemetry: input.telemetry,
    };
    app.store.put(
        "settings",
        &settings_key(&auth),
        auth.env(),
        &auth.actor().org_id,
        &auth.actor().principal_id,
        None,
        &value,
        None,
    )?;
    Ok(Json(json!({"data":value})))
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Report {
    pub id: String,
    pub environment: String,
    pub org_id: String,
    pub owner_id: String,
    pub message: String,
    pub pr: Option<String>,
    pub status: String,
    pub failure_reason: Option<String>,
    pub attempts: u32,
    pub next_attempt_at: i64,
    pub created_at: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportInput {
    pub message: String,
    pub pr: Option<String>,
}

fn report_payload(report: &Report, from: &str) -> Value {
    json!({"From":from,"To":RECIPIENTS,"Subject":format!("MCPort bug report {}",report.id),
        "TextBody":format!("Report: {}\nOrganization: {}\nReporter: {}\n\n{}\n\nProposed fix: {}",report.id,report.org_id,report.owner_id,report.message,report.pr.as_deref().unwrap_or("none")),
        "MessageStream":"outbound","TrackOpens":false,"TrackLinks":"None",
        "Headers":[{"Name":"Message-ID","Value":format!("<{}@mcport.teamofsilicons.com>",report.id)}]})
}

fn validate_report(input: &ReportInput) -> Result<()> {
    if input.message.trim().is_empty() || input.message.len() > 16_384 {
        return Err(Error::bad(
            "Report message must contain 1–16384 bytes of reproduction details.",
        ));
    }
    // A report includes only explicit user text; never append sessions, provider input or local logs.
    if [
        "Bearer ", "mpa_", "mpr_", "oat_", "ort_", "oac_", "ask_", "slt_",
    ]
    .iter()
    .any(|prefix| input.message.contains(prefix))
    {
        return Err(Error::bad(
            "The report appears to contain a credential. Remove or replace token values with [redacted] before submitting.",
        ));
    }
    if let Some(pr) = &input.pr {
        let parsed = url::Url::parse(pr)
            .map_err(|_| Error::bad("--pr must be an HTTPS pull-request URL."))?;
        if parsed.scheme() != "https"
            || parsed.host_str() != Some("github.com")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !parsed.path().contains("/pull/")
        {
            return Err(Error::bad(
                "--pr must be an HTTPS GitHub pull-request URL without credentials or query parameters.",
            ));
        }
    }
    Ok(())
}

pub async fn report(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<ReportInput>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let auth = auth::authenticate(&app, &headers).await?;
    let _environment_guard = auth::mutation_guard(&app, &auth).await?;
    validate_report(&input)?;
    let configured = app
        .config
        .postmark_token
        .as_ref()
        .is_some_and(|token| !token.is_empty());
    let testing = auth.env() != "production";
    let mut report = Report {
        id: String::new(),
        environment: auth.env().into(),
        org_id: auth.actor().org_id.clone(),
        owner_id: auth.actor().principal_id.clone(),
        message: input.message,
        pr: input.pr,
        status: if testing {
            "test_recorded"
        } else if configured {
            "delivery_pending"
        } else {
            "delivery_failed"
        }
        .into(),
        failure_reason: if testing {
            Some(
                "Testing reports are recorded in the isolated environment; no email is sent."
                    .into(),
            )
        } else if !configured {
            Some("POSTMARK_SERVER_TOKEN is not configured. The report is saved and delivery can resume after server configuration.".into())
        } else {
            None
        },
        attempts: 0,
        next_attempt_at: now(),
        created_at: now(),
    };
    let (report, _) = app.store.create_public(
        "report",
        auth.env(),
        &auth.actor().org_id,
        &auth.actor().principal_id,
        None,
        None,
        |id| {
            report.id = id;
            report
        },
    )?;
    Ok(Json(
        json!({"data":{"id":report.id,"status":report.status,"delivery_detail":report.failure_reason,"repository_url":REPOSITORY}}),
    ))
}

/// Run one worker for this backend. Pending/sending records survive process restarts.
pub async fn report_worker(app: App, shutdown: CancellationToken) {
    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(http) => http,
        Err(_) => {
            tracing::error!("Could not initialize bug-report mail transport");
            return;
        }
    };
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tick.tick() => {
                if let Err(error) = deliver_pending(&app, &http, "https://api.postmarkapp.com/email").await {
                    tracing::error!(code=%error.1.code,"Bug report delivery queue could not be processed");
                }
            }
        }
    }
}

async fn deliver_pending(app: &App, http: &reqwest::Client, endpoint: &str) -> Result<()> {
    let Some(token) = app
        .config
        .postmark_token
        .as_deref()
        .filter(|token| !token.is_empty())
    else {
        return Ok(());
    };
    // Never query or send reports from a Honeycomb testing plane through production Postmark.
    for candidate in app.store.list::<Report>("report", Some("production"))? {
        if !matches!(
            candidate.status.as_str(),
            "delivery_pending" | "delivery_sending" | "delivery_failed"
        ) || candidate.attempts >= 8
            || candidate.next_attempt_at > now()
        {
            continue;
        }
        let lock = app.lock(&format!("report:{}", candidate.id));
        let _guard = lock.lock().await;
        let Some(mut report) = app.store.get::<Report>("report", &candidate.id)? else {
            continue;
        };
        if report.environment != "production"
            || report.next_attempt_at > now()
            || !matches!(
                report.status.as_str(),
                "delivery_pending" | "delivery_sending" | "delivery_failed"
            )
            || report.attempts >= 8
        {
            continue;
        }
        report.status = "delivery_sending".into();
        report.attempts += 1;
        report.next_attempt_at = now() + 60;
        app.store.put(
            "report",
            &report.id,
            &report.environment,
            &report.org_id,
            &report.owner_id,
            None,
            &report,
            None,
        )?;
        let response = http
            .post(endpoint)
            .header("X-Postmark-Server-Token", token)
            .json(&report_payload(&report, &app.config.postmark_from))
            .send()
            .await;
        let outcome = match response {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    match response.json::<Value>().await {
                        // Acceptance is terminal for our outbox, but does not confirm inbox delivery.
                        Ok(value) if value.get("ErrorCode").and_then(Value::as_i64) == Some(0) => ("delivery_accepted", None),
                        _ => ("delivery_failed", Some("Postmark did not confirm successful acceptance.".into())),
                    }
                } else { ("delivery_failed", Some(format!("Postmark returned HTTP {}.",status.as_u16()))) }
            }
            Err(_) => ("delivery_pending", Some("Postmark transport unavailable; delivery will be retried from the durable outbox.".into())),
        };
        report.status = outcome.0.into();
        report.failure_reason = outcome.1;
        report.next_attempt_at = now() + 300;
        if report.attempts >= 8 && report.status != "delivery_accepted" {
            report.status = "delivery_failed".into();
        }
        app.store.put(
            "report",
            &report.id,
            &report.environment,
            &report.org_id,
            &report.owner_id,
            None,
            &report,
            None,
        )?;
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryInput {
    pub source: String,
    pub operation: String,
    pub step: String,
    pub outcome: String,
    pub progress: Option<f64>,
    pub correlation_id: Option<String>,
    pub duration_ms: Option<u64>,
}

fn validate_event(event: &TelemetryInput) -> Result<()> {
    if !matches!(
        event.source.as_str(),
        "web" | "cli" | "daemon" | "backend" | "rust-client"
    ) {
        return Err(Error::bad("Unknown telemetry source."));
    }
    if !matches!(
        event.operation.as_str(),
        "login"
            | "logout"
            | "connection.list"
            | "connection.create"
            | "connection.read"
            | "connection.update"
            | "connection.delete"
            | "tool.list"
            | "tool.call"
            | "tool.policy"
            | "resource.list"
            | "resource.read"
            | "prompt.list"
            | "prompt.get"
            | "completion.complete"
            | "account.connect"
            | "account.disconnect"
            | "access.grant"
            | "access.revoke"
            | "host.register"
            | "host.poll"
            | "host.execute"
            | "activity.read"
            | "activity.cancel"
            | "settings.update"
            | "report.submit"
            | "navigation"
    ) {
        return Err(Error::bad(
            "Unknown telemetry operation; send a fixed operation name, never tool input or provider data.",
        ));
    }
    if !matches!(
        event.step.as_str(),
        "start" | "validate" | "authorize" | "dispatch" | "provider" | "complete" | "render"
    ) || !matches!(
        event.outcome.as_str(),
        "pending" | "success" | "failure" | "cancelled"
    ) {
        return Err(Error::bad("Unknown telemetry step or outcome."));
    }
    if event
        .progress
        .is_some_and(|progress| !progress.is_finite() || !(0.0..=1.0).contains(&progress))
        || event
            .duration_ms
            .is_some_and(|duration| duration > 86_400_000)
    {
        return Err(Error::bad(
            "Telemetry progress must be 0–1 and duration must be bounded.",
        ));
    }
    if event
        .correlation_id
        .as_ref()
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
    {
        return Err(Error::bad(
            "Telemetry correlation_id must be a UUID, not free text.",
        ));
    }
    Ok(())
}

static TELEMETRY: OnceLock<HashMap<String, Arc<space_station::SpaceClient>>> = OnceLock::new();

/// Explicit operator configuration; missing table keys do not change the user's default-on preference.
/// Test environments require their own key and never fall back to the production table.
pub fn init_telemetry(app: &App) {
    TELEMETRY.get_or_init(|| {
        let mut keys = HashMap::<String, String>::new();
        if let Ok(key) = std::env::var("MCPORT_TELEMETRY_KEY") { keys.insert("production".into(), key); }
        if let Ok(test_keys) = std::env::var("MCPORT_TEST_TELEMETRY_KEYS")
            && let Ok(test_keys) = serde_json::from_str::<HashMap<String, String>>(&test_keys) {
            keys.extend(test_keys.into_iter().filter(|(environment, _)| environment != "production"));
        }
        let url = std::env::var("MCPORT_TELEMETRY_URL").unwrap_or_else(|_| space_station::DEFAULT_URL.into());
        keys.into_iter().filter_map(|(environment, key)| {
            let client = space_station::SpaceClient::builder(&key).home(app.config.data_dir.join("telemetry").join(hash(&environment))).url(&url).flush_timeout(Duration::from_millis(100)).on_error(|_| tracing::warn!("Space Station telemetry delivery unavailable; diagnostic content is not logged")).build();
            match client { Ok(client) => Some((environment, Arc::new(client))), Err(_) => { tracing::warn!("MCPort telemetry table key is not configured correctly"); None } }
        }).collect()
    });
}

pub fn record(app: &App, auth: &Auth, headers: &HeaderMap, event: &TelemetryInput) -> Result<bool> {
    validate_event(event)?;
    if !settings(app, auth)?.telemetry
        || headers
            .get("X-MCPort-Telemetry")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| matches!(value, "false" | "off" | "0"))
    {
        return Ok(false);
    }
    init_telemetry(app);
    let Some(client) = TELEMETRY.get().and_then(|clients| clients.get(auth.env())) else {
        return Ok(false);
    };
    client.record(json!({"application":"mcport","version":env!("CARGO_PKG_VERSION"),"environment":auth.env(),"org_id":auth.actor().org_id,"actor_id":auth.actor().principal_id,"source":event.source,"operation":event.operation,"step":event.step,"outcome":event.outcome,"progress":event.progress,"correlation_id":event.correlation_id,"duration_ms":event.duration_ms}));
    Ok(true)
}

pub async fn telemetry(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<TelemetryInput>,
) -> Result<Json<Value>> {
    auth::csrf(&app, &headers)?;
    let auth = auth::authenticate(&app, &headers).await?;
    let recorded = record(&app, &auth, &headers, &input)?;
    Ok(Json(json!({"data":{"recorded":recorded}})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_use_exact_recipients_and_do_not_attach_context_or_html() {
        let report = Report {
            id: "id".into(),
            environment: "production".into(),
            org_id: "tos".into(),
            owner_id: "c:owner".into(),
            message: "<html> remains text".into(),
            pr: None,
            status: "delivery_pending".into(),
            failure_reason: None,
            attempts: 0,
            next_attempt_at: 0,
            created_at: 0,
        };
        let payload = report_payload(&report, "mcport@teamofsilicons.com");
        assert_eq!(payload["To"], "saketdev12@gmail.com,shubhastro2@gmail.com");
        assert!(payload.get("HtmlBody").is_none());
        assert!(payload.get("Attachments").is_none());
        assert_eq!(payload["TrackOpens"], false);
        assert!(
            validate_report(&ReportInput {
                message: "mpa_secret".into(),
                pr: None
            })
            .is_err()
        );
    }
    #[test]
    fn telemetry_cannot_smuggle_freeform_payload_fields() {
        assert!(serde_json::from_value::<TelemetryInput>(json!({"source":"web","operation":"tool.call","step":"complete","outcome":"success","input":{"secret":"private"}})).is_err());
        let mut event = TelemetryInput {
            source: "web".into(),
            operation: "tool.call".into(),
            step: "complete".into(),
            outcome: "success".into(),
            progress: Some(1.0),
            correlation_id: None,
            duration_ms: Some(42),
        };
        assert!(validate_event(&event).is_ok());
        event.operation = "private provider response".into();
        assert!(validate_event(&event).is_err());
    }

    #[tokio::test]
    async fn report_outbox_survives_restart_and_never_sends_test_reports() {
        use axum::{Router, http::StatusCode, routing::post};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let sent = Arc::new(AtomicUsize::new(0));
        let count = sent.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/email", listener.local_addr().unwrap());
        let router = Router::new().route(
            "/email",
            post(move |headers: HeaderMap, Json(payload): Json<Value>| {
                let count = count.clone();
                async move {
                    assert_eq!(headers["X-Postmark-Server-Token"], "fixture-mail-token");
                    assert_eq!(payload["To"], RECIPIENTS);
                    if count.fetch_add(1, Ordering::SeqCst) == 0 {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"ErrorCode":100})),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            Json(json!({"ErrorCode":0,"MessageID":"fixture-message"})),
                        )
                    }
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let mut config = crate::state::Config::from_env();
        config.data_dir = directory.path().into();
        config.postmark_token = Some("fixture-mail-token".into());
        let app = App::new(config.clone()).unwrap();
        for (id, environment, status) in [
            ("production-report", "production", "delivery_pending"),
            ("testing-report", "test-isolated", "delivery_pending"),
            ("legacy-delivered-report", "production", "delivered"),
            ("already-accepted-report", "production", "delivery_accepted"),
        ] {
            let report = Report {
                id: id.into(),
                environment: environment.into(),
                org_id: "tos".into(),
                owner_id: "c:owner".into(),
                message: "Private fixture bug reproduction".into(),
                pr: None,
                status: status.into(),
                failure_reason: None,
                attempts: 0,
                next_attempt_at: 0,
                created_at: now(),
            };
            app.store
                .put(
                    "report",
                    id,
                    environment,
                    "tos",
                    "c:owner",
                    None,
                    &report,
                    Some(0),
                )
                .unwrap();
        }
        let http = reqwest::Client::new();
        deliver_pending(&app, &http, &endpoint).await.unwrap();
        let mut failed = app
            .store
            .get::<Report>("report", "production-report")
            .unwrap()
            .unwrap();
        assert_eq!(failed.status, "delivery_failed");
        assert_eq!(failed.attempts, 1);
        assert_eq!(
            app.store
                .get::<Report>("report", "testing-report")
                .unwrap()
                .unwrap()
                .attempts,
            0
        );
        failed.next_attempt_at = 0;
        failed.attempts = 7;
        app.store
            .put(
                "report",
                &failed.id,
                "production",
                "tos",
                "c:owner",
                None,
                &failed,
                None,
            )
            .unwrap();
        drop(app);
        let restarted = App::new(config).unwrap();
        deliver_pending(&restarted, &http, &endpoint).await.unwrap();
        deliver_pending(&restarted, &http, &endpoint).await.unwrap();
        assert_eq!(
            restarted
                .store
                .get::<Report>("report", "production-report")
                .unwrap()
                .unwrap()
                .status,
            "delivery_accepted"
        );
        assert_eq!(sent.load(Ordering::SeqCst), 2);
        let accepted = restarted
            .store
            .get::<Report>("report", "production-report")
            .unwrap()
            .unwrap();
        assert_eq!(accepted.attempts, 8);
        let legacy = restarted
            .store
            .get::<Report>("report", "legacy-delivered-report")
            .unwrap()
            .unwrap();
        assert_eq!(legacy.status, "delivered");
        assert_eq!(legacy.attempts, 0);
        assert_eq!(
            restarted
                .store
                .get::<Report>("report", "already-accepted-report")
                .unwrap()
                .unwrap()
                .attempts,
            0
        );
        assert_eq!(
            restarted
                .store
                .get::<Report>("report", "testing-report")
                .unwrap()
                .unwrap()
                .attempts,
            0
        );
        server.abort();
    }
}
