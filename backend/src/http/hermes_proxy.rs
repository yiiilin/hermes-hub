use axum::{
    body::{to_bytes, Body},
    extract::{OriginalUri, State},
    http::{header, HeaderMap},
    response::Response,
};
use percent_encoding::percent_decode_str;
use std::time::Instant;

use crate::{
    hermes::{
        instance::HermesInstanceStatus,
        proxy_client::{HermesProxyError, HermesProxyRequest},
    },
    http::{auth::current_user, ApiError},
    session::store::ProxyAuditEvent,
    AppState,
};

pub async fn proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    method: axum::http::Method,
    body: Body,
) -> Result<Response, ApiError> {
    let user = current_user(&state, &headers).await?;
    let path_and_query = hermes_path_and_query(&uri.to_string())?;

    if is_denied_path(&path_and_query) {
        return Err(ApiError::Forbidden);
    }

    let instance = state
        .store
        .hermes_instance_for_user(&user.id)
        .await
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;

    if instance.status != HermesInstanceStatus::Running {
        return Err(ApiError::Conflict("hermes instance is not running"));
    }

    let body = to_bytes(body, state.config.max_proxy_body_bytes)
        .await
        .map_err(|_| ApiError::BadRequest("request body could not be read"))?;
    let authorization = instance
        .api_token_secret_ref
        .as_ref()
        .map(|token| format!("Bearer {token}"));
    let method_for_audit = method.to_string();
    let request = HermesProxyRequest {
        method,
        instance_base_url: instance.base_url,
        path_and_query: path_and_query.clone(),
        authorization,
        content_type: headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned),
        body: body.to_vec(),
        timeout_seconds: state.config.proxy_timeout_seconds,
    };

    let started = Instant::now();
    let proxied = state
        .hermes_proxy
        .send(request)
        .await;

    match proxied {
        Ok(response) => {
            let status = response.status().as_u16();
            let _ = state
                .store
                .record_proxy_audit(ProxyAuditEvent {
                    user_id: Some(user.id),
                    hermes_instance_id: Some(instance.id),
                    direction: "browser_to_hermes".to_string(),
                    method: method_for_audit,
                    path: path_and_query,
                    status_code: Some(status),
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                    error_code: None,
                })
                .await;
            Ok(response)
        }
        Err(error) => {
            let mapped = map_proxy_error(error);
            let _ = state
                .store
                .record_proxy_audit(ProxyAuditEvent {
                    user_id: Some(user.id),
                    hermes_instance_id: Some(instance.id),
                    direction: "browser_to_hermes".to_string(),
                    method: method_for_audit,
                    path: path_and_query,
                    status_code: None,
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                    error_code: Some("proxy_failed".to_string()),
                })
                .await;
            Err(mapped)
        }
    }
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
    let path = normalize_path_for_policy(path_and_query);
    matches!(path, path if path.starts_with("/admin/")
        || path == "/admin"
        || path.starts_with("/internal/")
        || path == "/internal"
        || path.starts_with("/config/")
        || path == "/config"
        || path.starts_with("/models/config"))
}

fn normalize_path_for_policy(path_and_query: &str) -> String {
    let raw_path = path_and_query.split('?').next().unwrap_or(path_and_query);
    let decoded = percent_decode_str(raw_path).decode_utf8_lossy();
    let mut segments = Vec::new();

    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            value => segments.push(value),
        }
    }

    format!("/{}", segments.join("/"))
}

fn map_proxy_error(error: HermesProxyError) -> ApiError {
    match error {
        HermesProxyError::InvalidUrl => ApiError::BadGateway("hermes url is invalid"),
        HermesProxyError::Timeout => ApiError::GatewayTimeout("hermes request timed out"),
        HermesProxyError::LockFailed | HermesProxyError::Failed(_) => {
            ApiError::BadGateway("hermes request failed")
        }
    }
}
