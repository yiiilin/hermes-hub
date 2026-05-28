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
        attachments::{
            ensure_attachment_not_expired, map_channel_error,
            upload_session_attachments_for_context,
        },
        ApiError,
    },
    model_config::InstanceTokenContext,
    session::store::{HermesScheduledTaskSnapshot, HermesSchedulerSnapshotInput, StoreError},
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
    let client_message_key = payload.client_message_key;
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
    if let Some(run_id) = heartbeat_run_id.as_deref() {
        // Hermes 的执行步骤、状态文本和最终输出都代表任务仍然活跃；
        // 刷新 run 心跳，避免长任务超过恢复窗口后被重复派发。
        let _ = state
            .channel_store
            .heartbeat_run_for_session(&session_id, run_id)
            .await
            .map_err(map_channel_error)?;
    }
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
    let attachments = payload.attachments.unwrap_or_else(|| json!([]));
    let (status, message, created) = if is_execution_protocol_message(&payload.content) {
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
        (StatusCode::OK, message, false)
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
    if created {
        state.session_events.publish(SessionEvent::MessageCreated {
            message: message.clone(),
        });
    } else {
        state.session_events.publish(SessionEvent::MessageUpdated {
            message: message.clone(),
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
) -> Result<(StatusCode, crate::channel::service::ChannelMessage, bool), ApiError> {
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
        return Ok((StatusCode::OK, message, false));
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
    Ok((StatusCode::CREATED, message, true))
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
