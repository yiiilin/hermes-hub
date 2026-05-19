use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmProviderRequest {
    pub method: Method,
    pub provider_base_url: String,
    pub path: String,
    pub authorization: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmProviderResponse {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum LlmProviderError {
    #[error("provider proxy failed")]
    Failed,
}

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

    pub async fn send(
        &self,
        request: LlmProviderRequest,
    ) -> Result<LlmProviderResponse, LlmProviderError> {
        *self
            .last_request
            .lock()
            .map_err(|_| LlmProviderError::Failed)? = Some(request);
        Ok(self.response.clone())
    }

    pub fn last_request(&self) -> Option<LlmProviderRequest> {
        self.last_request.lock().ok()?.clone()
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
