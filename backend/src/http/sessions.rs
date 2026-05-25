use axum::{
    body::to_bytes,
    extract::{Multipart, Path, State},
    http::{HeaderMap, Method, StatusCode},
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
use std::{
    convert::Infallible,
    time::{Duration, Instant},
};

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
    llm_proxy::LlmProviderRequest,
    model_config::TITLE_MODEL_CONFIG_KIND,
    session::store::LlmUsageEvent,
    storage::ObjectStorageError,
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
            .and_then(|bytes| parse_title_response(bytes, prompt))
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

fn parse_title_response(bytes: &[u8], prompt: &str) -> Option<String> {
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    let title = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/output_text").and_then(Value::as_str))?;
    clean_generated_title(title, prompt)
}

fn title_generation_request(
    config: &crate::model_config::ModelConfig,
    prompt: &str,
) -> (String, Value) {
    if config.api_type == crate::model_config::RESPONSES_API_TYPE {
        let mut body = json!({
            "model": config.default_model,
            "stream": false,
            "max_output_tokens": 24,
            "input": [
                {
                    "role": "system",
                    "content": title_generation_system_prompt()
                },
                {
                    "role": "user",
                    "content": title_generation_user_prompt(prompt)
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
        "max_tokens": 24,
        "messages": [
            {
                "role": "system",
                "content": title_generation_system_prompt()
            },
            {
                "role": "user",
                "content": title_generation_user_prompt(prompt)
            }
        ]
    });
    if let Some(effort) = config.reasoning_effort.as_deref() {
        body["reasoning_effort"] = json!(effort);
    }

    ("/chat/completions".to_string(), body)
}

fn title_generation_system_prompt() -> &'static str {
    "你是会话标题生成器，不是问答助手。根据用户第一条消息生成一个短标题。只输出标题本身；不要回答用户问题；不要解释；不要使用“我/你/可以/能”等回答式措辞；不要句号。标题用中文优先，2 到 12 个汉字或最多 6 个英文词。"
}

fn title_generation_user_prompt(prompt: &str) -> String {
    format!("用户第一条消息：{prompt}\n\n请生成短标题，只输出标题。")
}

fn fallback_title(prompt: &str) -> String {
    clean_title(prompt).unwrap_or_else(|| "New conversation".to_string())
}

fn clean_generated_title(value: &str, prompt: &str) -> Option<String> {
    let title = clean_title(value)?;
    if title_looks_like_answer(&title) {
        return Some(fallback_title(prompt));
    }
    Some(title)
}

fn clean_title(value: &str) -> Option<String> {
    let title = value
        .lines()
        .next()
        .unwrap_or(value)
        .trim()
        .trim_matches('"')
        .trim_start_matches("标题：")
        .trim_start_matches("标题:")
        .trim()
        .chars()
        .take(48)
        .collect::<String>();

    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

fn title_looks_like_answer(title: &str) -> bool {
    let normalized = title.trim();
    let lowered = normalized.to_ascii_lowercase();

    normalized.starts_with("能，")
        || normalized.starts_with("可以")
        || normalized.starts_with("是的")
        || normalized.starts_with("当然")
        || normalized.starts_with("不能")
        || normalized.contains("我可以")
        || normalized.contains("我能")
        || normalized.contains("帮你")
        || lowered.starts_with("yes")
        || lowered.starts_with("no,")
        || lowered.starts_with("i can")
}
