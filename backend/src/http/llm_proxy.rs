use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::Value;

use crate::{
    http::ApiError, llm_proxy::LlmProviderRequest, model_config::ModelRegistryError, AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/internal/llm/v1/models", get(models))
        .route("/internal/llm/v1/chat/completions", post(chat_completions))
        .route("/internal/llm/v1/responses", post(responses))
}

async fn models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    verify_instance_token(&state, &headers)?;
    let payload = state
        .model_registry
        .models_payload()
        .map_err(map_model_error)?;

    Ok(Json(payload))
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    proxy_model_request(state, headers, "/chat/completions", body).await
}

async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    proxy_model_request(state, headers, "/responses", body).await
}

async fn proxy_model_request(
    state: AppState,
    headers: HeaderMap,
    path: &str,
    body: Body,
) -> Result<Response, ApiError> {
    verify_instance_token(&state, &headers)?;
    let body = to_bytes(body, usize::MAX)
        .await
        .map_err(|_| ApiError::BadRequest("request body could not be read"))?;
    let value = serde_json::from_slice::<Value>(&body)
        .map_err(|_| ApiError::BadRequest("request body must be json"))?;
    let (config, body) = state
        .model_registry
        .prepare_request_body(value)
        .map_err(map_model_error)?;
    let request = LlmProviderRequest {
        method: Method::POST,
        provider_base_url: config.provider_base_url,
        path: path.to_string(),
        authorization: format!("Bearer {}", config.provider_api_key),
        content_type: "application/json".to_string(),
        body,
    };
    let provider = state
        .llm_provider
        .send(request)
        .await
        .map_err(|_| ApiError::Internal)?;
    let mut response = (provider.status, provider.body).into_response();

    if let Some(content_type) = provider.content_type {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&content_type).map_err(|_| ApiError::Internal)?,
        );
    }

    Ok(response)
}

fn verify_instance_token(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;

    if !state.model_registry.verify_instance_token(token) {
        return Err(ApiError::Unauthorized);
    }

    Ok(())
}

fn map_model_error(error: ModelRegistryError) -> ApiError {
    match error {
        ModelRegistryError::ModelNotAllowed => ApiError::Forbidden,
        ModelRegistryError::InvalidRequest => ApiError::BadRequest("invalid model request"),
        ModelRegistryError::LockFailed => ApiError::Internal,
    }
}
