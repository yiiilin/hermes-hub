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
    ConflictMessage(String),
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
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, message) = match self {
            ApiError::BadRequest(message) => {
                (StatusCode::BAD_REQUEST, "bad_request", message.to_string())
            }
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "authentication required".to_string(),
            ),
            ApiError::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "admin access required".to_string(),
            ),
            ApiError::Conflict(message) => (StatusCode::CONFLICT, "conflict", message.to_string()),
            ApiError::ConflictMessage(message) => (StatusCode::CONFLICT, "conflict", message),
            ApiError::Gone(message) => (StatusCode::GONE, "gone", message.to_string()),
            ApiError::NotFound(message) => {
                (StatusCode::NOT_FOUND, "not_found", message.to_string())
            }
            ApiError::BadGateway(message) => {
                (StatusCode::BAD_GATEWAY, "bad_gateway", message.to_string())
            }
            ApiError::GatewayTimeout(message) => (
                StatusCode::GATEWAY_TIMEOUT,
                "gateway_timeout",
                message.to_string(),
            ),
            ApiError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal server error".to_string(),
            ),
        };

        (status, Json(ErrorBody { error, message })).into_response()
    }
}
