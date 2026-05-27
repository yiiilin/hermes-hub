pub mod admin;
pub mod attachments;
pub mod auth;
pub mod channel_protocol;
pub mod invites;
pub mod llm_proxy;
pub mod oidc;
pub mod sessions;
pub mod workspace;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json, Router,
};
use serde::Serialize;

use crate::{hermes::provisioner::ProvisionerError, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(auth::router())
        .merge(oidc::router())
        .merge(admin::router())
        .merge(invites::router())
        .merge(attachments::router())
        .merge(channel_protocol::router())
        .merge(sessions::router())
        .merge(llm_proxy::router())
        .merge(workspace::router())
        .merge(crate::channel::routes::router())
}

pub(crate) fn map_provisioner_error(error: ProvisionerError) -> ApiError {
    match error {
        ProvisionerError::InstanceNotFound => ApiError::NotFound("hermes container not found"),
        ProvisionerError::InvalidManagedInstance => {
            ApiError::Conflict("hermes instance is not managed by docker")
        }
        ProvisionerError::DockerRuntime(message) | ProvisionerError::DockerCommand(message) => {
            // Docker daemon/CLI 错误通常是管理员可处理的环境问题，要把摘要返回给页面。
            ApiError::BadGatewayMessage(format!("hermes docker operation failed: {message}"))
        }
        ProvisionerError::LockFailed
        | ProvisionerError::Filesystem(_)
        | ProvisionerError::ObjectStorage(_) => ApiError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::{map_provisioner_error, ApiError};
    use crate::hermes::provisioner::ProvisionerError;
    use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};
    use serde_json::Value;

    #[tokio::test]
    async fn dynamic_bad_gateway_error_exposes_message() {
        let response =
            ApiError::BadGatewayMessage("docker pull failed".to_string()).into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("error response body can be read");
        let payload: Value = serde_json::from_slice(&body).expect("error response is json");
        assert_eq!(payload["error"], "bad_gateway");
        assert_eq!(payload["message"], "docker pull failed");
    }

    #[tokio::test]
    async fn docker_provisioner_errors_become_visible_bad_gateway_errors() {
        let response = map_provisioner_error(ProvisionerError::DockerCommand(
            "docker pull failed: EOF".to_string(),
        ))
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("error response body can be read");
        let payload: Value = serde_json::from_slice(&body).expect("error response is json");
        assert_eq!(
            payload["message"],
            "hermes docker operation failed: docker pull failed: EOF"
        );
    }
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
    BadGatewayMessage(String),
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
            ApiError::BadGatewayMessage(message) => {
                (StatusCode::BAD_GATEWAY, "bad_gateway", message, None)
            }
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
