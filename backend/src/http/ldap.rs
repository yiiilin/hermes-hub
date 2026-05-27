use axum::{
    extract::State,
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    ldap::LdapAuthError,
    session::store::{LdapSettings, StoreError},
    AppState,
};

use super::{auth, workspace, ApiError};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/ldap/config", get(ldap_config))
        .route("/api/auth/ldap/login", post(ldap_login))
}

#[derive(Serialize)]
struct LdapPublicConfigResponse {
    ldap: LdapPublicConfig,
}

#[derive(Serialize)]
struct LdapPublicConfig {
    enabled: bool,
    display_name: String,
}

#[derive(Deserialize)]
struct LdapLoginRequest {
    email: String,
    password: String,
}

#[derive(Serialize)]
struct UserResponse {
    user: crate::domain::user::PublicUser,
}

async fn ldap_config(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(LdapPublicConfigResponse {
        ldap: LdapPublicConfig {
            enabled: settings.ldap.enabled,
            display_name: settings.ldap.display_name,
        },
    }))
}

async fn ldap_login(
    State(state): State<AppState>,
    Json(payload): Json<LdapLoginRequest>,
) -> Result<Response, ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    let ldap = enabled_ldap_settings(&settings.ldap)?;
    let requested_email = normalize_email(&payload.email).ok_or(ApiError::Unauthorized)?;
    let identity = state
        .ldap_authenticator
        .authenticate(&ldap, &requested_email, &payload.password)
        .await
        .map_err(map_ldap_auth_error)?;
    if identity.email != requested_email {
        return Err(ApiError::Unauthorized);
    }

    if ldap.auto_create_users
        && state
            .store
            .user_by_email(&identity.email)
            .await
            .map_err(|_| ApiError::Internal)?
            .is_none()
    {
        // 自动创建新用户前先确认模型配置完整，避免创建出无法启动 Hermes 的账号。
        workspace::ensure_required_model_configs(&state).await?;
    }

    let ldap_user = state
        .store
        .get_or_create_ldap_user(&identity.email, ldap.auto_create_users)
        .await
        .map_err(map_ldap_user_error)?;
    if ldap_user.created {
        workspace::ensure_managed_hermes_for_user(&state, &ldap_user.user.id).await?;
    }
    let session_token = state
        .store
        .create_session(&ldap_user.user.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let cookie = auth::session_cookie(&state.config.cookie_name, &session_token);

    let mut response = Json(UserResponse {
        user: ldap_user.user.public(),
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| ApiError::Internal)?,
    );

    Ok(response)
}

fn enabled_ldap_settings(ldap: &LdapSettings) -> Result<LdapSettings, ApiError> {
    if !ldap.enabled {
        return Err(ApiError::NotFound("LDAP is not enabled"));
    }
    Ok(ldap.clone())
}

fn normalize_email(email: &str) -> Option<String> {
    let email = email.trim().to_lowercase();
    (!email.is_empty()).then_some(email)
}

fn map_ldap_auth_error(error: LdapAuthError) -> ApiError {
    match error {
        LdapAuthError::InvalidCredentials => ApiError::Unauthorized,
        LdapAuthError::Misconfigured => ApiError::BadRequest("LDAP settings are invalid"),
        LdapAuthError::BackendFailed => ApiError::BadGateway("LDAP request failed"),
    }
}

fn map_ldap_user_error(error: StoreError) -> ApiError {
    match error {
        StoreError::InvalidCredentials => ApiError::Unauthorized,
        _ => ApiError::Internal,
    }
}
