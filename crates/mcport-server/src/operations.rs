//! The retained settings endpoint and durable production bug-mail delivery.
//! MCPort records no telemetry.
use crate::{
    auth::Auth,
    error::{Error, Result},
    state::{App, hash, now},
    store::ENV,
};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const REPOSITORY: &str = "https://github.com/teamofsilicons/silicon-mcport";
const RECIPIENTS: &str = "saketdev12@gmail.com,shubhastro2@gmail.com";

/// Key of an account's row in the `settings` kind. Releases before 0.4.0 stored a
/// telemetry preference there; the rows stay (re-keying and account deletion still
/// handle them) but nothing reads or writes them any more.
pub fn settings_key(uuid: &str) -> String {
    hash(&json!([uuid]).to_string())
}

/// `GET /api/v1/settings`, retained for clients released before 0.4.0. There are no
/// per-account settings, so the object is empty.
pub async fn get_settings(_auth: Auth) -> Json<Value> {
    Json(json!({"data":{}}))
}

/// The body clients released before 0.4.0 sent. `telemetry` is accepted and ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsInput {
    #[serde(default, rename = "telemetry")]
    _telemetry: Option<serde::de::IgnoredAny>,
}
/// `PATCH /api/v1/settings`, retained for clients released before 0.4.0: a no-op
/// that stores nothing.
pub async fn update_settings(_auth: Auth, Json(_input): Json<SettingsInput>) -> Json<Value> {
    Json(json!({"data":{}}))
}

/// Stored bug report. `other` keeps fields of earlier releases verbatim.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub id: String,
    /// The reporter's uuid (empty for reports not yet re-keyed).
    #[serde(default)]
    pub owner_uuid: String,
    /// The reporter's id when the report was sent.
    #[serde(default)]
    pub reporter_id: String,
    pub message: String,
    pub pr: Option<String>,
    pub status: String,
    pub failure_reason: Option<String>,
    pub attempts: u32,
    pub next_attempt_at: i64,
    pub created_at: i64,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}
impl Report {
    /// The reporter for the email: the uuid-era identity, or the IAM-era one
    /// kept from before 0.3.0.
    fn reporter(&self) -> String {
        if !self.owner_uuid.is_empty() {
            return format!("{} ({})", self.reporter_id, self.owner_uuid);
        }
        self.other
            .get("owner_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned()
    }
    /// Reports from removed testing environments are never mailed.
    fn deliverable(&self) -> bool {
        self.other
            .get("environment")
            .and_then(Value::as_str)
            .is_none_or(|environment| environment == ENV)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportInput {
    pub message: String,
    pub pr: Option<String>,
}

fn report_payload(report: &Report, from: &str) -> Value {
    json!({"From":from,"To":RECIPIENTS,"Subject":format!("MCPort bug report {}",report.id),
        "TextBody":format!("Report: {}\nReporter: {}\n\n{}\n\nProposed fix: {}",report.id,report.reporter(),report.message,report.pr.as_deref().unwrap_or("none")),
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
        "Bearer ", "Proof ", "mpa_", "mpr_", "mph_", "oat_", "ort_", "oac_", "ask_", "slt_",
        "sar_", "sap_", "sapr_", "whsec_", "stk-", "sa_app_", "eyJ",
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
    auth: Auth,
    Json(input): Json<ReportInput>,
) -> Result<Json<Value>> {
    validate_report(&input)?;
    let configured = app
        .config
        .postmark_token
        .as_ref()
        .is_some_and(|token| !token.is_empty());
    let mut report = Report {
        id: String::new(),
        owner_uuid: auth.uuid().into(),
        reporter_id: auth.account.label().into(),
        message: input.message,
        pr: input.pr,
        status: if configured {
            "delivery_pending"
        } else {
            "delivery_failed"
        }
        .into(),
        failure_reason: if !configured {
            Some("POSTMARK_SERVER_TOKEN is not configured. The report is saved and delivery can resume after server configuration.".into())
        } else {
            None
        },
        attempts: 0,
        next_attempt_at: now(),
        created_at: now(),
        other: Map::new(),
    };
    let (report, _) =
        app.store
            .create_public("report", ENV, auth.uuid(), auth.uuid(), None, None, |id| {
                report.id = id;
                report
            })?;
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
    // Rows of removed testing environments keep their own environment and are never sent.
    for candidate in app.store.list::<Report>("report", Some(ENV))? {
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
        if !report.deliverable()
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
        app.store.update::<Report>("report", &report.id, |stored| {
            *stored = report.clone();
            true
        })?;
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
        app.store.update::<Report>("report", &report.id, |stored| {
            *stored = report.clone();
            true
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_use_exact_recipients_and_do_not_attach_context_or_html() {
        let report = Report {
            id: "id".into(),
            owner_uuid: "zQo".into(),
            reporter_id: "c:owner".into(),
            message: "<html> remains text".into(),
            pr: None,
            status: "delivery_pending".into(),
            ..Default::default()
        };
        let payload = report_payload(&report, "mcport@teamofsilicons.com");
        assert_eq!(payload["To"], "saketdev12@gmail.com,shubhastro2@gmail.com");
        let body = payload["TextBody"].as_str().unwrap();
        assert!(body.contains("Reporter: c:owner (zQo)"), "{body}");
        assert!(!body.contains("Organization"));
        // Reports written before 0.3.0 still name their reporter.
        let legacy: Report = serde_json::from_value(json!({"id":"old","environment":"production","org_id":"tos","owner_id":"c:legacy","message":"m","pr":null,"status":"delivery_pending","failure_reason":null,"attempts":0,"next_attempt_at":0,"created_at":0})).unwrap();
        assert!(
            report_payload(&legacy, "x")["TextBody"]
                .as_str()
                .unwrap()
                .contains("Reporter: c:legacy")
        );
        for credential in [
            "sar_refresh",
            "sap_proof",
            "whsec_secret",
            "stk-0123",
            "eyJhbGciOi",
        ] {
            assert!(
                validate_report(&ReportInput {
                    message: format!("token {credential}"),
                    pr: None
                })
                .is_err()
            );
        }
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
    #[tokio::test]
    async fn settings_endpoint_is_a_retained_no_op_and_telemetry_route_is_gone() {
        use crate::test_support::fixture;
        use axum::http::StatusCode;
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        let (status, body) = f.as_("Ada", "GET", "/api/v1/settings", None).await;
        assert_eq!((status, body), (StatusCode::OK, json!({"data":{}})));
        // Clients released before 0.4.0 still send their preference; it is ignored.
        for body in [
            json!({"telemetry":false}),
            json!({"telemetry":true}),
            json!({}),
        ] {
            let (status, response) = f.as_("Ada", "PATCH", "/api/v1/settings", Some(body)).await;
            assert_eq!((status, response), (StatusCode::OK, json!({"data":{}})));
        }
        assert!(
            f.app
                .store
                .get::<Value>("settings", &settings_key("Ada"))
                .unwrap()
                .is_none()
        );
        let (status, _) = f
            .as_(
                "Ada",
                "PATCH",
                "/api/v1/settings",
                Some(json!({"theme":"dark"})),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = f
            .as_(
                "Ada",
                "POST",
                "/api/v1/telemetry",
                Some(
                    json!({"source":"cli","operation":"login","step":"start","outcome":"success"}),
                ),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn report_outbox_survives_restart_and_never_sends_test_reports() {
        use axum::{
            Router,
            http::{HeaderMap, StatusCode},
            routing::post,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
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
        let mut config = crate::state::Config::local(directory.path(), "http://127.0.0.1:1");
        config.postmark_token = Some("fixture-mail-token".into());
        let app = App::new(config.clone()).unwrap();
        for (id, environment, status) in [
            ("production-report", "production", "delivery_pending"),
            ("testing-report", "test-isolated", "delivery_pending"),
            ("legacy-delivered-report", "production", "delivered"),
            ("already-accepted-report", "production", "delivery_accepted"),
        ] {
            // Rows of a removed testing environment keep their environment.
            let mut report = Report {
                id: id.into(),
                owner_uuid: "zQo".into(),
                reporter_id: "c:owner".into(),
                message: "Private fixture bug reproduction".into(),
                pr: None,
                status: status.into(),
                created_at: now(),
                ..Default::default()
            };
            if environment != "production" {
                report
                    .other
                    .insert("environment".into(), json!(environment));
            }
            app.store
                .put(
                    "report",
                    id,
                    environment,
                    "zQo",
                    "zQo",
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
                "zQo",
                "zQo",
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
