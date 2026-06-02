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
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use crate::{
    channel::{
        events::SessionEvent,
        service::{
            ChannelActiveRun, ChannelAttachment, ChannelAttachmentDirection, ChannelMessage,
            ChannelMessageRole, ChannelRunStatus, ChannelSession, ChannelSessionKind,
            ChannelStoreError,
        },
    },
    http::{
        attachments::{map_channel_error, upload_session_attachments},
        auth::{cookie_value_from_headers, current_user},
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
            post(upload_attachments).layer(DefaultBodyLimit::disable()),
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
struct AttachmentListResponse {
    attachments: Vec<ChannelAttachment>,
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
    let is_authenticated = current_user(&state, &headers).await.is_ok();
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
        if public_token.is_none() {
            return session_list_response(&headers, vec![], None);
        }
        if let Some(token) = &public_token {
            if state
                .store
                .public_session_ids_for_token(token)
                .await
                .map_err(|_| ApiError::Internal)?
                .is_empty()
            {
                return session_list_response(&headers, vec![], public_token.clone());
            }
        }
    }
    let access = if is_authenticated {
        session_access(&state, &headers).await?
    } else {
        SessionAccess::Public {
            owner_user_id: public_platform_owner_user_id(&state).await?,
            token: public_token.clone(),
        }
    };
    if let SessionAccess::Public {
        token: Some(token), ..
    } = &access
    {
        if state
            .store
            .public_session_ids_for_token(token)
            .await
            .map_err(|_| ApiError::Internal)?
            .is_empty()
        {
            return session_list_response(&headers, vec![], public_token.clone());
        }
        ensure_managed_hermes_for_user(&state, access.user_id()).await?;
    }
    let Some(channel) = ensure_channel_for_access(&state, &access).await? else {
        return session_list_response(&headers, vec![], public_token.clone());
    };
    let sessions = state
        .channel_store
        .list_sessions(access.user_id(), &channel.id)
        .await
        .map_err(map_channel_error)?;
    let sessions = filter_sessions_for_access(&state, &access, sessions).await?;
    let mut summaries = Vec::with_capacity(sessions.len());
    for session in sessions {
        if matches!(access, SessionAccess::Public { .. })
            && cleanup_public_session_if_recycled(&state, &session.id).await?
        {
            continue;
        }
        summaries.push(public_session_summary_for_access(&state, &access, session).await?);
    }
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
    ensure_public_platform_enabled_for_anonymous(
        &state,
        current_user(&state, &headers).await.is_ok(),
    )
    .await?;
    let access = session_create_access(&state, &headers).await?;
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
            .create_session(&access.owner_user_id, &channel.id, kind, payload.title)
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

        if session.title.is_none() && !message.content.trim().is_empty() {
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

async fn update_session_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((session_id, message_id)): Path<(String, String)>,
    Json(payload): Json<UpdateMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let access = session_access_requiring_session(&state, &headers, &session_id).await?;
    let channel = ensure_public_channel(&state, access.user_id()).await?;
    // 前端会把执行步骤写成可更新的 assistant 消息；这里只保留消息编辑，不暴露 channel。
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
    let recycle_at = if matches!(access, SessionAccess::Public { .. }) {
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
    let recycle_at = if is_public_session {
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

async fn session_access(state: &AppState, headers: &HeaderMap) -> Result<SessionAccess, ApiError> {
    if let Ok(user) = current_user(state, headers).await {
        return Ok(SessionAccess::Authenticated { user_id: user.id });
    }
    ensure_public_platform_enabled_for_anonymous(state, false).await?;

    let owner_user_id = public_platform_owner_user_id(state).await?;

    Ok(SessionAccess::Public {
        owner_user_id,
        token: public_session_token_for_request(state, headers, None).await?,
    })
}

async fn session_create_access(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<PublicCreateAccess, ApiError> {
    if let Ok(user) = current_user(state, headers).await {
        return Ok(PublicCreateAccess {
            owner_user_id: user.id,
            token: None,
            set_cookie: None,
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
    })
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
    let token = public_session_token_for_session(state, headers, session_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    Ok(SessionAccess::Public {
        owner_user_id: public_platform_owner_user_id(state).await?,
        token: Some(token),
    })
}

async fn ensure_channel_for_access(
    state: &AppState,
    access: &SessionAccess,
) -> Result<Option<crate::channel::service::Channel>, ApiError> {
    if matches!(access, SessionAccess::Public { token: None, .. }) {
        return Ok(None);
    }
    ensure_public_channel(state, access.user_id())
        .await
        .map(Some)
}

async fn filter_sessions_for_access(
    state: &AppState,
    access: &SessionAccess,
    sessions: Vec<ChannelSession>,
) -> Result<Vec<ChannelSession>, ApiError> {
    let SessionAccess::Public {
        token: Some(token), ..
    } = access
    else {
        return Ok(sessions);
    };
    let allowed_session_ids = state
        .store
        .public_session_ids_for_token(token)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .collect::<HashSet<_>>();
    Ok(sessions
        .into_iter()
        .filter(|session| allowed_session_ids.contains(&session.id))
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
    format!("{PUBLIC_SESSION_COOKIE_NAME}={public_token}; HttpOnly; SameSite=Lax; Path=/")
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
    state
        .channel_store
        .ensure_hub_channel(user_id)
        .await
        .map_err(map_channel_error)
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
