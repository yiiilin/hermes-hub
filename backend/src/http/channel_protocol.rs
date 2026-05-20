use axum::{
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    channel::service::{
        ChannelAttachment, ChannelAttachmentDirection, ChannelMessage, ChannelMessageRole,
    },
    http::{
        attachments::{map_channel_error, upload_session_attachments_for_context},
        ApiError,
    },
    model_config::InstanceTokenContext,
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/internal/channel/v1/sessions/{session_id}/messages",
            post(deliver_message),
        )
        .route(
            "/internal/channel/v1/sessions/{session_id}/attachments",
            post(upload_output_attachments),
        )
}

#[derive(Deserialize)]
struct DeliverMessageRequest {
    role: String,
    content: String,
    attachments: Option<Value>,
}

#[derive(Serialize)]
struct MessageResponse {
    message: ChannelMessage,
}

#[derive(Serialize)]
struct AttachmentListResponse {
    attachments: Vec<ChannelAttachment>,
}

async fn deliver_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(payload): Json<DeliverMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let token_context = verify_instance_token(&state, &headers).await?;
    let session_context = state
        .channel_store
        .session_context(&session_id)
        .await
        .map_err(map_channel_error)?;
    ensure_token_can_access_session(
        &token_context,
        &session_context.user_id,
        &session_context.hermes_instance_id,
    )?;
    let role = ChannelMessageRole::parse(&payload.role).map_err(map_channel_error)?;
    let message = state
        .channel_store
        .append_session_message(
            &session_context.user_id,
            &session_context.channel_id,
            &session_id,
            role,
            payload.content,
            payload.attachments.unwrap_or_else(|| json!([])),
        )
        .await
        .map_err(map_channel_error)?;

    Ok((StatusCode::CREATED, Json(MessageResponse { message })))
}

async fn upload_output_attachments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    multipart: Multipart,
) -> Result<impl IntoResponse, ApiError> {
    let token_context = verify_instance_token(&state, &headers).await?;
    let session_context = state
        .channel_store
        .session_context(&session_id)
        .await
        .map_err(map_channel_error)?;
    ensure_token_can_access_session(
        &token_context,
        &session_context.user_id,
        &session_context.hermes_instance_id,
    )?;
    let attachments = upload_session_attachments_for_context(
        &state,
        &session_context.user_id,
        &session_context.channel_id,
        &session_id,
        ChannelAttachmentDirection::Output,
        multipart,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(AttachmentListResponse { attachments }),
    ))
}

async fn verify_instance_token(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<InstanceTokenContext, ApiError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;

    state
        .model_registry
        .instance_token_context(token)
        .await
        .ok_or(ApiError::Unauthorized)
}

fn ensure_token_can_access_session(
    token_context: &InstanceTokenContext,
    session_user_id: &str,
    session_instance_id: &Option<String>,
) -> Result<(), ApiError> {
    if let Some(token_user_id) = token_context.user_id.as_deref() {
        if token_user_id != session_user_id {
            return Err(ApiError::Forbidden);
        }
    }

    if let (Some(token_instance_id), Some(session_instance_id)) = (
        token_context.hermes_instance_id.as_deref(),
        session_instance_id.as_deref(),
    ) {
        if token_instance_id != session_instance_id {
            return Err(ApiError::Forbidden);
        }
    }

    Ok(())
}
