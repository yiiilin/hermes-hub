use axum::{
    body::{to_bytes, Body},
    extract::{OriginalUri, State},
    http::{header, HeaderMap},
    response::Response,
};
use percent_encoding::percent_decode_str;
use std::time::{Duration, Instant};

use crate::{
    hermes::{
        event_streams::HermesRunReference,
        instance::{HermesInstanceKind, HermesInstanceStatus},
        proxy_client::{HermesProxyError, HermesProxyRequest},
    },
    http::{auth::current_user, workspace::ensure_managed_hermes_for_user, ApiError},
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

    let mut instance = state
        .store
        .hermes_instance_for_user(&user.id)
        .await
        .map_err(|_| ApiError::NotFound("hermes instance not found"))?;

    if instance.kind == HermesInstanceKind::ManagedDocker {
        // 托管 Hermes 必须先通过统一的 ensure 流程确认配置和 adapter 版本。
        // 直连代理不再绕过该约束，否则会重新引入和 Hub channel run 不一致的旧路径。
        instance = ensure_managed_hermes_for_user(&state, &user.id).await?;
    }

    if instance.status != HermesInstanceStatus::Running {
        return Err(ApiError::Conflict("hermes instance is not running"));
    }

    let body = to_bytes(body, state.config.max_proxy_body_bytes)
        .await
        .map_err(|_| ApiError::BadRequest("request body could not be read"))?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let authorization = instance
        .api_token_secret_ref
        .as_ref()
        .map(|token| format!("Bearer {token}"));
    let method_for_audit = method.to_string();
    let event_stream_key = hermes_run_events_key(&user.id, &instance.id, &method, &path_and_query);
    let event_stream_run_ref =
        hermes_run_reference(&user.id, &instance.id, &method, &path_and_query);
    let received_event_bytes = event_stream_key
        .as_ref()
        .and_then(|_| hermes_hub_received_bytes(&headers));
    let request = HermesProxyRequest {
        method,
        instance_base_url: instance.base_url.clone(),
        path_and_query: path_and_query.clone(),
        authorization,
        content_type,
        body: body.to_vec(),
        timeout_seconds: state.config.proxy_timeout_seconds,
    };

    let started = Instant::now();
    let proxied = if let Some(event_stream_key) = event_stream_key {
        state
            .hermes_event_streams
            .open(
                event_stream_key,
                received_event_bytes.unwrap_or(0),
                state.hermes_proxy.clone(),
                request,
                event_stream_run_ref,
            )
            .await
    } else {
        send_hermes_request_with_cold_start_retry(&state, &instance.kind, request).await
    };

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

async fn send_hermes_request_with_cold_start_retry(
    state: &AppState,
    instance_kind: &HermesInstanceKind,
    request: HermesProxyRequest,
) -> Result<Response, HermesProxyError> {
    let max_attempts = if *instance_kind == HermesInstanceKind::ManagedDocker {
        8
    } else {
        1
    };

    for attempt in 1..=max_attempts {
        match state.hermes_proxy.send(request.clone()).await {
            Ok(response) => return Ok(response),
            Err(error @ HermesProxyError::Failed(_)) if attempt < max_attempts => {
                // 托管容器刚重建后，Docker 已经 running 但 gateway 端口可能还没 ready。
                tokio::time::sleep(Duration::from_millis(750)).await;
                let _ = error;
            }
            Err(error) => return Err(error),
        }
    }

    state.hermes_proxy.send(request).await
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

fn hermes_run_events_key(
    user_id: &str,
    instance_id: &str,
    method: &axum::http::Method,
    path_and_query: &str,
) -> Option<String> {
    if method.as_str() != "GET" {
        return None;
    }

    let path = path_and_query.split('?').next().unwrap_or(path_and_query);
    let parts = path
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() == 4 && parts[0] == "v1" && parts[1] == "runs" && parts[3] == "events" {
        return Some(hermes_run_events_cache_key(user_id, instance_id, path));
    }

    None
}

fn hermes_run_events_cache_key(user_id: &str, instance_id: &str, path: &str) -> String {
    format!("{user_id}:{instance_id}:{path}")
}

fn hermes_run_reference(
    user_id: &str,
    instance_id: &str,
    method: &axum::http::Method,
    path_and_query: &str,
) -> Option<HermesRunReference> {
    if method.as_str() != "GET" {
        return None;
    }

    let path = path_and_query.split('?').next().unwrap_or(path_and_query);
    let parts = path
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() == 4 && parts[0] == "v1" && parts[1] == "runs" && parts[3] == "events" {
        return Some(HermesRunReference {
            user_id: user_id.to_string(),
            instance_id: instance_id.to_string(),
            run_id: parts[2].to_string(),
        });
    }

    None
}

fn hermes_hub_received_bytes(headers: &HeaderMap) -> Option<usize> {
    headers
        .get("x-hermes-hub-received-bytes")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
}
