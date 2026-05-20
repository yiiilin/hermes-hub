use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{domain::invite::PublicInvite, session::store::StoreError, AppState};

use super::{auth::require_admin, workspace::ensure_required_model_configs, ApiError};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/invites", post(create_invite).get(list_invites))
        .route("/api/invites/{invite_id}/revoke", post(revoke_invite))
}

#[derive(Deserialize)]
struct CreateInviteRequest {
    expires_at: Option<u64>,
    max_uses: Option<u32>,
}

#[derive(Serialize)]
struct InviteResponse {
    invite: PublicInvite,
}

#[derive(Serialize)]
struct CreatedInviteResponse {
    token: String,
    invite: PublicInvite,
}

#[derive(Serialize)]
struct InviteListResponse {
    invites: Vec<PublicInvite>,
}

async fn create_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateInviteRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let admin = require_admin(&state, &headers).await?;
    let expires_at = payload
        .expires_at
        .ok_or(ApiError::BadRequest("expires_at is required"))?;
    let max_uses = payload
        .max_uses
        .ok_or(ApiError::BadRequest("max_uses is required"))?;

    if max_uses == 0 {
        return Err(ApiError::BadRequest("max_uses must be greater than zero"));
    }

    ensure_required_model_configs(&state).await?;
    let created = state
        .store
        .create_invite(&admin.id, expires_at, max_uses)
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedInviteResponse {
            token: created.token,
            invite: created.invite,
        }),
    ))
}

async fn list_invites(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let invites = state
        .store
        .list_invites()
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(InviteListResponse { invites }))
}

async fn revoke_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(invite_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state, &headers).await?;
    let invite = state
        .store
        .revoke_invite(&invite_id)
        .await
        .map_err(map_revoke_error)?;

    Ok(Json(InviteResponse { invite }))
}

fn map_revoke_error(error: StoreError) -> ApiError {
    match error {
        StoreError::InviteIdNotFound => ApiError::NotFound("invite not found"),
        _ => ApiError::Internal,
    }
}
