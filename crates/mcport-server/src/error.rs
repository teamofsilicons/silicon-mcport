use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use mcport_core::{ApiError, ErrorEnvelope};

#[derive(Debug)]
pub struct Error(pub StatusCode, pub ApiError);
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(status: u16, code: &str, message: impl Into<String>, recovery: &str) -> Self {
        Self(
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            ApiError {
                code: code.into(),
                message: message.into(),
                recovery: Some(recovery.into()),
                outcome_unknown: false,
            },
        )
    }
    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(
            400,
            "invalid_input",
            message,
            "Inspect this command's help and correct the input.",
        )
    }
    pub fn denied() -> Self {
        Self::new(
            403,
            "access_denied",
            "You do not have access to this action.",
            "Ask the connection owner to grant access.",
        )
    }
    pub fn missing() -> Self {
        Self::new(
            404,
            "not_found",
            "This resource does not exist or is not available to you.",
            "List the resources available in your current account and organization.",
        )
    }
    pub fn expired() -> Self {
        Self::new(
            401,
            "authentication_required",
            "The session is missing, expired or revoked.",
            "Refresh the session or log in with a new app-bound SLT.",
        )
    }
    pub fn internal() -> Self {
        Self::new(
            500,
            "internal_error",
            "The service could not complete this operation.",
            "Retry a read; inspect activity before repeating a write. Report the operation ID if this persists.",
        )
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorEnvelope { error: self.1 })).into_response()
    }
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        if matches!(e,rusqlite::Error::SqliteFailure(ref x,_) if x.code==rusqlite::ErrorCode::ConstraintViolation)
        {
            Self::new(
                409,
                "conflict",
                "A record with this identity or name already exists.",
                "Choose another name or update the existing resource.",
            )
        } else {
            tracing::error!(error=%e,"storage failure");
            Self::internal()
        }
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::internal()
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        tracing::error!(error=%e,"local storage failure");
        Self::internal()
    }
}
impl From<silicon_iam_client::Error> for Error {
    fn from(e: silicon_iam_client::Error) -> Self {
        let status = if matches!(e, silicon_iam_client::Error::Api(_)) {
            401
        } else {
            503
        };
        Self::new(
            status,
            "iam_unavailable_or_rejected",
            "IAM could not validate this session or operation.",
            "Check IAM availability and the current app configuration. Re-authenticate only if the credential was rejected.",
        )
    }
}
impl From<mcport_mcp::McpError> for Error {
    fn from(e: mcport_mcp::McpError) -> Self {
        Self(StatusCode::BAD_GATEWAY,ApiError{code:e.code,message:e.message,recovery:Some("Inspect connection health, provider authentication and the tool schema before retrying.".into()),outcome_unknown:e.outcome_unknown})
    }
}
