use axum::{
    extract::{DefaultBodyLimit, Multipart, State},
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use std::path::PathBuf;

use crate::{
    asr::{AsrError, AsrTranscriptionInput},
    http::{
        attachments::{
            drain_multipart_field_with_limit, read_multipart_text_field_with_limit,
            spool_multipart_file_to_temp_with_limit,
        },
        auth::current_user,
        ApiError,
    },
    AppState,
};

const SPEECH_INPUT_METADATA_MAX_BYTES: usize = 64 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/speech-input/config", get(get_speech_input_config))
        .route(
            "/api/speech-input/transcriptions",
            post(transcribe_speech_input).layer(DefaultBodyLimit::disable()),
        )
}

#[derive(Serialize)]
struct SpeechInputConfigResponse {
    speech_input: PublicSpeechInputConfig,
}

#[derive(Serialize)]
struct PublicSpeechInputConfig {
    enabled: bool,
    runtime_available: bool,
    max_audio_seconds: u32,
    max_upload_bytes: usize,
}

#[derive(Serialize)]
struct SpeechTranscriptionResponse {
    text: String,
}

async fn get_speech_input_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    current_user(&state, &headers).await?;
    Ok(Json(SpeechInputConfigResponse {
        speech_input: public_speech_input_config(&state).await?,
    }))
}

async fn transcribe_speech_input(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<impl IntoResponse, ApiError> {
    current_user(&state, &headers).await?;
    let speech_config = public_speech_input_config(&state).await?;
    if !speech_config.enabled {
        return Err(ApiError::ServiceUnavailable("speech input is disabled"));
    }

    let input = read_speech_input_multipart(&state, multipart).await?;
    let mut cleanup = SpeechInputTempPathGuard::new(input.file_path.clone());
    let result = state.asr_client.transcribe(input).await;
    cleanup.cleanup().await;
    let result = result.map_err(map_asr_error)?;
    Ok(Json(SpeechTranscriptionResponse { text: result.text }))
}

async fn public_speech_input_config(state: &AppState) -> Result<PublicSpeechInputConfig, ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    let runtime_available = state.config.speech_input.available();
    Ok(PublicSpeechInputConfig {
        enabled: runtime_available && settings.speech_input.enabled,
        runtime_available,
        max_audio_seconds: state.config.speech_input.max_audio_seconds,
        max_upload_bytes: state.config.speech_input.max_upload_bytes,
    })
}

struct PendingSpeechAudio {
    file: crate::http::attachments::SpooledMultipartFile,
}

struct SpeechInputTempPathGuard {
    path: Option<PathBuf>,
}

impl SpeechInputTempPathGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    async fn cleanup(&mut self) {
        if let Some(path) = self.path.clone() {
            let _ = tokio::fs::remove_file(&path).await;
            if self.path.as_ref() == Some(&path) {
                self.path.take();
            }
        }
    }
}

impl Drop for SpeechInputTempPathGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            // handler future 被取消时也要释放已落盘的音频临时文件。
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn read_speech_input_multipart(
    state: &AppState,
    mut multipart: Multipart,
) -> Result<AsrTranscriptionInput, ApiError> {
    let mut audio: Option<PendingSpeechAudio> = None;
    let mut language: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::BadRequest("multipart body is invalid"))?
    {
        let field_name = field.name().unwrap_or_default().to_string();
        if field.file_name().is_none() {
            if field_name == "language" {
                let value = read_multipart_text_field_with_limit(
                    field,
                    SPEECH_INPUT_METADATA_MAX_BYTES,
                    "multipart field is too large",
                )
                .await?;
                let value = value.trim().to_string();
                if !value.is_empty() {
                    language = Some(value);
                }
            } else {
                drain_multipart_field_with_limit(
                    field,
                    SPEECH_INPUT_METADATA_MAX_BYTES,
                    "multipart field is too large",
                )
                .await?;
            }
            continue;
        }
        if audio.is_some() {
            return Err(ApiError::BadRequest("only one audio file is allowed"));
        }

        let mut file = spool_multipart_file_to_temp_with_limit(
            field,
            Some(state.config.speech_input.max_upload_bytes),
        )
        .await?;
        if !speech_content_type_is_allowed(&file.content_type) {
            file.cleanup().await;
            return Err(ApiError::BadRequest("speech input must be an audio file"));
        }
        audio = Some(PendingSpeechAudio { file });
    }

    let audio = audio.ok_or(ApiError::BadRequest("speech input audio file is required"))?;
    let file = audio.file;
    let file_name = file.file_name.clone();
    let content_type = file.content_type.clone();
    let size = file.size;
    let file_path = file.into_path();
    Ok(AsrTranscriptionInput {
        file_name,
        content_type,
        size,
        file_path,
        language,
    })
}

fn speech_content_type_is_allowed(content_type: &str) -> bool {
    let normalized = content_type.to_ascii_lowercase();
    normalized.starts_with("audio/") || normalized == "application/octet-stream"
}

fn map_asr_error(error: AsrError) -> ApiError {
    match error {
        AsrError::Disabled | AsrError::EndpointMissing => {
            ApiError::ServiceUnavailable("speech input is disabled")
        }
        AsrError::Timeout => ApiError::GatewayTimeout("asr request timed out"),
        AsrError::UpstreamStatus { status, message } => {
            ApiError::BadGatewayMessage(format!("asr upstream failed {status}: {message}"))
        }
        AsrError::InvalidResponse => ApiError::BadGateway("asr response is invalid"),
        AsrError::RequestFailed(message) => {
            ApiError::BadGatewayMessage(format!("asr request failed: {message}"))
        }
    }
}
