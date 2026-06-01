use async_trait::async_trait;
use reqwest::{multipart, StatusCode};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc, time::Duration};
use thiserror::Error;

use crate::app_config::SpeechInputConfig;

pub type DynAsrClient = Arc<dyn AsrClient>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AsrTranscriptionInput {
    pub file_name: String,
    pub content_type: String,
    pub file_path: PathBuf,
    pub size: u64,
    pub language: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AsrTranscription {
    pub text: String,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AsrError {
    #[error("speech input is disabled")]
    Disabled,
    #[error("asr endpoint is not configured")]
    EndpointMissing,
    #[error("asr request timed out")]
    Timeout,
    #[error("asr upstream failed {status}: {message}")]
    UpstreamStatus { status: u16, message: String },
    #[error("asr response is invalid")]
    InvalidResponse,
    #[error("asr request failed: {0}")]
    RequestFailed(String),
}

#[async_trait]
pub trait AsrClient: Send + Sync {
    async fn transcribe(&self, input: AsrTranscriptionInput) -> Result<AsrTranscription, AsrError>;
}

#[derive(Clone)]
pub struct ReqwestAsrClient {
    client: reqwest::Client,
    endpoint: Option<String>,
    transcribe_path: String,
    asr_model: String,
}

impl ReqwestAsrClient {
    pub fn from_config(config: &SpeechInputConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .expect("reqwest client configuration is valid");
        Self {
            client,
            endpoint: config.asr_endpoint.clone(),
            transcribe_path: config.transcribe_path.clone(),
            asr_model: config.asr_model.clone(),
        }
    }

    pub fn shared(self) -> DynAsrClient {
        Arc::new(self)
    }

    fn transcribe_url(&self) -> Result<String, AsrError> {
        let endpoint = self
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(AsrError::EndpointMissing)?;
        Ok(join_endpoint_path(endpoint, &self.transcribe_path))
    }
}

#[async_trait]
impl AsrClient for ReqwestAsrClient {
    async fn transcribe(&self, input: AsrTranscriptionInput) -> Result<AsrTranscription, AsrError> {
        let url = self.transcribe_url()?;
        // 语音文件已经由 HTTP 层落到私有临时文件；这里直接流式转发，避免大录音常驻内存。
        let file_part = multipart::Part::file(&input.file_path)
            .await
            .map_err(|error| AsrError::RequestFailed(error.to_string()))?
            .file_name(input.file_name)
            .mime_str(&input.content_type)
            .map_err(|error| AsrError::RequestFailed(error.to_string()))?;
        let mut form = multipart::Form::new()
            .part("file", file_part)
            .text("model", self.asr_model.clone())
            .text("response_format", "json");
        if let Some(language) = input.language {
            form = form.text("language", language);
        }

        let response = self
            .client
            .post(url)
            .multipart(form)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| AsrError::RequestFailed(error.to_string()))?;
        if !status.is_success() {
            return Err(AsrError::UpstreamStatus {
                status: status.as_u16(),
                message: String::from_utf8_lossy(&bytes).trim().to_string(),
            });
        }

        let payload: AsrResponse =
            serde_json::from_slice(&bytes).map_err(|_| AsrError::InvalidResponse)?;
        let text = payload
            .text
            .or(payload.transcript)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or(AsrError::InvalidResponse)?;
        Ok(AsrTranscription { text })
    }
}

#[derive(Deserialize)]
struct AsrResponse {
    text: Option<String>,
    transcript: Option<String>,
}

fn map_reqwest_error(error: reqwest::Error) -> AsrError {
    if error.is_timeout() {
        AsrError::Timeout
    } else if error.status() == Some(StatusCode::NOT_FOUND) {
        AsrError::UpstreamStatus {
            status: StatusCode::NOT_FOUND.as_u16(),
            message: "asr endpoint not found".to_string(),
        }
    } else {
        AsrError::RequestFailed(error.to_string())
    }
}

fn join_endpoint_path(endpoint: &str, path: &str) -> String {
    let trimmed_endpoint = endpoint.trim_end_matches('/');
    let trimmed_path = path.trim();
    if trimmed_path.is_empty() || trimmed_path == "/" {
        return trimmed_endpoint.to_string();
    }
    format!(
        "{}/{}",
        trimmed_endpoint,
        trimmed_path.trim_start_matches('/')
    )
}

pub fn default_asr_client(config: &SpeechInputConfig) -> DynAsrClient {
    ReqwestAsrClient::from_config(config).shared()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Multipart, State},
        routing::post,
        Json, Router,
    };
    use serde_json::json;
    use std::sync::Mutex;
    use tempfile::NamedTempFile;
    use tokio::{io::AsyncWriteExt, net::TcpListener};

    #[derive(Debug, Default)]
    struct ReceivedAsrRequest {
        path_hit: bool,
        fields: Vec<(String, String)>,
        file_name: Option<String>,
        file_content_type: Option<String>,
        file_bytes: Vec<u8>,
    }

    #[tokio::test]
    async fn reqwest_client_posts_openai_compatible_multipart() {
        let received = Arc::new(Mutex::new(ReceivedAsrRequest::default()));
        let app = Router::new()
            .route("/v1/audio/transcriptions", post(capture_asr_request))
            .with_state(received.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test ASR server");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test ASR");
        });

        let temp_file = NamedTempFile::new().expect("temp speech file");
        let mut async_file = tokio::fs::File::create(temp_file.path())
            .await
            .expect("create speech file");
        async_file
            .write_all(b"voice bytes")
            .await
            .expect("write speech file");
        drop(async_file);

        let config = SpeechInputConfig {
            enabled: true,
            asr_endpoint: Some(format!("http://{addr}")),
            transcribe_path: "/v1/audio/transcriptions".to_string(),
            asr_model: "sensevoice".to_string(),
            timeout_seconds: 10,
            max_audio_seconds: 60,
            max_upload_bytes: 1024,
        };
        let client = ReqwestAsrClient::from_config(&config);
        let result = client
            .transcribe(AsrTranscriptionInput {
                file_name: "recording.webm".to_string(),
                content_type: "audio/webm".to_string(),
                file_path: temp_file.path().to_path_buf(),
                size: 11,
                language: Some("zh".to_string()),
            })
            .await
            .expect("ASR request succeeds");

        assert_eq!(result.text, "转写结果");
        let received = received.lock().expect("received request");
        assert!(received.path_hit);
        assert_eq!(received.file_name.as_deref(), Some("recording.webm"));
        assert_eq!(received.file_content_type.as_deref(), Some("audio/webm"));
        assert_eq!(received.file_bytes, b"voice bytes");
        assert!(received
            .fields
            .contains(&("model".to_string(), "sensevoice".to_string())));
        assert!(received
            .fields
            .contains(&("response_format".to_string(), "json".to_string())));
        assert!(received
            .fields
            .contains(&("language".to_string(), "zh".to_string())));
    }

    async fn capture_asr_request(
        State(received): State<Arc<Mutex<ReceivedAsrRequest>>>,
        mut multipart: Multipart,
    ) -> Json<serde_json::Value> {
        let mut captured = ReceivedAsrRequest {
            path_hit: true,
            ..ReceivedAsrRequest::default()
        };
        while let Some(field) = multipart.next_field().await.expect("multipart field") {
            let name = field.name().unwrap_or_default().to_string();
            let file_name = field.file_name().map(ToString::to_string);
            let content_type = field.content_type().map(ToString::to_string);
            if let Some(file_name) = file_name {
                captured.file_name = Some(file_name);
                captured.file_content_type = content_type;
                captured.file_bytes = field.bytes().await.expect("file bytes").to_vec();
            } else {
                captured
                    .fields
                    .push((name, field.text().await.expect("text field")));
            }
        }
        *received.lock().expect("received request") = captured;
        Json(json!({ "text": "转写结果" }))
    }
}
