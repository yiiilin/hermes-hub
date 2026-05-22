use axum::{
    body::{to_bytes, Body},
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::{
    channel::{
        events::SessionEvent,
        service::{
            ChannelAttachment, ChannelAttachmentDirection, ChannelMessage, ChannelMessageRole,
            ChannelRun, ChannelRunStatus,
        },
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
            "/internal/channel/v1/sessions/{session_id}/messages/{message_id}",
            axum::routing::put(update_message),
        )
        .route(
            "/internal/channel/v1/sessions/{session_id}/attachments",
            post(upload_output_attachments),
        )
        .route("/internal/channel/v1/inbox", get(poll_inbox))
        .route(
            "/internal/channel/v1/inbox/{run_id}/ack",
            post(ack_inbox_item),
        )
        .route(
            "/internal/channel/v1/runs/{run_id}/status",
            post(update_run_status),
        )
        .route(
            "/internal/channel/v1/attachments/{attachment_id}/download",
            get(download_input_attachment),
        )
}

#[derive(Deserialize)]
struct DeliverMessageRequest {
    role: String,
    content: String,
    attachments: Option<Value>,
    client_message_key: Option<String>,
    run_id: Option<String>,
}

#[derive(Deserialize)]
struct UpdateMessageRequest {
    content: String,
    attachments: Option<Value>,
    run_id: Option<String>,
}

#[derive(Deserialize)]
struct UpdateRunStatusRequest {
    status: String,
    error: Option<String>,
    output_message_id: Option<String>,
}

#[derive(Deserialize)]
struct AckRunRequest {
    output_message_id: Option<String>,
}

#[derive(Serialize)]
struct MessageResponse {
    message: ChannelMessage,
}

#[derive(Serialize)]
struct AttachmentListResponse {
    attachments: Vec<ChannelAttachment>,
}

#[derive(Serialize)]
struct InboxResponse {
    items: Vec<InboxItem>,
}

#[derive(Serialize)]
struct InboxItem {
    id: String,
    run_id: String,
    session_id: String,
    user_id: String,
    content: String,
    attachments: Value,
}

#[derive(Serialize)]
struct RunResponse {
    run: ChannelRun,
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
    if role != ChannelMessageRole::Assistant {
        // Hermes-Hub channel 的内部入口只接收 Hermes 输出；用户输入必须走带登录态的公开 API。
        return Err(ApiError::BadRequest(
            "channel message role must be assistant",
        ));
    }
    let client_message_key = payload.client_message_key;
    reject_late_terminal_output(
        &state,
        &token_context,
        payload.run_id.as_deref(),
        client_message_key.as_deref(),
    )
    .await?;
    let message = state
        .channel_store
        .append_session_message(
            &session_context.user_id,
            &session_context.channel_id,
            &session_id,
            role,
            client_message_key,
            payload.content,
            payload.attachments.unwrap_or_else(|| json!([])),
        )
        .await
        .map_err(map_channel_error)?;
    state.session_events.publish(SessionEvent::MessageCreated {
        message: message.clone(),
    });

    Ok((StatusCode::CREATED, Json(MessageResponse { message })))
}

async fn reject_late_terminal_output(
    state: &AppState,
    token_context: &InstanceTokenContext,
    run_id: Option<&str>,
    client_message_key: Option<&str>,
) -> Result<(), ApiError> {
    let Some(run_id) = run_id
        .and_then(normalize_protocol_run_id)
        .or_else(|| client_message_key.and_then(hermes_run_id_from_client_message_key))
    else {
        return Ok(());
    };

    match state
        .channel_store
        .get_run_for_instance(token_context.hermes_instance_id.as_deref(), run_id)
        .await
    {
        Ok(run) if run.status.is_terminal() => {
            // Stop/delete 后 Hermes 容器可能还会把旧任务的最终输出发回来；
            // 这些输出不能重新污染已经终止的 Hub 会话。
            Err(ApiError::Conflict(
                "channel run is no longer accepting output",
            ))
        }
        Ok(_) | Err(crate::channel::service::ChannelStoreError::RunNotFound) => Ok(()),
        Err(error) => Err(map_channel_error(error)),
    }
}

fn hermes_run_id_from_client_message_key(value: &str) -> Option<&str> {
    value.strip_prefix("hermes-run:").and_then(|rest| {
        let run_id = rest.split(':').next().unwrap_or(rest);
        run_id.starts_with("hub-run-").then_some(run_id)
    })
}

fn normalize_protocol_run_id(value: &str) -> Option<&str> {
    let run_id = value.strip_prefix("hermes-run:").unwrap_or(value);
    let run_id = run_id.split(':').next().unwrap_or(run_id);
    run_id.starts_with("hub-run-").then_some(run_id)
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

async fn update_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((session_id, message_id)): Path<(String, String)>,
    Json(payload): Json<UpdateMessageRequest>,
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
    reject_late_terminal_output(&state, &token_context, payload.run_id.as_deref(), None).await?;
    let message = state
        .channel_store
        .update_session_message(
            &session_context.user_id,
            &session_context.channel_id,
            &session_id,
            &message_id,
            payload.content,
            payload.attachments.unwrap_or_else(|| json!([])),
        )
        .await
        .map_err(map_channel_error)?;
    state.session_events.publish(SessionEvent::MessageUpdated {
        message: message.clone(),
    });

    Ok(Json(MessageResponse { message }))
}

async fn poll_inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, ApiError> {
    let token_context = verify_instance_token(&state, &headers).await?;
    let limit = query
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4)
        .clamp(1, 32);
    let timeout = query
        .get("timeout_seconds")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(30);
    let runs = poll_runs_for_instance(
        &state,
        token_context.hermes_instance_id.as_deref(),
        limit,
        Duration::from_secs(timeout),
    )
    .await?;
    let mut items = Vec::with_capacity(runs.len());
    for run in runs {
        items.push(inbox_item_for_run(&state, run).await?);
    }

    Ok(Json(InboxResponse { items }))
}

async fn poll_runs_for_instance(
    state: &AppState,
    instance_id: Option<&str>,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<ChannelRun>, ApiError> {
    let started = Instant::now();
    loop {
        let runs = state
            .channel_store
            .lease_runs_for_instance(instance_id, limit)
            .await
            .map_err(map_channel_error)?;
        if !runs.is_empty() || timeout.is_zero() {
            return Ok(runs);
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return Ok(runs);
        }

        // Hermes 容器会长期挂在 inbox 上；空队列时短暂等待，避免无任务时打满 Hub。
        let remaining = timeout.saturating_sub(elapsed);
        tokio::time::sleep(remaining.min(Duration::from_millis(250))).await;
    }
}

async fn ack_inbox_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    body: Body,
) -> Result<impl IntoResponse, ApiError> {
    let token_context = verify_instance_token(&state, &headers).await?;
    let payload = optional_ack_payload(body).await?;
    let run = state
        .channel_store
        .ack_run_for_instance(
            token_context.hermes_instance_id.as_deref(),
            &run_id,
            payload.output_message_id.as_deref(),
        )
        .await
        .map_err(map_channel_error)?;
    state
        .session_events
        .publish(SessionEvent::RunUpdated { run: run.clone() });

    Ok(Json(RunResponse { run }))
}

async fn optional_ack_payload(body: Body) -> Result<AckRunRequest, ApiError> {
    let bytes = to_bytes(body, 1024)
        .await
        .map_err(|_| ApiError::BadRequest("invalid ack body"))?;
    if bytes.is_empty() {
        return Ok(AckRunRequest {
            output_message_id: None,
        });
    }
    serde_json::from_slice(&bytes).map_err(|_| ApiError::BadRequest("invalid ack body"))
}

async fn update_run_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Json(payload): Json<UpdateRunStatusRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let token_context = verify_instance_token(&state, &headers).await?;
    let session_id = run_session_id_for_token(&state, &token_context, &run_id).await?;
    let status = ChannelRunStatus::parse(&payload.status).map_err(map_channel_error)?;
    let run = state
        .channel_store
        .update_run_status_for_session(
            &session_id,
            &run_id,
            status,
            payload.error,
            payload.output_message_id.as_deref(),
        )
        .await
        .map_err(map_channel_error)?;
    state
        .session_events
        .publish(SessionEvent::RunUpdated { run: run.clone() });

    Ok(Json(RunResponse { run }))
}

async fn download_input_attachment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(attachment_id): Path<String>,
) -> Result<Response, ApiError> {
    let token_context = verify_instance_token(&state, &headers).await?;
    let attachment = state
        .channel_store
        .get_attachment_for_internal(attachment_id.as_str())
        .await
        .map_err(map_channel_error)?;
    if attachment.direction != ChannelAttachmentDirection::Input {
        return Err(ApiError::Forbidden);
    }
    let session_context = state
        .channel_store
        .session_context(&attachment.session_id)
        .await
        .map_err(map_channel_error)?;
    ensure_token_can_access_session(
        &token_context,
        &session_context.user_id,
        &session_context.hermes_instance_id,
    )?;
    let bytes = state
        .object_storage
        .get(&attachment.object_key)
        .await
        .map_err(|_| ApiError::BadGateway("object storage request failed"))?;
    let mut response = (StatusCode::OK, Body::from(bytes)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&attachment.content_type).map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
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

async fn run_session_id_for_token(
    state: &AppState,
    token_context: &InstanceTokenContext,
    run_id: &str,
) -> Result<String, ApiError> {
    let run = state
        .channel_store
        .get_run_for_instance(token_context.hermes_instance_id.as_deref(), run_id)
        .await
        .map_err(map_channel_error)?;
    Ok(run.session_id)
}

async fn inbox_item_for_run(state: &AppState, run: ChannelRun) -> Result<InboxItem, ApiError> {
    let attachments = with_internal_download_urls(run.input_attachments.clone());
    let session_context = state
        .channel_store
        .session_context(&run.session_id)
        .await
        .map_err(map_channel_error)?;
    Ok(InboxItem {
        id: run.run_id.clone(),
        run_id: run.run_id,
        session_id: run.session_id,
        user_id: session_context.user_id,
        content: run.input,
        attachments,
    })
}

fn with_internal_download_urls(mut attachments: Value) -> Value {
    let Some(items) = attachments.as_array_mut() else {
        return attachments;
    };
    for item in items {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        let Some(id) = object
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        object.insert(
            "download_url".to_string(),
            Value::String(format!("/internal/channel/v1/attachments/{id}/download")),
        );
    }
    attachments
}
