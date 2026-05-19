use axum::{
    body::{to_bytes, Body},
    extract::{OriginalUri, State},
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
};

use crate::{
    hermes::{instance::HermesInstanceStatus, proxy_client::HermesProxyRequest},
    http::{auth::current_user, ApiError},
    AppState,
};

pub async fn proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    method: axum::http::Method,
    body: Body,
) -> Result<Response, ApiError> {
    let user = current_user(&state, &headers)?;
    let path_and_query = hermes_path_and_query(&uri.to_string())?;

    if is_denied_path(&path_and_query) {
        return Err(ApiError::Forbidden);
    }

    let instance = state
        .store
        .hermes_instance_for_user(&user.id)
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;

    if instance.status != HermesInstanceStatus::Running {
        return Err(ApiError::Conflict("hermes instance is not running"));
    }

    let body = to_bytes(body, usize::MAX)
        .await
        .map_err(|_| ApiError::BadRequest("request body could not be read"))?;
    let authorization = instance
        .api_token_secret_ref
        .as_ref()
        .map(|token| format!("Bearer {token}"));
    let request = HermesProxyRequest {
        method,
        instance_base_url: instance.base_url,
        path_and_query,
        authorization,
        content_type: headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned),
        body: body.to_vec(),
    };

    let proxied = state
        .hermes_proxy
        .send(request)
        .await
        .map_err(|_| ApiError::Internal)?;
    let mut response = (proxied.status, proxied.body).into_response();

    if let Some(content_type) = proxied.content_type {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&content_type).map_err(|_| ApiError::Internal)?,
        );
    }

    Ok(response)
}

fn hermes_path_and_query(original_uri: &str) -> Result<String, ApiError> {
    let stripped = original_uri
        .strip_prefix("/api/hermes")
        .ok_or(ApiError::NotFound("hermes path not found"))?;

    if stripped.is_empty() {
        return Ok("/".to_string());
    }

    Ok(stripped.to_string())
}

fn is_denied_path(path_and_query: &str) -> bool {
    let path = path_and_query.split('?').next().unwrap_or(path_and_query);
    matches!(path, path if path.starts_with("/admin/")
        || path == "/admin"
        || path.starts_with("/internal/")
        || path == "/internal"
        || path.starts_with("/config/")
        || path == "/config"
        || path.starts_with("/models/config"))
}
