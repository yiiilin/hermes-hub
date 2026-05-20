use axum::{
    body::to_bytes,
    extract::{Multipart, Path, State},
    http::{HeaderMap, Method},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;

use crate::{
    channel::service::{
        Channel, ChannelAttachment, ChannelAttachmentDirection, ChannelMessage, ChannelMessageRole,
        ChannelSession, ChannelSessionKind, ChannelStoreError,
    },
    http::{attachments::upload_session_attachments, auth::current_user, ApiError},
    llm_proxy::LlmProviderRequest,
    model_config::TITLE_MODEL_CONFIG_KIND,
    session::store::LlmUsageEvent,
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/channels", get(list_channels))
        .route("/api/channels/{channel_id}", get(get_channel))
        .route(
            "/api/channels/{channel_id}/sessions",
            get(list_sessions).post(create_session),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}",
            get(get_session),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}/attachments",
            post(upload_attachments),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}/messages",
            get(list_session_messages).post(append_session_message),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}/title",
            post(generate_session_title),
        )
}

#[derive(Deserialize)]
struct CreateSessionRequest {
    kind: String,
}

#[derive(Deserialize)]
struct GenerateTitleRequest {
    prompt: String,
}

#[derive(Deserialize)]
struct AppendMessageRequest {
    role: String,
    content: String,
    attachments: Option<Value>,
}

#[derive(Serialize)]
struct ChannelListResponse {
    channels: Vec<Channel>,
}

#[derive(Serialize)]
struct SessionResponse {
    session: ChannelSession,
}

#[derive(Serialize)]
struct SessionListResponse {
    sessions: Vec<ChannelSession>,
}

#[derive(Serialize)]
struct MessageResponse {
    message: ChannelMessage,
}

#[derive(Serialize)]
struct MessageListResponse {
    messages: Vec<ChannelMessage>,
}

#[derive(Serialize)]
struct AttachmentListResponse {
    attachments: Vec<ChannelAttachment>,
}

async fn list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channels = state
        .channel_store
        .list_channels(&user.id)
        .await
        .map_err(map_channel_error)?;

    Ok(Json(ChannelListResponse { channels }))
}

async fn get_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channel = state
        .channel_store
        .get_channel(&user.id, &channel_id)
        .await
        .map_err(map_channel_error)?;

    Ok(Json(serde_json::json!({ "channel": channel })))
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let kind = ChannelSessionKind::parse(&payload.kind).map_err(map_channel_error)?;
    let session = state
        .channel_store
        .create_session(&user.id, &channel_id, kind, None)
        .await
        .map_err(map_channel_error)?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(SessionResponse { session }),
    ))
}

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let sessions = state
        .channel_store
        .list_sessions(&user.id, &channel_id)
        .await
        .map_err(map_channel_error)?;

    Ok(Json(SessionListResponse { sessions }))
}

async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let session = state
        .channel_store
        .get_session(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?;

    Ok(Json(SessionResponse { session }))
}

async fn list_session_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let messages = state
        .channel_store
        .list_session_messages(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?;

    Ok(Json(MessageListResponse { messages }))
}

async fn append_session_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
    Json(payload): Json<AppendMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let role = ChannelMessageRole::parse(&payload.role).map_err(map_channel_error)?;
    let message = state
        .channel_store
        .append_session_message(
            &user.id,
            &channel_id,
            &session_id,
            role,
            payload.content,
            payload.attachments.unwrap_or_else(|| json!([])),
        )
        .await
        .map_err(map_channel_error)?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(MessageResponse { message }),
    ))
}

async fn upload_attachments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
    multipart: Multipart,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let attachments = upload_session_attachments(
        &state,
        &user.id,
        &channel_id,
        &session_id,
        ChannelAttachmentDirection::Input,
        multipart,
    )
    .await?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(AttachmentListResponse { attachments }),
    ))
}

async fn generate_session_title(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
    Json(payload): Json<GenerateTitleRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    state
        .channel_store
        .get_session(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?;

    let title = model_generated_title(&state, &user.id, &payload.prompt).await;
    let session = state
        .channel_store
        .update_session_title(&user.id, &channel_id, &session_id, title)
        .await
        .map_err(map_channel_error)?;

    Ok(Json(SessionResponse { session }))
}

async fn model_generated_title(state: &AppState, user_id: &str, prompt: &str) -> String {
    let fallback = fallback_title(prompt);
    let Ok(config) = state
        .model_registry
        .config_for_kind(TITLE_MODEL_CONFIG_KIND)
        .await
    else {
        return fallback;
    };
    let (path, body) = title_generation_request(&config, prompt);
    let request = LlmProviderRequest {
        method: Method::POST,
        provider_base_url: config.provider_base_url.clone(),
        path,
        authorization: format!("Bearer {}", config.provider_api_key),
        content_type: "application/json".to_string(),
        body: serde_json::to_vec(&body).unwrap_or_default(),
        timeout_seconds: config.request_timeout_seconds,
    };
    let started = Instant::now();

    if let Ok(response) = state.llm_provider.send(request).await {
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 128 * 1024).await.ok();
        let title = bytes
            .as_deref()
            .and_then(parse_title_response)
            .unwrap_or(fallback);
        let _ = state
            .store
            .record_llm_usage(LlmUsageEvent {
                user_id: Some(user_id.to_string()),
                hermes_instance_id: None,
                model: config.default_model,
                upstream_provider: config.provider_name,
                status_code: Some(status),
                duration_ms: Some(started.elapsed().as_millis() as u64),
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
            })
            .await;
        return title;
    }

    fallback
}

fn parse_title_response(bytes: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    let title = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/output_text").and_then(Value::as_str))?;
    clean_title(title)
}

fn title_generation_request(
    config: &crate::model_config::ModelConfig,
    prompt: &str,
) -> (String, Value) {
    if config.api_type == crate::model_config::RESPONSES_API_TYPE {
        let mut body = json!({
            "model": config.default_model,
            "stream": false,
            "input": [
                {
                    "role": "system",
                    "content": "Generate a concise conversation title. Return only the title."
                },
                {
                    "role": "user",
                    "content": prompt
                }
            ]
        });
        if let Some(effort) = config.reasoning_effort.as_deref() {
            body["reasoning"] = json!({ "effort": effort });
        }
        return ("/responses".to_string(), body);
    }

    let mut body = json!({
        "model": config.default_model,
        "stream": false,
        "messages": [
            {
                "role": "system",
                "content": "Generate a concise conversation title. Return only the title."
            },
            {
                "role": "user",
                "content": prompt
            }
        ]
    });
    if let Some(effort) = config.reasoning_effort.as_deref() {
        body["reasoning_effort"] = json!(effort);
    }

    ("/chat/completions".to_string(), body)
}

fn fallback_title(prompt: &str) -> String {
    clean_title(prompt).unwrap_or_else(|| "New conversation".to_string())
}

fn clean_title(value: &str) -> Option<String> {
    let title = value
        .lines()
        .next()
        .unwrap_or(value)
        .trim()
        .trim_matches('"')
        .chars()
        .take(48)
        .collect::<String>();

    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

fn map_channel_error(error: ChannelStoreError) -> ApiError {
    match error {
        ChannelStoreError::ChannelNotFound => ApiError::NotFound("channel not found"),
        ChannelStoreError::InvalidSessionKind => ApiError::BadRequest("invalid session kind"),
        ChannelStoreError::InvalidMessageRole => ApiError::BadRequest("invalid message role"),
        ChannelStoreError::InvalidAttachment => ApiError::BadRequest("invalid attachment"),
        ChannelStoreError::AttachmentNotFound => ApiError::NotFound("attachment not found"),
        ChannelStoreError::LockFailed | ChannelStoreError::DatabaseFailed => ApiError::Internal,
    }
}
