use axum::{body::to_bytes, http::Method};
use serde_json::{json, Value};
use std::time::Instant;

use crate::{
    llm_proxy::LlmProviderRequest,
    model_config::{ModelConfig, ModelFallbackConfig, RESPONSES_API_TYPE, TITLE_MODEL_CONFIG_KIND},
    session::store::LlmUsageEvent,
    AppState,
};

struct TitleProviderCall {
    request: LlmProviderRequest,
    model: String,
    upstream_provider: String,
}

pub async fn model_generated_title(state: &AppState, user_id: &str, prompt: &str) -> String {
    let fallback = fallback_title(prompt);
    let Ok(config) = state
        .model_registry
        .config_for_kind(TITLE_MODEL_CONFIG_KIND)
        .await
    else {
        return fallback;
    };

    if let Some(title) = request_model_title(
        state,
        user_id,
        prompt,
        title_provider_call_from_model_config(&config, prompt),
    )
    .await
    {
        return title;
    }

    if let Some(fallback_config) = config.fallback.as_ref().filter(|fallback| fallback.enabled) {
        if let Some(title) = request_model_title(
            state,
            user_id,
            prompt,
            title_provider_call_from_fallback_config(fallback_config, prompt),
        )
        .await
        {
            return title;
        }
    }

    fallback
}

async fn request_model_title(
    state: &AppState,
    user_id: &str,
    prompt: &str,
    call: TitleProviderCall,
) -> Option<String> {
    let started = Instant::now();
    let response = state.llm_provider.send(call.request).await;

    match response {
        Ok(response) => {
            let status = response.status();
            let status_code = status.as_u16();
            let is_success = status.is_success();
            let bytes = to_bytes(response.into_body(), 128 * 1024).await.ok();
            record_title_usage(
                state,
                user_id,
                call.model,
                call.upstream_provider,
                Some(status_code),
                started.elapsed().as_millis() as u64,
            )
            .await;
            if !is_success {
                return None;
            }
            bytes
                .as_deref()
                .and_then(|bytes| parse_title_response(bytes, prompt))
        }
        Err(_) => {
            record_title_usage(
                state,
                user_id,
                call.model,
                call.upstream_provider,
                None,
                started.elapsed().as_millis() as u64,
            )
            .await;
            None
        }
    }
}

async fn record_title_usage(
    state: &AppState,
    user_id: &str,
    model: String,
    upstream_provider: String,
    status_code: Option<u16>,
    duration_ms: u64,
) {
    let _ = state
        .store
        .record_llm_usage(LlmUsageEvent {
            user_id: Some(user_id.to_string()),
            hermes_instance_id: None,
            model,
            upstream_provider,
            status_code,
            duration_ms: Some(duration_ms),
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
        })
        .await;
}

fn title_provider_call_from_model_config(config: &ModelConfig, prompt: &str) -> TitleProviderCall {
    let (path, body) = title_generation_request(config, prompt);
    TitleProviderCall {
        request: LlmProviderRequest {
            method: Method::POST,
            provider_base_url: config.provider_base_url.clone(),
            path,
            authorization: format!("Bearer {}", config.provider_api_key),
            content_type: "application/json".to_string(),
            body: serde_json::to_vec(&body).unwrap_or_default(),
            timeout_seconds: config.request_timeout_seconds,
        },
        model: config.default_model.clone(),
        upstream_provider: config.provider_name.clone(),
    }
}

fn title_provider_call_from_fallback_config(
    config: &ModelFallbackConfig,
    prompt: &str,
) -> TitleProviderCall {
    let (path, body) = title_generation_request_from_fields(
        &config.default_model,
        &config.api_type,
        config.reasoning_effort.as_deref(),
        prompt,
    );
    TitleProviderCall {
        request: LlmProviderRequest {
            method: Method::POST,
            provider_base_url: config.provider_base_url.clone(),
            path,
            authorization: format!("Bearer {}", config.provider_api_key),
            content_type: "application/json".to_string(),
            body: serde_json::to_vec(&body).unwrap_or_default(),
            timeout_seconds: config.request_timeout_seconds,
        },
        model: config.default_model.clone(),
        upstream_provider: config.provider_name.clone(),
    }
}

pub(crate) fn parse_title_response(bytes: &[u8], prompt: &str) -> Option<String> {
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    let title = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/output_text").and_then(Value::as_str))?;
    clean_generated_title(title, prompt)
}

pub(crate) fn title_generation_request(config: &ModelConfig, prompt: &str) -> (String, Value) {
    title_generation_request_from_fields(
        &config.default_model,
        &config.api_type,
        config.reasoning_effort.as_deref(),
        prompt,
    )
}

fn title_generation_request_from_fields(
    model: &str,
    api_type: &str,
    reasoning_effort: Option<&str>,
    prompt: &str,
) -> (String, Value) {
    if api_type == RESPONSES_API_TYPE {
        let mut body = json!({
            "model": model,
            "stream": false,
            "max_output_tokens": 24,
            "input": [
                {
                    "role": "system",
                    "content": title_generation_system_prompt()
                },
                {
                    "role": "user",
                    "content": title_generation_user_prompt(prompt)
                }
            ]
        });
        if let Some(effort) = reasoning_effort {
            body["reasoning"] = json!({ "effort": effort });
        }
        return ("/responses".to_string(), body);
    }

    let mut body = json!({
        "model": model,
        "stream": false,
        "max_tokens": 24,
        "messages": [
            {
                "role": "system",
                "content": title_generation_system_prompt()
            },
            {
                "role": "user",
                "content": title_generation_user_prompt(prompt)
            }
        ]
    });
    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }

    ("/chat/completions".to_string(), body)
}

fn title_generation_system_prompt() -> &'static str {
    "你是会话标题生成器，不是问答助手。根据用户第一条消息生成一个短标题。只输出标题本身；不要回答用户问题；不要解释；不要使用“我/你/可以/能”等回答式措辞；不要句号。标题用中文优先，2 到 12 个汉字或最多 6 个英文词。"
}

fn title_generation_user_prompt(prompt: &str) -> String {
    format!("用户第一条消息：{prompt}\n\n请生成短标题，只输出标题。")
}

fn fallback_title(prompt: &str) -> String {
    clean_title(prompt).unwrap_or_else(|| "New conversation".to_string())
}

pub(crate) fn clean_generated_title(value: &str, prompt: &str) -> Option<String> {
    let title = clean_title(value)?;
    if title_looks_like_answer(&title) {
        return Some(fallback_title(prompt));
    }
    Some(title)
}

fn clean_title(value: &str) -> Option<String> {
    let title = value
        .lines()
        .next()
        .unwrap_or(value)
        .trim()
        .trim_matches('"')
        .trim_start_matches("标题：")
        .trim_start_matches("标题:")
        .trim()
        .chars()
        .take(48)
        .collect::<String>();

    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

fn title_looks_like_answer(title: &str) -> bool {
    let normalized = title.trim();
    let lowered = normalized.to_ascii_lowercase();

    // 标题模型偶尔会直接回答用户问题；这类输出宁可回退到用户原话，也不要污染会话列表。
    normalized.starts_with("能，")
        || normalized.starts_with("可以")
        || normalized.starts_with("是的")
        || normalized.starts_with("当然")
        || normalized.starts_with("不能")
        || normalized.contains("我可以")
        || normalized.contains("我能")
        || normalized.contains("帮你")
        || lowered.starts_with("yes")
        || lowered.starts_with("no,")
        || lowered.starts_with("i can")
}

#[cfg(test)]
mod tests {
    use super::{clean_generated_title, title_generation_request};
    use crate::model_config::{ModelConfig, RESPONSES_API_TYPE, TITLE_MODEL_CONFIG_KIND};

    #[test]
    fn generated_title_falls_back_when_model_answers_question() {
        let title = clean_generated_title(
            "能，我可以帮你生成图示、流程图、ASCII 图，或者用 Mermaid 画图。",
            "你能画图吗？",
        )
        .expect("title should be cleaned");

        assert_eq!(title, "你能画图吗？");
    }

    #[test]
    fn title_generation_prompt_tells_model_not_to_answer() {
        let config = ModelConfig {
            config_kind: TITLE_MODEL_CONFIG_KIND.to_string(),
            provider_name: "openai-compatible".to_string(),
            provider_base_url: "https://provider.example/v1".to_string(),
            provider_api_key: "secret".to_string(),
            default_model: "gpt-4.1-mini".to_string(),
            allowed_models: vec!["gpt-4.1-mini".to_string()],
            api_type: RESPONSES_API_TYPE.to_string(),
            reasoning_effort: None,
            enabled: true,
            allow_streaming: false,
            request_timeout_seconds: 30,
            context_window_tokens: 128_000,
            max_output_tokens: 4096,
            temperature: 0.7,
            supports_parallel_tools: true,
            fallback: None,
        };

        let (_path, body) = title_generation_request(&config, "你能画图吗？");
        let system = body["input"][0]["content"].as_str().expect("system prompt");

        assert!(system.contains("不是问答助手"));
        assert!(system.contains("不要回答用户问题"));
        assert_eq!(body["max_output_tokens"], 24);
    }
}
