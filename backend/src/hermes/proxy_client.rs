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
use futures_util::{StreamExt, TryStreamExt};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermesProxyRequest {
    pub method: Method,
    pub instance_base_url: String,
    pub path_and_query: String,
    pub authorization: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermesProxyResponse {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum HermesProxyError {
    #[error("proxy lock failed")]
    LockFailed,
    #[error("hermes base url is invalid")]
    InvalidUrl,
    #[error("hermes request timed out")]
    Timeout,
    #[error("proxy failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait HermesProxyClient: Send + Sync {
    async fn send(&self, request: HermesProxyRequest) -> Result<Response, HermesProxyError>;
}

pub type DynHermesProxyClient = Arc<dyn HermesProxyClient>;

#[derive(Clone)]
pub struct InMemoryHermesProxyClient {
    response: HermesProxyResponse,
    last_request: Arc<Mutex<Option<HermesProxyRequest>>>,
}

impl InMemoryHermesProxyClient {
    pub fn new(response: HermesProxyResponse) -> Self {
        Self {
            response,
            last_request: Arc::new(Mutex::new(None)),
        }
    }

    pub fn shared(self) -> DynHermesProxyClient {
        Arc::new(self)
    }

    fn response(&self) -> Result<Response, HermesProxyError> {
        let mut response = (self.response.status, self.response.body.clone()).into_response();

        if let Some(content_type) = &self.response.content_type {
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(content_type)
                    .map_err(|error| HermesProxyError::Failed(error.to_string()))?,
            );
        }

        Ok(response)
    }

    pub fn last_request(&self) -> Option<HermesProxyRequest> {
        self.last_request.lock().ok()?.clone()
    }
}

#[async_trait]
impl HermesProxyClient for InMemoryHermesProxyClient {
    async fn send(&self, request: HermesProxyRequest) -> Result<Response, HermesProxyError> {
        *self
            .last_request
            .lock()
            .map_err(|_| HermesProxyError::LockFailed)? = Some(request);
        self.response()
    }
}

impl Default for InMemoryHermesProxyClient {
    fn default() -> Self {
        Self::new(HermesProxyResponse {
            status: StatusCode::OK,
            content_type: Some("application/json".to_string()),
            body: b"{}".to_vec(),
        })
    }
}

/// 生产使用的 Hermes HTTP 代理客户端。
///
/// Hub 只在这里追加实例级 Authorization，不把浏览器 Cookie 或用户输入头透传给
/// Hermes，避免跨租户状态和敏感头泄漏。
#[derive(Clone)]
pub struct ReqwestHermesProxyClient {
    client: reqwest::Client,
}

impl ReqwestHermesProxyClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    pub fn shared(self) -> DynHermesProxyClient {
        Arc::new(self)
    }
}

impl Default for ReqwestHermesProxyClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HermesProxyClient for ReqwestHermesProxyClient {
    async fn send(&self, request: HermesProxyRequest) -> Result<Response, HermesProxyError> {
        let url = join_base_and_path(&request.instance_base_url, &request.path_and_query)?;
        let method = reqwest::Method::from_bytes(request.method.as_str().as_bytes())
            .map_err(|error| HermesProxyError::Failed(error.to_string()))?;
        let mut builder = self
            .client
            .request(method, url)
            .timeout(Duration::from_secs(request.timeout_seconds.max(1)))
            .body(request.body);

        if let Some(authorization) = request.authorization {
            builder = builder.header(header::AUTHORIZATION, authorization);
        }
        if let Some(content_type) = request.content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }

        let upstream = builder.send().await.map_err(map_reqwest_error)?;
        response_from_reqwest(upstream).await
    }
}

fn join_base_and_path(base: &str, path_and_query: &str) -> Result<reqwest::Url, HermesProxyError> {
    let mut url = reqwest::Url::parse(base).map_err(|_| HermesProxyError::InvalidUrl)?;

    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(HermesProxyError::InvalidUrl);
    }

    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
    let base_path = url.path().trim_end_matches('/');
    let suffix = path.trim_start_matches('/');
    let joined = if base_path.is_empty() {
        format!("/{suffix}")
    } else {
        format!("{base_path}/{suffix}")
    };

    url.set_path(&joined);
    url.set_query(query);
    url.set_fragment(None);
    Ok(url)
}

async fn response_from_reqwest(upstream: reqwest::Response) -> Result<Response, HermesProxyError> {
    let status = StatusCode::from_u16(upstream.status().as_u16())
        .map_err(|error| HermesProxyError::Failed(error.to_string()))?;
    let headers = upstream.headers().clone();
    let is_event_stream = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));
    let upstream_stream = upstream.bytes_stream();
    let body = if is_event_stream {
        // Hermes 生图时偶发在已输出 run.completed 后提前结束 chunked body。
        // 对 SSE 来说，保留已收到事件并正常结束比把底层读取错误传给浏览器更可靠。
        Body::from_stream(upstream_stream.filter_map(|item| async move {
            match item {
                Ok(bytes) => Some(Ok::<_, std::io::Error>(bytes)),
                Err(error) => {
                    tracing::warn!(error = %error, "hermes event stream ended with upstream read error");
                    None
                }
            }
        }))
    } else {
        Body::from_stream(
            upstream_stream.map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error)),
        )
    };
    let mut response = Response::builder()
        .status(status)
        .body(body)
        .map_err(|error| HermesProxyError::Failed(error.to_string()))?;

    copy_safe_response_headers(&headers, response.headers_mut());
    Ok(response)
}

fn copy_safe_response_headers(upstream: &HeaderMap, downstream: &mut HeaderMap) {
    for name in [
        header::CONTENT_TYPE,
        header::CACHE_CONTROL,
        header::RETRY_AFTER,
        HeaderName::from_static("x-request-id"),
    ] {
        if let Some(value) = upstream.get(&name) {
            downstream.insert(name, value.clone());
        }
    }
}

fn map_reqwest_error(error: reqwest::Error) -> HermesProxyError {
    if error.is_timeout() {
        HermesProxyError::Timeout
    } else if error.is_builder() {
        HermesProxyError::InvalidUrl
    } else {
        HermesProxyError::Failed(error.to_string())
    }
}
