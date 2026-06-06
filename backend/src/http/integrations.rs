use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
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
use std::{convert::Infallible, time::Duration};

use crate::{
    channel::{
        events::SessionEvent,
        service::{
            Channel, ChannelActiveRun, ChannelAttachment, ChannelAttachmentDirection,
            ChannelMessage, ChannelMessageRole, ChannelRunStatus, ChannelSession,
            ChannelSessionKind, ChannelStoreError,
        },
    },
    http::{
        attachments::upload_session_attachments,
        auth::{current_bearer_auth_context, AuthContext},
        sessions::{apply_sse_no_buffer_headers, delete_managed_cron_jobs_for_session},
        workspace::ensure_managed_hermes_for_user,
        ApiError,
    },
    session::store::SessionPurpose,
    storage::ObjectStorageError,
    title_generation::model_generated_title,
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/integrations/sessions",
            get(list_sessions).post(create_session),
        )
        .route(
            "/api/integrations/sessions/{session_id}",
            delete(delete_session),
        )
        .route(
            "/api/integrations/sessions/{session_id}/messages",
            post(append_session_message),
        )
        .route(
            "/api/integrations/sessions/{session_id}/messages/{message_id}",
            put(update_session_message),
        )
        .route(
            "/api/integrations/sessions/{session_id}/attachments",
            post(upload_attachments).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/integrations/sessions/{session_id}/events",
            get(session_events),
        )
        .route(
            "/api/integrations/sessions/{session_id}/stop",
            post(stop_active_run),
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
struct UpdateMessageRequest {
    content: String,
    attachments: Option<Value>,
}

#[derive(Serialize)]
struct IntegrationSessionSummary {
    id: String,
    title: Option<String>,
    is_home: bool,
    deletable: bool,
    hidden_from_web: bool,
    created_at: u64,
    updated_at: u64,
}

#[derive(Serialize)]
struct SessionListResponse {
    sessions: Vec<IntegrationSessionSummary>,
}

#[derive(Serialize)]
struct SessionResponse {
    session: IntegrationSessionSummary,
}

#[derive(Serialize)]
struct MessageResponse {
    message: ChannelMessage,
}

#[derive(Serialize)]
struct AttachmentListResponse {
    attachments: Vec<ChannelAttachment>,
}

struct IntegrationAccess {
    auth: AuthContext,
    channel: Channel,
    integration_id: String,
}

impl IntegrationAccess {
    fn user_id(&self) -> &str {
        &self.auth.user.id
    }

    fn integration_id(&self) -> &str {
        &self.integration_id
    }
}

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let access = integration_access(&state, &headers).await?;
    let sessions = state
        .channel_store
        .list_sessions(access.user_id(), &access.channel.id)
        .await
        .map_err(map_channel_error)?;
    let sessions = sessions
        .into_iter()
        .filter(|session| !session.is_home)
        .map(integration_session_summary)
        .collect();

    Ok(Json(SessionListResponse { sessions }))
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let access = integration_access(&state, &headers).await?;
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
            access.user_id(),
            &access.channel.id,
            kind,
            payload.title,
            settings.max_sessions_per_user,
            true,
        )
        .await
        .map_err(map_channel_error)?;

    Ok((
        StatusCode::CREATED,
        Json(SessionResponse {
            session: integration_session_summary(session),
        }),
    ))
}

async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let access = integration_access(&state, &headers).await?;
    state
        .channel_store
        .get_session(access.user_id(), &access.channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let _ = stop_active_run_for_session(&state, access.user_id(), &access.channel.id, &session_id)
        .await?;
    delete_managed_cron_jobs_for_session(&state, access.user_id(), &session_id).await?;
    let deleted = state
        .channel_store
        .delete_session(access.user_id(), &access.channel.id, &session_id)
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
    let mut access = integration_access(&state, &headers).await?;
    let session = state
        .channel_store
        .get_session(access.user_id(), &access.channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let role = ChannelMessageRole::parse(&payload.role).map_err(map_channel_error)?;

    if role == ChannelMessageRole::User {
        let instance = ensure_managed_hermes_for_user(&state, access.user_id()).await?;
        access.channel = state
            .channel_store
            .bind_integration_channel_to_instance(
                access.user_id(),
                access.integration_id(),
                &instance.id,
            )
            .await
            .map_err(map_channel_error)?;
    }

    let message = state
        .channel_store
        .append_session_message(
            access.user_id(),
            &access.channel.id,
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
                access.user_id(),
                &access.channel.id,
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
                .update_session_title(access.user_id(), &access.channel.id, &session_id, title)
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
    let access = integration_access(&state, &headers).await?;
    let message = state
        .channel_store
        .update_session_message(
            access.user_id(),
            &access.channel.id,
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
    let access = integration_access(&state, &headers).await?;
    let attachments = upload_session_attachments(
        &state,
        access.user_id(),
        &access.channel.id,
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
    let access = integration_access(&state, &headers).await?;
    let receiver = state.session_events.subscribe();
    let session = state
        .channel_store
        .get_session(access.user_id(), &access.channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let messages = state
        .channel_store
        .list_session_messages(access.user_id(), &access.channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    let active_run = state
        .channel_store
        .get_active_run_for_session(access.user_id(), &access.channel.id, &session_id)
        .await
        .map_err(map_channel_error)?
        .map(ChannelActiveRun::from);
    let snapshot = json!({
        "type": "messages_snapshot",
        "messages": messages,
        "active_run": active_run,
        "session": integration_session_summary(session),
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

async fn stop_active_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let access = integration_access(&state, &headers).await?;
    state
        .channel_store
        .get_session(access.user_id(), &access.channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;
    stop_active_run_for_session(&state, access.user_id(), &access.channel.id, &session_id).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn integration_access(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<IntegrationAccess, ApiError> {
    let auth = current_bearer_auth_context(state, headers).await?;
    if auth.session_purpose != SessionPurpose::OAuth {
        return Err(ApiError::Unauthorized);
    }
    let integration_id = auth.integration_id.clone().ok_or(ApiError::Unauthorized)?;
    let channel = state
        .channel_store
        .ensure_integration_channel(&auth.user.id, &integration_id)
        .await
        .map_err(map_channel_error)?;

    Ok(IntegrationAccess {
        auth,
        channel,
        integration_id,
    })
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

fn integration_session_summary(session: ChannelSession) -> IntegrationSessionSummary {
    IntegrationSessionSummary {
        id: session.id,
        title: session.title,
        is_home: session.is_home,
        deletable: session.deletable,
        hidden_from_web: session.hidden_from_web,
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
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
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return None,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        }
    })
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

fn map_channel_error(error: ChannelStoreError) -> ApiError {
    match error {
        ChannelStoreError::InvalidSessionKind
        | ChannelStoreError::InvalidMessageRole
        | ChannelStoreError::InvalidRunStatus
        | ChannelStoreError::InvalidAttachment
        | ChannelStoreError::InvalidIntegrationId => {
            ApiError::BadRequest("invalid integration request")
        }
        ChannelStoreError::ChannelNotFound
        | ChannelStoreError::AttachmentNotFound
        | ChannelStoreError::RunNotFound => ApiError::NotFound("channel resource not found"),
        ChannelStoreError::SessionLimitExceeded {
            max_sessions_per_user,
        } => ApiError::SessionLimitExceeded {
            max_sessions_per_user,
        },
        ChannelStoreError::ProtectedSession => ApiError::Forbidden,
        ChannelStoreError::LockFailed | ChannelStoreError::DatabaseFailed => ApiError::Internal,
    }
}
