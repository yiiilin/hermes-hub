use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use base64::Engine as _;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    session::store::{OidcSettings, StoreError},
    AppState,
};

use super::{auth, workspace, ApiError};

const OIDC_STATE_COOKIE: &str = "hermes_hub_oidc_state";
const OIDC_NONCE_COOKIE: &str = "hermes_hub_oidc_nonce";
const QUERY_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b'/')
    .add(b':')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/oidc/config", get(oidc_config))
        .route("/api/auth/oidc/start", get(oidc_start))
        .route("/api/auth/oidc/callback", get(oidc_callback))
}

#[derive(Serialize)]
struct OidcPublicConfigResponse {
    oidc: OidcPublicConfig,
}

#[derive(Serialize)]
struct OidcPublicConfig {
    enabled: bool,
    display_name: String,
    allow_password_login: bool,
}

#[derive(Deserialize)]
struct OidcCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct TokenRequest<'a> {
    grant_type: &'static str,
    code: &'a str,
    redirect_uri: &'a str,
    client_id: &'a str,
    client_secret: &'a str,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: Option<String>,
}

async fn oidc_config(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(OidcPublicConfigResponse {
        oidc: OidcPublicConfig {
            enabled: settings.oidc.enabled,
            display_name: settings.oidc.display_name,
            // 账号统一按邮箱关联，密码登录不再因 OIDC 开关被全局关闭；该字段保留给旧前端兼容。
            allow_password_login: true,
        },
    }))
}

async fn oidc_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    let oidc = enabled_oidc_settings(&settings.oidc)?;
    let redirect_uri = oidc_redirect_uri(&state, &headers);
    let state_token = random_url_token();
    let nonce = random_url_token();
    let location = authorization_url(&oidc, &redirect_uri, &state_token, &nonce);

    let mut response = redirect_to(&location)?;
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&short_lived_cookie(OIDC_STATE_COOKIE, &state_token))
            .map_err(|_| ApiError::Internal)?,
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&short_lived_cookie(OIDC_NONCE_COOKIE, &nonce))
            .map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}

async fn oidc_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<OidcCallbackQuery>,
) -> Result<Response, ApiError> {
    if query.error.is_some() {
        return Err(ApiError::Unauthorized);
    }
    let code = query
        .code
        .as_deref()
        .ok_or(ApiError::BadRequest("missing OIDC code"))?;
    let returned_state = query
        .state
        .as_deref()
        .ok_or(ApiError::BadRequest("missing OIDC state"))?;
    let cookie_state = auth::cookie_value_from_headers(&headers, OIDC_STATE_COOKIE)
        .ok_or(ApiError::Unauthorized)?;
    if cookie_state != returned_state {
        return Err(ApiError::Unauthorized);
    }

    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    let oidc = enabled_oidc_settings(&settings.oidc)?;
    let redirect_uri = oidc_redirect_uri(&state, &headers);
    let access_token = exchange_code(&oidc, code, &redirect_uri).await?;
    let userinfo = fetch_userinfo(&oidc, &access_token).await?;
    let email = claim_string(&userinfo, &oidc.email_claim)
        .ok_or(ApiError::Unauthorized)?
        .to_lowercase();
    if oidc.auto_create_users
        && state
            .store
            .user_by_email(&email)
            .await
            .map_err(|_| ApiError::Internal)?
            .is_none()
    {
        // 新 OIDC 用户创建后必须马上有托管 Hermes；先校验模型配置，避免创建无法使用的账号。
        workspace::ensure_required_model_configs(&state).await?;
    }
    let oidc_user = state
        .store
        .get_or_create_oidc_user(&email, oidc.auto_create_users)
        .await
        .map_err(map_oidc_user_error)?;
    if oidc_user.created {
        workspace::ensure_managed_hermes_for_user(&state, &oidc_user.user.id).await?;
    }
    let session_token = state
        .store
        .create_session(&oidc_user.user.id)
        .await
        .map_err(|_| ApiError::Internal)?;

    let mut response = redirect_to("/")?;
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&auth::session_cookie(
            &state.config.cookie_name,
            &session_token,
        ))
        .map_err(|_| ApiError::Internal)?,
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&auth::clear_cookie(OIDC_STATE_COOKIE))
            .map_err(|_| ApiError::Internal)?,
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&auth::clear_cookie(OIDC_NONCE_COOKIE))
            .map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}

fn enabled_oidc_settings(oidc: &OidcSettings) -> Result<OidcSettings, ApiError> {
    if !oidc.enabled {
        return Err(ApiError::NotFound("OIDC is not enabled"));
    }
    Ok(oidc.clone())
}

fn authorization_url(
    oidc: &OidcSettings,
    redirect_uri: &str,
    state_token: &str,
    nonce: &str,
) -> String {
    let mut separator = if oidc.authorization_url.contains('?') {
        '&'
    } else {
        '?'
    };
    let mut url = oidc.authorization_url.clone();
    for (key, value) in [
        ("client_id", oidc.client_id.as_str()),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", oidc.scopes.as_str()),
        ("state", state_token),
        ("nonce", nonce),
    ] {
        url.push(separator);
        separator = '&';
        url.push_str(key);
        url.push('=');
        url.push_str(&encode_query(value));
    }
    url
}

async fn exchange_code(
    oidc: &OidcSettings,
    code: &str,
    redirect_uri: &str,
) -> Result<String, ApiError> {
    let client = Client::new();
    let response = client
        .post(&oidc.token_url)
        .form(&TokenRequest {
            grant_type: "authorization_code",
            code,
            redirect_uri,
            client_id: &oidc.client_id,
            client_secret: &oidc.client_secret,
        })
        .send()
        .await
        .map_err(|_| ApiError::BadGateway("OIDC token request failed"))?;
    if !response.status().is_success() {
        return Err(ApiError::BadGateway("OIDC token request failed"));
    }
    let token = response
        .json::<TokenResponse>()
        .await
        .map_err(|_| ApiError::BadGateway("OIDC token response is invalid"))?;
    let _token_type = token.token_type;
    Ok(token.access_token)
}

async fn fetch_userinfo(oidc: &OidcSettings, access_token: &str) -> Result<Value, ApiError> {
    let response = Client::new()
        .get(&oidc.userinfo_url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| ApiError::BadGateway("OIDC userinfo request failed"))?;
    if !response.status().is_success() {
        return Err(ApiError::BadGateway("OIDC userinfo request failed"));
    }
    response
        .json::<Value>()
        .await
        .map_err(|_| ApiError::BadGateway("OIDC userinfo response is invalid"))
}

fn claim_string(userinfo: &Value, claim: &str) -> Option<String> {
    userinfo
        .get(claim)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn oidc_redirect_uri(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(origin) = external_origin(headers) {
        return format!("{origin}/api/auth/oidc/callback");
    }

    let host = state.config.bind_addr;
    let base = if host.port() == 0 || host.port() == 80 {
        "http://localhost".to_string()
    } else {
        format!("http://localhost:{}", host.port())
    };
    format!("{base}/api/auth/oidc/callback")
}

fn external_origin(headers: &HeaderMap) -> Option<String> {
    let host = header_value(headers, "x-forwarded-host")
        .or_else(|| header_value(headers, "host"))?
        .split(',')
        .next()?
        .trim();
    if host.is_empty() {
        return None;
    }

    let proto = header_value(headers, "x-forwarded-proto")
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| value.eq_ignore_ascii_case("http") || value.eq_ignore_ascii_case("https"))
        .unwrap_or("http");

    Some(format!("{proto}://{host}"))
}

fn header_value<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn encode_query(value: &str) -> String {
    utf8_percent_encode(value, QUERY_ENCODE_SET).to_string()
}

fn random_url_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("secure random token");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn short_lived_cookie(name: &str, value: &str) -> String {
    format!("{name}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age=600")
}

fn redirect_to(location: &str) -> Result<Response, ApiError> {
    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(location).map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}

fn map_oidc_user_error(error: StoreError) -> ApiError {
    match error {
        StoreError::InvalidCredentials => ApiError::Unauthorized,
        _ => ApiError::Internal,
    }
}
