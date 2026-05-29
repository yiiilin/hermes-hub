use axum::{
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{delete, get, post, put},
    Json, Router,
};
use futures_util::{stream, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{convert::Infallible, path::PathBuf, time::Duration};

use crate::{
    channel::{
        events::SessionEvent,
        service::{
            ChannelActiveRun, ChannelAttachment, ChannelAttachmentDirection, ChannelMessage,
            ChannelMessageRole, ChannelRunStatus, ChannelSession, ChannelSessionKind,
        },
    },
    http::{
        attachments::{map_channel_error, upload_session_attachments},
        auth::current_user,
        workspace::ensure_managed_hermes_for_user,
        ApiError,
    },
    storage::ObjectStorageError,
    title_generation::model_generated_title,
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route("/api/sessions/{session_id}", delete(delete_session))
        .route(
            "/api/sessions/{session_id}/messages",
            post(append_session_message),
        )
        .route(
            "/api/sessions/{session_id}/messages/{message_id}",
            put(update_session_message),
        )
        .route(
            "/api/sessions/{session_id}/attachments",
            post(upload_attachments),
        )
        .route("/api/sessions/{session_id}/events", get(session_events))
        .route("/api/sessions/{session_id}/stop", post(stop_active_run))
}

#[derive(Deserialize)]
struct CreateSessionRequest {
    kind: Option<String>,
    title: Option<String>,
}

#[derive(Deserialize)]
struct AppendMessageRequest {
    role: String,
    content: String,
    attachments: Option<Value>,
    client_message_key: Option<String>,
}

#[derive(Deserialize)]
struct UpdateMessageRequest {
    content: String,
    attachments: Option<Value>,
}

#[derive(Serialize)]
struct PublicSessionSummary {
    id: String,
    title: Option<String>,
    created_at: u64,
    updated_at: u64,
}

#[derive(Serialize)]
struct SessionListResponse {
    sessions: Vec<PublicSessionSummary>,
}

#[derive(Serialize)]
struct SessionResponse {
    session: PublicSessionSummary,
}

#[derive(Serialize)]
struct MessageResponse {
    message: ChannelMessage,
}

#[derive(Serialize)]
struct AttachmentListResponse {
    attachments: Vec<ChannelAttachment>,
}

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channel = ensure_public_channel(&state, &user.id).await?;
    let sessions = state
        .channel_store
        .list_sessions(&user.id, &channel.id)
        .await
        .map_err(map_channel_error)?;

    Ok(Json(SessionListResponse {
        sessions: sessions.into_iter().map(public_session_summary).collect(),
    }))
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channel = ensure_public_channel(&state, &user.id).await?;
    let kind = ChannelSessionKind::parse(payload.kind.as_deref().unwrap_or("agent"))
        .map_err(map_channel_error)?;
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    let session = state
        .channel_store
        .create_session_with_limit(
            &user.id,
            &channel.id,
            kind,
            payload.title,
            settings.max_sessions_per_user,
        )
        .await
        .map_err(map_channel_error)?;

    Ok((
        StatusCode::CREATED,
        Json(SessionResponse {
            session: public_session_summary(session),
        }),
    ))
}

async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channel = ensure_public_channel(&state, &user.id).await?;
    state
        .channel_store
        .get_session(&user.id, &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;

    // 删除会话前先停掉正在跑的 Hermese run，避免后台继续写入已经删除的 session。
    let _ = stop_active_run_for_session(&state, &user.id, &channel.id, &session_id).await?;
    delete_managed_cron_jobs_for_session(&state, &user.id, &session_id).await?;
    let deleted = state
        .channel_store
        .delete_session(&user.id, &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    state.session_events.publish(SessionEvent::SessionDeleted {
        session_id: session_id.clone(),
    });
    delete_session_objects(&state, &deleted.attachments).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn append_session_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(payload): Json<AppendMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channel = ensure_public_channel(&state, &user.id).await?;
    let session = state
        .channel_store
        .get_session(&user.id, &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let role = ChannelMessageRole::parse(&payload.role).map_err(map_channel_error)?;

    if role == ChannelMessageRole::User {
        // 用户消息在 public API 中仍然触发一次 Hub run，但不把 run 详情暴露给前端。
        ensure_managed_hermes_for_user(&state, &user.id).await?;
    }

    let message = state
        .channel_store
        .append_session_message(
            &user.id,
            &channel.id,
            &session_id,
            role.clone(),
            payload.client_message_key,
            payload.content.clone(),
            payload.attachments.unwrap_or_else(|| json!([])),
        )
        .await
        .map_err(map_channel_error)?;
    state.session_events.publish(SessionEvent::MessageCreated {
        message: message.clone(),
    });

    if role == ChannelMessageRole::User {
        let run = state
            .channel_store
            .create_channel_run(
                &user.id,
                &channel.id,
                &session_id,
                &message.id,
                message.content.clone(),
                message.attachments.clone(),
            )
            .await
            .map_err(map_channel_error)?;
        state
            .session_events
            .publish(SessionEvent::RunUpdated { run: run.clone() });

        if session.title.is_none() && !message.content.trim().is_empty() {
            let title = model_generated_title(&state, &user.id, &message.content).await;
            let _ = state
                .channel_store
                .update_session_title(&user.id, &channel.id, &session_id, title)
                .await
                .map_err(map_channel_error)?;
        }
    }

    Ok((StatusCode::CREATED, Json(MessageResponse { message })))
}

async fn update_session_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((session_id, message_id)): Path<(String, String)>,
    Json(payload): Json<UpdateMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channel = ensure_public_channel(&state, &user.id).await?;
    // 前端会把执行步骤写成可更新的 assistant 消息；这里只保留消息编辑，不暴露 channel。
    let message = state
        .channel_store
        .update_session_message(
            &user.id,
            &channel.id,
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

async fn upload_attachments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    multipart: Multipart,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channel = ensure_public_channel(&state, &user.id).await?;
    let attachments = upload_session_attachments(
        &state,
        &user.id,
        &channel.id,
        &session_id,
        ChannelAttachmentDirection::Input,
        multipart,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(AttachmentListResponse { attachments }),
    ))
}

async fn session_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channel = ensure_public_channel(&state, &user.id).await?;
    state
        .channel_store
        .get_session(&user.id, &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;

    // SSE 首包带完整快照，浏览器只需要订阅这一条 public 事件流。
    let receiver = state.session_events.subscribe();
    let messages = state
        .channel_store
        .list_session_messages(&user.id, &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let active_run = state
        .channel_store
        .get_active_run_for_session(&user.id, &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?
        .map(ChannelActiveRun::from);

    let snapshot = json!({
        "type": "messages_snapshot",
        "messages": messages,
        "active_run": active_run,
    });
    let snapshot_stream =
        stream::once(async move { sse_json_event("messages_snapshot", &snapshot) });
    let live_stream = session_live_event_stream(receiver, session_id);

    Ok(Sse::new(snapshot_stream.chain(live_stream)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

async fn stop_active_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channel = ensure_public_channel(&state, &user.id).await?;
    state
        .channel_store
        .get_session(&user.id, &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    stop_active_run_for_session(&state, &user.id, &channel.id, &session_id).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn stop_active_run_for_session(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    session_id: &str,
) -> Result<Option<ChannelActiveRun>, ApiError> {
    let Some(run) = state
        .channel_store
        .get_active_run_for_session(user_id, channel_id, session_id)
        .await
        .map_err(map_channel_error)?
    else {
        clear_persisted_hermes_run(state, user_id, channel_id, session_id).await?;
        return Ok(None);
    };

    let updated = state
        .channel_store
        .update_run_status_for_session(
            session_id,
            &run.run_id,
            ChannelRunStatus::Cancelled,
            None,
            None,
        )
        .await
        .map_err(map_channel_error)?;
    state.session_events.publish(SessionEvent::RunUpdated {
        run: updated.clone(),
    });
    clear_persisted_hermes_run(state, user_id, channel_id, session_id).await?;
    state.session_events.publish(SessionEvent::RunCleared {
        session_id: session_id.to_string(),
    });

    Ok(Some(ChannelActiveRun::from(updated)))
}

async fn clear_persisted_hermes_run(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    session_id: &str,
) -> Result<(), ApiError> {
    state
        .channel_store
        .clear_session_hermes_run_id(user_id, channel_id, session_id)
        .await
        .map_err(map_channel_error)?;
    Ok(())
}

fn public_session_summary(session: ChannelSession) -> PublicSessionSummary {
    PublicSessionSummary {
        id: session.id,
        title: session.title,
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

async fn ensure_public_channel(
    state: &AppState,
    user_id: &str,
) -> Result<crate::channel::service::Channel, ApiError> {
    state
        .channel_store
        .ensure_hub_channel(user_id)
        .await
        .map_err(map_channel_error)
}

fn session_live_event_stream(
    receiver: tokio::sync::broadcast::Receiver<SessionEvent>,
    session_id: String,
) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold(receiver, move |mut receiver| {
        let session_id = session_id.clone();
        async move {
            loop {
                match receiver.recv().await {
                    Ok(event) if event.session_id() == session_id => {
                        return Some((
                            sse_json_event(session_event_name(&event), &event),
                            receiver,
                        ));
                    }
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // 连接落后时直接结束，浏览器会重连并重新拿 snapshot。
                        return None;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        }
    })
}

fn session_event_name(event: &SessionEvent) -> &'static str {
    match event {
        SessionEvent::MessageCreated { .. } => "message_created",
        SessionEvent::MessageUpdated { .. } => "message_updated",
        SessionEvent::RunUpdated { .. } => "run_updated",
        SessionEvent::RunCleared { .. } => "run_cleared",
        SessionEvent::SessionDeleted { .. } => "session_deleted",
    }
}

fn sse_json_event<T: Serialize>(name: &'static str, payload: &T) -> Result<Event, Infallible> {
    let data = serde_json::to_string(payload)
        .unwrap_or_else(|_| json!({"type": "serialization_failed"}).to_string());
    Ok(Event::default().event(name).data(data))
}

async fn delete_session_objects(
    state: &AppState,
    attachments: &[ChannelAttachment],
) -> Result<(), ApiError> {
    for attachment in attachments {
        match state.object_storage.delete(&attachment.object_key).await {
            Ok(()) | Err(ObjectStorageError::NotFound) => {}
            Err(_) => return Err(ApiError::Internal),
        }
    }

    Ok(())
}

pub(crate) async fn delete_managed_cron_jobs_for_session(
    state: &AppState,
    user_id: &str,
    session_id: &str,
) -> Result<(), ApiError> {
    let instance = match state.store.hermes_instance_for_user(user_id).await {
        Ok(instance) => instance,
        Err(_) => return Ok(()),
    };
    let Some(host_config_path) = instance.host_config_path.as_deref() else {
        return Ok(());
    };
    let jobs_path = PathBuf::from(host_config_path)
        .join("cron")
        .join("jobs.json");
    let raw_jobs = match tokio::fs::read_to_string(&jobs_path).await {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(ApiError::Internal),
    };
    let mut jobs_value: Value = serde_json::from_str(&raw_jobs).map_err(|_| ApiError::Internal)?;
    let removed_job_ids = remove_cron_jobs_for_session(&mut jobs_value, session_id);
    if removed_job_ids.is_empty() {
        return Ok(());
    }

    let next_jobs = serde_json::to_string_pretty(&jobs_value).map_err(|_| ApiError::Internal)?;
    tokio::fs::write(&jobs_path, next_jobs)
        .await
        .map_err(|_| ApiError::Internal)?;

    for job_id in removed_job_ids {
        // Hermes 会把 cron 输出按 job id 放在 cron/output 下；删除 session 时一并清理这些孤儿输出。
        let output_path = PathBuf::from(host_config_path)
            .join("cron")
            .join("output")
            .join(job_id);
        match tokio::fs::remove_dir_all(output_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ApiError::Internal),
        }
    }

    Ok(())
}

fn remove_cron_jobs_for_session(jobs_value: &mut Value, session_id: &str) -> Vec<String> {
    let Some(jobs) = cron_jobs_array_mut(jobs_value) else {
        return Vec::new();
    };
    let mut removed_job_ids = Vec::new();
    jobs.retain(|job| {
        if cron_job_targets_session(job, session_id) {
            removed_job_ids.push(
                job.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            );
            false
        } else {
            true
        }
    });
    removed_job_ids
        .into_iter()
        .filter(|job_id| !job_id.is_empty())
        .collect()
}

fn cron_jobs_array_mut(value: &mut Value) -> Option<&mut Vec<Value>> {
    if value.is_array() {
        return value.as_array_mut();
    }
    value.get_mut("jobs").and_then(Value::as_array_mut)
}

fn cron_job_targets_session(job: &Value, session_id: &str) -> bool {
    let origin = job.get("origin").unwrap_or(&Value::Null);
    [
        job.get("session_id"),
        job.get("chat_id"),
        job.get("thread_id"),
        origin.get("session_id"),
        origin.get("chat_id"),
        origin.get("thread_id"),
    ]
    .iter()
    .flatten()
    .any(|value| value.as_str() == Some(session_id))
}
