use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post, put},
    Json, Router,
};
use futures_util::{stream, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    convert::Infallible,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use crate::{
    channel::{
        events::SessionEvent,
        service::{
            is_business_tool_request_message, ChannelActiveRun, ChannelAttachment,
            ChannelAttachmentDirection, ChannelMessage, ChannelMessageRole, ChannelRun,
            ChannelRunStatus, ChannelSession, ChannelSessionKind, ChannelStoreError,
            HiddenSessionCleanupCandidate,
        },
    },
    domain::user::User,
    http::{
        attachments::{map_channel_error, upload_session_attachments},
        auth::{cookie_value_from_headers, current_user},
        integrations::is_reserved_business_tool_client_key,
        workspace::ensure_managed_hermes_for_user,
        ApiError,
    },
    public_platform,
    storage::ObjectStorageError,
    title_generation::model_generated_title,
    AppState,
};

const PUBLIC_SESSION_COOKIE_NAME: &str = "hermes_hub_public_session";
const PUBLIC_SESSION_TOKEN_HEADER: &str = "x-hermes-hub-public-session";
const DEFAULT_HIDDEN_SESSION_IDLE_TIMEOUT_SECONDS: u64 = 3600;

fn elapsed_ms(started_at: Instant) -> u64 {
    started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route("/api/sessions/{session_id}", delete(delete_session))
        .route(
            "/api/sessions/{session_id}/messages",
            get(list_session_messages).post(append_session_message),
        )
        .route("/api/sessions/{session_id}/runs", post(create_channel_run))
        .route(
            "/api/sessions/{session_id}/active-run",
            get(get_active_run).delete(clear_active_run),
        )
        .route(
            "/api/sessions/{session_id}/messages/{message_id}",
            put(update_session_message),
        )
        .route(
            "/api/sessions/{session_id}/attachments",
            post(upload_attachments).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/sessions/{session_id}/events", get(session_events))
        .route("/api/sessions/{session_id}/stop", post(stop_active_run))
        .route(
            "/api/sessions/{session_id}/title",
            post(generate_session_title),
        )
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

#[derive(Deserialize)]
struct GenerateTitleRequest {
    prompt: String,
}

fn reject_reserved_business_tool_public_message(
    content: &str,
    client_message_key: Option<&str>,
) -> Result<(), ApiError> {
    if is_business_tool_request_message(content)
        || client_message_key.is_some_and(is_reserved_business_tool_client_key)
    {
        // 公共会话的写入口不允许占用业务工具保留 key，也不允许伪造业务工具请求 marker。
        return Err(ApiError::BadRequest("reserved client message key"));
    }
    Ok(())
}

#[derive(Serialize)]
struct PublicSessionSummary {
    id: String,
    title: Option<String>,
    is_home: bool,
    deletable: bool,
    created_at: u64,
    updated_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    recycle_at: Option<u64>,
}

#[derive(Serialize)]
struct SessionListResponse {
    sessions: Vec<PublicSessionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_token: Option<String>,
}

#[derive(Serialize)]
struct SessionResponse {
    session: PublicSessionSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_token: Option<String>,
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
struct RunCreateResponse {
    message: ChannelMessage,
    run: ChannelRun,
}

#[derive(Serialize)]
struct AttachmentListResponse {
    attachments: Vec<ChannelAttachment>,
}

#[derive(Serialize)]
struct ActiveRunResponse {
    active_run: Option<ChannelActiveRun>,
}

#[derive(Deserialize)]
struct ListSessionsQuery {
    session_id: Option<String>,
}

enum SessionAccess {
    Authenticated {
        user_id: String,
    },
    Public {
        owner_user_id: String,
        token: Option<String>,
    },
}

struct PublicCreateAccess {
    owner_user_id: String,
    token: Option<String>,
    set_cookie: Option<String>,
    hidden_from_web: bool,
}

impl SessionAccess {
    fn user_id(&self) -> &str {
        match self {
            SessionAccess::Authenticated { user_id } => user_id,
            SessionAccess::Public { owner_user_id, .. } => owner_user_id,
        }
    }
}

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListSessionsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let request_started_at = Instant::now();
    let auth_started_at = Instant::now();
    // 列表接口是登录后首屏路径，复用这次鉴权结果，避免同一请求重复查 session cookie。
    let authenticated_user = optional_web_user(&state, &headers).await?;
    let auth_elapsed_ms = elapsed_ms(auth_started_at);
    let is_authenticated = authenticated_user.is_some();

    let public_prepare_started_at = Instant::now();
    ensure_public_platform_enabled_for_anonymous(&state, is_authenticated).await?;
    let mut public_token = if is_authenticated {
        None
    } else {
        public_session_token_for_request(&state, &headers, query.session_id.as_deref()).await?
    };
    if !is_authenticated {
        cleanup_expired_public_sessions(&state).await?;
        if let Some(session_id) = normalize_public_session_token(query.session_id.as_deref()) {
            if let Some(token) =
                grant_public_session_path_access(&state, public_token.clone(), &session_id).await?
            {
                public_token = Some(token);
            }
        }
    }
    let public_prepare_elapsed_ms = elapsed_ms(public_prepare_started_at);

    let access = if let Some(user) = authenticated_user {
        SessionAccess::Authenticated {
            user_id: user.id.clone(),
        }
    } else {
        SessionAccess::Public {
            owner_user_id: public_platform_owner_user_id(&state).await?,
            token: public_token.clone(),
        }
    };
    let mut ensure_hermes_elapsed_ms = None;
    if matches!(access, SessionAccess::Public { .. }) {
        let ensure_hermes_started_at = Instant::now();
        ensure_managed_hermes_for_user(&state, access.user_id()).await?;
        ensure_hermes_elapsed_ms = Some(elapsed_ms(ensure_hermes_started_at));
    }
    let channel_started_at = Instant::now();
    let Some(channel) = ensure_channel_for_access(&state, &access).await? else {
        tracing::info!(
            authenticated = is_authenticated,
            auth_elapsed_ms,
            public_prepare_elapsed_ms,
            ensure_hermes_elapsed_ms = ?ensure_hermes_elapsed_ms,
            channel_elapsed_ms = elapsed_ms(channel_started_at),
            total_elapsed_ms = elapsed_ms(request_started_at),
            "api sessions list completed without a channel"
        );
        return session_list_response(&headers, vec![], public_token.clone());
    };
    let channel_elapsed_ms = elapsed_ms(channel_started_at);
    let list_started_at = Instant::now();
    let sessions = state
        .channel_store
        .list_sessions(access.user_id(), &channel.id)
        .await
        .map_err(map_channel_error)?;
    let raw_session_count = sessions.len();
    let sessions = if is_authenticated {
        sessions
            .into_iter()
            .filter(|session| !session.hidden_from_web)
            .collect()
    } else {
        sessions
    };
    let list_elapsed_ms = elapsed_ms(list_started_at);
    let filter_started_at = Instant::now();
    let sessions = filter_sessions_for_access(&state, &access, sessions).await?;
    let filter_elapsed_ms = elapsed_ms(filter_started_at);
    let summary_started_at = Instant::now();
    let mut summaries = Vec::with_capacity(sessions.len());
    for session in sessions {
        if matches!(access, SessionAccess::Public { .. })
            && cleanup_public_session_if_recycled(&state, &session.id).await?
        {
            continue;
        }
        summaries.push(public_session_summary_for_access(&state, &access, session).await?);
    }
    let summary_elapsed_ms = elapsed_ms(summary_started_at);
    tracing::info!(
        authenticated = is_authenticated,
        raw_session_count,
        session_count = summaries.len(),
        auth_elapsed_ms,
        public_prepare_elapsed_ms,
        ensure_hermes_elapsed_ms = ?ensure_hermes_elapsed_ms,
        channel_elapsed_ms,
        list_elapsed_ms,
        filter_elapsed_ms,
        summary_elapsed_ms,
        total_elapsed_ms = elapsed_ms(request_started_at),
        "api sessions list completed"
    );
    if !is_authenticated && summaries.is_empty() {
        return session_list_response(&headers, summaries, None);
    }

    session_list_response(&headers, summaries, public_token)
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<Response, ApiError> {
    let authenticated_user = optional_web_user(&state, &headers).await?;
    ensure_public_platform_enabled_for_anonymous(&state, authenticated_user.is_some()).await?;
    let access = session_create_access(&state, &headers, authenticated_user).await?;
    if access.token.is_some() {
        cleanup_expired_public_sessions(&state).await?;
        ensure_managed_hermes_for_user(&state, &access.owner_user_id).await?;
    }
    let channel = ensure_public_channel(&state, &access.owner_user_id).await?;
    let kind = ChannelSessionKind::parse(payload.kind.as_deref().unwrap_or("agent"))
        .map_err(map_channel_error)?;
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    let is_public_session = access.token.is_some();
    let session = if is_public_session {
        state
            .channel_store
            .create_session(
                &access.owner_user_id,
                &channel.id,
                kind,
                payload.title,
                access.hidden_from_web,
            )
            .await
            .map_err(map_channel_error)?
    } else {
        state
            .channel_store
            .create_session_with_limit(
                &access.owner_user_id,
                &channel.id,
                kind,
                payload.title,
                settings.max_sessions_per_user,
                access.hidden_from_web,
            )
            .await
            .map_err(map_channel_error)?
    };
    if let Some(token) = &access.token {
        state
            .store
            .grant_public_session_access(
                token,
                &session.id,
                settings.public_platform.temporary_session_retention_hours,
            )
            .await
            .map_err(|_| ApiError::Internal)?;
    }
    let session = public_session_summary_for_create(&state, is_public_session, session).await?;

    let public_token = access.token.clone();
    let mut response = (
        StatusCode::CREATED,
        Json(SessionResponse {
            session,
            public_token,
        }),
    )
        .into_response();
    if let Some(cookie) = access.set_cookie {
        response.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).map_err(|_| ApiError::Internal)?,
        );
    }

    Ok(response)
}

async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    state
        .channel_store
        .get_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;

    // 删除会话前先停掉正在跑的 Hermese run，避免后台继续写入已经删除的 session。
    let _ = stop_active_run_for_session(&state, access.user_id(), &channel.id, &session_id).await?;
    delete_managed_cron_jobs_for_session(&state, access.user_id(), &session_id).await?;
    let deleted = state
        .channel_store
        .delete_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    if matches!(access, SessionAccess::Public { .. }) {
        state
            .store
            .delete_public_session_access_for_session(&session_id)
            .await
            .map_err(|_| ApiError::Internal)?;
    }
    state.session_events.publish(SessionEvent::SessionDeleted {
        session_id: session_id.clone(),
    });
    delete_session_objects(&state, &deleted.attachments).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn list_session_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    let messages = state
        .channel_store
        .list_session_messages(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;

    Ok(Json(MessageListResponse { messages }))
}

async fn get_active_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    state
        .channel_store
        .get_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let active_run = state
        .channel_store
        .get_active_run_for_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?
        .map(ChannelActiveRun::from);

    Ok(Json(ActiveRunResponse { active_run }))
}

async fn clear_active_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    state
        .channel_store
        .get_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    clear_persisted_hermes_run(&state, access.user_id(), &channel.id, &session_id).await?;
    state.session_events.publish(SessionEvent::RunCleared {
        session_id: session_id.clone(),
    });

    Ok(StatusCode::NO_CONTENT)
}

async fn append_session_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(payload): Json<AppendMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    let session = state
        .channel_store
        .get_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let role = ChannelMessageRole::parse(&payload.role).map_err(map_channel_error)?;
    reject_reserved_business_tool_public_message(
        &payload.content,
        payload.client_message_key.as_deref(),
    )?;

    if role == ChannelMessageRole::User {
        // 用户消息在 public API 中仍然触发一次 Hub run，但不把 run 详情暴露给前端。
        ensure_managed_hermes_for_user(&state, access.user_id()).await?;
    }

    let message = state
        .channel_store
        .append_session_message(
            access.user_id(),
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
    if let Some(updated_session) = refresh_public_session_retention_from_message(
        &state,
        access.user_id(),
        &channel.id,
        &session_id,
        message.updated_at,
    )
    .await?
    {
        state.session_events.publish(SessionEvent::SessionUpdated {
            session: updated_session,
        });
    }

    if role == ChannelMessageRole::User {
        let run = state
            .channel_store
            .create_channel_run(
                access.user_id(),
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

        if !session.is_home && session.title.is_none() && !message.content.trim().is_empty() {
            let title = model_generated_title(&state, access.user_id(), &message.content).await;
            let updated_session = state
                .channel_store
                .update_session_title(access.user_id(), &channel.id, &session_id, title)
                .await
                .map_err(map_channel_error)?;
            state.session_events.publish(SessionEvent::SessionUpdated {
                session: updated_session,
            });
        }
    }

    Ok((StatusCode::CREATED, Json(MessageResponse { message })))
}

async fn create_channel_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(payload): Json<CreateRunRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    let session = state
        .channel_store
        .get_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    reject_reserved_business_tool_public_message(
        &payload.content,
        payload.client_message_key.as_deref(),
    )?;

    // 兼容旧前端 client 的 run 创建入口；公开 URL 不再带 channel，但仍由后端统一创建用户消息和 Hub run。
    ensure_managed_hermes_for_user(&state, access.user_id()).await?;
    let message = state
        .channel_store
        .append_session_message(
            access.user_id(),
            &channel.id,
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
    if let Some(updated_session) = refresh_public_session_retention_from_message(
        &state,
        access.user_id(),
        &channel.id,
        &session_id,
        message.updated_at,
    )
    .await?
    {
        state.session_events.publish(SessionEvent::SessionUpdated {
            session: updated_session,
        });
    }

    let run = state
        .channel_store
        .create_channel_run(
            access.user_id(),
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

    if !session.is_home && session.title.is_none() && !message.content.trim().is_empty() {
        let title = model_generated_title(&state, access.user_id(), &message.content).await;
        let updated_session = state
            .channel_store
            .update_session_title(access.user_id(), &channel.id, &session_id, title)
            .await
            .map_err(map_channel_error)?;
        state.session_events.publish(SessionEvent::SessionUpdated {
            session: updated_session,
        });
    }

    Ok((
        StatusCode::CREATED,
        Json(RunCreateResponse { message, run }),
    ))
}

async fn update_session_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((session_id, message_id)): Path<(String, String)>,
    Json(payload): Json<UpdateMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    // 前端会把执行步骤写成可更新的 assistant 消息；这里只保留消息编辑，不暴露 channel。
    let existing_message = state
        .channel_store
        .list_session_messages(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?
        .into_iter()
        .find(|message| message.id == message_id)
        .ok_or(ApiError::NotFound("message not found"))?;
    if is_business_tool_request_message(&existing_message.content)
        || is_business_tool_request_message(&payload.content)
    {
        return Err(ApiError::BadRequest(
            "business tool request messages are immutable",
        ));
    }
    let message = state
        .channel_store
        .update_session_message(
            access.user_id(),
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
    if let Some(updated_session) = refresh_public_session_retention_from_message(
        &state,
        access.user_id(),
        &channel.id,
        &session_id,
        message.updated_at,
    )
    .await?
    {
        state.session_events.publish(SessionEvent::SessionUpdated {
            session: updated_session,
        });
    }

    Ok(Json(MessageResponse { message }))
}

async fn upload_attachments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    multipart: Multipart,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    let attachments = upload_session_attachments(
        &state,
        access.user_id(),
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
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    // 先订阅事件，再读取快照，避免读取快照期间提交的新消息丢掉 live 事件。
    let receiver = state.session_events.subscribe();
    let session = state
        .channel_store
        .get_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let snapshot_session = public_session_summary_for_access(&state, &access, session).await?;

    // SSE 首包带完整快照，浏览器只需要订阅这一条 public 事件流。
    let messages = state
        .channel_store
        .list_session_messages(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let active_run = state
        .channel_store
        .get_active_run_for_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?
        .map(ChannelActiveRun::from);

    let snapshot = json!({
        "type": "messages_snapshot",
        "messages": messages,
        "active_run": active_run,
        "session": snapshot_session,
    });
    let snapshot_stream =
        stream::once(async move { sse_json_event("messages_snapshot", &snapshot) });
    let live_stream = session_live_event_stream(
        state.clone(),
        receiver,
        session_id,
        matches!(access, SessionAccess::Public { .. }),
    );

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

async fn stop_active_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    state
        .channel_store
        .get_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    stop_active_run_for_session(&state, access.user_id(), &channel.id, &session_id).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn generate_session_title(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(payload): Json<GenerateTitleRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    state
        .channel_store
        .get_session(access.user_id(), &channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;

    let title = model_generated_title(&state, access.user_id(), &payload.prompt).await;
    let session = state
        .channel_store
        .update_session_title(access.user_id(), &channel.id, &session_id, title)
        .await
        .map_err(map_channel_error)?;
    state.session_events.publish(SessionEvent::SessionUpdated {
        session: session.clone(),
    });
    let session = public_session_summary_for_access(&state, &access, session).await?;

    Ok(Json(SessionResponse {
        session,
        public_token: None,
    }))
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

async fn public_session_summary_for_access(
    state: &AppState,
    access: &SessionAccess,
    session: ChannelSession,
) -> Result<PublicSessionSummary, ApiError> {
    let recycle_at = if matches!(access, SessionAccess::Public { .. }) && !session.is_home {
        Some(public_session_recycle_at(state, &session).await?)
    } else {
        None
    };
    Ok(public_session_summary(session, recycle_at))
}

async fn public_session_summary_for_create(
    state: &AppState,
    is_public_session: bool,
    session: ChannelSession,
) -> Result<PublicSessionSummary, ApiError> {
    let recycle_at = if is_public_session && !session.is_home {
        Some(public_session_recycle_at(state, &session).await?)
    } else {
        None
    };
    Ok(public_session_summary(session, recycle_at))
}

fn public_session_summary(
    session: ChannelSession,
    recycle_at: Option<u64>,
) -> PublicSessionSummary {
    PublicSessionSummary {
        id: session.id,
        title: session.title,
        is_home: session.is_home,
        deletable: session.deletable,
        created_at: session.created_at,
        updated_at: session.updated_at,
        recycle_at,
    }
}

pub(crate) async fn public_session_recycle_at(
    state: &AppState,
    session: &ChannelSession,
) -> Result<u64, ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    public_session_recycle_at_with_retention(
        state,
        session,
        settings.public_platform.temporary_session_retention_hours,
    )
    .await
}

async fn public_session_recycle_at_with_retention(
    state: &AppState,
    session: &ChannelSession,
    retention_hours: u32,
) -> Result<u64, ApiError> {
    let latest_message_at = state
        .channel_store
        .latest_session_message_updated_at(&session.id)
        .await
        .map_err(map_channel_error)?;
    let anchor_at = latest_message_at.unwrap_or(session.created_at);
    Ok(public_session_recycle_at_from_anchor(
        anchor_at,
        retention_hours,
    ))
}

fn public_session_recycle_at_from_anchor(anchor_at: u64, retention_hours: u32) -> u64 {
    anchor_at.saturating_add(u64::from(retention_hours) * 60 * 60)
}

pub(crate) async fn refresh_public_session_retention_from_message(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    session_id: &str,
    message_updated_at: u64,
) -> Result<Option<ChannelSession>, ApiError> {
    let Some(public_user_id) = state
        .store
        .public_platform_user_id()
        .await
        .map_err(|_| ApiError::Internal)?
    else {
        return Ok(None);
    };
    if public_user_id != user_id {
        return Ok(None);
    }
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    let recycle_at = public_session_recycle_at_from_anchor(
        message_updated_at,
        settings.public_platform.temporary_session_retention_hours,
    );
    state
        .store
        .refresh_public_session_access_for_session(session_id, recycle_at)
        .await
        .map_err(|_| ApiError::Internal)?;

    // 推一个 session_updated 事件给公共页面，让标题栏回收时间不必等手动刷新。
    let session = state
        .channel_store
        .get_session(user_id, channel_id, session_id)
        .await
        .map_err(map_channel_error)?;
    Ok(Some(session))
}

pub async fn cleanup_expired_hidden_sessions(state: &AppState) -> Result<(), ApiError> {
    let minimum_idle_timeout = hidden_session_idle_timeout_floor(state).await?;
    let cutoff = unix_now().saturating_sub(minimum_idle_timeout);
    let candidates = state
        .channel_store
        .hidden_sessions_older_than(cutoff)
        .await
        .map_err(map_channel_error)?;
    for candidate in candidates {
        if let Err(error) = cleanup_hidden_session_candidate(state, &candidate).await {
            tracing::warn!(
                ?error,
                session_id = %candidate.session_id,
                "hidden session cleanup failed"
            );
        }
    }

    Ok(())
}

async fn hidden_session_idle_timeout_floor(state: &AppState) -> Result<u64, ApiError> {
    let mut floor = DEFAULT_HIDDEN_SESSION_IDLE_TIMEOUT_SECONDS;
    let apps = state
        .store
        .list_integration_apps()
        .await
        .map_err(|_| ApiError::Internal)?;
    for app in apps {
        floor = floor.min(app.hidden_session_idle_timeout_seconds);
    }
    Ok(floor)
}

async fn cleanup_hidden_session_candidate(
    state: &AppState,
    candidate: &HiddenSessionCleanupCandidate,
) -> Result<(), ApiError> {
    let session = match state
        .channel_store
        .get_session(
            &candidate.user_id,
            &candidate.channel_id,
            &candidate.session_id,
        )
        .await
    {
        Ok(session) => session,
        Err(_) => return Ok(()),
    };
    let channel = match state
        .channel_store
        .get_channel(&candidate.user_id, &candidate.channel_id)
        .await
    {
        Ok(channel) => channel,
        Err(_) => return Ok(()),
    };
    let integration_id = channel
        .name
        .strip_prefix("integration:")
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(integration_id) = integration_id else {
        return Ok(());
    };
    let idle_timeout = state
        .store
        .integration_app_by_integration_id(integration_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .map(|app| app.hidden_session_idle_timeout_seconds)
        .unwrap_or(DEFAULT_HIDDEN_SESSION_IDLE_TIMEOUT_SECONDS);
    if session.updated_at.saturating_add(idle_timeout) > unix_now() {
        return Ok(());
    }
    if let Some(run) = state
        .channel_store
        .get_active_run_for_session(
            &candidate.user_id,
            &candidate.channel_id,
            &candidate.session_id,
        )
        .await
        .map_err(map_channel_error)?
    {
        if !run.status.is_terminal() {
            // 隐藏会话的“空闲销毁”只适用于真正仍在运行的会话；终态 run 不应阻止回收。
            return Ok(());
        }
    }
    delete_managed_cron_jobs_for_session(state, &candidate.user_id, &candidate.session_id).await?;
    let deleted = state
        .channel_store
        .delete_session(
            &candidate.user_id,
            &candidate.channel_id,
            &candidate.session_id,
        )
        .await
        .map_err(map_channel_error)?;
    state.session_events.publish(SessionEvent::SessionDeleted {
        session_id: candidate.session_id.clone(),
    });
    delete_session_objects(state, &deleted.attachments).await?;
    Ok(())
}

async fn session_create_access(
    state: &AppState,
    headers: &HeaderMap,
    authenticated_user: Option<User>,
) -> Result<PublicCreateAccess, ApiError> {
    if let Some(user) = authenticated_user {
        return Ok(PublicCreateAccess {
            owner_user_id: user.id,
            token: None,
            set_cookie: None,
            hidden_from_web: false,
        });
    }
    ensure_public_platform_enabled_for_anonymous(state, false).await?;

    let owner_user_id = public_platform_owner_user_id(state).await?;
    let token = public_session_token_for_request(state, headers, None)
        .await?
        .unwrap_or_else(generate_public_session_token);
    let set_cookie = public_session_cookie_refresh(headers, &token);

    Ok(PublicCreateAccess {
        owner_user_id,
        token: Some(token),
        set_cookie,
        hidden_from_web: false,
    })
}

async fn optional_web_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<User>, ApiError> {
    match current_user(state, headers).await {
        Ok(user) => Ok(Some(user)),
        // `/api/sessions` 支持匿名公共平台，但 Authorization 头属于受保护 API。
        // OAuth bearer 不能在这里被降级成匿名公共会话。
        Err(_) if headers.get(header::AUTHORIZATION).is_some() => Err(ApiError::Unauthorized),
        Err(_) => Ok(None),
    }
}

async fn session_access_requiring_session(
    state: &AppState,
    headers: &HeaderMap,
    session_id: &str,
) -> Result<SessionAccess, ApiError> {
    if let Ok(user) = current_user(state, headers).await {
        return Ok(SessionAccess::Authenticated { user_id: user.id });
    }
    ensure_public_platform_enabled_for_anonymous(state, false).await?;
    if Uuid::parse_str(session_id).is_err() {
        return Err(ApiError::Unauthorized);
    }
    cleanup_expired_public_sessions(state).await?;
    if cleanup_public_session_if_recycled(state, session_id).await? {
        return Err(ApiError::Unauthorized);
    }
    let owner_user_id = public_platform_owner_user_id(state).await?;
    if public_session_is_home(state, &owner_user_id, session_id).await? {
        // 公共平台的主会话只用于 Hermes 内部 home channel，不对匿名访客暴露。
        return Err(ApiError::Unauthorized);
    }
    let token = public_session_token_for_session(state, headers, session_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    Ok(SessionAccess::Public {
        owner_user_id,
        token: Some(token),
    })
}

async fn ensure_channel_for_access(
    state: &AppState,
    access: &SessionAccess,
) -> Result<Option<crate::channel::service::Channel>, ApiError> {
    ensure_public_channel(state, access.user_id())
        .await
        .map(Some)
}

async fn filter_sessions_for_access(
    state: &AppState,
    access: &SessionAccess,
    sessions: Vec<ChannelSession>,
) -> Result<Vec<ChannelSession>, ApiError> {
    let SessionAccess::Public { token, .. } = access else {
        return Ok(sessions);
    };
    let allowed_session_ids = if let Some(token) = token {
        state
            .store
            .public_session_ids_for_token(token)
            .await
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };
    Ok(sessions
        .into_iter()
        .filter(|session| !session.is_home && allowed_session_ids.contains(&session.id))
        .collect())
}

async fn grant_public_session_path_access(
    state: &AppState,
    current_token: Option<String>,
    session_id: &str,
) -> Result<Option<String>, ApiError> {
    if Uuid::parse_str(session_id).is_err() {
        return Ok(current_token);
    }
    let owner_user_id = public_platform_owner_user_id(state).await?;
    let channel = ensure_public_channel(state, &owner_user_id).await?;
    let session = match state
        .channel_store
        .get_session(&owner_user_id, &channel.id, session_id)
        .await
    {
        Ok(session) => session,
        Err(_) => return Ok(current_token),
    };
    if session.is_home {
        return Ok(current_token);
    }
    if cleanup_public_session_if_recycled(state, session_id).await? {
        return Ok(current_token);
    }
    let token = current_token.unwrap_or_else(generate_public_session_token);
    let expires_at = public_session_recycle_at(state, &session).await?;
    state
        .store
        .grant_public_session_access_until(&token, session_id, expires_at)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Some(token))
}

async fn public_session_is_home(
    state: &AppState,
    owner_user_id: &str,
    session_id: &str,
) -> Result<bool, ApiError> {
    let context = match state.channel_store.session_context(session_id).await {
        Ok(context) => context,
        Err(ChannelStoreError::ChannelNotFound) => return Ok(false),
        Err(error) => return Err(map_channel_error(error)),
    };
    if context.user_id != owner_user_id {
        return Ok(false);
    }
    let session = state
        .channel_store
        .get_session(&context.user_id, &context.channel_id, session_id)
        .await
        .map_err(map_channel_error)?;
    Ok(session.is_home)
}

pub(crate) async fn public_session_token_for_request(
    state: &AppState,
    headers: &HeaderMap,
    requested_session_id: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let cookie_token = public_session_cookie_token(headers);
    let header_token = public_session_header_token(headers);
    if let Some(session_id) =
        requested_session_id.and_then(|value| normalize_public_session_token(Some(value)))
    {
        if let Some(token) = cookie_token.as_deref() {
            if public_token_can_access_session(state, token, &session_id).await? {
                return Ok(cookie_token);
            }
        }
        if let Some(token) = header_token.as_deref() {
            if public_token_can_access_session(state, token, &session_id).await? {
                return Ok(header_token);
            }
        }
    }
    if let Some(token) = cookie_token.as_deref() {
        if public_token_has_sessions(state, token).await? {
            return Ok(cookie_token);
        }
    }
    if let Some(token) = header_token.as_deref() {
        if public_token_has_sessions(state, token).await? {
            return Ok(header_token);
        }
    }
    Ok(None)
}

pub(crate) async fn public_session_token_for_session(
    state: &AppState,
    headers: &HeaderMap,
    session_id: &str,
) -> Result<Option<String>, ApiError> {
    let cookie_token = public_session_cookie_token(headers);
    if let Some(token) = cookie_token.as_deref() {
        if public_token_can_access_session(state, token, session_id).await? {
            return Ok(cookie_token);
        }
    }
    let header_token = public_session_header_token(headers);
    if let Some(token) = header_token.as_deref() {
        if public_token_can_access_session(state, token, session_id).await? {
            return Ok(header_token);
        }
    }
    Ok(None)
}

async fn public_token_has_sessions(state: &AppState, token: &str) -> Result<bool, ApiError> {
    Ok(!state
        .store
        .public_session_ids_for_token(token)
        .await
        .map_err(|_| ApiError::Internal)?
        .is_empty())
}

async fn public_token_can_access_session(
    state: &AppState,
    token: &str,
    session_id: &str,
) -> Result<bool, ApiError> {
    state
        .store
        .public_token_can_access_session(token, session_id)
        .await
        .map_err(|_| ApiError::Internal)
}

fn public_session_header_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(PUBLIC_SESSION_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| normalize_public_session_token(Some(value)))
}

fn public_session_cookie_token(headers: &HeaderMap) -> Option<String> {
    normalize_public_session_token(
        cookie_value_from_headers(headers, PUBLIC_SESSION_COOKIE_NAME).as_deref(),
    )
}

fn normalize_public_session_token(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
}

fn public_session_cookie_refresh(headers: &HeaderMap, token: &str) -> Option<String> {
    if public_session_cookie_token(headers).as_deref() == Some(token) {
        return None;
    }
    Some(public_session_cookie(token))
}

fn generate_public_session_token() -> String {
    Uuid::new_v4().to_string()
}

fn public_session_cookie(public_token: &str) -> String {
    format!(
        "{PUBLIC_SESSION_COOKIE_NAME}={public_token}; HttpOnly; SameSite=Lax; Path=/{}",
        secure_cookie_suffix()
    )
}

fn secure_cookie_suffix() -> &'static str {
    if cfg!(debug_assertions) {
        ""
    } else {
        "; Secure"
    }
}

fn session_list_response(
    headers: &HeaderMap,
    sessions: Vec<PublicSessionSummary>,
    public_token: Option<String>,
) -> Result<Response, ApiError> {
    let mut response = Json(SessionListResponse {
        sessions,
        public_token: public_token.clone(),
    })
    .into_response();
    if let Some(token) = public_token {
        if let Some(cookie) = public_session_cookie_refresh(headers, &token) {
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_str(&cookie).map_err(|_| ApiError::Internal)?,
            );
        }
    }
    Ok(response)
}

async fn ensure_public_platform_enabled_for_anonymous(
    state: &AppState,
    is_authenticated: bool,
) -> Result<(), ApiError> {
    if is_authenticated {
        return Ok(());
    }
    let readiness = public_platform::public_hermes_readiness(state).await?;
    if readiness.ready {
        Ok(())
    } else if readiness.configured_enabled {
        Err(ApiError::ServiceUnavailable("public platform is not ready"))
    } else {
        Err(ApiError::NotFound("public platform is disabled"))
    }
}

async fn public_platform_owner_user_id(state: &AppState) -> Result<String, ApiError> {
    public_platform::ensure_public_owner_user_id(state).await
}

async fn ensure_public_channel(
    state: &AppState,
    user_id: &str,
) -> Result<crate::channel::service::Channel, ApiError> {
    match state.store.hermes_instance_for_user(user_id).await {
        Ok(instance) => state
            .channel_store
            .bind_hub_channel_to_instance(user_id, &instance.id)
            .await
            .map_err(map_channel_error),
        Err(crate::session::store::StoreError::InviteNotFound) => state
            .channel_store
            .ensure_hub_channel(user_id)
            .await
            .map_err(map_channel_error),
        Err(_) => Err(ApiError::Internal),
    }
}

fn session_live_event_stream(
    state: AppState,
    receiver: tokio::sync::broadcast::Receiver<SessionEvent>,
    session_id: String,
    include_public_recycle_at: bool,
) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold(receiver, move |mut receiver| {
        let session_id = session_id.clone();
        let state = state.clone();
        async move {
            loop {
                match receiver.recv().await {
                    Ok(event) if event.session_id() == session_id => {
                        let event_name = event.event_name();
                        if include_public_recycle_at {
                            let payload = public_session_event_payload(&state, &event).await;
                            return Some((sse_json_event(event_name, &payload), receiver));
                        }
                        return Some((sse_json_event(event_name, &event), receiver));
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

async fn public_session_event_payload(state: &AppState, event: &SessionEvent) -> Value {
    match event {
        SessionEvent::SessionUpdated { session } => {
            match public_session_recycle_at(state, session).await {
                Ok(recycle_at) => json!({
                    "type": "session_updated",
                    "session": public_session_summary(session.clone(), Some(recycle_at)),
                }),
                Err(error) => {
                    tracing::warn!(error = ?error, session_id = %session.id, "public recycle_at event mapping failed");
                    json!(event)
                }
            }
        }
        _ => json!(event),
    }
}

fn sse_json_event<T: Serialize>(name: &'static str, payload: &T) -> Result<Event, Infallible> {
    let data = serde_json::to_string(payload)
        .unwrap_or_else(|_| json!({"type": "serialization_failed"}).to_string());
    Ok(Event::default().event(name).data(data))
}

pub(crate) fn apply_sse_no_buffer_headers(headers: &mut HeaderMap) {
    // SSE 首包必须尽快穿过代理；no-transform 和 X-Accel-Buffering=no
    // 可以避免代理把空闲流缓存到下一次 keep-alive 才交给浏览器。
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store, no-transform"),
    );
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
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

pub(crate) async fn cleanup_expired_public_sessions(state: &AppState) -> Result<(), ApiError> {
    let expired_session_ids = state
        .store
        .expired_public_session_ids()
        .await
        .map_err(|_| ApiError::Internal)?;
    if expired_session_ids.is_empty() {
        return Ok(());
    }

    let Some(public_user_id) = state
        .store
        .public_platform_user_id()
        .await
        .map_err(|_| ApiError::Internal)?
    else {
        return Ok(());
    };
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    let retention_hours = settings.public_platform.temporary_session_retention_hours;
    let now = unix_now();

    for session_id in expired_session_ids {
        cleanup_public_session_if_recycled_with_retention(
            state,
            &session_id,
            &public_user_id,
            retention_hours,
            now,
        )
        .await?;
    }

    Ok(())
}

pub(crate) async fn cleanup_public_session_if_recycled(
    state: &AppState,
    session_id: &str,
) -> Result<bool, ApiError> {
    let Some(public_user_id) = state
        .store
        .public_platform_user_id()
        .await
        .map_err(|_| ApiError::Internal)?
    else {
        return Ok(false);
    };
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    cleanup_public_session_if_recycled_with_retention(
        state,
        session_id,
        &public_user_id,
        settings.public_platform.temporary_session_retention_hours,
        unix_now(),
    )
    .await
}

pub(crate) async fn force_delete_public_session(
    state: &AppState,
    session_id: &str,
) -> Result<(), ApiError> {
    let Some(public_user_id) = state
        .store
        .public_platform_user_id()
        .await
        .map_err(|_| ApiError::Internal)?
    else {
        return Err(ApiError::NotFound("public session not found"));
    };
    let context = state
        .channel_store
        .session_context(session_id)
        .await
        .map_err(map_channel_error)?;
    if context.user_id != public_user_id {
        return Err(ApiError::NotFound("public session not found"));
    }
    delete_recycled_public_session(state, &context, session_id).await
}

async fn cleanup_public_session_if_recycled_with_retention(
    state: &AppState,
    session_id: &str,
    public_user_id: &str,
    retention_hours: u32,
    now: u64,
) -> Result<bool, ApiError> {
    let context = match state.channel_store.session_context(session_id).await {
        Ok(context) => context,
        Err(ChannelStoreError::ChannelNotFound) => {
            state
                .store
                .delete_public_session_access_for_session(session_id)
                .await
                .map_err(|_| ApiError::Internal)?;
            return Ok(true);
        }
        Err(error) => return Err(map_channel_error(error)),
    };
    if context.user_id != public_user_id {
        return Ok(false);
    }
    let session = state
        .channel_store
        .get_session(&context.user_id, &context.channel_id, session_id)
        .await
        .map_err(map_channel_error)?;
    if session.is_home {
        return Ok(false);
    }
    let recycle_at =
        public_session_recycle_at_with_retention(state, &session, retention_hours).await?;
    if recycle_at > now {
        state
            .store
            .refresh_public_session_access_for_session(session_id, recycle_at)
            .await
            .map_err(|_| ApiError::Internal)?;
        return Ok(false);
    }

    delete_recycled_public_session(state, &context, session_id).await?;
    Ok(true)
}

async fn delete_recycled_public_session(
    state: &AppState,
    context: &crate::channel::service::ChannelSessionContext,
    session_id: &str,
) -> Result<(), ApiError> {
    // 公共会话过期是真删除：停止仍在跑的 run，清理 cron 关联，再删 session 和对象。
    let _ = stop_active_run_for_session(state, &context.user_id, &context.channel_id, session_id)
        .await?;
    delete_managed_cron_jobs_for_session(state, &context.user_id, session_id).await?;
    let attachments = state
        .channel_store
        .list_session_attachments(session_id)
        .await
        .map_err(map_channel_error)?;
    delete_session_objects(state, &attachments).await?;
    let deleted = match state
        .channel_store
        .delete_session(&context.user_id, &context.channel_id, session_id)
        .await
    {
        Ok(deleted) => deleted,
        Err(ChannelStoreError::ChannelNotFound) => {
            state
                .store
                .delete_public_session_access_for_session(session_id)
                .await
                .map_err(|_| ApiError::Internal)?;
            return Ok(());
        }
        Err(error) => return Err(map_channel_error(error)),
    };
    state.session_events.publish(SessionEvent::SessionDeleted {
        session_id: deleted.session.id.clone(),
    });
    state
        .store
        .delete_public_session_access_for_session(session_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
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
