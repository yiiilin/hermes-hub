pub mod auth;
pub mod hermes_proxy;
pub mod invites;
pub mod llm_proxy;

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
        .merge(invites::router())
        .merge(llm_proxy::router())
        .merge(crate::channel::routes::router())
        .route("/api/hermes/{*path}", any(hermes_proxy::proxy))
}

#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    Unauthorized,
    Forbidden,
    Conflict(&'static str),
    Gone(&'static str),
    NotFound(&'static str),
    Internal,
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
    message: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, message) = match self {
            ApiError::BadRequest(message) => (StatusCode::BAD_REQUEST, "bad_request", message),
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "authentication required",
            ),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "admin access required"),
            ApiError::Conflict(message) => (StatusCode::CONFLICT, "conflict", message),
            ApiError::Gone(message) => (StatusCode::GONE, "gone", message),
            ApiError::NotFound(message) => (StatusCode::NOT_FOUND, "not_found", message),
            ApiError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal server error",
            ),
        };

        (status, Json(ErrorBody { error, message })).into_response()
    }
}
