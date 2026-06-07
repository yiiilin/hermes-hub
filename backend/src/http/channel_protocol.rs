use axum::{
    body::{to_bytes, Body},
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
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
        events::{BusinessToolRequestStatus, SessionEvent},
        service::{
            AppendSessionMessageOutcome, ChannelAttachment, ChannelAttachmentDirection,
            ChannelMessage, ChannelMessageRole, ChannelRun, ChannelRunStatus,
        },
    },
    http::integrations::{
        business_tool_request_client_key, business_tool_request_event,
        is_business_tool_request_message, is_reserved_business_tool_client_key,
        validate_business_tool_request_message, ValidatedBusinessToolRequest,
    },
    http::{
        attachments::{
            create_session_attachment_from_file, drain_multipart_field_with_limit,
            effective_attachment_upload_limit, ensure_attachment_not_expired, map_channel_error,
            read_multipart_text_field_with_limit, spool_multipart_file_to_temp_with_limit,
            upload_session_attachments_from_instance,
        },
        sessions::refresh_public_session_retention_from_message,
        ApiError,
    },
    model_config::InstanceTokenContext,
    session::store::{HermesScheduledTaskSnapshot, HermesSchedulerSnapshotInput, StoreError},
    AppState,
};

const MEDIA_OUTPUT_CONTENT_MAX_BYTES: usize = 2 * 1024 * 1024;
const MEDIA_OUTPUT_ID_FIELD_MAX_BYTES: usize = 4096;
const MEDIA_OUTPUT_UNKNOWN_FIELD_MAX_BYTES: usize = 64 * 1024;

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
            post(upload_output_attachments).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/internal/channel/v1/sessions/{session_id}/outputs/media",
            post(deliver_media_output).layer(DefaultBodyLimit::disable()),
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
        .route(
            "/internal/channel/v1/instance/status",
            post(report_instance_status),
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

#[derive(Deserialize)]
struct InstanceStatusReportRequest {
    runtime_image: Option<String>,
    runtime_version: Option<String>,
    scheduler_snapshot: Option<SchedulerSnapshotReportRequest>,
}

#[derive(Deserialize, Default)]
struct SchedulerSnapshotReportRequest {
    status: Option<String>,
    scheduler_enabled: Option<bool>,
    running_jobs_count: Option<u32>,
    generated_at: Option<Value>,
    reported_at: Option<Value>,
    source: Option<String>,
    snapshot_hash: Option<String>,
    next_wake_at: Option<Value>,
    jobs: Option<Vec<SchedulerJobReportRequest>>,
    tasks: Option<Vec<SchedulerJobReportRequest>>,
}

#[derive(Deserialize, Default)]
struct SchedulerJobReportRequest {
    id: Option<Value>,
    name: Option<Value>,
    enabled: Option<bool>,
    schedule: Option<Value>,
    cron: Option<Value>,
    timezone: Option<Value>,
    next_run_at: Option<Value>,
    last_run_at: Option<Value>,
    status: Option<Value>,
    state: Option<Value>,
    last_status: Option<Value>,
    source: Option<Value>,
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
    #[serde(rename = "type")]
    item_type: String,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    attachments: Value,
}

#[derive(Serialize)]
struct RunResponse {
    run: ChannelRun,
}

#[derive(Serialize)]
struct HermesInstanceResponse {
    hermes_instance: crate::hermes::instance::HermesInstance,
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
    let client_message_key = payload.client_message_key.and_then(trimmed_non_empty);
    if client_message_key
        .as_deref()
        .is_some_and(is_reserved_business_tool_client_key)
    {
        // business-tool-* key 只允许业务工具请求路径生成，避免普通 adapter 输出抢占回调幂等位。
        return Err(ApiError::BadRequest("reserved client message key"));
    }
    let heartbeat_run_id = payload
        .run_id
        .as_deref()
        .and_then(normalize_protocol_run_id)
        .or_else(|| {
            client_message_key
                .as_deref()
                .and_then(hermes_run_id_from_client_message_key)
        })
        .map(str::to_string);
    reject_late_terminal_output(
        &state,
        &token_context,
        payload.run_id.as_deref(),
        client_message_key.as_deref(),
    )
    .await?;
    let business_tool_request = validate_internal_business_tool_request_message(
        &state,
        &session_context.user_id,
        &session_context.channel_id,
        &payload.content,
    )
    .await?;
    let message_content = business_tool_request
        .as_ref()
        .map(|(_, request)| request.normalized_content.clone())
        .unwrap_or(payload.content);
    let client_message_key = business_tool_request
        .as_ref()
        .map(|(_, request)| business_tool_request_client_key(&request.request_id))
        .or(client_message_key);

    let append_outcome = state
        .channel_store
        .append_session_message_with_outcome(
            &session_context.user_id,
            &session_context.channel_id,
            &session_id,
            role,
            client_message_key,
            message_content,
            payload.attachments.unwrap_or_else(|| json!([])),
        )
        .await
        .map_err(map_channel_error)?;
    let (message, created) = match append_outcome {
        AppendSessionMessageOutcome::Created(message) => (message, true),
        AppendSessionMessageOutcome::Existing(message) => (message, false),
    };
    if let Some(run_id) = heartbeat_run_id.as_deref() {
        // Hermes 的执行步骤、状态文本和最终输出都代表任务仍然活跃；
        // 刷新 run 心跳，避免长任务超过恢复窗口后被重复派发。
        let _ = state
            .channel_store
            .heartbeat_run_for_session(&session_id, run_id)
            .await
            .map_err(map_channel_error)?;
    }
    if let Some((integration_id, request)) = business_tool_request {
        if created {
            state
                .session_events
                .publish(SessionEvent::BusinessToolRequest {
                    request: business_tool_request_event(
                        &message,
                        &integration_id,
                        &request,
                        BusinessToolRequestStatus::Pending,
                        None,
                    ),
                });
        }
    } else {
        state.session_events.publish(SessionEvent::MessageCreated {
            message: message.clone(),
        });
    }
    if let Some(updated_session) = refresh_public_session_retention_from_message(
        &state,
        &session_context.user_id,
        &session_context.channel_id,
        &session_id,
        message.updated_at,
    )
    .await?
    {
        state.session_events.publish(SessionEvent::SessionUpdated {
            session: updated_session,
        });
    }

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

async fn validate_internal_business_tool_request_message(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    content: &str,
) -> Result<Option<(String, ValidatedBusinessToolRequest)>, ApiError> {
    if !is_business_tool_request_message(content) {
        return Ok(None);
    }
    let channel = state
        .channel_store
        .get_channel(user_id, channel_id)
        .await
        .map_err(map_channel_error)?;
    let integration_id = channel
        .name
        .strip_prefix("integration:")
        .map(str::to_string)
        .ok_or(ApiError::BadRequest(
            "business tool request requires integration session",
        ))?;
    let request = validate_business_tool_request_message(state, &integration_id, content)
        .await?
        .ok_or(ApiError::BadRequest("invalid business tool request"))?;

    Ok(Some((integration_id, request)))
}

fn hermes_run_id_from_client_message_key(value: &str) -> Option<&str> {
    let value = value.trim();
    value.strip_prefix("hermes-run:").and_then(|rest| {
        let run_id = rest.split(':').next().unwrap_or(rest);
        run_id.starts_with("hub-run-").then_some(run_id)
    })
}

fn normalize_protocol_run_id(value: &str) -> Option<&str> {
    let value = value.trim();
    let run_id = value.strip_prefix("hermes-run:").unwrap_or(value);
    let run_id = run_id.split(':').next().unwrap_or(run_id);
    run_id.starts_with("hub-run-").then_some(run_id)
}

fn trimmed_non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
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
    let attachments = upload_session_attachments_from_instance(
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

#[derive(Default)]
struct MediaOutputForm {
    content: String,
    client_message_key: Option<String>,
    run_id: Option<String>,
    files: Vec<crate::http::attachments::SpooledMultipartFile>,
}

async fn deliver_media_output(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    mut multipart: Multipart,
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
    let max_upload_bytes = effective_attachment_upload_limit(&state).await?;

    let mut form = MediaOutputForm::default();
    let parse_result = async {
        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|_| ApiError::BadRequest("multipart body is invalid"))?
        {
            let name = field.name().unwrap_or("").to_string();
            match name.as_str() {
                "file" => {
                    reject_late_terminal_output(
                        &state,
                        &token_context,
                        form.run_id.as_deref(),
                        form.client_message_key.as_deref(),
                    )
                    .await?;
                    // Hermes 输出附件和用户上传附件使用同一个系统参数上限，避免实例 token 绕过大小策略。
                    form.files.push(
                        spool_multipart_file_to_temp_with_limit(field, Some(max_upload_bytes))
                            .await?,
                    );
                }
                "content" | "caption" => {
                    form.content = read_multipart_text_field_with_limit(
                        field,
                        MEDIA_OUTPUT_CONTENT_MAX_BYTES,
                        "media output content is too large",
                    )
                    .await?;
                    if is_business_tool_request_message(&form.content) {
                        return Err(ApiError::BadRequest(
                            "business tool requests must use the append endpoint",
                        ));
                    }
                }
                "client_message_key" => {
                    let value = read_multipart_text_field_with_limit(
                        field,
                        MEDIA_OUTPUT_ID_FIELD_MAX_BYTES,
                        "media output metadata field is too large",
                    )
                    .await?;
                    if let Some(value) = trimmed_non_empty(value) {
                        if is_reserved_business_tool_client_key(&value) {
                            return Err(ApiError::BadRequest("reserved client message key"));
                        }
                        form.client_message_key = Some(value);
                        if let Some(existing) = state
                            .channel_store
                            .find_session_message_by_client_key(
                                &session_id,
                                form.client_message_key.as_deref().unwrap_or(""),
                            )
                            .await
                            .map_err(map_channel_error)?
                        {
                            return Ok(Some(existing));
                        }
                        reject_late_terminal_output(
                            &state,
                            &token_context,
                            form.run_id.as_deref(),
                            form.client_message_key.as_deref(),
                        )
                        .await?;
                    }
                }
                "run_id" => {
                    let value = read_multipart_text_field_with_limit(
                        field,
                        MEDIA_OUTPUT_ID_FIELD_MAX_BYTES,
                        "media output metadata field is too large",
                    )
                    .await?;
                    form.run_id = trimmed_non_empty(value);
                    if form.client_message_key.is_some() {
                        reject_late_terminal_output(
                            &state,
                            &token_context,
                            form.run_id.as_deref(),
                            form.client_message_key.as_deref(),
                        )
                        .await?;
                    }
                }
                _ => {
                    reject_late_terminal_output(
                        &state,
                        &token_context,
                        form.run_id.as_deref(),
                        form.client_message_key.as_deref(),
                    )
                    .await?;
                    // 允许未来 adapter 增加轻量字段；未知字段直接消费丢弃，避免 multipart 卡住。
                    drain_multipart_field_with_limit(
                        field,
                        MEDIA_OUTPUT_UNKNOWN_FIELD_MAX_BYTES,
                        "multipart field is too large",
                    )
                    .await?;
                }
            }
        }
        Ok::<Option<ChannelMessage>, ApiError>(None)
    }
    .await;
    match parse_result {
        Ok(Some(existing)) => {
            for file in &mut form.files {
                file.cleanup().await;
            }
            return Ok((StatusCode::OK, Json(MessageResponse { message: existing })));
        }
        Ok(None) => {}
        Err(error) => {
            for file in &mut form.files {
                file.cleanup().await;
            }
            return Err(error);
        }
    }

    if form.files.is_empty() {
        return Err(ApiError::BadRequest("media file is required"));
    }
    if let Some(client_message_key) = form.client_message_key.as_deref() {
        if is_reserved_business_tool_client_key(client_message_key) {
            for file in &mut form.files {
                file.cleanup().await;
            }
            return Err(ApiError::BadRequest("reserved client message key"));
        }
        if let Some(existing) = state
            .channel_store
            .find_session_message_by_client_key(&session_id, client_message_key)
            .await
            .map_err(map_channel_error)?
        {
            for file in &mut form.files {
                file.cleanup().await;
            }
            return Ok((StatusCode::OK, Json(MessageResponse { message: existing })));
        }
    }
    if let Err(error) = validate_attachment_placeholders(&form.content, form.files.len()) {
        // 占位符校验在对象入库前执行；失败时必须主动清理 multipart 临时文件。
        for file in &mut form.files {
            file.cleanup().await;
        }
        return Err(error);
    }
    if let Err(error) = reject_late_terminal_output(
        &state,
        &token_context,
        form.run_id.as_deref(),
        form.client_message_key.as_deref(),
    )
    .await
    {
        for file in &mut form.files {
            file.cleanup().await;
        }
        return Err(error);
    }
    let mut created_attachments = Vec::with_capacity(form.files.len());
    for file in &mut form.files {
        let attachment = create_session_attachment_from_file(
            &state,
            &session_context.user_id,
            &session_context.channel_id,
            &session_id,
            ChannelAttachmentDirection::Output,
            file.file_name.clone(),
            file.content_type.clone(),
            file.size,
            file.path(),
        )
        .await;
        file.cleanup().await;
        match attachment {
            Ok(attachment) => created_attachments.push(attachment),
            Err(error) => {
                cleanup_created_output_attachments(&state, &session_id, &created_attachments).await;
                return Err(error);
            }
        }
    }
    if let Err(error) = reject_late_terminal_output(
        &state,
        &token_context,
        form.run_id.as_deref(),
        form.client_message_key.as_deref(),
    )
    .await
    {
        // 文件上传期间用户可能停止/取消 run；append 前再查一次，避免终态后补写输出。
        cleanup_created_output_attachments(&state, &session_id, &created_attachments).await;
        return Err(error);
    }
    let attachments = json!(created_attachments.clone());
    let heartbeat_run_id = form
        .run_id
        .as_deref()
        .and_then(normalize_protocol_run_id)
        .or_else(|| {
            form.client_message_key
                .as_deref()
                .and_then(hermes_run_id_from_client_message_key)
        })
        .map(str::to_string);
    let (status, message, created) = match state
        .channel_store
        .append_session_message(
            &session_context.user_id,
            &session_context.channel_id,
            &session_id,
            ChannelMessageRole::Assistant,
            form.client_message_key.clone(),
            form.content,
            attachments,
        )
        .await
    {
        Ok(message) => {
            if message_has_attachment_ids(&message, &created_attachments) {
                (StatusCode::CREATED, message, true)
            } else {
                // 并发重复 client_message_key 时，store 会返回已有消息；这次刚上传的附件
                // 没有绑定到消息，必须立即删除，避免对象存储和附件表留下孤儿数据。
                cleanup_created_output_attachments(&state, &session_id, &created_attachments).await;
                (StatusCode::OK, message, false)
            }
        }
        Err(error) => {
            cleanup_created_output_attachments(&state, &session_id, &created_attachments).await;
            return Err(map_channel_error(error));
        }
    };
    if let Some(run_id) = heartbeat_run_id.as_deref() {
        if let Err(error) = state
            .channel_store
            .heartbeat_run_for_session(&session_id, run_id)
            .await
        {
            tracing::warn!(
                session_id = %session_id,
                run_id = %run_id,
                error = %error,
                "media output heartbeat failed after message creation"
            );
        }
    }
    if created {
        state.session_events.publish(SessionEvent::MessageCreated {
            message: message.clone(),
        });
    }
    if let Some(updated_session) = refresh_public_session_retention_from_message(
        &state,
        &session_context.user_id,
        &session_context.channel_id,
        &session_id,
        message.updated_at,
    )
    .await?
    {
        state.session_events.publish(SessionEvent::SessionUpdated {
            session: updated_session,
        });
    }

    Ok((status, Json(MessageResponse { message })))
}

async fn cleanup_created_output_attachments(
    state: &AppState,
    session_id: &str,
    attachments: &[ChannelAttachment],
) {
    for attachment in attachments {
        let _ = state
            .channel_store
            .delete_attachment_for_session(session_id, &attachment.id)
            .await;
        let _ = state.object_storage.delete(&attachment.object_key).await;
    }
}

fn message_has_attachment_ids(message: &ChannelMessage, attachments: &[ChannelAttachment]) -> bool {
    let expected: std::collections::HashSet<&str> = attachments
        .iter()
        .map(|attachment| attachment.id.as_str())
        .collect();
    message.attachments.as_array().is_some_and(|items| {
        let actual: std::collections::HashSet<&str> = items
            .iter()
            .filter_map(|attachment| attachment.get("id"))
            .filter_map(Value::as_str)
            .collect();
        expected.is_subset(&actual)
    })
}

fn validate_attachment_placeholders(
    content: &str,
    attachment_count: usize,
) -> Result<(), ApiError> {
    let indexes = attachment_placeholder_indexes(content)?;
    if attachment_count == 0 {
        if indexes.is_empty() {
            return Ok(());
        }
        return Err(ApiError::BadRequest(
            "attachment placeholders require output attachments",
        ));
    }
    if indexes.len() != attachment_count {
        return Err(ApiError::BadRequest(
            "each output attachment must be referenced exactly once",
        ));
    }

    let mut seen = vec![false; attachment_count];
    for index in indexes {
        if index >= attachment_count {
            return Err(ApiError::BadRequest(
                "attachment placeholder index is out of range",
            ));
        }
        if seen[index] {
            return Err(ApiError::BadRequest(
                "attachment placeholder index is duplicated",
            ));
        }
        seen[index] = true;
    }
    if seen.iter().all(|value| *value) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "each output attachment must be referenced exactly once",
        ))
    }
}

fn attachment_placeholder_indexes(content: &str) -> Result<Vec<usize>, ApiError> {
    const MARKER: &str = "{{attachment:";
    let mut indexes = Vec::new();
    let mut offset = 0;
    while let Some(position) = content[offset..].find(MARKER) {
        let placeholder_start = offset + position;
        let index_start = offset + position + MARKER.len();
        let rest = &content[index_start..];
        let Some(end_position) = rest.find("}}") else {
            return Err(ApiError::BadRequest("attachment placeholder is malformed"));
        };
        let placeholder_end = index_start + end_position + 2;
        if !attachment_placeholder_is_own_line(content, placeholder_start, placeholder_end) {
            return Err(ApiError::BadRequest(
                "attachment placeholder must be on its own line",
            ));
        }
        let raw_index = &rest[..end_position];
        if raw_index.is_empty() || !raw_index.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(ApiError::BadRequest("attachment placeholder is malformed"));
        }
        let index = raw_index
            .parse::<usize>()
            .map_err(|_| ApiError::BadRequest("attachment placeholder index is invalid"))?;
        indexes.push(index);
        offset = index_start + end_position + 2;
    }
    Ok(indexes)
}

fn attachment_placeholder_is_own_line(content: &str, start: usize, end: usize) -> bool {
    let line_start = content[..start]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let line_end = content[end..]
        .find('\n')
        .map(|index| end + index)
        .unwrap_or(content.len());

    content[line_start..start].trim().is_empty() && content[end..line_end].trim().is_empty()
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
    let existing_message = state
        .channel_store
        .list_session_messages(
            &session_context.user_id,
            &session_context.channel_id,
            &session_id,
        )
        .await
        .map_err(map_channel_error)?
        .into_iter()
        .find(|message| message.id == message_id)
        .ok_or(ApiError::NotFound("message not found"))?;
    if is_business_tool_request_message(&existing_message.content) {
        return Err(ApiError::BadRequest(
            "business tool request messages are immutable",
        ));
    }
    let attachments = payload.attachments.unwrap_or_else(|| json!([]));
    if is_business_tool_request_message(&payload.content) {
        return Err(ApiError::BadRequest(
            "business tool requests must use the append endpoint",
        ));
    }
    let (status, message, created, emit_message_event) =
        if is_execution_protocol_message(&payload.content) {
            upsert_latest_execution_message(
                &state,
                &session_context.user_id,
                &session_context.channel_id,
                &session_id,
                &message_id,
                payload.content,
                attachments,
            )
            .await?
        } else {
            let message = state
                .channel_store
                .update_session_message(
                    &session_context.user_id,
                    &session_context.channel_id,
                    &session_id,
                    &message_id,
                    payload.content,
                    attachments,
                )
                .await
                .map_err(map_channel_error)?;
            (StatusCode::OK, message, false, true)
        };
    if let Some(run_id) = payload
        .run_id
        .as_deref()
        .and_then(normalize_protocol_run_id)
    {
        // 同一条执行步骤消息会被 adapter 持续编辑；编辑也必须刷新 run 心跳。
        let _ = state
            .channel_store
            .heartbeat_run_for_session(&session_id, run_id)
            .await
            .map_err(map_channel_error)?;
    }
    if emit_message_event {
        if created {
            state.session_events.publish(SessionEvent::MessageCreated {
                message: message.clone(),
            });
        } else {
            state.session_events.publish(SessionEvent::MessageUpdated {
                message: message.clone(),
            });
        }
    }
    if let Some(updated_session) = refresh_public_session_retention_from_message(
        &state,
        &session_context.user_id,
        &session_context.channel_id,
        &session_id,
        message.updated_at,
    )
    .await?
    {
        state.session_events.publish(SessionEvent::SessionUpdated {
            session: updated_session,
        });
    }

    Ok((status, Json(MessageResponse { message })))
}

async fn upsert_latest_execution_message(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    session_id: &str,
    message_id: &str,
    content: String,
    attachments: serde_json::Value,
) -> Result<
    (
        StatusCode,
        crate::channel::service::ChannelMessage,
        bool,
        bool,
    ),
    ApiError,
> {
    let messages = state
        .channel_store
        .list_session_messages(user_id, channel_id, session_id)
        .await
        .map_err(map_channel_error)?;
    let target_exists = messages.iter().any(|message| message.id == message_id);
    if !target_exists {
        return Err(map_channel_error(
            crate::channel::service::ChannelStoreError::ChannelNotFound,
        ));
    }

    let latest = messages.last();
    let update_message_id = latest.and_then(|message| {
        if message.id == message_id
            || (message.role == ChannelMessageRole::Assistant
                && is_execution_protocol_message(&message.content))
        {
            Some(message.id.as_str())
        } else {
            None
        }
    });

    if let Some(update_message_id) = update_message_id {
        let message = state
            .channel_store
            .update_session_message(
                user_id,
                channel_id,
                session_id,
                update_message_id,
                content,
                attachments,
            )
            .await
            .map_err(map_channel_error)?;
        return Ok((StatusCode::OK, message, false, true));
    }

    // Hermes 对旧执行步骤消息的 edit 可能在用户新消息或正式回复之后才到达。
    // 这时不能继续改旧气泡，否则执行步骤会“漂”回历史位置；应在 session 末尾新开执行气泡。
    let message = state
        .channel_store
        .append_session_message(
            user_id,
            channel_id,
            session_id,
            ChannelMessageRole::Assistant,
            None,
            content,
            attachments,
        )
        .await
        .map_err(map_channel_error)?;
    Ok((StatusCode::CREATED, message, true, true))
}

fn is_execution_protocol_message(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("<!-- hermes-hub:execution:v1 -->")
        || trimmed.starts_with("执行步骤\n")
        || trimmed.lines().any(is_legacy_hermes_tool_line)
}

fn is_legacy_hermes_tool_line(line: &str) -> bool {
    let line = line.trim();
    let Some((split_at, _)) = line.char_indices().find(|(_, ch)| ch.is_whitespace()) else {
        return false;
    };
    let rest = line[split_at..].trim_start();
    let Some(open_paren) = rest.find('(') else {
        return false;
    };
    if !rest.ends_with(')') {
        return false;
    }
    let tool_name = &rest[..open_paren];
    !tool_name.is_empty()
        && tool_name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

async fn poll_inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, ApiError> {
    let token_context = verify_instance_token(&state, &headers).await?;
    if let Some(instance_id) = token_context.hermes_instance_id.as_deref() {
        if take_gateway_restart_control(&state, instance_id).await? {
            return Ok(Json(InboxResponse {
                items: vec![gateway_restart_control_item(instance_id)],
            }));
        }
    }
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

async fn take_gateway_restart_control(
    state: &AppState,
    instance_id: &str,
) -> Result<bool, ApiError> {
    match state.store.take_hermes_gateway_restart(instance_id).await {
        Ok(pending) => Ok(pending),
        // 兼容仅注册 token 的测试/开发模式；没有实例记录时继续按普通消息队列处理。
        Err(StoreError::InviteNotFound) => Ok(false),
        Err(_) => Err(ApiError::Internal),
    }
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

async fn report_instance_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<InstanceStatusReportRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let token_context = verify_instance_token(&state, &headers).await?;
    let instance_id = token_context
        .hermes_instance_id
        .as_deref()
        .ok_or(ApiError::Unauthorized)?;
    let runtime_image = clean_runtime_report_value(payload.runtime_image, 512);
    let runtime_version = clean_runtime_version_report_value(payload.runtime_version, 128);
    let scheduler_snapshot = payload
        .scheduler_snapshot
        .map(scheduler_snapshot_input)
        .transpose()?;
    if runtime_image.is_none() && runtime_version.is_none() && scheduler_snapshot.is_none() {
        return Err(ApiError::BadRequest("runtime status report is empty"));
    }

    // adapter 从 Hermes 容器内部上送真实版本；Docker 镜像 tag 只作为未上报前的兜底。
    let hermes_instance = state
        .store
        .update_hermes_instance_runtime(instance_id, runtime_image, runtime_version)
        .await
        .map_err(|_| ApiError::Internal)?;
    if let Some(snapshot) = scheduler_snapshot {
        state
            .store
            .record_hermes_scheduler_snapshot(instance_id, snapshot)
            .await
            .map_err(|_| ApiError::Internal)?;
    }
    Ok(Json(HermesInstanceResponse { hermes_instance }))
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
    ensure_attachment_not_expired(&state, &attachment).await?;
    let stream = state
        .object_storage
        .get_stream(&attachment.object_key)
        .await
        .map_err(|_| ApiError::BadGateway("object storage request failed"))?;
    let mut response = (StatusCode::OK, Body::from_stream(stream)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&attachment.content_type).map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}

fn clean_runtime_report_value(value: Option<String>, max_len: usize) -> Option<String> {
    value
        .map(|value| value.trim().chars().take(max_len).collect::<String>())
        .filter(|value| !value.is_empty())
}

fn clean_runtime_version_report_value(value: Option<String>, max_len: usize) -> Option<String> {
    // latest 只是镜像滚动标签，不是可追溯的 Hermes 发布版本。
    clean_runtime_report_value(value, max_len).filter(|value| value != "latest")
}

fn scheduler_snapshot_input(
    payload: SchedulerSnapshotReportRequest,
) -> Result<HermesSchedulerSnapshotInput, ApiError> {
    let source = clean_runtime_report_value(payload.source, 128)
        .unwrap_or_else(|| "hermes-adapter".to_string());
    let scheduler_status =
        clean_runtime_report_value(payload.status, 64).unwrap_or_else(|| "unavailable".to_string());
    let raw_tasks = payload.tasks.or(payload.jobs).unwrap_or_default();
    let tasks = raw_tasks
        .into_iter()
        .enumerate()
        .map(|(index, task)| scheduler_task_snapshot(task, index, &source))
        .collect::<Vec<_>>();
    let running_jobs_count = payload.running_jobs_count.unwrap_or_else(|| {
        tasks
            .iter()
            .filter(|task| task.status == "running" || task.status == "leased")
            .count() as u32
    });
    let reported_at = payload
        .reported_at
        .as_ref()
        .and_then(timestamp_from_report_value)
        .or_else(|| {
            payload
                .generated_at
                .as_ref()
                .and_then(timestamp_from_report_value)
        })
        .unwrap_or_else(unix_now);

    Ok(HermesSchedulerSnapshotInput {
        scheduler_status: scheduler_status.clone(),
        scheduler_enabled: payload
            .scheduler_enabled
            .unwrap_or_else(|| scheduler_status == "ok"),
        running_jobs_count,
        reported_at,
        source,
        snapshot_hash: payload
            .snapshot_hash
            .and_then(|value| clean_runtime_report_value(Some(value), 256)),
        next_wake_at: payload
            .next_wake_at
            .as_ref()
            .and_then(timestamp_from_report_value),
        tasks,
    })
}

fn scheduler_task_snapshot(
    payload: SchedulerJobReportRequest,
    index: usize,
    snapshot_source: &str,
) -> HermesScheduledTaskSnapshot {
    let name = payload
        .name
        .as_ref()
        .and_then(string_from_report_value)
        .unwrap_or_default();
    let id = payload
        .id
        .as_ref()
        .and_then(string_from_report_value)
        .or_else(|| (!name.is_empty()).then(|| name.clone()))
        .unwrap_or_else(|| format!("task-{index}"));
    let enabled = payload.enabled.unwrap_or(true);
    let status = payload
        .status
        .as_ref()
        .and_then(string_from_report_value)
        .or_else(|| payload.state.as_ref().and_then(string_from_report_value))
        .or_else(|| {
            payload
                .last_status
                .as_ref()
                .and_then(string_from_report_value)
        })
        .unwrap_or_else(|| {
            if enabled {
                "scheduled".to_string()
            } else {
                "disabled".to_string()
            }
        });

    HermesScheduledTaskSnapshot {
        id,
        name,
        enabled,
        schedule: payload
            .schedule
            .as_ref()
            .or(payload.cron.as_ref())
            .and_then(string_from_report_value)
            .unwrap_or_default(),
        timezone: payload
            .timezone
            .as_ref()
            .and_then(string_from_report_value)
            .unwrap_or_else(|| "UTC".to_string()),
        next_run_at: payload
            .next_run_at
            .as_ref()
            .and_then(timestamp_from_report_value),
        last_run_at: payload
            .last_run_at
            .as_ref()
            .and_then(timestamp_from_report_value),
        status,
        source: payload
            .source
            .as_ref()
            .and_then(string_from_report_value)
            .unwrap_or_else(|| snapshot_source.to_string()),
    }
}

fn string_from_report_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.trim().chars().take(512).collect::<String>()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
    .filter(|value| !value.is_empty())
}

fn timestamp_from_report_value(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_f64().map(|value| value as u64)),
        Value::String(value) => {
            let value = value.trim();
            value.parse::<u64>().ok()
        }
        _ => None,
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
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

    let token_context = state
        .model_registry
        .instance_token_context(token)
        .await
        .ok_or(ApiError::Unauthorized)?;

    if let Some(instance_id) = token_context.hermes_instance_id.as_deref() {
        match state
            .store
            .record_hermes_instance_adapter_heartbeat(instance_id)
            .await
        {
            Ok(_) | Err(StoreError::InviteNotFound) => {}
            Err(_) => return Err(ApiError::Internal),
        }
    }

    Ok(token_context)
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
        item_type: "message".to_string(),
        id: run.run_id.clone(),
        action: None,
        run_id: Some(run.run_id),
        session_id: Some(run.session_id),
        user_id: Some(session_context.user_id),
        content: Some(run.input),
        attachments,
    })
}

fn gateway_restart_control_item(instance_id: &str) -> InboxItem {
    InboxItem {
        item_type: "control".to_string(),
        id: format!("control:restart_gateway:{instance_id}"),
        action: Some("restart_gateway".to_string()),
        run_id: None,
        session_id: None,
        user_id: None,
        content: None,
        attachments: json!([]),
    }
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
