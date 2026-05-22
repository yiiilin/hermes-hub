pub mod admin;
pub mod attachments;
pub mod auth;
pub mod channel_protocol;
pub mod hermes_proxy;
pub mod invites;
pub mod llm_proxy;
pub mod workspace;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde::Serialize;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(auth::router())
        .merge(admin::router())
        .merge(invites::router())
        .merge(attachments::router())
        .merge(channel_protocol::router())
        .merge(llm_proxy::router())
        .merge(workspace::router())
        .merge(crate::channel::routes::router())
        .route("/api/hermes/{*path}", any(hermes_proxy::proxy))
}

#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    Unauthorized,
    Forbidden,
    Conflict(&'static str),
    SessionLimitExceeded { max_sessions_per_user: u32 },
    Gone(&'static str),
    NotFound(&'static str),
    BadGateway(&'static str),
    GatewayTimeout(&'static str),
    Internal,
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_sessions_per_user: Option<u32>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, message, max_sessions_per_user) = match self {
            ApiError::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                "bad_request",
                message.to_string(),
                None,
            ),
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "authentication required".to_string(),
                None,
            ),
            ApiError::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "admin access required".to_string(),
                None,
            ),
            ApiError::Conflict(message) => {
                (StatusCode::CONFLICT, "conflict", message.to_string(), None)
            }
            ApiError::SessionLimitExceeded {
                max_sessions_per_user,
            } => (
                StatusCode::CONFLICT,
                "session_limit_exceeded",
                "session limit exceeded".to_string(),
                Some(max_sessions_per_user),
            ),
            ApiError::Gone(message) => (StatusCode::GONE, "gone", message.to_string(), None),
            ApiError::NotFound(message) => (
                StatusCode::NOT_FOUND,
                "not_found",
                message.to_string(),
                None,
            ),
            ApiError::BadGateway(message) => (
                StatusCode::BAD_GATEWAY,
                "bad_gateway",
                message.to_string(),
                None,
            ),
            ApiError::GatewayTimeout(message) => (
                StatusCode::GATEWAY_TIMEOUT,
                "gateway_timeout",
                message.to_string(),
                None,
            ),
            ApiError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal server error".to_string(),
                None,
            ),
        };

        (
            status,
            Json(ErrorBody {
                error,
                message,
                max_sessions_per_user,
            }),
        )
            .into_response()
    }
}
