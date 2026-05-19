use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermesProxyRequest {
    pub method: Method,
    pub instance_base_url: String,
    pub path_and_query: String,
    pub authorization: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermesProxyResponse {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum HermesProxyError {
    #[error("proxy failed")]
    Failed,
}

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

    pub async fn send(
        &self,
        request: HermesProxyRequest,
    ) -> Result<HermesProxyResponse, HermesProxyError> {
        *self
            .last_request
            .lock()
            .map_err(|_| HermesProxyError::Failed)? = Some(request);
        Ok(self.response.clone())
    }

    pub fn last_request(&self) -> Option<HermesProxyRequest> {
        self.last_request.lock().ok()?.clone()
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
