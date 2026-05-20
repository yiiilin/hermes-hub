use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    domain::user::{PublicUser, User, UserRole},
    session::store::StoreError,
    AppState,
};

use super::{workspace, ApiError};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/bootstrap-status", get(bootstrap_status))
        .route("/api/auth/bootstrap-register", post(bootstrap_register))
        .route("/api/auth/register", post(register_with_invite))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/me", get(me))
}

#[derive(Deserialize)]
struct BootstrapRegisterRequest {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct InviteRegisterRequest {
    invite_token: String,
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Serialize)]
struct UserResponse {
    user: PublicUser,
}

#[derive(Serialize)]
struct BootstrapStatusResponse {
    bootstrap_open: bool,
}

async fn bootstrap_status(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let bootstrap_open = state
        .store
        .bootstrap_open()
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(BootstrapStatusResponse { bootstrap_open }))
}

async fn bootstrap_register(
    State(state): State<AppState>,
    Json(payload): Json<BootstrapRegisterRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = state
        .store
        .create_bootstrap_admin(&payload.email, &payload.password)
        .await
        .map_err(map_register_error)?;

    Ok((
        StatusCode::CREATED,
        Json(UserResponse {
            user: user.public(),
        }),
    ))
}

async fn register_with_invite(
    State(state): State<AppState>,
    Json(payload): Json<InviteRegisterRequest>,
) -> Result<impl IntoResponse, ApiError> {
    workspace::ensure_required_model_configs(&state).await?;
    let user = state
        .store
        .register_with_invite(&payload.invite_token, &payload.email, &payload.password)
        .await
        .map_err(map_invite_register_error)?;
    workspace::ensure_managed_hermes_for_user(&state, &user.id).await?;

    Ok((
        StatusCode::CREATED,
        Json(UserResponse {
            user: user.public(),
        }),
    ))
}

async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let user = state
        .store
        .login(&payload.email, &payload.password)
        .await
        .map_err(map_login_error)?;
    let session_token = state
        .store
        .create_session(&user.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let cookie = session_cookie(&state.config.cookie_name, &session_token);

    let mut response = Json(UserResponse {
        user: user.public(),
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| ApiError::Internal)?,
    );

    Ok(response)
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    if let Some(token) = session_token_from_headers(&headers, &state.config.cookie_name) {
        state
            .store
            .delete_session(&token)
            .await
            .map_err(|_| ApiError::Internal)?;
    }

    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&clear_session_cookie(&state.config.cookie_name))
            .map_err(|_| ApiError::Internal)?,
    );

    Ok(response)
}

async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_user(&state, &headers).await?;

    Ok(Json(UserResponse {
        user: user.public(),
    }))
}

pub async fn current_user(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    let token = session_token_from_headers(headers, &state.config.cookie_name)
        .ok_or(ApiError::Unauthorized)?;

    state
        .store
        .user_by_session_token(&token)
        .await
        .map_err(|_| ApiError::Unauthorized)
}

pub async fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    let user = current_user(state, headers).await?;

    if user.role != UserRole::Admin {
        return Err(ApiError::Forbidden);
    }

    Ok(user)
}

fn session_cookie(cookie_name: &str, session_token: &str) -> String {
    format!("{cookie_name}={session_token}; HttpOnly; SameSite=Lax; Path=/; Max-Age=604800")
}

fn clear_session_cookie(cookie_name: &str) -> String {
    format!("{cookie_name}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}

fn session_token_from_headers(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;

    cookie_header
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| {
            if name == cookie_name {
                Some(value.to_string())
            } else {
                None
            }
        })
}

fn map_register_error(error: StoreError) -> ApiError {
    match error {
        StoreError::BootstrapClosed => ApiError::Conflict("bootstrap registration is closed"),
        StoreError::EmailAlreadyRegistered => ApiError::Conflict("email is already registered"),
        StoreError::PasswordFailed => ApiError::BadRequest("password could not be stored"),
        _ => ApiError::Internal,
    }
}

fn map_invite_register_error(error: StoreError) -> ApiError {
    match error {
        StoreError::InviteExpired | StoreError::InviteRevoked | StoreError::InviteNotFound => {
            ApiError::Gone("invite is not available")
        }
        StoreError::InviteExhausted => ApiError::Conflict("invite has no remaining uses"),
        StoreError::EmailAlreadyRegistered => ApiError::Conflict("email is already registered"),
        StoreError::PasswordFailed => ApiError::BadRequest("password could not be stored"),
        _ => ApiError::Internal,
    }
}

fn map_login_error(error: StoreError) -> ApiError {
    match error {
        StoreError::InvalidCredentials => ApiError::Unauthorized,
        _ => ApiError::Internal,
    }
}
