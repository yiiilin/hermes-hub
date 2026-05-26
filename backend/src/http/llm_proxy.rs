use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, HeaderMap, Method},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::Value;
use std::time::Instant;

use crate::{
    http::ApiError,
    llm_proxy::{LlmProviderError, LlmProviderRequest},
    model_config::{
        ModelRegistryError, CHAT_COMPLETIONS_API_TYPE, IMAGES_GENERATIONS_API_TYPE,
        IMAGE_MODEL_CONFIG_KIND, LLM_MODEL_CONFIG_KIND, RESPONSES_API_TYPE,
    },
    session::store::LlmUsageEvent,
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/internal/llm/v1/models", get(models))
        .route("/internal/llm/v1/chat/completions", post(chat_completions))
        .route("/internal/llm/v1/responses", post(responses))
        .route(
            "/internal/llm/v1/images/generations",
            post(images_generations),
        )
}

async fn models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    verify_instance_token(&state, &headers).await?;
    let payload = state
        .model_registry
        .models_payload()
        .await
        .map_err(map_model_error)?;

    Ok(Json(payload))
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    proxy_model_request(
        state,
        headers,
        "/chat/completions",
        LLM_MODEL_CONFIG_KIND,
        CHAT_COMPLETIONS_API_TYPE,
        body,
    )
    .await
}

async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    proxy_model_request(
        state,
        headers,
        "/responses",
        LLM_MODEL_CONFIG_KIND,
        RESPONSES_API_TYPE,
        body,
    )
    .await
}

async fn images_generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    proxy_model_request(
        state,
        headers,
        "/images/generations",
        IMAGE_MODEL_CONFIG_KIND,
        IMAGES_GENERATIONS_API_TYPE,
        body,
    )
    .await
}

async fn proxy_model_request(
    state: AppState,
    headers: HeaderMap,
    path: &str,
    config_kind: &str,
    api_type: &str,
    body: Body,
) -> Result<Response, ApiError> {
    let token = bearer_token(&headers)?;
    let token_context = state
        .model_registry
        .instance_token_context(token)
        .await
        .ok_or(ApiError::Unauthorized)?;
    let body = to_bytes(body, state.config.max_proxy_body_bytes)
        .await
        .map_err(|_| ApiError::BadRequest("request body could not be read"))?;
    let value = serde_json::from_slice::<Value>(&body)
        .map_err(|_| ApiError::BadRequest("request body must be json"))?;
    let prepared = state
        .model_registry
        .prepare_request_body_for_kind(value, config_kind, api_type)
        .await
        .map_err(map_model_error)?;
    let config = prepared.config.clone();
    let request = LlmProviderRequest {
        method: Method::POST,
        provider_base_url: config.provider_base_url.clone(),
        path: path.to_string(),
        authorization: format!("Bearer {}", config.provider_api_key),
        content_type: "application/json".to_string(),
        body: prepared.body,
        timeout_seconds: config.request_timeout_seconds,
    };

    let started = Instant::now();
    let provider = state.llm_provider.send(request).await;

    match provider {
        Ok(response) => {
            let status = response.status().as_u16();
            let _ = state
                .store
                .record_llm_usage(LlmUsageEvent {
                    user_id: token_context.user_id,
                    hermes_instance_id: token_context.hermes_instance_id,
                    model: prepared.model,
                    upstream_provider: config.provider_name,
                    status_code: Some(status),
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                })
                .await;
            Ok(response)
        }
        Err(error) => {
            let mapped = map_provider_error(error);
            let _ = state
                .store
                .record_llm_usage(LlmUsageEvent {
                    user_id: token_context.user_id,
                    hermes_instance_id: token_context.hermes_instance_id,
                    model: prepared.model,
                    upstream_provider: config.provider_name,
                    status_code: None,
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                })
                .await;
            Err(mapped)
        }
    }
}

async fn verify_instance_token(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = bearer_token(headers)?;

    if !state.model_registry.verify_instance_token(token).await {
        return Err(ApiError::Unauthorized);
    }

    Ok(())
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)
}

fn map_model_error(error: ModelRegistryError) -> ApiError {
    match error {
        ModelRegistryError::ModelDisabled => ApiError::Conflict("model config is disabled"),
        ModelRegistryError::ModelNotAllowed => ApiError::Forbidden,
        ModelRegistryError::InvalidRequest => ApiError::BadRequest("invalid model request"),
        ModelRegistryError::StreamingDisabled => ApiError::Forbidden,
        ModelRegistryError::LockFailed
        | ModelRegistryError::DatabaseFailed
        | ModelRegistryError::SecretFailed => ApiError::Internal,
    }
}

fn map_provider_error(error: LlmProviderError) -> ApiError {
    match error {
        LlmProviderError::InvalidUrl => ApiError::BadGateway("provider url is invalid"),
        LlmProviderError::Timeout => ApiError::GatewayTimeout("provider request timed out"),
        LlmProviderError::LockFailed | LlmProviderError::Failed(_) => {
            ApiError::BadGateway("provider request failed")
        }
    }
}
