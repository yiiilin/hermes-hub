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
    session::store::IntegrationApp,
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
    let user = current_web_user(&state, &headers).await?;
    let response_type = query
        .response_type
        .as_deref()
        .ok_or(ApiError::BadRequest("missing response_type"))?;
    if response_type != "code" {
        return Err(ApiError::BadRequest("unsupported response_type"));
    }
    let app = enabled_integration_app_by_client_id(&state, query.client_id.as_deref()).await?;
    let redirect_uri = query
        .redirect_uri
        .as_deref()
        .ok_or(ApiError::BadRequest("missing redirect_uri"))?;
    if !is_allowed_redirect_uri(&app, redirect_uri) {
        return Err(ApiError::Unauthorized);
    }
    let scope = normalize_scope(query.scope.as_deref().unwrap_or(app.scopes.as_str()))?;
    validate_scope(&app, &scope)?;
    let code = state
        .store
        .create_business_oauth_authorization_code(
            &user.id,
            &app.client_id,
            redirect_uri,
            &scope,
            app.authorization_code_ttl_seconds,
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
    if form.grant_type.as_deref() != Some("authorization_code") {
        return Err(ApiError::BadRequest("unsupported grant_type"));
    }
    let app = enabled_integration_app_by_client_id(&state, form.client_id.as_deref()).await?;
    match form.client_secret.as_deref() {
        Some(secret)
            if state
                .store
                .verify_integration_app_secret(&app, secret)
                .await => {}
        _ => return Err(ApiError::Unauthorized),
    }
    let redirect_uri = form
        .redirect_uri
        .as_deref()
        .ok_or(ApiError::BadRequest("missing redirect_uri"))?;
    if !is_allowed_redirect_uri(&app, redirect_uri) {
        return Err(ApiError::Unauthorized);
    }
    let code = form
        .code
        .as_deref()
        .ok_or(ApiError::BadRequest("missing code"))?;
    let grant = state
        .store
        .consume_business_oauth_authorization_code(code, &app.client_id, redirect_uri)
        .await
        .map_err(|_| ApiError::Unauthorized)?;

    let access_token = state
        .store
        .create_oauth_session(&grant.user_id, &app.integration_id)
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
    let app = state
        .store
        .integration_app_by_integration_id(&integration_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .filter(|app| app.enabled)
        .ok_or(ApiError::Unauthorized)?;
    let toolset_names = state
        .store
        .list_integration_tools(&app.integration_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();

    Ok(Json(UserInfoResponse {
        id: auth.user.id.clone(),
        sub: auth.user.id,
        email: auth.user.email,
        integration_id,
        toolset_names,
    }))
}

async fn enabled_integration_app_by_client_id(
    state: &AppState,
    client_id: Option<&str>,
) -> Result<IntegrationApp, ApiError> {
    let client_id = client_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ApiError::BadRequest("missing client_id"))?;
    let app = state
        .store
        .integration_app_by_client_id(client_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or(ApiError::Unauthorized)?;
    if !app.enabled {
        return Err(ApiError::NotFound("integration app is not enabled"));
    }
    Ok(app)
}

fn is_allowed_redirect_uri(app: &IntegrationApp, redirect_uri: &str) -> bool {
    app.redirect_uri == redirect_uri
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

fn validate_scope(app: &IntegrationApp, scope: &str) -> Result<(), ApiError> {
    let allowed = app
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
