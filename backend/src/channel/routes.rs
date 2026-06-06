use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::HeaderMap,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post, put},
    Json, Router,
};
use futures_util::{stream, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{convert::Infallible, time::Duration};

use crate::{
    channel::{
        events::SessionEvent,
        service::{
            Channel, ChannelActiveRun, ChannelAttachment, ChannelAttachmentDirection,
            ChannelMessage, ChannelMessageRole, ChannelRun, ChannelRunStatus, ChannelSession,
            ChannelSessionKind, ChannelStoreError,
        },
    },
    http::{
        attachments::upload_session_attachments,
        auth::current_user,
        sessions::{apply_sse_no_buffer_headers, delete_managed_cron_jobs_for_session},
        workspace::ensure_managed_hermes_for_user,
        ApiError,
    },
    storage::ObjectStorageError,
    title_generation::model_generated_title,
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
            get(get_session).delete(delete_session),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}/active-run",
            get(get_active_run).delete(clear_active_run),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}/active-run/stop",
            post(stop_active_run),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}/runs",
            post(create_channel_run),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}/attachments",
            post(upload_attachments).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}/messages",
            get(list_session_messages).post(append_session_message),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}/events",
            get(session_events),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}/messages/{message_id}",
            put(update_session_message),
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
    client_message_key: Option<String>,
}

#[derive(Deserialize)]
struct CreateRunRequest {
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
struct RunCreateResponse {
    message: ChannelMessage,
    run: ChannelRun,
}

#[derive(Serialize)]
struct MessageListResponse {
    messages: Vec<ChannelMessage>,
}

#[derive(Serialize)]
struct AttachmentListResponse {
    attachments: Vec<ChannelAttachment>,
}

#[derive(Serialize)]
struct ActiveRunResponse {
    active_run: Option<ChannelActiveRun>,
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
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    let session = state
        .channel_store
        .create_session_with_limit(
            &user.id,
            &channel_id,
            kind,
            None,
            settings.max_sessions_per_user,
            false,
        )
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
    let sessions = sessions
        .into_iter()
        .filter(|session| !session.hidden_from_web)
        .collect();

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

async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    state
        .channel_store
        .get_session(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?;

    // 删除会话前先停止当前 Hermes run，避免容器继续写入已经被删除的会话。
    let _ = stop_active_run_for_session(&state, &user.id, &channel_id, &session_id).await?;
    delete_managed_cron_jobs_for_session(&state, &user.id, &session_id).await?;
    let deleted = state
        .channel_store
        .delete_session(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?;
    state.session_events.publish(SessionEvent::SessionDeleted {
        session_id: session_id.clone(),
    });
    delete_session_objects(&state, &deleted.attachments).await?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

async fn get_active_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    state
        .channel_store
        .get_session(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?;

    let active_run = state
        .channel_store
        .get_active_run_for_session(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?
        .map(ChannelActiveRun::from);

    Ok(Json(ActiveRunResponse { active_run }))
}

async fn stop_active_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    state
        .channel_store
        .get_session(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?;
    stop_active_run_for_session(&state, &user.id, &channel_id, &session_id).await?;

    Ok(Json(ActiveRunResponse { active_run: None }))
}

async fn clear_active_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    state
        .channel_store
        .get_session(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?;
    clear_persisted_hermes_run(&state, &user.id, &channel_id, &session_id).await?;
    state.session_events.publish(SessionEvent::RunCleared {
        session_id: session_id.clone(),
    });

    Ok(axum::http::StatusCode::NO_CONTENT)
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
            payload.client_message_key,
            payload.content,
            payload.attachments.unwrap_or_else(|| json!([])),
        )
        .await
        .map_err(map_channel_error)?;
    state.session_events.publish(SessionEvent::MessageCreated {
        message: message.clone(),
    });

    Ok((
        axum::http::StatusCode::CREATED,
        Json(MessageResponse { message }),
    ))
}

async fn create_channel_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
    Json(payload): Json<CreateRunRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    // 创建 Hub run 的前置条件是托管 Hermes 已运行当前 adapter 规格；
    // 这个约束由后端统一保证，前端不参与容器版本判断。
    ensure_managed_hermes_for_user(&state, &user.id).await?;
    // 用户输入必须先作为普通会话消息落库；adapter 只消费 Hub 队列，不再由前端直连 Hermes /v1/runs。
    let message = state
        .channel_store
        .append_session_message(
            &user.id,
            &channel_id,
            &session_id,
            ChannelMessageRole::User,
            payload.client_message_key,
            payload.content,
            payload.attachments.unwrap_or_else(|| json!([])),
        )
        .await
        .map_err(map_channel_error)?;
    state.session_events.publish(SessionEvent::MessageCreated {
        message: message.clone(),
    });
    let run = state
        .channel_store
        .create_channel_run(
            &user.id,
            &channel_id,
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

    Ok((
        axum::http::StatusCode::CREATED,
        Json(RunCreateResponse { message, run }),
    ))
}

async fn update_session_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id, message_id)): Path<(String, String, String)>,
    Json(payload): Json<UpdateMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    // 前端会把正在执行的工具步骤保存为一条可更新的 assistant 消息；
    // 所有权仍由 channel/session 校验保证，用户不能跨会话修改消息。
    let message = state
        .channel_store
        .update_session_message(
            &user.id,
            &channel_id,
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

async fn session_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel_id, session_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    // 必须先订阅再读取 snapshot，避免“历史读取完成前后”之间的落库事件丢失。
    // 这会把同一消息同时出现在 snapshot 和 live event 的最坏情况交给前端按 id 去重，
    // 但不会漏掉 Hermes 已经持久化的输出。
    let receiver = state.session_events.subscribe();
    let messages = state
        .channel_store
        .list_session_messages(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let active_run = state
        .channel_store
        .get_active_run_for_session(&user.id, &channel_id, &session_id)
        .await
        .map_err(map_channel_error)?
        .map(ChannelActiveRun::from);

    // 浏览器进入 room 时先拿到持久化历史和当前 run，再接收后续实时事件。
    let snapshot = json!({
        "type": "messages_snapshot",
        "messages": messages,
        "active_run": active_run,
    });
    let snapshot_stream =
        stream::once(async move { sse_json_event("messages_snapshot", &snapshot) });
    let live_stream = session_live_event_stream(receiver, session_id);

    let mut response = Sse::new(snapshot_stream.chain(live_stream))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response();
    apply_sse_no_buffer_headers(response.headers_mut());
    Ok(response)
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
    state.session_events.publish(SessionEvent::SessionUpdated {
        session: session.clone(),
    });

    Ok(Json(SessionResponse { session }))
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
                        return Some((sse_json_event(event.event_name(), &event), receiver));
                    }
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // 连接落后时主动结束本次 SSE；浏览器 EventSource 会重连并重新读取 snapshot。
                        return None;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        }
    })
}

fn sse_json_event<T: Serialize>(name: &'static str, payload: &T) -> Result<Event, Infallible> {
    let data = serde_json::to_string(payload).unwrap_or_else(|_| {
        // 序列化失败只可能来自内部结构异常；保持 SSE 连接可读，方便前端重连恢复。
        json!({"type": "serialization_failed"}).to_string()
    });
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

fn map_channel_error(error: ChannelStoreError) -> ApiError {
    match error {
        ChannelStoreError::ChannelNotFound => ApiError::NotFound("channel not found"),
        ChannelStoreError::InvalidIntegrationId => ApiError::BadRequest("invalid integration id"),
        ChannelStoreError::InvalidSessionKind => ApiError::BadRequest("invalid session kind"),
        ChannelStoreError::InvalidMessageRole => ApiError::BadRequest("invalid message role"),
        ChannelStoreError::InvalidAttachment => ApiError::BadRequest("invalid attachment"),
        ChannelStoreError::InvalidRunStatus => ApiError::BadRequest("invalid run status"),
        ChannelStoreError::AttachmentNotFound => ApiError::NotFound("attachment not found"),
        ChannelStoreError::RunNotFound => ApiError::NotFound("run not found"),
        ChannelStoreError::SessionLimitExceeded {
            max_sessions_per_user,
        } => ApiError::SessionLimitExceeded {
            max_sessions_per_user,
        },
        ChannelStoreError::ProtectedSession => ApiError::Conflict("session is protected"),
        ChannelStoreError::LockFailed | ChannelStoreError::DatabaseFailed => ApiError::Internal,
    }
}
