use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::HeaderMap,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite};

use crate::{
    asr::{stream_url_from_config, AsrStreamError, STREAM_SAMPLE_RATE},
    http::{auth::current_user, ApiError},
    AppState,
};

const STREAM_BYTES_PER_SAMPLE: u64 = 2;
const STREAM_MAX_BINARY_FRAME_SECONDS: u64 = 5;
const STREAM_MAX_CONTROL_MESSAGE_BYTES: usize = 16 * 1024;
const STREAM_FINALIZATION_TIMEOUT_CAP_SECONDS: u64 = 15;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/speech-input/config", get(get_speech_input_config))
        .route("/api/speech-input/stream", get(stream_speech_input))
}

#[derive(Serialize)]
struct SpeechInputConfigResponse {
    speech_input: PublicSpeechInputConfig,
}

#[derive(Serialize)]
struct PublicSpeechInputConfig {
    enabled: bool,
    runtime_available: bool,
    max_duration_seconds: u32,
    sample_rate: u32,
    model: String,
}

#[derive(Serialize)]
struct StreamErrorMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    message: &'a str,
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

async fn stream_speech_input(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError> {
    current_user(&state, &headers).await?;
    let speech_config = public_speech_input_config(&state).await?;
    if !speech_config.enabled {
        return Err(ApiError::ServiceUnavailable("speech input is disabled"));
    }

    Ok(ws.on_upgrade(move |socket| proxy_speech_stream(state, socket)))
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
        max_duration_seconds: state.config.speech_input.max_audio_seconds,
        sample_rate: STREAM_SAMPLE_RATE,
        model: state.config.speech_input.asr_model.clone(),
    })
}

async fn proxy_speech_stream(state: AppState, browser_socket: WebSocket) {
    let stream_url = match stream_url_from_config(&state.config.speech_input) {
        Ok(value) => value,
        Err(error) => {
            let _ = send_browser_error(browser_socket, asr_stream_error_message(error)).await;
            return;
        }
    };

    let connect_timeout = Duration::from_secs(state.config.speech_input.timeout_seconds.max(1));
    let upstream = match tokio::time::timeout(connect_timeout, connect_async(&stream_url)).await {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(error)) => {
            tracing::warn!(%stream_url, %error, "speech ASR stream connection failed");
            let _ = send_browser_error(browser_socket, "asr stream connection failed").await;
            return;
        }
        Err(error) => {
            tracing::warn!(%stream_url, %error, "speech ASR stream connection timed out");
            let _ = send_browser_error(browser_socket, "asr stream connection timed out").await;
            return;
        }
    };

    let max_duration = Duration::from_secs(u64::from(
        state.config.speech_input.max_audio_seconds.max(1),
    ));
    let finalization_timeout = Duration::from_secs(
        state
            .config
            .speech_input
            .timeout_seconds
            .clamp(1, STREAM_FINALIZATION_TIMEOUT_CAP_SECONDS),
    );
    let max_audio_bytes = stream_max_audio_bytes(state.config.speech_input.max_audio_seconds);
    let max_binary_frame_bytes = stream_max_binary_frame_bytes();
    let mut forwarded_audio_bytes = 0_u64;
    let (mut browser_tx, mut browser_rx) = browser_socket.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    let mut stream_timer = Box::pin(tokio::time::sleep(max_duration));
    let mut stream_timer_kind = StreamTimerKind::InputDuration;
    let mut browser_stop_requested = false;

    loop {
        tokio::select! {
            _ = &mut stream_timer => {
                let error_message = match stream_timer_kind {
                    StreamTimerKind::InputDuration => "speech input exceeded max duration",
                    StreamTimerKind::Finalization => "asr stream finalization timed out",
                };
                let _ = browser_tx
                    .send(Message::Text(stream_error_text(error_message).into()))
                    .await;
                let _ = browser_tx.send(Message::Close(None)).await;
                let _ = upstream_tx.send(tungstenite::Message::Close(None)).await;
                break;
            }
            browser_message = browser_rx.next() => {
                match browser_message {
                    Some(Ok(message)) => {
                        if browser_stop_requested {
                            if matches!(message, Message::Close(_)) {
                                let _ = upstream_tx.send(tungstenite::Message::Close(None)).await;
                                break;
                            }
                            // stop 后只等待 ASR 收尾；忽略浏览器迟到的音频/控制帧，避免重新驱动上游。
                            continue;
                        }
                        match forward_browser_message(
                            message,
                            &mut upstream_tx,
                            &mut forwarded_audio_bytes,
                            max_audio_bytes,
                            max_binary_frame_bytes,
                        )
                        .await
                        {
                            Ok(ForwardedBrowserMessage::Continue) => {}
                            Ok(ForwardedBrowserMessage::StopRequested) => {
                                browser_stop_requested = true;
                                stream_timer_kind = StreamTimerKind::Finalization;
                                stream_timer.as_mut().reset(
                                    tokio::time::Instant::now() + finalization_timeout,
                                );
                            }
                            Ok(ForwardedBrowserMessage::Closed) => {
                                break;
                            }
                            Err(BrowserMessageError::AudioLimitExceeded) => {
                                let _ = browser_tx
                                    .send(Message::Text(stream_error_text("speech input exceeded max audio size").into()))
                                    .await;
                                let _ = browser_tx.send(Message::Close(None)).await;
                                let _ = upstream_tx.send(tungstenite::Message::Close(None)).await;
                                break;
                            }
                            Err(BrowserMessageError::ControlMessageTooLarge) => {
                                let _ = browser_tx
                                    .send(Message::Text(stream_error_text("speech stream control message too large").into()))
                                    .await;
                                let _ = browser_tx.send(Message::Close(None)).await;
                                let _ = upstream_tx.send(tungstenite::Message::Close(None)).await;
                                break;
                            }
                            Err(BrowserMessageError::UpstreamSend) => {
                                let _ = browser_tx
                                    .send(Message::Text(stream_error_text("asr stream send failed").into()))
                                    .await;
                                let _ = browser_tx.send(Message::Close(None)).await;
                                break;
                            }
                        }
                    }
                    Some(Err(error)) => {
                        tracing::debug!(%error, "browser speech stream closed with error");
                        let _ = upstream_tx.send(tungstenite::Message::Close(None)).await;
                        break;
                    }
                    None => {
                        let _ = upstream_tx.send(tungstenite::Message::Close(None)).await;
                        break;
                    }
                }
            }
            upstream_message = upstream_rx.next() => {
                match upstream_message {
                    Some(Ok(message)) => {
                        match forward_upstream_message(message, &mut browser_tx).await {
                            Ok(ForwardedUpstreamMessage::Continue) => {}
                            Ok(ForwardedUpstreamMessage::Closed) => {
                                break;
                            }
                            Err(_) => {
                                let _ = upstream_tx.send(tungstenite::Message::Close(None)).await;
                                break;
                            }
                        }
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "asr speech stream closed with error");
                        let _ = browser_tx
                            .send(Message::Text(stream_error_text("asr stream failed").into()))
                            .await;
                        let _ = browser_tx.send(Message::Close(None)).await;
                        break;
                    }
                    None => {
                        let _ = browser_tx
                            .send(Message::Text(stream_error_text("asr stream closed").into()))
                            .await;
                        let _ = browser_tx.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
        }
    }
}

enum StreamTimerKind {
    InputDuration,
    Finalization,
}

enum ForwardedBrowserMessage {
    Continue,
    StopRequested,
    Closed,
}

enum BrowserMessageError {
    AudioLimitExceeded,
    ControlMessageTooLarge,
    UpstreamSend,
}

impl From<tungstenite::Error> for BrowserMessageError {
    fn from(_error: tungstenite::Error) -> Self {
        Self::UpstreamSend
    }
}

async fn forward_browser_message<S>(
    message: Message,
    upstream_tx: &mut S,
    forwarded_audio_bytes: &mut u64,
    max_audio_bytes: u64,
    max_binary_frame_bytes: u64,
) -> Result<ForwardedBrowserMessage, BrowserMessageError>
where
    S: futures_util::Sink<tungstenite::Message, Error = tungstenite::Error> + Unpin,
{
    match message {
        Message::Text(value) => {
            if value.len() > STREAM_MAX_CONTROL_MESSAGE_BYTES {
                return Err(BrowserMessageError::ControlMessageTooLarge);
            }
            let text = value.to_string();
            let is_stop = is_stop_control_message(&text);
            upstream_tx
                .send(tungstenite::Message::Text(text.into()))
                .await?;
            Ok(if is_stop {
                ForwardedBrowserMessage::StopRequested
            } else {
                ForwardedBrowserMessage::Continue
            })
        }
        Message::Binary(value) => {
            let frame_len = value.len() as u64;
            // 浏览器只应发送实时 PCM 小块；这里同时限制单帧和总音频量，避免登录用户灌入大帧拖垮 ASR。
            if frame_len > max_binary_frame_bytes
                || forwarded_audio_bytes.saturating_add(frame_len) > max_audio_bytes
            {
                return Err(BrowserMessageError::AudioLimitExceeded);
            }
            *forwarded_audio_bytes += frame_len;
            upstream_tx
                .send(tungstenite::Message::Binary(value))
                .await?;
            Ok(ForwardedBrowserMessage::Continue)
        }
        Message::Ping(value) => {
            upstream_tx.send(tungstenite::Message::Ping(value)).await?;
            Ok(ForwardedBrowserMessage::Continue)
        }
        Message::Pong(value) => {
            upstream_tx.send(tungstenite::Message::Pong(value)).await?;
            Ok(ForwardedBrowserMessage::Continue)
        }
        Message::Close(frame) => {
            let _ = frame;
            upstream_tx.send(tungstenite::Message::Close(None)).await?;
            Ok(ForwardedBrowserMessage::Closed)
        }
    }
}

fn is_stop_control_message(value: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()
        .and_then(|message| {
            message
                .get("type")
                .and_then(|kind| kind.as_str())
                .map(|kind| kind == "stop")
        })
        .unwrap_or(false)
}

enum ForwardedUpstreamMessage {
    Continue,
    Closed,
}

async fn forward_upstream_message<S>(
    message: tungstenite::Message,
    browser_tx: &mut S,
) -> Result<ForwardedUpstreamMessage, axum::Error>
where
    S: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    match message {
        tungstenite::Message::Text(value) => {
            browser_tx
                .send(Message::Text(value.to_string().into()))
                .await?;
            Ok(ForwardedUpstreamMessage::Continue)
        }
        tungstenite::Message::Binary(value) => {
            browser_tx.send(Message::Binary(value)).await?;
            Ok(ForwardedUpstreamMessage::Continue)
        }
        tungstenite::Message::Ping(value) => {
            browser_tx.send(Message::Ping(value)).await?;
            Ok(ForwardedUpstreamMessage::Continue)
        }
        tungstenite::Message::Pong(value) => {
            browser_tx.send(Message::Pong(value)).await?;
            Ok(ForwardedUpstreamMessage::Continue)
        }
        tungstenite::Message::Close(frame) => {
            let _ = frame;
            browser_tx.send(Message::Close(None)).await?;
            Ok(ForwardedUpstreamMessage::Closed)
        }
        // sherpa 服务不会发送这些帧；统一忽略，避免把内部控制帧暴露给浏览器。
        tungstenite::Message::Frame(_) => Ok(ForwardedUpstreamMessage::Continue),
    }
}

async fn send_browser_error(
    mut browser_socket: WebSocket,
    message: &str,
) -> Result<(), axum::Error> {
    browser_socket
        .send(Message::Text(stream_error_text(message).into()))
        .await?;
    browser_socket.send(Message::Close(None)).await
}

fn stream_error_text(message: &str) -> String {
    serde_json::to_string(&StreamErrorMessage {
        kind: "error",
        message,
    })
    .expect("stream error message serializes")
}

fn asr_stream_error_message(error: AsrStreamError) -> &'static str {
    match error {
        AsrStreamError::EndpointMissing => "speech input is disabled",
        AsrStreamError::UnsupportedScheme => "asr endpoint scheme is unsupported",
    }
}

fn stream_max_audio_bytes(max_audio_seconds: u32) -> u64 {
    u64::from(STREAM_SAMPLE_RATE)
        .saturating_mul(u64::from(max_audio_seconds.max(1)))
        .saturating_mul(STREAM_BYTES_PER_SAMPLE)
}

fn stream_max_binary_frame_bytes() -> u64 {
    u64::from(STREAM_SAMPLE_RATE)
        .saturating_mul(STREAM_MAX_BINARY_FRAME_SECONDS)
        .saturating_mul(STREAM_BYTES_PER_SAMPLE)
}
