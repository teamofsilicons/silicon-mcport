//! Restore McPort's application authorization through the Cells service gateway.
//!
//! Cells consumes its own Authorization header. The public edge carries McPort's
//! original value in `x-mcport-authorization` instead. That value is never trusted
//! as identity: the usual Accounts JWT or host-token verifier still checks it.
//! Before restoring it, verify Cells' opaque request capability with the local
//! instance broker. Caller-supplied x-cells-identity is deliberately ignored.
use crate::error::{Error, Result};
use axum::{
    extract::{Request, State},
    http::{Method, header},
    middleware::Next,
    response::Response,
};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct CellsIngress {
    context_url: String,
    http: reqwest::Client,
}

impl CellsIngress {
    pub fn from_env() -> std::result::Result<Option<Self>, String> {
        match std::env::var("CELLS_API_URL") {
            Ok(url) => Self::new(&url).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err("CELLS_API_URL must be valid UTF-8".into()),
        }
    }

    fn new(base: &str) -> std::result::Result<Self, String> {
        let url = url::Url::parse(base).map_err(|_| "Invalid CELLS_API_URL")?;
        let loopback = url.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        });
        if url.scheme() != "http"
            || !loopback
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err("CELLS_API_URL must be the runtime's literal loopback HTTP broker".into());
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| "Could not initialize the Cells context verifier")?;
        Ok(Self {
            context_url: format!("{}/context", base.trim_end_matches('/')),
            http,
        })
    }

    async fn verify(&self, headers: &axum::http::HeaderMap) -> Result<()> {
        let capability = headers.get("x-cells-context").ok_or_else(denied)?;
        if capability.as_bytes().is_empty() || capability.as_bytes().len() > 4096 {
            return Err(denied());
        }
        let mut response = self
            .http
            .get(&self.context_url)
            .header("x-cells-context", capability)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(if response.status().is_client_error() {
                denied()
            } else {
                unavailable()
            });
        }
        if response.content_length().is_some_and(|n| n > 16 * 1024) {
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if bytes.len().saturating_add(chunk.len()) > 16 * 1024 {
                return Err(unavailable());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
        if !body.is_object()
            || body.get("identity").is_none()
            || !body.get("params").is_some_and(Value::is_object)
        {
            return Err(unavailable());
        }
        Ok(())
    }
}

fn denied() -> Error {
    Error::new(
        401,
        "cells_context_required",
        "This request has no active Cells service context.",
        "Use the public McPort API address.",
    )
}
fn unavailable() -> Error {
    Error::new(
        503,
        "cells_context_unavailable",
        "Cells could not confirm this request's service context.",
        "Retry when the Cells service is available.",
    )
}

pub async fn restore(
    State(ingress): State<Option<CellsIngress>>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    // A direct runtime health probe has no ingress context and carries no data.
    let health = request.uri().path() == "/health"
        && matches!(*request.method(), Method::GET | Method::HEAD);
    let original = request.headers_mut().remove("x-mcport-authorization");
    if let Some(ingress) = ingress {
        if !health {
            ingress.verify(request.headers()).await?;
        }
        // No credential may slip through under both names. Cells normally strips
        // Authorization; this also prevents accidental bypass in a miswired edge.
        request.headers_mut().remove(header::AUTHORIZATION);
        if !health && let Some(value) = original {
            request.headers_mut().insert(header::AUTHORIZATION, value);
        }
    }
    request.headers_mut().remove("x-cells-context");
    request.headers_mut().remove("x-cells-identity");
    request.headers_mut().remove("x-cells-params");
    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{HeaderMap, StatusCode},
        routing::get,
    };
    use tower::ServiceExt;

    #[test]
    fn broker_is_only_literal_loopback_without_credentials_or_redirect_paths() {
        for bad in [
            "https://127.0.0.1:18080",
            "http://localhost:18080",
            "http://10.0.0.1",
            "http://user@127.0.0.1",
            "http://127.0.0.1/other",
            "http://127.0.0.1?q=1",
        ] {
            assert!(CellsIngress::new(bad).is_err(), "{bad}");
        }
        assert!(CellsIngress::new("http://127.0.0.1:18080").is_ok());
    }

    async fn echo(headers: HeaderMap) -> String {
        assert!(!headers.contains_key("x-cells-context"));
        assert!(!headers.contains_key("x-cells-identity"));
        assert!(!headers.contains_key("x-mcport-authorization"));
        headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("none")
            .into()
    }

    #[tokio::test]
    async fn only_a_verified_cells_capability_restores_application_authorization() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let broker = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/context",
                    get(|headers: HeaderMap| async move {
                        if headers
                            .get("x-cells-context")
                            .is_some_and(|h| h == "active-capability")
                        {
                            (
                                StatusCode::OK,
                                axum::Json(serde_json::json!({"identity":null,"params":{}})),
                            )
                        } else {
                            (
                                StatusCode::FORBIDDEN,
                                axum::Json(serde_json::json!({"error":"expired"})),
                            )
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let app = Router::new()
            .route("/api/v1/me", get(echo))
            .route("/health", get(echo))
            .layer(axum::middleware::from_fn_with_state(
                Some(CellsIngress::new(&format!("http://{addr}")).unwrap()),
                restore,
            ));
        for (capability, status) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some("forged"), StatusCode::UNAUTHORIZED),
            (Some("active-capability"), StatusCode::OK),
        ] {
            let mut request = axum::http::Request::get("/api/v1/me")
                .header("x-mcport-authorization", "Bearer application-token")
                .header("authorization", "Bearer must-not-win")
                .header("x-cells-identity", "forged");
            if let Some(capability) = capability {
                request = request.header("x-cells-context", capability);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            if status == StatusCode::OK {
                assert_eq!(
                    axum::body::to_bytes(response.into_body(), 1024)
                        .await
                        .unwrap(),
                    "Bearer application-token"
                );
            }
        }
        let response = app
            .oneshot(
                axum::http::Request::get("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        broker.abort();
    }

    #[tokio::test]
    async fn standalone_mode_does_not_accept_the_forwarding_header() {
        let app = Router::new().route("/api/v1/me", get(echo)).layer(
            axum::middleware::from_fn_with_state(None::<CellsIngress>, restore),
        );
        let response = app
            .oneshot(
                axum::http::Request::get("/api/v1/me")
                    .header("x-mcport-authorization", "Bearer forged")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap(),
            "none"
        );
    }
}
