//! Authentication: every `/api/v1` route except discovery takes a Silicon
//! Accounts access token (`Authorization: Bearer <JWT>`, `aud` = MCPort's app id),
//! verified locally against the cached JWKS. Sensitive routes also ask Accounts
//! (introspection, cached ≤ 30 s) so revocation applies at once. MCPort issues no
//! sessions or cookies of its own.
use crate::{
    accounts::{self, ACCOUNT_MAX_AGE, AccountRow, SIGN_IN_AGAIN},
    error::{Error, Result},
    state::{App, now},
};
use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, header, request::Parts},
};
use std::sync::Arc;

/// The verified caller of one request.
#[derive(Clone)]
pub struct Auth {
    /// MCPort's row for the caller, refreshed as described in `accounts`.
    pub account: AccountRow,
    /// Token expiry (Unix seconds).
    pub expires_at: i64,
    token: Arc<str>,
}
impl Auth {
    pub fn uuid(&self) -> &str {
        &self.account.uuid
    }
    pub fn token(&self) -> &str {
        &self.token
    }
    /// An authority for stored work that runs without a request (host jobs, OAuth
    /// callbacks). It carries no token, so it cannot be introspected.
    pub fn for_row(account: AccountRow) -> Self {
        Self {
            account,
            expires_at: now(),
            token: Arc::from(""),
        }
    }
}

/// A caller whose sign-in Silicon Accounts confirmed as active just now (≤ 30 s):
/// used for execution, provider accounts, sharing, deletion and host creation.
pub struct Live(pub Auth);

impl FromRequestParts<App> for Auth {
    type Rejection = Error;
    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self> {
        authenticate(app, &parts.headers).await
    }
}
impl FromRequestParts<App> for Live {
    type Rejection = Error;
    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self> {
        let auth = authenticate(app, &parts.headers).await?;
        app.accounts
            .ensure_active(auth.token(), auth.uuid())
            .await?;
        Ok(Live(auth))
    }
}

pub fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then(|| token.to_owned())
}
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|s| {
            s.trim()
                .strip_prefix(&format!("{name}="))
                .map(str::to_owned)
        })
}
fn access_token(headers: &HeaderMap) -> Result<String> {
    if let Some(token) = bearer(headers) {
        if token.len() > 16 * 1024 {
            return Err(Error::new(
                401,
                "invalid_token",
                "The access token is too large to be a Silicon Accounts token.",
                SIGN_IN_AGAIN,
            ));
        }
        return Ok(token);
    }
    let scheme = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(' ').next().unwrap_or("").to_ascii_lowercase());
    if scheme.as_deref() == Some("proof") {
        return Err(Error::new(
            401,
            "proof_not_accepted",
            "MCPort does not accept Silicon Accounts proofs from other apps yet; it accepts only access tokens issued to MCPort.",
            "Send Authorization: Bearer <a Silicon Accounts access token for mcport>. The scopes mcport.connections.read and mcport.tools.call are reserved for a later release.",
        ));
    }
    Err(Error::expired())
}

/// Verify the request's access token and return the caller.
pub async fn authenticate(app: &App, headers: &HeaderMap) -> Result<Auth> {
    let token = access_token(headers)?;
    let claims = app.accounts.verify(&token).await?;
    if !crate::accounts::valid_account_uuid(&claims.sub) {
        return Err(Error::new(
            401,
            "invalid_token",
            "The access token does not name a valid account.",
            SIGN_IN_AGAIN,
        ));
    }
    let kind = claims.kind.map(|kind| kind.as_str()).ok_or_else(|| {
        Error::new(
            401,
            "invalid_token",
            "The access token does not say whether it belongs to a Carbon or a Silicon.",
            SIGN_IN_AGAIN,
        )
    })?;
    if app.store.retired_account_uuid(&claims.sub)? {
        return Err(signed_out());
    }
    let iat = claims.iat.unwrap_or(0);
    // `iat` is in whole seconds, so a token issued in the very second of a
    // revocation may predate it (an STK rotation just ended it) or follow it (a new
    // sign-in right after). Silicon Accounts knows which: introspection decides.
    let mut confirmed = false;
    let (mut account, lookup) = loop {
        let mut verdict = None;
        let mut lookup = false;
        let mut tie = false;
        let row = app.store.update_account(&claims.sub, |current| {
            let mut row = current
                .clone()
                .unwrap_or_else(|| AccountRow::new(&claims.sub, kind));
            if current.is_none() {
                lookup = true;
            }
            if row.status == "deleted" {
                verdict = Some(deleted());
                return None;
            }
            if iat < row.revoked_before {
                verdict = Some(signed_out());
                return None;
            }
            if iat == row.revoked_before && row.revoked_before > 0 && !confirmed {
                tie = true;
                return None;
            }
            // A token issued after access was removed means the account signed in again.
            row.status = "active".into();
            row.signed_in_at = row.signed_in_at.max(iat);
            row.kind = kind.into();
            if let Some(id) = claims.id.as_deref().filter(|id| !id.is_empty())
                && iat.saturating_mul(1000) >= row.synced_at_ms
            {
                row.id = id.into();
            }
            if claims.fid.is_some() && claims.fid != row.last_fid {
                // A new sign-in: confirm custodian and profile with Accounts.
                row.last_fid.clone_from(&claims.fid);
                lookup = true;
            }
            if now() - row.looked_up_at > ACCOUNT_MAX_AGE {
                lookup = true;
            }
            Some(row)
        })?;
        if let Some(error) = verdict {
            return Err(error);
        }
        if tie {
            app.accounts
                .ensure_active(&token, &claims.sub)
                .await
                .map_err(|error| {
                    if error.1.code == "sign_in_revoked" {
                        signed_out()
                    } else {
                        error
                    }
                })?;
            confirmed = true;
            continue;
        }
        break (row.ok_or_else(Error::internal)?, lookup);
    };
    if lookup {
        match accounts::refresh(app, &account.uuid, false).await {
            // Lookups carry no name or photo; the caller shared them by signing in.
            Ok(Some(fresh)) => account = accounts::apply_profile(app, fresh).await,
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(code = %error.1.code, "Continuing with token claims after a failed account lookup");
            }
        }
        if account.status == "deleted" {
            return Err(deleted());
        }
    }
    Ok(Auth {
        account,
        expires_at: claims.exp,
        token: Arc::from(token),
    })
}

/// Revalidate work accepted earlier for `uuid` at `since` (Unix seconds): the
/// account must still exist and must not have been signed out or lost access
/// after the work was accepted. Times are whole seconds, so a revocation in the
/// same second as the work counts as after it (signing in again and retrying works).
pub fn for_account(app: &App, uuid: &str, since: i64) -> Result<Auth> {
    let account = app.store.account(uuid)?.ok_or_else(|| {
        Error::new(
            403,
            "access_changed",
            "The account that started this work is not known to MCPort any more.",
            "Start the work again while signed in.",
        )
    })?;
    if !account.active() || account.revoked_before > 0 && account.revoked_before >= since {
        return Err(Error::new(
            403,
            "access_changed",
            "The account that started this work signed out or removed MCPort's access after it started.",
            "Sign in again and start a new request.",
        ));
    }
    Ok(Auth::for_row(account))
}

fn deleted() -> Error {
    Error::new(
        401,
        "account_deleted",
        "This Silicon Accounts account was deleted.",
        "Use another account.",
    )
}
fn signed_out() -> Error {
    Error::new(
        401,
        "signed_out",
        "This sign-in ended: the account signed out everywhere, rotated its key or removed MCPort's access after this token was issued.",
        SIGN_IN_AGAIN,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestKey, fixture};
    use axum::http::StatusCode;
    use serde_json::{Value, json};
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn canonical_account_uuids_survive_auth_custody_and_webhooks() {
        let f = fixture().await;
        let owner = "bd5d6e33-3eec-470f-9a28-2efb2c1ef1c9";
        let child = "3ab13a61-b343-40c1-ad13-54b1a071a10a";
        f.carbon(owner, "c:uuid-owner");
        f.silicon(child, "si:uuid-child", owner);
        let (status, body) = f.as_(child, "GET", "/api/v1/me", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["uuid"], child);
        assert_eq!(body["data"]["custodian"]["uuid"], owner);
        let (status, body) = f
            .as_(
                owner,
                "POST",
                "/api/v1/connections",
                Some(crate::test_support::connection(
                    "uuid-connection",
                    "none",
                    "circle",
                )),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["owner"]["uuid"], owner);
        let (status, body) = f.as_(child, "GET", "/api/v1/connections", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.to_string().contains("uuid-connection"));
        let (status, body) = f
            .webhook(&crate::test_support::event(
                "evt-uuid-delete",
                "account.deleted",
                &crate::test_support::at(0),
                json!({"uuid":owner,"kind":"carbon","id":"c:uuid-owner"}),
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body) = f.as_(child, "GET", "/api/v1/connections", None).await;
        assert!(!body.to_string().contains("uuid-connection"));
    }

    #[tokio::test]
    async fn bearer_tokens_are_verified_locally_and_name_the_account() {
        let f = fixture().await;
        f.carbon("Ca1", "c:ada");
        f.silicon("Si1", "si:scout", "Ca1");
        let (status, body) = f.as_("Ca1", "GET", "/api/v1/me", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["uuid"], "Ca1");
        assert_eq!(body["data"]["id"], "c:ada");
        assert_eq!(body["data"]["kind"], "carbon");
        assert_eq!(body["data"]["display_name"], "ada");
        assert!(body["data"]["custodian"].is_null());
        let (status, body) = f.as_("Si1", "GET", "/api/v1/me", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["kind"], "silicon");
        assert_eq!(body["data"]["custodian"]["uuid"], "Ca1");
        assert_eq!(body["data"]["custodian"]["id"], "c:ada");
        // The JWKS was fetched once and reused; no introspection on plain reads.
        assert_eq!(f.stub.jwks_requests.load(Ordering::SeqCst), 1);
        assert_eq!(f.stub.introspections.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn names_and_photos_come_from_the_user_base_and_are_never_cleared_by_lookups() {
        let f = fixture().await;
        f.carbon("Ca1", "c:ada");
        f.member(
            "Ca1",
            "c:ada",
            "Ada Lovelace",
            Some("https://accounts.example/ada.png"),
        );
        f.carbon("Ca2", "c:grace");
        f.stub.users.lock().unwrap().remove("Ca2");
        // A sign-in reads the user base once: an app's lookup carries no name or photo.
        let (_, body) = f.as_("Ca1", "GET", "/api/v1/me", None).await;
        assert_eq!(body["data"]["display_name"], "Ada Lovelace", "{body}");
        assert_eq!(body["data"]["pfp_url"], "https://accounts.example/ada.png");
        assert_eq!(f.stub.user_reads.load(Ordering::SeqCst), 1);
        f.as_("Ca1", "GET", "/api/v1/me", None).await;
        assert_eq!(
            f.stub.user_reads.load(Ordering::SeqCst),
            1,
            "same sign-in: no new read"
        );
        // An account outside the user base keeps its id as its only name.
        let (status, body) = f.as_("Ca2", "GET", "/api/v1/me", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["display_name"], "");
        assert!(
            body["data"].get("pfp_url").is_none_or(Value::is_null),
            "{body}"
        );
        // A photo learned from account.updated survives the next lookup (which has none).
        let (status, _) = f
            .webhook(&crate::test_support::event(
                "evt-photo",
                "account.updated",
                &crate::test_support::at(0),
                json!({"uuid":"Ca2","membership_id":"mcport:Ca2","changed":["display_name","pfp_url"],
                    "account":{"uuid":"Ca2","membership_id":"mcport:Ca2","kind":"carbon","id":"c:grace",
                        "display_name":"Grace Hopper","pfp_url":"https://accounts.example/grace.png","version":3}}),
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        refresh_row_now(&f, "Ca2").await;
        let row = f.app.store.account("Ca2").unwrap().unwrap();
        assert_eq!(row.display_name, "Grace Hopper");
        assert_eq!(
            row.pfp_url.as_deref(),
            Some("https://accounts.example/grace.png")
        );
    }
    async fn refresh_row_now(f: &crate::test_support::Fixture, uuid: &str) {
        f.app
            .store
            .update_account(uuid, |row| {
                let mut row = row?;
                row.looked_up_at = 0;
                Some(row)
            })
            .unwrap();
        let fresh = accounts::refresh(&f.app, uuid, true)
            .await
            .unwrap()
            .unwrap();
        assert!(fresh.looked_up_at > 0);
    }

    #[tokio::test]
    async fn wrong_audience_issuer_expiry_signature_and_kind_are_refused() {
        let f = fixture().await;
        f.carbon("Ca1", "c:ada");
        let forged = TestKey::new("k1", 9).sign(&f.claims("Ca1"));
        let cases = [
            (
                f.token_with("Ca1", |c| c["aud"] = json!("briefcase")),
                "wrong_audience",
            ),
            (
                f.token_with("Ca1", |c| c["iss"] = json!("https://accounts.example")),
                "wrong_issuer",
            ),
            (
                f.token_with("Ca1", |c| {
                    c["exp"] = json!(now() - 3600);
                    c["iat"] = json!(now() - 7200);
                }),
                "token_expired",
            ),
            (forged, "invalid_token"),
            (
                f.token_with("Ca1", |c| c["kind"] = json!(null)),
                "invalid_token",
            ),
            (
                f.token_with("Ca1", |c| c["sub"] = json!("bad/uuid")),
                "invalid_token",
            ),
            ("not-a-jwt".to_owned(), "invalid_token"),
        ];
        for (token, code) in cases {
            let (status, body) = f
                .call("GET", "/api/v1/connections", Some(&token), None)
                .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{code}: {body}");
            assert_eq!(body["error"]["code"], code, "{body}");
            assert!(
                body["error"]["recovery"]
                    .as_str()
                    .unwrap()
                    .contains("mcport login")
            );
        }
    }

    #[tokio::test]
    async fn unknown_key_ids_refetch_the_jwks_at_most_every_thirty_seconds() {
        let f = fixture().await;
        f.carbon("Ca1", "c:ada");
        assert_eq!(
            f.as_("Ca1", "GET", "/api/v1/me", None).await.0,
            StatusCode::OK
        );
        // Accounts rotates to a new key: the first token signed with it refetches.
        let rotated = TestKey::new("k2", 11);
        f.stub.keys.lock().unwrap().push(rotated.jwk());
        let token = rotated.sign(&f.claims("Ca1"));
        let (status, body) = f.call("GET", "/api/v1/me", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(f.stub.jwks_requests.load(Ordering::SeqCst), 2);
        // A flood of unknown kids within the interval does not reach Accounts.
        for seed in 20..25 {
            let token = TestKey::new(&format!("forged-{seed}"), seed).sign(&f.claims("Ca1"));
            let (status, body) = f.call("GET", "/api/v1/me", Some(&token), None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["error"]["code"], "unknown_signing_key");
        }
        assert_eq!(f.stub.jwks_requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn only_bearer_access_tokens_are_accepted() {
        let f = fixture().await;
        for (header, code) in [
            (None, "authentication_required"),
            (Some("Proof sap_fixture"), "proof_not_accepted"),
            (
                Some("Basic bWNwb3J0OnNlY3JldA=="),
                "authentication_required",
            ),
            (Some("Bearer "), "authentication_required"),
        ] {
            let mut request = axum::http::Request::get("/api/v1/connections");
            if let Some(header) = header {
                request = request.header("authorization", header);
            }
            use tower::ServiceExt;
            let response = crate::router(f.app.clone())
                .oneshot(request.body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["error"]["code"], code, "{header:?}");
        }
    }

    #[tokio::test]
    async fn revocation_refuses_older_tokens_until_the_account_signs_in_again() {
        let f = fixture().await;
        f.carbon("Ca1", "c:ada");
        let old = f.token_with("Ca1", |c| c["iat"] = json!(now() - 120));
        assert_eq!(
            f.call("GET", "/api/v1/me", Some(&old), None).await.0,
            StatusCode::OK
        );
        crate::accounts_webhook::revoke(&f.app, "Ca1", (now() - 60) * 1000, Some("access_removed"))
            .unwrap();
        let (status, body) = f.call("GET", "/api/v1/me", Some(&old), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "signed_out");
        // A token issued after the revocation is a new sign-in: access returns.
        assert_eq!(
            f.as_("Ca1", "GET", "/api/v1/me", None).await.0,
            StatusCode::OK
        );
        assert_eq!(
            f.app.store.account("Ca1").unwrap().unwrap().status,
            "active"
        );
        // Delivery retries may arrive after that new sign-in. The older removal
        // still rejects old tokens but cannot freeze work owned by the account.
        crate::accounts_webhook::revoke(&f.app, "Ca1", (now() - 60) * 1000, Some("access_removed"))
            .unwrap();
        assert!(f.app.store.account("Ca1").unwrap().unwrap().active());
        // Deleted accounts never authenticate again.
        crate::accounts_webhook::revoke(&f.app, "Ca1", now() * 1000, Some("deleted")).unwrap();
        let fresh = f.token_with("Ca1", |c| c["iat"] = json!(now() + 5));
        let (status, body) = f.call("GET", "/api/v1/me", Some(&fresh), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "account_deleted");
    }

    #[tokio::test]
    async fn a_token_from_the_second_of_a_revocation_is_settled_by_introspection() {
        let f = fixture().await;
        f.carbon("Ca1", "c:ada");
        let second = now() - 30;
        let before = f.token_with("Ca1", |c| c["iat"] = json!(second));
        let after = f.token_with("Ca1", |c| c["iat"] = json!(second));
        assert_eq!(
            f.call("GET", "/api/v1/me", Some(&before), None).await.0,
            StatusCode::OK
        );
        // An STK rotation half a second after `before` was issued, in the same second.
        crate::accounts_webhook::revoke(&f.app, "Ca1", second * 1000 + 500, None).unwrap();
        f.stub.inactive.lock().unwrap().insert(before.clone());
        let introspections = f.stub.introspections.load(Ordering::SeqCst);
        let (status, body) = f
            .call("GET", "/api/v1/connections", Some(&before), None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(body["error"]["code"], "signed_out");
        // A new sign-in in that same second is confirmed active and accepted.
        let (status, body) = f
            .call("GET", "/api/v1/connections", Some(&after), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            f.stub.introspections.load(Ordering::SeqCst),
            introspections + 2
        );
        // Tokens from other seconds need no introspection.
        let older = f.token_with("Ca1", |c| c["iat"] = json!(second - 1));
        assert_eq!(
            f.call("GET", "/api/v1/me", Some(&older), None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            f.as_("Ca1", "GET", "/api/v1/me", None).await.0,
            StatusCode::OK
        );
        assert_eq!(
            f.stub.introspections.load(Ordering::SeqCst),
            introspections + 2
        );
        // Work accepted in the second of a revocation counts as accepted before it.
        assert_eq!(
            for_account(&f.app, "Ca1", second).err().unwrap().1.code,
            "access_changed"
        );
        assert!(for_account(&f.app, "Ca1", second + 1).is_ok());
    }

    #[tokio::test]
    async fn sensitive_routes_introspect_and_reuse_a_positive_answer_briefly() {
        let f = fixture().await;
        f.carbon("Ca1", "c:ada");
        let token = f.token("Ca1");
        let (status, body) = f
            .call(
                "POST",
                "/api/v1/hosts",
                Some(&token),
                Some(json!({"name":"mac"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(f.stub.introspections.load(Ordering::SeqCst), 1);
        let (status, _) = f
            .call(
                "POST",
                "/api/v1/hosts",
                Some(&token),
                Some(json!({"name":"mac-2"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            f.stub.introspections.load(Ordering::SeqCst),
            1,
            "cached for 30 s"
        );
        // Plain reads never introspect.
        assert_eq!(
            f.call("GET", "/api/v1/hosts", Some(&token), None).await.0,
            StatusCode::OK
        );
        assert_eq!(f.stub.introspections.load(Ordering::SeqCst), 1);
        // Accounts says the sign-in ended: sensitive routes refuse at once.
        f.stub.inactive.lock().unwrap().insert(token.clone());
        f.app.accounts.forget_introspections();
        let (status, body) = f
            .call(
                "POST",
                "/api/v1/hosts",
                Some(&token),
                Some(json!({"name":"mac-3"})),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "sign_in_revoked");
    }

    #[tokio::test]
    async fn deferred_work_is_refused_after_a_later_revocation() {
        let f = fixture().await;
        f.carbon("Ca1", "c:ada");
        f.auth("Ca1").await;
        let started = now() - 30;
        assert!(for_account(&f.app, "Ca1", started).is_ok());
        crate::accounts_webhook::revoke(&f.app, "Ca1", (now() - 10) * 1000, None).unwrap();
        assert_eq!(
            for_account(&f.app, "Ca1", started).err().unwrap().1.code,
            "access_changed"
        );
        assert!(for_account(&f.app, "Ca1", now()).is_ok());
        assert!(for_account(&f.app, "unknown", now()).is_err());
    }

    #[tokio::test]
    async fn removed_sign_in_routes_tell_old_clients_what_to_do() {
        let f = fixture().await;
        for (method, path) in [
            ("GET", "/api/v1/iam"),
            ("POST", "/api/v1/auth/login"),
            ("POST", "/api/v1/auth/browser/complete"),
            ("GET", "/api/v1/auth/status"),
        ] {
            let (status, body) = f.call(method, path, None, None).await;
            assert_eq!(status, StatusCode::GONE, "{path}");
            assert_eq!(body["error"]["code"], "client_update_required");
            assert!(
                body["error"]["recovery"]
                    .as_str()
                    .unwrap()
                    .contains("silicon-apps install mcport")
            );
        }
        let (status, body) = f.call("POST", "/webhooks/iam", None, Some(json!({}))).await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["error"]["code"], "webhook_moved");
        let (status, body) = f.call("GET", "/api/v1/discovery", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["app_id"], "mcport");
        assert_eq!(body["data"]["client_id"], "mcport");
        assert_eq!(body["data"]["accounts_url"], f.url);
        assert_eq!(
            body["data"]["install_url"],
            "https://apps.teamofsilicons.com/apps/mcport"
        );
        // The backend serves no website any more.
        let (status, _) = f.call("GET", "/", None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
