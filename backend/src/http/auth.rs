use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    domain::user::{PublicUser, User, UserRole},
    public_platform,
    session::store::{SessionPurpose, StoreError},
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
        .route("/api/auth/password", put(update_password))
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

#[derive(Deserialize)]
struct UpdatePasswordRequest {
    new_password: String,
}

#[derive(Serialize)]
struct UserResponse {
    user: PublicUser,
}

#[derive(Clone, Debug)]
pub struct AuthContext {
    pub user: User,
    pub session_purpose: SessionPurpose,
    pub integration_id: Option<String>,
}

#[derive(Serialize)]
struct BootstrapStatusResponse {
    bootstrap_open: bool,
    public_platform_enabled: bool,
    empty_chat_prompt: String,
}

async fn bootstrap_status(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let bootstrap_open = state
        .store
        .bootstrap_open()
        .await
        .map_err(|_| ApiError::Internal)?;
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    let public_platform_enabled = public_platform::public_hermes_readiness(&state)
        .await?
        .ready;

    Ok(Json(BootstrapStatusResponse {
        bootstrap_open,
        public_platform_enabled,
        empty_chat_prompt: settings.empty_chat_prompt,
    }))
}

async fn bootstrap_register(
    State(state): State<AppState>,
    Json(payload): Json<BootstrapRegisterRequest>,
) -> Result<Response, ApiError> {
    let user = state
        .store
        .create_bootstrap_admin(&payload.email, &payload.password)
        .await
        .map_err(map_register_error)?;
    match workspace::ensure_managed_hermes_for_user(&state, &user.id).await {
        Ok(_) => {}
        // 首个管理员通常需要先进入系统配置模型；模型未就绪时不能反过来阻断初始化。
        Err(ApiError::Conflict(_)) => {}
        Err(error) => return Err(error),
    }
    // 首个管理员创建完成后应立即拥有登录态，否则前端进入工作台后会用无 cookie 的请求访问受保护接口。
    let session_token = state
        .store
        .create_session(&user.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let cookie = session_cookie(&state.config.cookie_name, &session_token);

    let mut response = (
        StatusCode::CREATED,
        Json(UserResponse {
            user: user.public(),
        }),
    )
        .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| ApiError::Internal)?,
    );

    Ok(response)
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
    if let Some(token) = cookie_value_from_headers(&headers, &state.config.cookie_name) {
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

async fn update_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdatePasswordRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_web_user(&state, &headers).await?;
    if payload.new_password.trim().is_empty() {
        return Err(ApiError::BadRequest("password cannot be empty"));
    }

    // 个人设置更新的是 Hub 本地密码；OIDC/LDAP 登录仍按同一邮箱复用这个账号。
    state
        .store
        .update_user_password(&user.id, &payload.new_password)
        .await
        .map_err(map_update_password_error)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_web_user(&state, &headers).await?;

    Ok(Json(UserResponse {
        user: user.public(),
    }))
}

pub async fn current_user(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    current_web_user(state, headers).await
}

pub async fn current_bearer_auth_context(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthContext, ApiError> {
    let token = bearer_token_from_headers(headers).ok_or(ApiError::Unauthorized)?;
    auth_context_by_token(state, &token).await
}

async fn current_cookie_auth_context(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthContext, ApiError> {
    let token = cookie_value_from_headers(headers, &state.config.cookie_name)
        .ok_or(ApiError::Unauthorized)?;
    auth_context_by_token(state, &token).await
}

async fn auth_context_by_token(state: &AppState, token: &str) -> Result<AuthContext, ApiError> {
    let lookup = state
        .store
        .user_and_session_lookup_by_session_token(token)
        .await
        .map_err(|_| ApiError::Unauthorized)?;

    Ok(AuthContext {
        user: lookup.user,
        session_purpose: lookup.purpose,
        integration_id: lookup.integration_id,
    })
}

pub async fn current_oauth_user(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    let context = current_bearer_auth_context(state, headers).await?;
    if context.session_purpose != SessionPurpose::OAuth {
        return Err(ApiError::Unauthorized);
    }
    Ok(context.user)
}

pub async fn current_web_user(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    let context = current_cookie_auth_context(state, headers).await?;
    if context.session_purpose != SessionPurpose::Web {
        return Err(ApiError::Unauthorized);
    }
    Ok(context.user)
}

pub async fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    let user = current_web_user(state, headers).await?;

    if user.role != UserRole::Admin {
        return Err(ApiError::Forbidden);
    }

    Ok(user)
}

pub fn session_cookie(cookie_name: &str, session_token: &str) -> String {
    format!(
        "{cookie_name}={session_token}; HttpOnly; SameSite=Lax; Path=/{}; Max-Age=604800",
        secure_cookie_suffix()
    )
}

fn clear_session_cookie(cookie_name: &str) -> String {
    clear_cookie(cookie_name)
}

pub fn clear_cookie(cookie_name: &str) -> String {
    format!(
        "{cookie_name}=; HttpOnly; SameSite=Lax; Path=/{}; Max-Age=0",
        secure_cookie_suffix()
    )
}

fn bearer_token_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?
        .trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

pub fn cookie_value_from_headers(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
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

fn secure_cookie_suffix() -> &'static str {
    if cfg!(debug_assertions) {
        ""
    } else {
        "; Secure"
    }
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

fn map_update_password_error(error: StoreError) -> ApiError {
    match error {
        StoreError::Unauthorized => ApiError::Unauthorized,
        StoreError::PasswordFailed => ApiError::BadRequest("password could not be stored"),
        _ => ApiError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        channel::{events::SessionEventHub, service::ChannelStore},
        docker_config_from_app,
        hermes::docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
        ldap::DefaultLdapAuthenticator,
        llm_proxy::InMemoryLlmProviderClient,
        model_config::ModelRegistry,
        session::store::SessionStore,
        storage::object_storage_from_config,
        AppConfig, AppState,
    };
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use serde_json::Value;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn system_setting_opens_public_platform_only_after_public_hermes_is_ready() {
        let mut config = AppConfig::for_tests();
        config.initial_model_config.provider_base_url = "https://ready-provider.example/v1".into();
        config.initial_model_config.provider_api_key = "ready-provider-key".into();
        let store = SessionStore::default();
        store
            .create_bootstrap_admin("admin@example.com", "admin-password-123")
            .await
            .expect("admin can be created");
        let mut settings = store
            .system_settings()
            .await
            .expect("system settings can be read");
        settings.public_platform.enabled = true;
        settings.empty_chat_prompt = "Ask Hermes anything".into();
        store
            .update_system_settings(settings)
            .await
            .expect("public platform setting can be saved");
        let state = test_state(config, store);
        let app = crate::build_router_with_state(state.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/auth/bootstrap-status")
                    .body(Body::empty())
                    .expect("request can be built"),
            )
            .await
            .expect("bootstrap status request succeeds");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("bootstrap status body can be read");
        let payload: Value = serde_json::from_slice(&body).expect("bootstrap status is json");
        assert_eq!(payload["public_platform_enabled"], false);
        assert_eq!(payload["empty_chat_prompt"], "Ask Hermes anything");

        crate::public_platform::ensure_public_hermes_if_enabled(&state)
            .await
            .expect("public Hermes can be prestarted")
            .expect("public Hermes is enabled");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/auth/bootstrap-status")
                    .body(Body::empty())
                    .expect("request can be built"),
            )
            .await
            .expect("bootstrap status request succeeds after prestart");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("bootstrap status body can be read");
        let payload: Value = serde_json::from_slice(&body).expect("bootstrap status is json");
        assert_eq!(payload["public_platform_enabled"], true);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/sessions")
                    .body(Body::empty())
                    .expect("request can be built"),
            )
            .await
            .expect("anonymous sessions request succeeds");
        assert_eq!(response.status(), StatusCode::OK);
    }

    fn test_state(config: AppConfig, store: SessionStore) -> AppState {
        let object_storage = object_storage_from_config(&config.object_storage);
        let docker_provisioner = DockerProvisioner::new_with_runtime_and_object_storage(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
            object_storage.clone(),
        );
        AppState {
            model_registry: ModelRegistry::new(config.initial_model_config.clone()),
            config,
            store,
            channel_store: ChannelStore::default(),
            llm_provider: InMemoryLlmProviderClient::default().shared(),
            ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
            docker_provisioner,
            object_storage,
            session_events: SessionEventHub::default(),
        }
    }
}
