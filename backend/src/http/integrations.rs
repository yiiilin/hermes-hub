use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{delete, get, post, put},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures_util::{stream, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    convert::Infallible,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    channel::{
        events::{BusinessToolRequestEvent, BusinessToolRequestStatus, SessionEvent},
        service::{
            AppendSessionMessageOutcome, Channel, ChannelActiveRun, ChannelAttachment,
            ChannelAttachmentDirection, ChannelMessage, ChannelMessageRole, ChannelRunStatus,
            ChannelSession, ChannelSessionKind, ChannelStoreError,
        },
    },
    http::{
        attachments::upload_session_attachments,
        auth::{current_bearer_auth_context, AuthContext},
        sessions::{apply_sse_no_buffer_headers, delete_managed_cron_jobs_for_session},
        workspace::ensure_managed_hermes_for_user,
        ApiError,
    },
    session::store::{
        IncomingIntegrationToolDefinition, IntegrationApp, IntegrationToolDefinition,
        SessionPurpose,
    },
    storage::ObjectStorageError,
    title_generation::model_generated_title,
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/integrations/apps/self/tools",
            get(list_current_integration_tools).put(replace_current_integration_tools),
        )
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
        .route(
            "/api/integrations/sessions/{session_id}/business-tool-requests/{request_id}/result",
            post(submit_business_tool_request_result),
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

#[derive(Deserialize)]
struct ReplaceIntegrationToolsRequest {
    tools: Vec<IncomingIntegrationToolDefinition>,
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

#[derive(Serialize)]
struct IntegrationToolsResponse {
    tools: Vec<IntegrationToolDefinition>,
}

const BUSINESS_TOOL_REQUEST_MARKER: &str = "<!-- hermes-hub:business-tool-request:v1 -->";

pub(crate) fn is_business_tool_request_message(content: &str) -> bool {
    content
        .trim_start()
        .starts_with(BUSINESS_TOOL_REQUEST_MARKER)
}

pub(crate) fn business_tool_request_client_key(request_id: &str) -> String {
    stable_business_tool_client_key("business-tool-request", request_id)
}

pub(crate) fn business_tool_request_result_client_key(request_id: &str) -> String {
    stable_business_tool_client_key("business-tool-result", request_id)
}

pub(crate) fn is_reserved_business_tool_client_key(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("business-tool-request:") || value.starts_with("business-tool-result:")
}

fn stable_business_tool_client_key(prefix: &str, request_id: &str) -> String {
    // 业务工具请求和回调都依赖稳定 key 做幂等；用哈希避免超长 request_id 被截断后碰撞。
    let digest = Sha256::digest(request_id.as_bytes());
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{prefix}:{digest}")
}

#[derive(Deserialize, Serialize)]
struct BusinessToolRequestEnvelope {
    request_id: String,
    tool_name: String,
    #[serde(default)]
    arguments: Value,
    timeout_seconds: Option<u64>,
    expires_at: Option<u64>,
}

#[derive(Deserialize)]
struct SubmitBusinessToolRequestResultRequest {
    result: String,
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

async fn list_current_integration_tools(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let app = integration_app_by_basic_auth(&state, &headers).await?;
    let tools = state
        .store
        .list_integration_tools(&app.integration_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(IntegrationToolsResponse { tools }))
}

async fn replace_current_integration_tools(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ReplaceIntegrationToolsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let app = integration_app_by_basic_auth(&state, &headers).await?;
    let tools = state
        .store
        .replace_integration_tools(&app.integration_id, payload.tools)
        .await
        .map_err(map_channel_error_from_store)?;
    Ok(Json(IntegrationToolsResponse { tools }))
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
    if role != ChannelMessageRole::Assistant && is_business_tool_request_message(&payload.content) {
        return Err(ApiError::BadRequest(
            "business tool request requires assistant role",
        ));
    }
    if payload
        .client_message_key
        .as_deref()
        .is_some_and(is_reserved_business_tool_client_key)
    {
        // business-tool-* client key 由 Hub 生成，用于请求/结果幂等；外部系统不能预占。
        return Err(ApiError::BadRequest("reserved client message key"));
    }
    let business_tool_request = if role == ChannelMessageRole::Assistant {
        validate_business_tool_request_message(&state, access.integration_id(), &payload.content)
            .await?
    } else {
        None
    };
    let message_content = business_tool_request
        .as_ref()
        .map(|request| request.normalized_content.clone())
        .unwrap_or_else(|| payload.content.clone());
    let client_message_key = business_tool_request
        .as_ref()
        .map(|request| business_tool_request_client_key(&request.request_id))
        .or(payload.client_message_key);

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

    let append_outcome = state
        .channel_store
        .append_session_message_with_outcome(
            access.user_id(),
            &access.channel.id,
            &session_id,
            role.clone(),
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
    let is_business_tool_request = business_tool_request.is_some();
    if let Some(request) = business_tool_request.as_ref() {
        if created {
            state
                .session_events
                .publish(SessionEvent::BusinessToolRequest {
                    request: business_tool_request_event(
                        &message,
                        access.integration_id(),
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

    let status = if is_business_tool_request && !created {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };

    Ok((status, Json(MessageResponse { message })))
}

async fn update_session_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((session_id, message_id)): Path<(String, String)>,
    Json(payload): Json<UpdateMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let access = integration_access(&state, &headers).await?;
    let existing_message = state
        .channel_store
        .list_session_messages(access.user_id(), &access.channel.id, &session_id)
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
    let business_tool_requests =
        session_business_tool_requests(&messages, access.integration_id())?;
    let snapshot = json!({
        "type": "messages_snapshot",
        "messages": messages,
        "active_run": active_run,
        "session": integration_session_summary(session),
        "business_tool_requests": business_tool_requests,
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

fn session_business_tool_requests(
    messages: &[ChannelMessage],
    integration_id: &str,
) -> Result<Vec<SessionEvent>, ApiError> {
    let mut business_tool_requests = Vec::new();
    let now = unix_now();
    for message in messages {
        if message.role != ChannelMessageRole::Assistant
            || !is_business_tool_request_message(&message.content)
        {
            continue;
        }
        let Some(envelope) = (match parse_business_tool_request_message(&message.content) {
            Ok(envelope) => envelope,
            Err(ApiError::BadRequest(_)) => continue,
            Err(error) => return Err(error),
        }) else {
            continue;
        };
        let Ok(request) = validate_stored_business_tool_request(message.clone(), envelope) else {
            continue;
        };
        let result_client_key = business_tool_request_result_client_key(&request.request_id);
        let result_message = messages.iter().find(|candidate| {
            candidate.client_message_key.as_deref() == Some(result_client_key.as_str())
        });
        let (status, result_message_id) = if let Some(result_message) = result_message {
            (
                BusinessToolRequestStatus::Completed,
                Some(result_message.id.clone()),
            )
        } else if now >= request.expires_at {
            (BusinessToolRequestStatus::Expired, None)
        } else {
            (BusinessToolRequestStatus::Pending, None)
        };
        business_tool_requests.push(SessionEvent::BusinessToolRequest {
            request: business_tool_request_event(
                message,
                integration_id,
                &request,
                status,
                result_message_id,
            ),
        });
    }
    Ok(business_tool_requests)
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

async fn submit_business_tool_request_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((session_id, request_id)): Path<(String, String)>,
    Json(payload): Json<SubmitBusinessToolRequestResultRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let access = integration_access(&state, &headers).await?;
    state
        .channel_store
        .get_session(access.user_id(), &access.channel.id, &session_id)
        .await
        .map_err(map_channel_error)?;

    let (request_message, request) = find_business_tool_request_message(
        &state,
        access.user_id(),
        &access.channel.id,
        &session_id,
        &request_id,
    )
    .await?
    .ok_or(ApiError::NotFound("business tool request not found"))?;
    let result_client_key = business_tool_request_result_client_key(&request_id);
    if let Some(existing_message) = state
        .channel_store
        .find_session_message_by_client_key(&session_id, &result_client_key)
        .await
        .map_err(map_channel_error)?
    {
        return Ok((
            StatusCode::OK,
            Json(MessageResponse {
                message: existing_message,
            }),
        ));
    }
    if unix_now() >= request.expires_at {
        return Err(ApiError::Gone("business tool request expired"));
    }

    let result_outcome = state
        .channel_store
        .append_session_message_with_outcome_before_deadline(
            access.user_id(),
            &access.channel.id,
            &session_id,
            ChannelMessageRole::Assistant,
            Some(result_client_key),
            payload.result,
            json!([]),
            request.expires_at,
        )
        .await
        .map_err(map_channel_error)?;
    let Some(result_outcome) = result_outcome else {
        return Err(ApiError::Gone("business tool request expired"));
    };
    let (result_message, created) = match result_outcome {
        AppendSessionMessageOutcome::Created(message) => (message, true),
        AppendSessionMessageOutcome::Existing(message) => (message, false),
    };
    if created {
        state.session_events.publish(SessionEvent::MessageCreated {
            message: result_message.clone(),
        });
        state
            .session_events
            .publish(SessionEvent::BusinessToolRequest {
                request: business_tool_request_event(
                    &request_message,
                    access.integration_id(),
                    &request,
                    BusinessToolRequestStatus::Completed,
                    Some(result_message.id.clone()),
                ),
            });
    }

    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(MessageResponse {
            message: result_message,
        }),
    ))
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedBusinessToolRequest {
    pub(crate) request_id: String,
    pub(crate) tool_name: String,
    pub(crate) arguments: Value,
    pub(crate) timeout_seconds: u64,
    pub(crate) expires_at: u64,
    pub(crate) normalized_content: String,
}

pub(crate) async fn validate_business_tool_request_message(
    state: &AppState,
    integration_id: &str,
    content: &str,
) -> Result<Option<ValidatedBusinessToolRequest>, ApiError> {
    let Some(envelope) = parse_business_tool_request_message(content)? else {
        return Ok(None);
    };

    let app = state
        .store
        .integration_app_by_integration_id(integration_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or(ApiError::NotFound("integration app not found"))?;
    if !app.enabled {
        return Err(ApiError::NotFound("integration app is not enabled"));
    }

    let tool_names = state
        .store
        .list_integration_tools(&app.integration_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|tool| tool.name)
        .collect::<HashSet<_>>();
    let request_id = envelope.request_id.trim().to_string();
    let tool_name = envelope.tool_name.trim().to_string();
    if !tool_names.contains(tool_name.as_str()) {
        return Err(ApiError::BadRequest("invalid business tool request"));
    }

    let timeout_seconds = effective_business_tool_timeout_seconds(
        envelope.timeout_seconds,
        app.default_tool_timeout_seconds,
        app.max_tool_timeout_seconds,
    )?;
    let expires_at = unix_now().saturating_add(timeout_seconds);
    let normalized_content = render_business_tool_request_content(
        &request_id,
        &tool_name,
        envelope.arguments.clone(),
        timeout_seconds,
        expires_at,
    )?;

    Ok(Some(ValidatedBusinessToolRequest {
        request_id,
        tool_name,
        arguments: envelope.arguments,
        timeout_seconds,
        expires_at,
        normalized_content,
    }))
}

async fn find_business_tool_request_message(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    session_id: &str,
    request_id: &str,
) -> Result<Option<(ChannelMessage, ValidatedBusinessToolRequest)>, ApiError> {
    let request_client_key = business_tool_request_client_key(request_id);
    if let Some(message) = state
        .channel_store
        .find_session_message_by_client_key(session_id, &request_client_key)
        .await
        .map_err(map_channel_error)?
    {
        if let Some(envelope) = parse_business_tool_request_message(&message.content)? {
            if message.role == ChannelMessageRole::Assistant && envelope.request_id == request_id {
                return Ok(Some((
                    message.clone(),
                    validate_stored_business_tool_request(message, envelope)?,
                )));
            }
        }
    }

    let messages = state
        .channel_store
        .list_session_messages(user_id, channel_id, session_id)
        .await
        .map_err(map_channel_error)?;
    for message in messages {
        if message.role != ChannelMessageRole::Assistant {
            continue;
        }
        if !is_business_tool_request_message(&message.content) {
            continue;
        }
        let Some(envelope) = (match parse_business_tool_request_message(&message.content) {
            Ok(envelope) => envelope,
            Err(ApiError::BadRequest(_)) => continue,
            Err(error) => return Err(error),
        }) else {
            continue;
        };
        if envelope.request_id != request_id {
            continue;
        }
        return Ok(Some((
            message.clone(),
            validate_stored_business_tool_request(message, envelope)?,
        )));
    }
    Ok(None)
}

fn validate_stored_business_tool_request(
    message: ChannelMessage,
    envelope: BusinessToolRequestEnvelope,
) -> Result<ValidatedBusinessToolRequest, ApiError> {
    // 旧 envelope 可能只有 timeout_seconds；这里用消息创建时间补齐 expires_at，
    // 保证超时判断始终基于持久化消息而不是回调请求。
    let timeout_seconds = envelope
        .timeout_seconds
        .ok_or(ApiError::BadRequest("invalid business tool request"))?;
    let expires_at = envelope
        .expires_at
        .unwrap_or_else(|| message.created_at.saturating_add(timeout_seconds));
    Ok(ValidatedBusinessToolRequest {
        request_id: envelope.request_id,
        tool_name: envelope.tool_name,
        arguments: envelope.arguments,
        timeout_seconds,
        expires_at,
        normalized_content: message.content,
    })
}

fn parse_business_tool_request_message(
    content: &str,
) -> Result<Option<BusinessToolRequestEnvelope>, ApiError> {
    let trimmed = content.trim_start();
    let Some(json) = trimmed.strip_prefix(BUSINESS_TOOL_REQUEST_MARKER) else {
        return Ok(None);
    };
    let json = json.trim_start();
    let envelope: BusinessToolRequestEnvelope = serde_json::from_str(json)
        .map_err(|_| ApiError::BadRequest("invalid business tool request"))?;
    if envelope.request_id.trim().is_empty() || envelope.tool_name.trim().is_empty() {
        return Err(ApiError::BadRequest("invalid business tool request"));
    }
    if !is_valid_business_tool_request_id(&envelope.request_id) {
        return Err(ApiError::BadRequest("invalid business tool request"));
    }
    if !envelope.arguments.is_object() {
        // arguments 会直接交给业务系统执行工具；只接受 JSON object，避免数组/null
        // 这类无法按工具参数表解释的 envelope 被持久化。
        return Err(ApiError::BadRequest("invalid business tool request"));
    }
    if envelope
        .timeout_seconds
        .is_some_and(|timeout_seconds| timeout_seconds == 0)
    {
        return Err(ApiError::BadRequest("invalid business tool request"));
    }
    Ok(Some(envelope))
}

fn is_valid_business_tool_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn effective_business_tool_timeout_seconds(
    requested: Option<u64>,
    default_timeout_seconds: u64,
    max_timeout_seconds: u64,
) -> Result<u64, ApiError> {
    let timeout_seconds = requested.unwrap_or(default_timeout_seconds);
    if timeout_seconds == 0 {
        return Err(ApiError::BadRequest("invalid business tool request"));
    }
    Ok(timeout_seconds.min(max_timeout_seconds))
}

fn render_business_tool_request_content(
    request_id: &str,
    tool_name: &str,
    arguments: Value,
    timeout_seconds: u64,
    expires_at: u64,
) -> Result<String, ApiError> {
    // 业务工具请求没有单独表；规范化后的 envelope 写回消息正文，
    // 后续 SSE 重放和结果回调都从这份持久化 JSON 读取 timeout/expires_at。
    serde_json::to_string(&BusinessToolRequestEnvelope {
        request_id: request_id.to_string(),
        tool_name: tool_name.to_string(),
        arguments,
        timeout_seconds: Some(timeout_seconds),
        expires_at: Some(expires_at),
    })
    .map(|json| format!("{BUSINESS_TOOL_REQUEST_MARKER}\n{json}"))
    .map_err(|_| ApiError::Internal)
}

pub(crate) fn business_tool_request_event(
    message: &ChannelMessage,
    integration_id: &str,
    request: &ValidatedBusinessToolRequest,
    status: BusinessToolRequestStatus,
    result_message_id: Option<String>,
) -> BusinessToolRequestEvent {
    let updated_at = unix_now();
    BusinessToolRequestEvent {
        request_id: request.request_id.clone(),
        session_id: message.session_id.clone(),
        integration_id: integration_id.to_string(),
        tool_name: request.tool_name.clone(),
        arguments: request.arguments.clone(),
        timeout_seconds: request.timeout_seconds,
        expires_at: request.expires_at,
        status,
        created_at: message.created_at,
        updated_at,
        result_message_id,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

async fn integration_app_by_basic_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<IntegrationApp, ApiError> {
    let (client_id, client_secret) =
        basic_auth_credentials(headers).ok_or(ApiError::Unauthorized)?;
    let app = state
        .store
        .integration_app_by_client_id(&client_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or(ApiError::Unauthorized)?;
    if !app.enabled {
        return Err(ApiError::NotFound("integration app is not enabled"));
    }
    if !state
        .store
        .verify_integration_app_secret(&app, &client_secret)
        .await
    {
        return Err(ApiError::Unauthorized);
    }
    Ok(app)
}

fn basic_auth_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    // 业务系统发布/启动时会用接入应用自身的 client_id + client_secret 做机器认证，
    // 不依赖某个用户的 OAuth bearer token。
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let encoded = value
        .strip_prefix("Basic ")
        .or_else(|| value.strip_prefix("basic "))?
        .trim();
    if encoded.is_empty() {
        return None;
    }
    let decoded = BASE64_STANDARD.decode(encoded).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (client_id, client_secret) = decoded.split_once(':')?;
    let client_id = client_id.trim();
    if client_id.is_empty() || client_secret.is_empty() {
        return None;
    }
    Some((client_id.to_string(), client_secret.to_string()))
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
    let app = state
        .store
        .integration_app_by_integration_id(&integration_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or(ApiError::NotFound("integration app not found"))?;
    if !app.enabled {
        return Err(ApiError::NotFound("integration app is not enabled"));
    }
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

fn map_channel_error_from_store(error: crate::session::store::StoreError) -> ApiError {
    match error {
        crate::session::store::StoreError::InvalidSystemSettings => {
            ApiError::BadRequest("invalid integration tools")
        }
        _ => ApiError::Internal,
    }
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
