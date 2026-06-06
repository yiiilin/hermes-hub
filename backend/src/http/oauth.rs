use axum::{
    extract::{Form, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::{
    http::auth::{current_bearer_auth_context, current_web_user},
    session::store::BusinessOAuthSettings,
    AppState,
};

use super::ApiError;

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
        .route("/api/oauth/authorize", get(authorize))
        .route("/api/oauth/token", post(token))
        .route("/api/oauth/userinfo", get(userinfo))
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    scope: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
struct TokenForm {
    grant_type: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    redirect_uri: Option<String>,
    code: Option<String>,
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: &'static str,
    expires_in: u64,
    scope: String,
}

#[derive(Serialize)]
struct UserInfoResponse {
    id: String,
    sub: String,
    email: String,
    integration_id: String,
    toolset_names: Vec<String>,
}

async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthorizeQuery>,
) -> Result<Response, ApiError> {
    let settings = enabled_business_oauth_settings(&state).await?;
    let user = current_web_user(&state, &headers).await?;
    let response_type = query
        .response_type
        .as_deref()
        .ok_or(ApiError::BadRequest("missing response_type"))?;
    if response_type != "code" {
        return Err(ApiError::BadRequest("unsupported response_type"));
    }
    let client_id = query
        .client_id
        .as_deref()
        .ok_or(ApiError::BadRequest("missing client_id"))?;
    if client_id != settings.client_id {
        return Err(ApiError::Unauthorized);
    }
    let redirect_uri = query
        .redirect_uri
        .as_deref()
        .ok_or(ApiError::BadRequest("missing redirect_uri"))?;
    if !is_allowed_redirect_uri(&settings, redirect_uri) {
        return Err(ApiError::Unauthorized);
    }
    let scope = normalize_scope(query.scope.as_deref().unwrap_or(settings.scopes.as_str()))?;
    validate_scope(&settings, &scope)?;
    let code = state
        .store
        .create_business_oauth_authorization_code(
            &user.id,
            &settings.client_id,
            redirect_uri,
            &scope,
            settings.authorization_code_ttl_seconds,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

    let response = redirect_to(&append_oauth_query(
        redirect_uri,
        &[
            ("code", &code),
            ("state", query.state.as_deref().unwrap_or("")),
        ],
    ))?;
    Ok(response)
}

async fn token(
    State(state): State<AppState>,
    Form(form): Form<TokenForm>,
) -> Result<impl IntoResponse, ApiError> {
    let settings = enabled_business_oauth_settings(&state).await?;
    if form.grant_type.as_deref() != Some("authorization_code") {
        return Err(ApiError::BadRequest("unsupported grant_type"));
    }
    if form.client_id.as_deref() != Some(settings.client_id.as_str())
        || form.client_secret.as_deref() != Some(settings.client_secret.as_str())
    {
        return Err(ApiError::Unauthorized);
    }
    let redirect_uri = form
        .redirect_uri
        .as_deref()
        .ok_or(ApiError::BadRequest("missing redirect_uri"))?;
    if !is_allowed_redirect_uri(&settings, redirect_uri) {
        return Err(ApiError::Unauthorized);
    }
    let code = form
        .code
        .as_deref()
        .ok_or(ApiError::BadRequest("missing code"))?;
    let grant = state
        .store
        .consume_business_oauth_authorization_code(code, &settings.client_id, redirect_uri)
        .await
        .map_err(|_| ApiError::Unauthorized)?;

    let access_token = state
        .store
        .create_oauth_session(&grant.user_id, &settings.client_id)
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(TokenResponse {
        access_token,
        token_type: "Bearer",
        expires_in: 7 * 24 * 60 * 60,
        scope: grant.scope,
    }))
}

async fn userinfo(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let auth = current_bearer_auth_context(&state, &headers).await?;
    if auth.session_purpose != crate::session::store::SessionPurpose::OAuth {
        return Err(ApiError::Unauthorized);
    }
    let integration_id = auth.integration_id.clone().ok_or(ApiError::Unauthorized)?;
    let settings = enabled_business_oauth_settings(&state).await?;

    Ok(Json(UserInfoResponse {
        id: auth.user.id.clone(),
        sub: auth.user.id,
        email: auth.user.email,
        integration_id,
        toolset_names: settings.toolset_names,
    }))
}

async fn enabled_business_oauth_settings(
    state: &AppState,
) -> Result<BusinessOAuthSettings, ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    if !settings.business_oauth.enabled {
        return Err(ApiError::NotFound("business oauth is not enabled"));
    }
    Ok(settings.business_oauth)
}

fn is_allowed_redirect_uri(settings: &BusinessOAuthSettings, redirect_uri: &str) -> bool {
    settings
        .allowed_redirect_uris
        .iter()
        .any(|value| value == redirect_uri)
}

fn normalize_scope(scope: &str) -> Result<String, ApiError> {
    let tokens = scope
        .split_whitespace()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return Err(ApiError::BadRequest("missing scope"));
    }
    Ok(tokens.join(" "))
}

fn validate_scope(settings: &BusinessOAuthSettings, scope: &str) -> Result<(), ApiError> {
    let allowed = settings
        .scopes
        .split_whitespace()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    if scope
        .split_whitespace()
        .map(str::trim)
        .any(|value| !allowed.contains(value))
    {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}

fn append_oauth_query(base: &str, pairs: &[(&str, &str)]) -> String {
    let mut url = base.to_string();
    let mut separator = if base.contains('?') { '&' } else { '?' };
    for (key, value) in pairs {
        if value.is_empty() {
            continue;
        }
        url.push(separator);
        separator = '&';
        url.push_str(key);
        url.push('=');
        url.push_str(&encode_query(value));
    }
    url
}

fn encode_query(value: &str) -> String {
    utf8_percent_encode(value, QUERY_ENCODE_SET).to_string()
}

fn redirect_to(location: &str) -> Result<Response, ApiError> {
    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(location).map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}
