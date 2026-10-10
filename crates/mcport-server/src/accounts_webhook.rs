//! `POST /webhooks/accounts`: signed events from Silicon Accounts.
//!
//! The signature is checked over the raw body (`X-Accounts-Timestamp`,
//! `X-Accounts-Signature: v1=…`, 5-minute tolerance) before parsing, and each
//! `event_id` is handled once. Delivery order is not guaranteed, so changes
//! older than what MCPort already applied are ignored.
use crate::{
    accounts::AccountRow,
    error::{Error, Result},
    execution,
    state::{App, now},
};
use axum::{Json, body::Bytes, extract::State, http::HeaderMap};
use serde_json::{Value, json};
use silicon_accounts_client::{
    DEFAULT_WEBHOOK_TOLERANCE, SIGNATURE_HEADER, TIMESTAMP_HEADER, WebhookError, WebhookEvent,
    WebhookPayload, verify_and_parse_webhook,
};

pub async fn receive(
    State(app): State<App>,
    headers: HeaderMap,
    raw: Bytes,
) -> Result<Json<Value>> {
    let secret = app.config.accounts_webhook_secret.as_deref().ok_or_else(|| {
        Error::new(
            503,
            "webhook_not_configured",
            "MCPort has no Silicon Accounts webhook secret configured.",
            "Set MCPORT_ACCOUNTS_WEBHOOK_SECRET to the whsec_… secret of MCPort's webhook and restart.",
        )
    })?;
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    let event = verify_and_parse_webhook(
        secret,
        &header(TIMESTAMP_HEADER),
        &header(SIGNATURE_HEADER),
        &raw,
        DEFAULT_WEBHOOK_TOLERANCE,
    )
    .map_err(|error| match error {
        WebhookError::InvalidBody(detail) => Error::new(
            400,
            "invalid_webhook_body",
            format!("The webhook body is not a Silicon Accounts event: {detail}"),
            "Send the event exactly as Silicon Accounts signed it.",
        ),
        other => Error::new(
            401,
            "invalid_webhook_signature",
            format!("Webhook verification failed: {other}"),
            "Check the X-Accounts-Timestamp and X-Accounts-Signature headers, the exact body bytes and MCPORT_ACCOUNTS_WEBHOOK_SECRET.",
        ),
    })?;
    if event
        .app_id
        .as_deref()
        .is_some_and(|app_id| app_id != app.config.app_id)
    {
        return Err(Error::new(
            400,
            "wrong_app",
            "This webhook event is addressed to another app.",
            "Point this app's webhook only at its own endpoint.",
        ));
    }
    let lock = app.lock(&format!("accounts-webhook:{}", event.event_id));
    let _guard = lock.lock().await;
    if app.store.webhook_handled(&event.event_id)? {
        return Ok(Json(json!({"received": true, "duplicate": true})));
    }
    let occurred_ms = event
        .occurred_at
        .map(|at| (at.unix_timestamp_nanos() / 1_000_000) as i64);
    handle(&app, &event, occurred_ms.unwrap_or(now() * 1000)).await?;
    app.store
        .record_webhook(&event.event_id, &event.event_type, occurred_ms)?;
    Ok(Json(json!({"received": true})))
}

async fn handle(app: &App, event: &WebhookEvent, at_ms: i64) -> Result<()> {
    match &event.payload {
        WebhookPayload::AccountIdChanged(data) => {
            app.store.update_account(&data.uuid, |row| {
                let mut row = row?;
                if at_ms < row.synced_at_ms {
                    return None;
                }
                row.id.clone_from(&data.new_id);
                if let Some(kind) = data.kind {
                    row.kind = kind.as_str().into();
                }
                row.synced_at_ms = at_ms;
                Some(row)
            })?;
            app.accounts.forget_resolution(&data.old_id);
            app.accounts.forget_resolution(&data.new_id);
            tracing::info!(event = %event.event_id, "Applied an account id change");
        }
        WebhookPayload::AccountUpdated(data) => {
            if let Some(account) = &data.account {
                app.store.update_account(&data.uuid, |row| {
                    let mut row = row?;
                    if account.version < row.version
                        || account.version == row.version && at_ms < row.synced_at_ms
                    {
                        return None;
                    }
                    row.kind = account.kind.as_str().into();
                    // Profile versions do not order id/custodian events. A late
                    // profile may improve the name, but never undo a newer transfer.
                    if at_ms >= row.synced_at_ms && !account.id.is_empty() {
                        row.id.clone_from(&account.id);
                    }
                    row.display_name.clone_from(&account.display_name);
                    row.pfp_url = Some(account.pfp_url.clone()).filter(|url| !url.is_empty());
                    if at_ms >= row.synced_at_ms
                        && row.is_silicon()
                        && let Some(custodian) = &account.custodian
                    {
                        row.custodian_uuid = Some(custodian.uuid.clone());
                        row.custodian_id = Some(custodian.id.clone()).filter(|id| !id.is_empty());
                    }
                    row.version = account.version;
                    row.synced_at_ms = row.synced_at_ms.max(at_ms);
                    Some(row)
                })?;
            }
        }
        WebhookPayload::CustodianChanged(data) => {
            // Circles are derived from this cache on every request, so the
            // previous custodian loses access as soon as this row changes.
            app.store.update_account(&data.uuid, |row| {
                let mut row = row.unwrap_or_else(|| AccountRow::new(&data.uuid, "silicon"));
                if at_ms < row.synced_at_ms {
                    return None;
                }
                row.kind = "silicon".into();
                row.custodian_uuid = data.to.as_ref().map(|to| to.uuid.clone());
                row.custodian_id = data
                    .to
                    .as_ref()
                    .map(|to| to.id.clone())
                    .filter(|id| !id.is_empty());
                row.synced_at_ms = at_ms;
                row.looked_up_at = now();
                Some(row)
            })?;
            tracing::info!(event = %event.event_id, "Applied a custodian change");
        }
        WebhookPayload::MembershipSignedOut(data) => {
            if data.reason.as_deref() == Some("app_revoked") {
                // MCPort itself revoked one refresh family (one CLI's logout or the
                // website's sign-out); the account's other sign-ins stay valid.
                tracing::info!(event = %event.event_id, "Ignoring a sign-out MCPort requested");
            } else {
                revoke(app, &data.uuid, at_ms, None)?;
            }
        }
        WebhookPayload::MembershipAccessRemoved(data) => {
            revoke(app, &data.uuid, at_ms, Some("access_removed"))?;
        }
        WebhookPayload::AccountDeleted(data) => {
            revoke(app, &data.uuid, at_ms, Some("deleted"))?;
            delete_account_data(app, &data.uuid)?;
        }
        WebhookPayload::Ping => {}
        _ => {
            tracing::info!(event = %event.event_id, kind = %event.event_type, "Ignoring a webhook event type MCPort does not use");
        }
    }
    Ok(())
}

/// Refuse tokens issued before `at_ms`, end the account's pending work and its
/// provider authorizations in progress, and record `status` if given.
pub fn revoke(app: &App, uuid: &str, at_ms: i64, status: Option<&str>) -> Result<()> {
    let before = at_ms.div_euclid(1000);
    app.store.update_account(uuid, |row| {
        let mut row = row.unwrap_or_else(|| AccountRow::new(uuid, ""));
        row.revoked_before = row.revoked_before.max(before);
        match status {
            Some("deleted") => {
                row.status = "deleted".into();
                row.id.clear();
                row.display_name.clear();
                row.pfp_url = None;
            }
            Some(status) if row.status != "deleted" => row.status = status.into(),
            _ => {}
        }
        Some(row)
    })?;
    app.accounts.forget_introspections();
    execution::cancel_account_work(app, uuid)?;
    for (kind, id) in app.store.owned_by(uuid, &["oauth_attempt"])? {
        app.store.delete(&kind, &id)?;
    }
    tracing::info!("Revoked an account's sign-ins and pending work");
    Ok(())
}

/// `account.deleted`: delete the account's connections (with everything on them),
/// hosts, directory entries, calls, settings, reports and personal provider
/// accounts; remove its access to what others shared with it. Others' data stays.
pub fn delete_account_data(app: &App, uuid: &str) -> Result<()> {
    use crate::{
        connections::{ConnectionRecord, GrantRecord, PolicyRecord, grant_key, policy_key},
        directory::EntryGrant,
    };
    for c in app.store.list::<ConnectionRecord>("connection", None)? {
        if c.owner_uuid == uuid {
            app.store.delete_connection(&c.id)?;
            execution::invalidate_connection(app, &c.id)?;
        }
    }
    for (kind, id) in app.store.owned_by(
        uuid,
        &[
            "host",
            "directory",
            "credential",
            "account_epoch",
            "call",
            "settings",
            "report",
            "oauth_attempt",
            "grant",
            "policy",
            "directory_grant",
        ],
    )? {
        app.store.delete(&kind, &id)?;
    }
    for grant in app.store.list::<GrantRecord>("grant", None)? {
        if grant.account_uuid == uuid || grant.created_by.as_deref() == Some(uuid) {
            app.store.delete(
                "grant",
                &grant_key(&grant.connection_id, &grant.account_uuid),
            )?;
            execution::invalidate_connection(app, &grant.connection_id)?;
        }
    }
    for policy in app.store.list::<PolicyRecord>("policy", None)? {
        if policy.account_uuid.as_deref() == Some(uuid) {
            app.store.delete(
                "policy",
                &policy_key(&policy.connection_id, &policy.tool, Some(uuid)),
            )?;
        }
    }
    for grant in app.store.list::<EntryGrant>("directory_grant", None)? {
        if grant.account_uuid == uuid || grant.created_by == uuid {
            app.store.delete(
                "directory_grant",
                &crate::directory::entry_grant_key(&grant.entry_id, &grant.account_uuid),
            )?;
        }
    }
    app.store.forget_allowances(uuid)?;
    app.store.forget_identity_links(uuid)?;
    tracing::info!("Deleted a deleted account's MCPort data");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, at, connection, event, fixture};
    use axum::http::StatusCode;
    use tower::ServiceExt;

    async fn deliver_raw(
        f: &Fixture,
        body: &str,
        secret: &str,
        timestamp: i64,
    ) -> (StatusCode, Value) {
        let signature = silicon_accounts_client::sign_webhook(secret, timestamp, body.as_bytes());
        let response = crate::router(f.app.clone())
            .oneshot(
                axum::http::Request::post("/webhooks/accounts")
                    .header("content-type", "application/json")
                    .header("X-Accounts-Timestamp", timestamp.to_string())
                    .header("X-Accounts-Signature", signature)
                    .body(axum::body::Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn deliveries_are_verified_on_raw_bytes_and_handled_once() {
        let f = fixture().await;
        let ping = event("evt_ping", "ping", &at(0), json!({}));
        let body = ping.to_string();
        let (status, reply) = deliver_raw(&f, &body, "whsec_someone_else_secret_000", now()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(reply["error"]["code"], "invalid_webhook_signature");
        let (status, _) =
            deliver_raw(&f, &body, crate::test_support::WEBHOOK_SECRET, now() - 301).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "older than the 5-minute tolerance"
        );
        let response = crate::router(f.app.clone())
            .oneshot(
                axum::http::Request::post("/webhooks/accounts")
                    .body(axum::body::Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "unsigned");
        let (status, reply) =
            deliver_raw(&f, "{not json", crate::test_support::WEBHOOK_SECRET, now()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(reply["error"]["code"], "invalid_webhook_body");
        let (status, reply) =
            deliver_raw(&f, &body, crate::test_support::WEBHOOK_SECRET, now()).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert_eq!(reply, json!({"received": true}));
        let (status, reply) = f.webhook(&ping).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(reply["duplicate"], true);
        let (status, _) = f
            .webhook(&event(
                "evt_new",
                "account.something_new",
                &at(0),
                json!({"uuid":"x"}),
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "unknown types are acknowledged");
        let mut other = event("evt_other_app", "ping", &at(0), json!({}));
        other["app_id"] = json!("briefcase");
        assert_eq!(f.webhook(&other).await.0, StatusCode::BAD_REQUEST);
        // Without a configured secret the endpoint says so.
        let mut config = (*f.app.config).clone();
        config.accounts_webhook_secret = None;
        let mut app = f.app.clone();
        app.config = std::sync::Arc::new(config);
        let response = crate::router(app)
            .oneshot(
                axum::http::Request::post("/webhooks/accounts")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn id_changes_and_profile_updates_apply_newest_first() {
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        f.auth("Ada").await;
        let renamed = event(
            "evt_1",
            "account.id_changed",
            &at(2),
            json!({"uuid":"Ada","membership_id":"mcport:Ada","kind":"carbon","old_id":"c:ada","new_id":"c:ada2"}),
        );
        assert_eq!(f.webhook(&renamed).await.0, StatusCode::OK);
        // A token minted before the change still carries the old id; the newer event wins.
        let (_, me) = f.as_("Ada", "GET", "/api/v1/me", None).await;
        assert_eq!(me["data"]["id"], "c:ada2");
        let older = event(
            "evt_0",
            "account.id_changed",
            &at(-60),
            json!({"uuid":"Ada","membership_id":"mcport:Ada","kind":"carbon","old_id":"c:old","new_id":"c:older"}),
        );
        assert_eq!(f.webhook(&older).await.0, StatusCode::OK);
        assert_eq!(f.app.store.account("Ada").unwrap().unwrap().id, "c:ada2");
        let updated = |id: &str, version: i64, name: &str| {
            event(
                id,
                "account.updated",
                &at(3),
                json!({"uuid":"Ada","membership_id":"mcport:Ada","changed":["display_name"],
            "account":{"uuid":"Ada","membership_id":"mcport:Ada","kind":"carbon","id":"c:ada2","display_name":name,"pfp_url":"https://accounts.example/p.png","version":version}}),
            )
        };
        f.webhook(&updated("evt_2", 5, "Ada Lovelace")).await;
        f.webhook(&updated("evt_3", 4, "Stale Name")).await;
        let row = f.app.store.account("Ada").unwrap().unwrap();
        assert_eq!(row.display_name, "Ada Lovelace");
        assert_eq!(row.version, 5);
        assert_eq!(
            row.pfp_url.as_deref(),
            Some("https://accounts.example/p.png")
        );
        // Unknown accounts are not created by profile events.
        f.webhook(&event(
            "evt_4",
            "account.id_changed",
            &at(0),
            json!({"uuid":"Nobody","old_id":"c:a","new_id":"c:b"}),
        ))
        .await;
        assert!(f.app.store.account("Nobody").unwrap().is_none());
    }

    #[tokio::test]
    async fn custodian_changes_move_access_at_once() {
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        f.carbon("Bob", "c:bob");
        f.silicon("Scout", "si:scout", "Ada");
        let (_, body) = f
            .as_(
                "Scout",
                "POST",
                "/api/v1/connections",
                Some(connection("scout-tools", "none", "")),
            )
            .await;
        let id = body["data"]["id"].as_str().unwrap().to_owned();
        assert_eq!(
            f.as_("Ada", "GET", &format!("/api/v1/connections/{id}"), None)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            f.as_("Bob", "GET", &format!("/api/v1/connections/{id}"), None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let moved = event(
            "evt_c",
            "silicon.custodian_changed",
            &at(1),
            json!({"uuid":"Scout","membership_id":"mcport:Scout",
            "from":{"uuid":"Ada","id":"c:ada"},"to":{"uuid":"Bob","id":"c:bob"}}),
        );
        assert_eq!(f.webhook(&moved).await.0, StatusCode::OK);
        // A high profile version is still older than the transfer; it must not
        // reinstate the former custodian or an earlier public id.
        let late = event(
            "evt_profile_before_transfer",
            "account.updated",
            &at(0),
            json!({"uuid":"Scout", "account": {"uuid":"Scout", "membership_id":"mcport:Scout",
                "kind":"silicon", "id":"si:scout", "display_name":"Scout renamed", "pfp_url":"",
                "version":99, "custodian":{"uuid":"Ada","id":"c:ada"}}}),
        );
        assert_eq!(f.webhook(&late).await.0, StatusCode::OK);
        assert_eq!(
            f.as_("Ada", "GET", &format!("/api/v1/connections/{id}"), None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let (status, body) = f
            .as_("Bob", "GET", &format!("/api/v1/connections/{id}"), None)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["access"], "custodian");
    }

    #[tokio::test]
    async fn sign_outs_end_older_tokens_and_pending_work_except_mcports_own() {
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        let old = f.token_with("Ada", |c| c["iat"] = json!(now() - 300));
        assert_eq!(
            f.call("GET", "/api/v1/me", Some(&old), None).await.0,
            StatusCode::OK
        );
        let pending = crate::execution::CallRecord {
            invocation: crate::execution::InvocationData {
                id: "pEn".into(),
                connection_id: "gone".into(),
                status: "queued".into(),
                created_at: now() - 200,
                ..Default::default()
            },
            caller_uuid: "Ada".into(),
            expires_at: now() + 60,
            ..Default::default()
        };
        crate::execution::save(&f.app, &pending).unwrap();
        let signed_out = |id: &str, reason: &str| {
            event(
                id,
                "membership.signed_out",
                &at(-100),
                json!({"uuid":"Ada","membership_id":"mcport:Ada","reason":reason}),
            )
        };
        f.webhook(&signed_out("evt_a", "app_revoked")).await;
        assert_eq!(
            f.call("GET", "/api/v1/me", Some(&old), None).await.0,
            StatusCode::OK,
            "one machine's logout"
        );
        f.webhook(&signed_out("evt_b", "session_revoked")).await;
        let (status, body) = f.call("GET", "/api/v1/me", Some(&old), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "signed_out");
        let call = f
            .app
            .store
            .get::<crate::execution::CallRecord>("call", "pEn")
            .unwrap()
            .unwrap();
        assert_eq!(call.invocation.status, "cancelled");
        assert_eq!(call.invocation.error.unwrap().code, "access_changed");
        assert_eq!(
            f.as_("Ada", "GET", "/api/v1/me", None).await.0,
            StatusCode::OK,
            "a new sign-in works"
        );
    }

    #[tokio::test]
    async fn removed_access_keeps_data_and_deletion_removes_only_the_accounts_own() {
        let f = fixture().await;
        f.carbon("Ada", "c:ada");
        f.carbon("Bob", "c:bob");
        let (_, body) = f
            .as_(
                "Ada",
                "POST",
                "/api/v1/connections",
                Some(connection("ada-tools", "none", "")),
            )
            .await;
        let ada_connection = body["data"]["id"].as_str().unwrap().to_owned();
        f.as_("Ada", "POST", "/api/v1/hosts", Some(json!({"name":"mac"})))
            .await;
        f.as_(
            "Ada",
            "PATCH",
            "/api/v1/settings",
            Some(json!({"telemetry":false})),
        )
        .await;
        f.as_(
            "Ada",
            "POST",
            "/api/v1/directory",
            Some(json!({"name":"Ada setup"})),
        )
        .await;
        let (_, body) = f
            .as_(
                "Bob",
                "POST",
                "/api/v1/connections",
                Some(connection("bob-tools", "none", "")),
            )
            .await;
        let bob_connection = body["data"]["id"].as_str().unwrap().to_owned();
        f.as_(
            "Bob",
            "POST",
            &format!("/api/v1/connections/{bob_connection}/access"),
            Some(json!({"account":"c:ada"})),
        )
        .await;
        f.as_(
            "Bob",
            "POST",
            &format!("/api/v1/connections/{ada_connection}/access"),
            Some(json!({"account":"c:bob"})),
        )
        .await;
        f.as_(
            "Ada",
            "POST",
            &format!("/api/v1/connections/{ada_connection}/access"),
            Some(json!({"account":"c:bob"})),
        )
        .await;
        let old = f.token_with("Ada", |c| c["iat"] = json!(now() - 300));
        let removed = event(
            "evt_r",
            "membership.access_removed",
            &at(-100),
            json!({"uuid":"Ada","membership_id":"mcport:Ada"}),
        );
        f.webhook(&removed).await;
        assert_eq!(
            f.call("GET", "/api/v1/connections", Some(&old), None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            f.app.store.account("Ada").unwrap().unwrap().status,
            "access_removed"
        );
        // Her data stays; what she shared is frozen for others meanwhile.
        assert!(
            f.app
                .store
                .get::<crate::connections::ConnectionRecord>("connection", &ada_connection)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            f.as_(
                "Bob",
                "GET",
                &format!("/api/v1/connections/{ada_connection}"),
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        let deleted = event(
            "evt_d",
            "account.deleted",
            &at(0),
            json!({"uuid":"Ada","membership_id":"mcport:Ada"}),
        );
        assert_eq!(f.webhook(&deleted).await.0, StatusCode::OK);
        for kind in ["connection", "host", "settings", "directory"] {
            assert!(
                f.app.store.owned_by("Ada", &[kind]).unwrap().is_empty(),
                "{kind}"
            );
        }
        assert!(
            f.app
                .store
                .get::<crate::connections::ConnectionRecord>("connection", &ada_connection)
                .unwrap()
                .is_none()
        );
        // Bob's connection survives; only Ada's access to it is gone.
        let (status, body) = f
            .as_(
                "Bob",
                "GET",
                &format!("/api/v1/connections/{bob_connection}/access"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"], json!([]));
        let row = f.app.store.account("Ada").unwrap().unwrap();
        assert_eq!((row.status.as_str(), row.id.as_str()), ("deleted", ""));
        assert_eq!(
            f.as_("Ada", "GET", "/api/v1/me", None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
}
