use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::TryStreamExt;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmProviderRequest {
    pub method: Method,
    pub provider_base_url: String,
    pub path: String,
    pub authorization: String,
    pub content_type: String,
    pub body: Vec<u8>,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmProviderResponse {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum LlmProviderError {
    #[error("provider proxy lock failed")]
    LockFailed,
    #[error("provider url is invalid")]
    InvalidUrl,
    #[error("provider request timed out")]
    Timeout,
    #[error("provider proxy failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait LlmProviderClient: Send + Sync {
    async fn send(&self, request: LlmProviderRequest) -> Result<Response, LlmProviderError>;
}

pub type DynLlmProviderClient = Arc<dyn LlmProviderClient>;

#[derive(Clone)]
pub struct InMemoryLlmProviderClient {
    response: LlmProviderResponse,
    last_request: Arc<Mutex<Option<LlmProviderRequest>>>,
}

impl InMemoryLlmProviderClient {
    pub fn new(response: LlmProviderResponse) -> Self {
        Self {
            response,
            last_request: Arc::new(Mutex::new(None)),
        }
    }

    pub fn shared(self) -> DynLlmProviderClient {
        Arc::new(self)
    }

    fn response(&self) -> Result<Response, LlmProviderError> {
        let mut response = (self.response.status, self.response.body.clone()).into_response();

        if let Some(content_type) = &self.response.content_type {
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(content_type)
                    .map_err(|error| LlmProviderError::Failed(error.to_string()))?,
            );
        }

        Ok(response)
    }

    pub fn last_request(&self) -> Option<LlmProviderRequest> {
        self.last_request.lock().ok()?.clone()
    }
}

#[async_trait]
impl LlmProviderClient for InMemoryLlmProviderClient {
    async fn send(&self, request: LlmProviderRequest) -> Result<Response, LlmProviderError> {
        *self
            .last_request
            .lock()
            .map_err(|_| LlmProviderError::LockFailed)? = Some(request);
        self.response()
    }
}

impl Default for InMemoryLlmProviderClient {
    fn default() -> Self {
        Self::new(LlmProviderResponse {
            status: StatusCode::OK,
            content_type: Some("application/json".to_string()),
            body: b"{}".to_vec(),
        })
    }
}

/// 生产使用的 OpenAI-compatible 上游 HTTP 客户端。
///
/// 这里不解析或记录 prompt body，只负责把经过 allowlist 校验后的请求转发给
/// 管理员配置的 provider，并把上游响应以流式 Body 交还给 Axum。
#[derive(Clone)]
pub struct ReqwestLlmProviderClient {
    client: reqwest::Client,
}

impl ReqwestLlmProviderClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    pub fn shared(self) -> DynLlmProviderClient {
        Arc::new(self)
    }
}

impl Default for ReqwestLlmProviderClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmProviderClient for ReqwestLlmProviderClient {
    async fn send(&self, request: LlmProviderRequest) -> Result<Response, LlmProviderError> {
        let url = join_base_and_path(&request.provider_base_url, &request.path)?;
        let method = reqwest::Method::from_bytes(request.method.as_str().as_bytes())
            .map_err(|error| LlmProviderError::Failed(error.to_string()))?;
        let timeout = provider_request_timeout(&request);
        let upstream = self
            .client
            .request(method, url)
            .timeout(timeout)
            .header(header::AUTHORIZATION, request.authorization)
            .header(header::CONTENT_TYPE, request.content_type)
            .body(request.body)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        response_from_reqwest(upstream).await
    }
}

fn provider_request_timeout(request: &LlmProviderRequest) -> Duration {
    if provider_request_is_streaming(&request.body) {
        // 流式 LLM 响应可能持续数分钟。这里保留管理员配置对普通请求的约束，
        // 但对 stream=true 的请求使用长超时，避免 Hub 在 60 秒处截断 Hermes 的上游流。
        return Duration::from_secs(request.timeout_seconds.max(3600));
    }

    Duration::from_secs(request.timeout_seconds.max(1))
}

fn provider_request_is_streaming(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

pub fn in_memory_client(response: LlmProviderResponse) -> InMemoryLlmProviderClient {
    InMemoryLlmProviderClient::new(response)
}

fn join_base_and_path(base: &str, path: &str) -> Result<reqwest::Url, LlmProviderError> {
    let mut url = reqwest::Url::parse(base).map_err(|_| LlmProviderError::InvalidUrl)?;

    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(LlmProviderError::InvalidUrl);
    }

    let base_path = url.path().trim_end_matches('/');
    let suffix = path.trim_start_matches('/');
    let joined = if base_path.is_empty() {
        format!("/{suffix}")
    } else {
        format!("{base_path}/{suffix}")
    };
    url.set_path(&joined);
    url.set_query(None);
    url.set_fragment(None);

    Ok(url)
}

async fn response_from_reqwest(upstream: reqwest::Response) -> Result<Response, LlmProviderError> {
    let status = StatusCode::from_u16(upstream.status().as_u16())
        .map_err(|error| LlmProviderError::Failed(error.to_string()))?;
    let headers = upstream.headers().clone();
    let body = Body::from_stream(
        upstream
            .bytes_stream()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error)),
    );
    let mut response = Response::builder()
        .status(status)
        .body(body)
        .map_err(|error| LlmProviderError::Failed(error.to_string()))?;

    copy_safe_response_headers(&headers, response.headers_mut())?;
    Ok(response)
}

pub fn copy_safe_response_headers(
    upstream: &HeaderMap,
    downstream: &mut HeaderMap,
) -> Result<(), LlmProviderError> {
    for name in [
        header::CONTENT_TYPE,
        header::CACHE_CONTROL,
        header::RETRY_AFTER,
        HeaderName::from_static("openai-request-id"),
        HeaderName::from_static("x-request-id"),
    ] {
        if let Some(value) = upstream.get(&name) {
            downstream.insert(name, value.clone());
        }
    }

    Ok(())
}

fn map_reqwest_error(error: reqwest::Error) -> LlmProviderError {
    if error.is_timeout() {
        LlmProviderError::Timeout
    } else if error.is_builder() {
        LlmProviderError::InvalidUrl
    } else {
        LlmProviderError::Failed(error.to_string())
    }
}
