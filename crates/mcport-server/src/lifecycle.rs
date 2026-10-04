use crate::{
    auth,
    error::{Error, Result},
    execution,
    state::{App, Environment, hash},
};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use silicon_iam_client::{
    EnvironmentKey,
    webhook::{WebhookSecret, WebhookSecretKeyring, WebhookVerifier},
};
#[derive(Clone, Serialize, Deserialize)]
struct Lifecycle {
    fingerprint: String,
    receipt: Value,
    org: String,
    action: String,
    #[serde(default = "pending_phase")]
    phase: String,
    #[serde(default)]
    control_revision: i64,
}
fn pending_phase() -> String {
    "pending".into()
}
fn authority(app: &App, headers: &HeaderMap) -> Result<()> {
    use subtle::ConstantTimeEq;
    let expected = app
        .config
        .lifecycle_secret
        .as_deref()
        .filter(|s| s.len() >= 32)
        .ok_or_else(|| {
            Error::new(
                503,
                "lifecycle_not_configured",
                "Honeycomb lifecycle authority is not configured.",
                "Configure a dedicated service token on the backend and participant registration.",
            )
        })?;
    let actual = auth::bearer(headers).unwrap_or_default();
    if !bool::from(hash(expected).as_bytes().ct_eq(hash(&actual).as_bytes())) {
        return Err(Error::expired());
    }
    Ok(())
}
fn conflict() -> Error {
    Error::new(
        409,
        "lifecycle_conflict",
        "The lifecycle operation is stale, conflicting or out of order.",
        "Finish the previous operation or send the current Honeycomb revision.",
    )
}
pub async fn apply(
    State(app): State<App>,
    headers: HeaderMap,
    Path((org, environment, operation)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    authority(&app, &headers)?;
    for value in [&environment, &operation] {
        if uuid::Uuid::parse_str(value).map_or(true, |v| v.is_nil()) {
            return Err(Error::bad("Lifecycle identifiers must be non-nil UUIDs."));
        }
    }
    if org.is_empty()
        || org.len() > 128
        || body["org_id"] != org
        || body["app_id"] != app.config.app_id
        || body["environment_id"] != environment
        || body["operation_id"] != operation
    {
        return Err(Error::bad(
            "Lifecycle path, environment, application and body must agree.",
        ));
    }
    let action = body["action"]
        .as_str()
        .ok_or_else(|| Error::bad("Lifecycle action is required."))?;
    if !matches!(
        action,
        "prepare"
            | "import"
            | "clean"
            | "disable"
            | "restore"
            | "purge"
            | "rotate"
            | "rotate-key"
            | "retire-applications"
    ) {
        return Err(Error::bad("Unsupported lifecycle action."));
    }
    let number = |k: &str| {
        body[k]
            .as_i64()
            .filter(|v| *v > 0)
            .ok_or_else(|| Error::bad(format!("{k} must be positive.")))
    };
    let revision = number("environment_revision")?;
    let generation = number("generation")?;
    let key_version = number("key_version")?;
    let testing_key = body["testing_key"]
        .as_str()
        .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        .ok_or_else(|| Error::bad("Supply the current 32-character testing key."))?;
    let lock = app.lock(&format!("environment:{environment}"));
    let _guard = lock.lock().await;
    let fingerprint = hash(&body.to_string());
    let prior_op = app.store.get::<Lifecycle>("lifecycle", &operation)?;
    if let Some(op) = &prior_op {
        if op.fingerprint != fingerprint {
            return Err(conflict());
        }
        if op.receipt["state"] == "completed" {
            return Ok(Json(op.receipt.clone()));
        }
    }
    let previous = app.store.get::<Environment>("environment", &environment)?;
    let last = app
        .store
        .get::<Value>("lifecycle", &format!("state:{environment}"))?;
    // A persisted unfinished operation owns this environment even if a crash
    // occurred before it could publish the pending environment marker, or after
    // it published the active marker but before writing its completed receipt.
    for op in app.store.list::<Value>("lifecycle", Some(&environment))? {
        if op.get("receipt").is_some()
            && op["receipt"]["operation_id"] != operation
            && op["receipt"]["state"] != "completed"
        {
            return Err(conflict());
        }
    }
    let same_revision = last
        .as_ref()
        .is_some_and(|last| last["revision"].as_i64() == Some(revision));
    if let Some(last) = &last {
        let last_revision = last["revision"].as_i64().unwrap_or(0);
        if last["org_id"] != org || last_revision > revision {
            return Err(conflict());
        }
        if same_revision {
            if prior_op.is_none()
                || last["generation"].as_i64() != Some(generation)
                || last["key_version"].as_i64() != Some(key_version)
                || last.get("operation_id").is_some_and(|id| id != &operation)
                || last.get("action").is_some_and(|value| value != action)
            {
                return Err(conflict());
            }
        } else {
            if last["state"] == "pending"
                || last["state"] == "purged"
                || last["generation"].as_i64().unwrap_or(0) > generation
                || last["key_version"].as_i64().unwrap_or(0) > key_version
                || action != "clean" && last["generation"].as_i64() != Some(generation)
                || action == "clean" && last["generation"].as_i64().unwrap_or(0) >= generation
                || matches!(action, "rotate" | "rotate-key")
                    && last["key_version"].as_i64().unwrap_or(0) >= key_version
                || last["state"] == "disabled" && !matches!(action, "restore" | "purge")
                || action == "restore" && last["state"] != "disabled"
                || last["state"] == "retired" && !matches!(action, "import" | "purge")
            {
                return Err(conflict());
            }
            if last["key_version"].as_i64() == Some(key_version)
                && previous.as_ref().and_then(|env| env.iam_key.as_deref()) != Some(testing_key)
            {
                return Err(conflict());
            }
        }
    } else if !matches!(action, "prepare" | "import") {
        return Err(conflict());
    }
    // Legacy receipts may have reached the final state without the new applied
    // marker. A final state at this exact revision proves effects already ran.
    let published = same_revision && last.as_ref().is_some_and(|last| last["state"] != "pending");
    let applied = published || prior_op.as_ref().is_some_and(|op| op.phase == "applied");
    if applied && !same_revision {
        return Err(conflict());
    }
    let control_revision = prior_op
        .as_ref()
        .map(|op| op.control_revision)
        .filter(|revision| *revision > 0)
        .unwrap_or_else(|| {
            previous.as_ref().map_or(1, |env| {
                if same_revision {
                    env.control_revision.max(1)
                } else {
                    env.control_revision + 1
                }
            })
        });
    let app_secret = app
        .configured_test_secret(&environment)
        .cloned()
        .or_else(|| previous.as_ref().map(|e| e.app_secret.clone()))
        .unwrap_or_default();
    let mut env = Environment {
        id: environment.clone(),
        state: "pending".into(),
        generation,
        control_revision,
        app_secret,
        iam_key: Some(testing_key.into()),
    };
    let mut receipt = json!({"operation_id":operation,"environment_id":environment,"app_id":app.config.app_id,"environment_revision":revision,"generation":generation,"key_version":key_version,"state":"pending","retired_apps":body.get("retired_apps").cloned().unwrap_or(json!([]))});
    let mut op = Lifecycle {
        fingerprint,
        receipt: receipt.clone(),
        org: org.clone(),
        action: action.into(),
        phase: if applied { "applied" } else { "pending" }.into(),
        control_revision,
    };
    if !applied {
        app.store.put(
            "lifecycle",
            &operation,
            &environment,
            &org,
            "",
            None,
            &op,
            None,
        )?;
        app.store.put(
            "environment",
            &environment,
            &environment,
            &org,
            "",
            None,
            &env,
            None,
        )?;
        app.store.put("lifecycle", &format!("state:{environment}"), &environment, &org, "", None,
            &json!({"org_id":org,"operation_id":operation,"action":action,"revision":revision,"generation":generation,"key_version":key_version,"state":"pending"}), None)?;
        for c in app
            .store
            .list::<mcport_core::Connection>("connection", Some(&environment))?
        {
            execution::invalidate_connection(&app, &c.id)?;
        }
        if matches!(action, "clean" | "purge") {
            app.store.clear_environment(&environment)?;
        } else {
            // A control transition requires a fresh login; host registrations
            // remain, but no previous job or authority can cross the transition.
            for s in app
                .store
                .list::<auth::StoredSession>("session", Some(&environment))?
            {
                app.store.delete("session", &s.key)?;
            }
            for mut h in app
                .store
                .list::<crate::hosts::HostRecord>("host", Some(&environment))?
            {
                h.last_seen = 0;
                h.registered.clear();
                h.capabilities.clear();
                h.generation = generation;
                app.store.put(
                    "host",
                    &h.host.id,
                    &environment,
                    &h.host.org_id,
                    &h.host.owner_id,
                    Some(&h.host.name),
                    &h,
                    None,
                )?;
            }
        }
        // This checkpoint must be durable before making the environment active.
        // A retry can repeat idempotent effects only while access is still blocked.
        op.phase = "applied".into();
        app.store.put(
            "lifecycle",
            &operation,
            &environment,
            &org,
            "",
            None,
            &op,
            None,
        )?;
    }
    env.state = match action {
        "disable" => "disabled",
        "purge" => "purged",
        "retire-applications"
            if body["retired_apps"]
                .as_array()
                .is_some_and(|xs| xs.iter().any(|v| v == &app.config.app_id)) =>
        {
            "retired"
        }
        _ => "active",
    }
    .into();
    app.store.put(
        "environment",
        &environment,
        &environment,
        &org,
        "",
        None,
        &env,
        None,
    )?;
    app.store.put("lifecycle",&format!("state:{environment}"),&environment,&org,"",None,&json!({"org_id":org,"operation_id":operation,"action":action,"revision":revision,"generation":generation,"key_version":key_version,"state":env.state}),None)?;
    receipt["state"] = json!("completed");
    op.receipt = receipt.clone();
    op.phase = "completed".into();
    app.store.put(
        "lifecycle",
        &operation,
        &environment,
        &org,
        "",
        None,
        &op,
        None,
    )?;
    Ok(Json(receipt))
}
pub async fn receipt(
    State(app): State<App>,
    headers: HeaderMap,
    Path((org, environment, operation)): Path<(String, String, String)>,
) -> Result<Json<Value>> {
    authority(&app, &headers)?;
    let op = app
        .store
        .get::<Lifecycle>("lifecycle", &operation)?
        .ok_or_else(Error::missing)?;
    if op.org != org || op.receipt["environment_id"] != environment {
        return Err(Error::missing());
    }
    Ok(Json(op.receipt))
}
pub async fn webhook(
    State(app): State<App>,
    headers: HeaderMap,
    raw: Bytes,
) -> Result<Json<Value>> {
    let secret = app.config.webhook_secret.clone().ok_or_else(|| {
        Error::new(
            503,
            "webhook_not_configured",
            "IAM webhook signing material is not configured.",
            "Configure the separate signing secret issued by Honeycomb.",
        )
    })?;
    let version = std::env::var("MCPORT_WEBHOOK_SECRET_VERSION")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(1);
    let verifier = WebhookVerifier::new(
        WebhookSecretKeyring::new(
            version,
            WebhookSecret::new(secret).map_err(|_| Error::internal())?,
        )
        .map_err(|_| Error::internal())?,
    );
    let event = verifier.verify(&headers, &raw).map_err(|_| {
        Error::new(
            401,
            "invalid_webhook",
            "Webhook verification failed.",
            "Check the delivery's signature, timestamp, key version and exact body bytes.",
        )
    })?;
    let event_value = serde_json::to_value(event.event())?;
    let environment = if event.is_testing() {
        event_value["aggregate"]["environment_id"]
            .as_str()
            .ok_or_else(Error::denied)?
            .to_owned()
    } else {
        "production".into()
    };
    let environment_lock = app.lock(&format!("environment:{environment}"));
    let _environment_guard = environment_lock.lock().await;
    let lock = app.lock(&format!("webhook:{environment}"));
    let _guard = lock.lock().await;
    if event.is_testing() {
        let e = app.environment(&environment)?;
        let key = e.iam_key.as_ref().ok_or_else(Error::denied)?;
        event
            .verify_testing_environment(&EnvironmentKey::new(key)?)
            .map_err(|_| Error::denied())?;
        if event_value["aggregate"]["generation"].as_i64() != Some(e.generation) {
            return Err(Error::denied());
        }
    }
    // Authorization is live-introspected on every request and before dispatch;
    // no webhook payload can overwrite identity or expand access. Persist only
    // a dedup/version receipt, never undisclosed profiles or test credentials.
    let id = event.event_id().to_string();
    if app.store.get::<Value>("webhook", &id)?.is_some() {
        return Ok(Json(json!({"received":true})));
    }
    let aggregate = hash(
        &json!([
            environment,
            event_value["aggregate"]["type"],
            event_value["aggregate"]["id"]
        ])
        .to_string(),
    );
    let version = event_value["aggregate"]["version"].as_i64().unwrap_or(0);
    let prior = app
        .store
        .get::<Value>("webhook_version", &aggregate)?
        .and_then(|v| v["version"].as_i64())
        .unwrap_or(0);
    if version > prior {
        // Cancellation is conservative and idempotent. A duplicate received
        // after a crash cannot restore work or authorization.
        for c in app
            .store
            .list::<mcport_core::Connection>("connection", Some(&environment))?
        {
            execution::invalidate_connection(&app, &c.id)?;
        }
        app.store.put(
            "webhook_version",
            &aggregate,
            &environment,
            "",
            "",
            None,
            &json!({"version":version}),
            None,
        )?;
    }
    app.store.put(
        "webhook",
        &id,
        &environment,
        "",
        "",
        None,
        &json!({"received":true,"aggregate":aggregate,"version":version}),
        Some(0),
    )?;
    Ok(Json(json!({"received":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Config, now};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use std::time::Duration;

    const ENV: &str = "11111111-1111-4111-8111-111111111111";
    const KEY: &str = "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6";
    const SERVICE: &str = "lifecycle-service-secret-0000000001";
    const WEBHOOK: &str = "webhook-signing-secret-000000000001";

    fn fixture() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::from_env();
        config.data_dir = dir.path().into();
        config.app_id = "mcport".into();
        config.app_secret = "fixture-production-credential".into();
        config.test_app_secrets = "{}".into();
        config.lifecycle_secret = Some(SERVICE.into());
        config.webhook_secret = Some(WEBHOOK.into());
        (App::new(config).unwrap(), dir)
    }
    fn request(action: &str, revision: i64, generation: i64) -> Value {
        json!({"org_id":"tos","app_id":"mcport","environment_id":ENV,
            "operation_id":uuid::Uuid::new_v4().to_string(),"action":action,
            "environment_revision":revision,"generation":generation,"key_version":1,"testing_key":KEY})
    }
    async fn run(app: &App, body: &Value) -> Result<Value> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {SERVICE}").parse().unwrap(),
        );
        apply(
            State(app.clone()),
            headers,
            Path((
                "tos".into(),
                ENV.into(),
                body["operation_id"].as_str().unwrap().into(),
            )),
            Json(body.clone()),
        )
        .await
        .map(|result| result.0)
    }
    fn save_op(app: &App, body: &Value, phase: &str) {
        app.store.put("lifecycle", body["operation_id"].as_str().unwrap(), ENV, "tos", "", None,
            &Lifecycle { fingerprint: hash(&body.to_string()),
                receipt: json!({"operation_id":body["operation_id"],"environment_id":ENV,"state":"pending"}),
                org:"tos".into(), action:body["action"].as_str().unwrap().into(), phase:phase.into(),
                control_revision:body["environment_revision"].as_i64().unwrap() }, None).unwrap();
    }

    fn with_test_secret(app: &App, secret: &str) -> App {
        let mut config = (*app.config).clone();
        config.test_app_secrets = json!({ENV: secret}).to_string();
        App::new(config).unwrap()
    }

    #[tokio::test]
    async fn configured_test_credential_after_import_preserves_receipt_and_fences() {
        let (app, _dir) = fixture();
        let import = request("import", 1, 1);
        let receipt = run(&app, &import).await.unwrap();
        let original = app.environment(ENV).unwrap();
        assert!(original.app_secret.is_empty());
        assert_eq!(
            auth::iam(&app, &original).await.err().unwrap().1.code,
            "application_not_configured"
        );

        let restarted = with_test_secret(&app, "fresh-test-credential");
        let selected = restarted.environment(ENV).unwrap();
        assert_eq!(selected.app_secret, "fresh-test-credential");
        assert_eq!(selected.generation, original.generation);
        assert_eq!(selected.control_revision, original.control_revision);
        assert_eq!(selected.iam_key, original.iam_key);
        assert_eq!(run(&restarted, &import).await.unwrap(), receipt);
        // Resolving configuration never mutates a receipt or secretly persists it.
        assert!(
            restarted
                .store
                .get::<Environment>("environment", ENV)
                .unwrap()
                .unwrap()
                .app_secret
                .is_empty()
        );
        let removed = App::new((*app.config).clone()).unwrap();
        assert!(removed.environment(ENV).unwrap().app_secret.is_empty());
    }

    #[tokio::test]
    async fn test_configuration_never_provisions_or_reactivates_an_environment() {
        let (app, _dir) = fixture();
        let configured = with_test_secret(&app, "test-credential");
        assert_eq!(
            configured.environment(ENV).err().unwrap().1.code,
            "test_environment_unavailable"
        );
        run(&configured, &request("import", 1, 1)).await.unwrap();
        let selected = configured.environment(ENV).unwrap();
        run(&configured, &request("disable", 2, 1)).await.unwrap();
        let restarted = with_test_secret(&configured, "replacement-test-credential");
        assert_eq!(
            restarted.environment(ENV).err().unwrap().1.code,
            "test_environment_disabled"
        );
        assert!(restarted.assert_environment(&selected).is_err());
        run(&restarted, &request("purge", 3, 1)).await.unwrap();
        assert!(
            with_test_secret(&restarted, "test-credential")
                .environment(ENV)
                .is_err()
        );
    }

    #[tokio::test]
    async fn configured_test_rotation_is_explicit_and_production_stays_separate() {
        let (app, _dir) = fixture();
        let configured = with_test_secret(&app, "original-test-credential");
        run(&configured, &request("import", 1, 1)).await.unwrap();
        let rotated = with_test_secret(&configured, "rotated-test-credential");
        assert_eq!(
            rotated.environment(ENV).unwrap().app_secret,
            "rotated-test-credential"
        );
        assert_eq!(
            rotated.environment("production").unwrap().app_secret,
            "fixture-production-credential"
        );
        let mut config = (*rotated.config).clone();
        config.test_app_secrets = "{}".into();
        // Without an override, only this plane's persisted credential is eligible.
        let stored = App::new(config).unwrap();
        assert_eq!(
            stored.environment(ENV).unwrap().app_secret,
            "original-test-credential"
        );
        let stale = rotated.environment(ENV).unwrap();
        run(&rotated, &request("clean", 2, 2)).await.unwrap();
        assert_eq!(rotated.environment(ENV).unwrap().generation, 2);
        assert_eq!(
            rotated.assert_environment(&stale).err().unwrap().1.code,
            "environment_changed"
        );
    }

    #[tokio::test]
    async fn transitions_advance_control_fence_once_and_restore_requires_fresh_login() {
        let (app, _dir) = fixture();
        let prepare = request("prepare", 1, 1);
        run(&app, &prepare).await.unwrap();
        assert_eq!(app.environment(ENV).unwrap().control_revision, 1);
        run(&app, &prepare).await.unwrap();
        assert_eq!(app.environment(ENV).unwrap().control_revision, 1);
        let disable = request("disable", 2, 1);
        run(&app, &disable).await.unwrap();
        assert!(app.environment(ENV).is_err());
        let restore = request("restore", 3, 1);
        run(&app, &restore).await.unwrap();
        let current = app.environment(ENV).unwrap();
        assert_eq!(current.generation, 1);
        assert_eq!(current.control_revision, 3);
    }

    #[tokio::test]
    async fn unfinished_operation_owns_crash_window_and_cannot_rewind_newer_state() {
        let (app, _dir) = fixture();
        run(&app, &request("prepare", 1, 1)).await.unwrap();
        let old = request("clean", 2, 2);
        save_op(&app, &old, "pending"); // Crash before publishing environment pending.
        let newer = request("clean", 3, 3);
        assert_eq!(
            run(&app, &newer).await.unwrap_err().1.code,
            "lifecycle_conflict"
        );
        // A database from an older release may already contain a superseding
        // operation. Resuming the old receipt must not rewrite that state.
        app.store.put("lifecycle", &format!("state:{ENV}"), ENV, "tos", "", None,
            &json!({"org_id":"tos","operation_id":newer["operation_id"],"revision":3,"generation":3,"key_version":1,"state":"active"}), None).unwrap();
        assert_eq!(
            run(&app, &old).await.unwrap_err().1.code,
            "lifecycle_conflict"
        );
        assert_eq!(
            app.store
                .get::<Value>("lifecycle", &format!("state:{ENV}"))
                .unwrap()
                .unwrap()["revision"],
            3
        );
    }

    #[tokio::test]
    async fn cleanup_retry_after_publication_preserves_new_data_even_after_restart() {
        let (app, _dir) = fixture();
        run(&app, &request("prepare", 1, 1)).await.unwrap();
        app.store
            .put(
                "fixture",
                "old",
                ENV,
                "tos",
                "",
                None,
                &json!({"before":true}),
                None,
            )
            .unwrap();
        let clean = request("clean", 2, 2);
        run(&app, &clean).await.unwrap();
        assert!(app.store.get::<Value>("fixture", "old").unwrap().is_none());
        // Simulate the precise crash boundary: effects and active state were
        // published, but the receipt remained at its durable applied checkpoint.
        save_op(&app, &clean, "applied");
        app.store
            .put(
                "fixture",
                "new",
                ENV,
                "tos",
                "",
                None,
                &json!({"after":true}),
                None,
            )
            .unwrap();
        let restarted = App::new((*app.config).clone()).unwrap();
        run(&restarted, &clean).await.unwrap();
        assert_eq!(
            restarted
                .store
                .get::<Value>("fixture", "new")
                .unwrap()
                .unwrap()["after"],
            true
        );
        assert_eq!(restarted.environment(ENV).unwrap().control_revision, 2);
        // Legacy pending receipts with a matching published final state are safe too.
        save_op(&restarted, &clean, "pending");
        run(&restarted, &clean).await.unwrap();
        assert!(
            restarted
                .store
                .get::<Value>("fixture", "new")
                .unwrap()
                .is_some()
        );
    }

    fn delivery(generation: i64, version: i64) -> (HeaderMap, Bytes, String) {
        let event_id = uuid::Uuid::new_v4().to_string();
        let body = json!({"test":{"testing_key":KEY,"metadata":{"spec_version":"1.0","event_id":event_id,
            "event_type":"organization.membership.created.v1","occurred_at":"2026-09-04T00:00:00Z","organization_id":null,
            "aggregate":{"type":"membership","id":"00000000-0000-0000-0000-000000000002","version":version,"environment_id":ENV,"generation":generation}},"data":{}}});
        let raw = serde_json::to_vec(&body).unwrap();
        let timestamp = now().to_string();
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(WEBHOOK.as_bytes()).unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(&raw);
        let signature = format!("v1={}", hex::encode(mac.finalize().into_bytes()));
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-silicon-iam-event-id", event_id.clone()),
            ("x-silicon-iam-timestamp", timestamp),
            ("x-silicon-iam-key-version", "1".into()),
            ("x-silicon-iam-signature", signature),
        ] {
            headers.insert(name, value.parse().unwrap());
        }
        (headers, raw.into(), event_id)
    }

    #[tokio::test]
    async fn webhook_waiting_on_lifecycle_rechecks_generation_before_any_write() {
        let (app, _dir) = fixture();
        run(&app, &request("prepare", 1, 1)).await.unwrap();
        let lock = app.lock(&format!("environment:{ENV}"));
        let guard = lock.lock().await;
        let (headers, raw, event_id) = delivery(1, 1);
        let task_app = app.clone();
        let mut task = tokio::spawn(async move { webhook(State(task_app), headers, raw).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut task)
                .await
                .is_err()
        );
        // Publish a new plane under the same lock lifecycle owns.
        let mut env = app.environment(ENV).unwrap();
        env.generation = 2;
        env.control_revision = 2;
        app.store
            .put("environment", ENV, ENV, "tos", "", None, &env, None)
            .unwrap();
        drop(guard);
        assert_eq!(task.await.unwrap().unwrap_err().1.code, "access_denied");
        assert!(
            app.store
                .get::<Value>("webhook", &event_id)
                .unwrap()
                .is_none()
        );
        assert!(
            app.store
                .list::<Value>("webhook_version", Some(ENV))
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn concurrent_webhooks_deduplicate_and_never_lower_aggregate_version() {
        let (app, _dir) = fixture();
        run(&app, &request("prepare", 1, 1)).await.unwrap();
        let (high_headers, high_raw, _) = delivery(1, 5);
        let (low_headers, low_raw, _) = delivery(1, 2);
        let (a, b, c) = tokio::join!(
            webhook(State(app.clone()), high_headers.clone(), high_raw.clone()),
            webhook(State(app.clone()), low_headers, low_raw),
            webhook(State(app.clone()), high_headers, high_raw)
        );
        let _ = a.unwrap();
        let _ = b.unwrap();
        let _ = c.unwrap();
        assert_eq!(
            app.store.list::<Value>("webhook", Some(ENV)).unwrap().len(),
            2
        );
        let versions = app
            .store
            .list::<Value>("webhook_version", Some(ENV))
            .unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0]["version"], 5);
    }
}
