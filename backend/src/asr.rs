use thiserror::Error;

use crate::app_config::SpeechInputConfig;

pub const STREAM_SAMPLE_RATE: u32 = 16_000;
const STREAM_PATH: &str = "/stream";

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AsrStreamError {
    #[error("asr endpoint is not configured")]
    EndpointMissing,
    #[error("asr endpoint must use http, https, ws, or wss")]
    UnsupportedScheme,
}

pub fn stream_url_from_config(config: &SpeechInputConfig) -> Result<String, AsrStreamError> {
    let endpoint = config
        .asr_endpoint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(AsrStreamError::EndpointMissing)?;
    let ws_endpoint = endpoint_to_ws(endpoint)?;
    if endpoint_has_stream_path(&ws_endpoint) {
        return Ok(ws_endpoint.trim_end_matches('/').to_string());
    }
    Ok(join_endpoint_path(&ws_endpoint, STREAM_PATH))
}

fn endpoint_to_ws(endpoint: &str) -> Result<String, AsrStreamError> {
    if let Some(rest) = endpoint.strip_prefix("http://") {
        return Ok(format!("ws://{rest}"));
    }
    if let Some(rest) = endpoint.strip_prefix("https://") {
        return Ok(format!("wss://{rest}"));
    }
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Ok(endpoint.to_string());
    }
    Err(AsrStreamError::UnsupportedScheme)
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

fn endpoint_has_stream_path(endpoint: &str) -> bool {
    let Some((_, after_scheme)) = endpoint.split_once("://") else {
        return false;
    };
    let Some((_, path)) = after_scheme.split_once('/') else {
        return false;
    };
    let path = path.split('?').next().unwrap_or(path);
    path.trim_end_matches('/') == "stream" || path.trim_end_matches('/').ends_with("/stream")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_endpoint(endpoint: &str) -> SpeechInputConfig {
        SpeechInputConfig {
            enabled: true,
            asr_endpoint: Some(endpoint.to_string()),
            asr_model: "streaming-zh".to_string(),
            timeout_seconds: 30,
            max_audio_seconds: 60,
        }
    }

    #[test]
    fn stream_url_reuses_existing_endpoint_with_fixed_path() {
        assert_eq!(
            stream_url_from_config(&config_with_endpoint("http://asr:9991")).expect("url"),
            "ws://asr:9991/stream"
        );
        assert_eq!(
            stream_url_from_config(&config_with_endpoint("https://asr.example/base/"))
                .expect("url"),
            "wss://asr.example/base/stream"
        );
        assert_eq!(
            stream_url_from_config(&config_with_endpoint("http://asr:9991/stream")).expect("url"),
            "ws://asr:9991/stream"
        );
        assert_eq!(
            stream_url_from_config(&config_with_endpoint("wss://asr.example/stream/"))
                .expect("url"),
            "wss://asr.example/stream"
        );
    }
}
