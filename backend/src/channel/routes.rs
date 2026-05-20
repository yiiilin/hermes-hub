use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    channel::service::{Channel, ChannelSession, ChannelSessionKind, ChannelStoreError},
    http::{auth::current_user, ApiError},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/channels", get(list_channels).post(create_channel))
        .route("/api/channels/{channel_id}", get(get_channel))
        .route(
            "/api/channels/{channel_id}/sessions",
            get(list_sessions).post(create_session),
        )
        .route(
            "/api/channels/{channel_id}/sessions/{session_id}",
            get(get_session),
        )
}

#[derive(Deserialize)]
struct CreateChannelRequest {
    name: String,
    description: Option<String>,
}

#[derive(Deserialize)]
struct CreateSessionRequest {
    kind: String,
    title: Option<String>,
}

#[derive(Serialize)]
struct ChannelResponse {
    channel: Channel,
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

async fn create_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateChannelRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let channel = state
        .channel_store
        .create_channel(&user.id, &payload.name, payload.description)
        .await
        .map_err(map_channel_error)?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(ChannelResponse { channel }),
    ))
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

    Ok(Json(ChannelResponse { channel }))
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;
    let kind = ChannelSessionKind::parse(&payload.kind).map_err(map_channel_error)?;
    let session = state
        .channel_store
        .create_session(&user.id, &channel_id, kind, payload.title)
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

fn map_channel_error(error: ChannelStoreError) -> ApiError {
    match error {
        ChannelStoreError::ChannelNotFound => ApiError::NotFound("channel not found"),
        ChannelStoreError::InvalidSessionKind => ApiError::BadRequest("invalid session kind"),
        ChannelStoreError::LockFailed | ChannelStoreError::DatabaseFailed => ApiError::Internal,
    }
}
