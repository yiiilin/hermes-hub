use axum::{
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use hermes_hub_backend::{
    channel::service::ChannelStore,
    docker_config_from_app,
    hermes::docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
    ldap::DefaultLdapAuthenticator,
    llm_proxy::{
        DynLlmProviderClient, LlmProviderClient, LlmProviderError, LlmProviderRequest,
        LlmProviderResponse,
    },
    model_config::{
        ModelConfig, ModelFallbackConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE,
        LLM_MODEL_CONFIG_KIND, RESPONSES_API_TYPE, TITLE_MODEL_CONFIG_KIND,
    },
    session::store::SessionStore,
    storage::InMemoryObjectStorage,
    title_generation::model_generated_title,
    AppConfig, AppState,
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct SequenceLlmProviderClient {
    responses: Arc<Mutex<Vec<Result<LlmProviderResponse, LlmProviderError>>>>,
    requests: Arc<Mutex<Vec<LlmProviderRequest>>>,
}

impl SequenceLlmProviderClient {
    fn new(responses: Vec<Result<LlmProviderResponse, LlmProviderError>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn shared(self) -> DynLlmProviderClient {
        Arc::new(self)
    }

    fn requests(&self) -> Vec<LlmProviderRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

#[async_trait::async_trait]
impl LlmProviderClient for SequenceLlmProviderClient {
    async fn send(&self, request: LlmProviderRequest) -> Result<Response, LlmProviderError> {
        self.requests.lock().expect("requests lock").push(request);
        let response = self.responses.lock().expect("responses lock").remove(0)?;
        let mut axum_response = (response.status, response.body).into_response();
        if let Some(content_type) = response.content_type {
            axum_response.headers_mut().insert(
                header::CONTENT_TYPE,
                content_type.parse().expect("content type header"),
            );
        }
        Ok(axum_response)
    }
}

async fn test_state(provider: DynLlmProviderClient) -> AppState {
    let config = AppConfig::for_tests();
    let registry = ModelRegistry::new(ModelConfig {
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: "primary-provider".to_string(),
        provider_base_url: "https://primary.example/v1".to_string(),
        provider_api_key: "primary-token".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string()],
        api_type: CHAT_COMPLETIONS_API_TYPE.to_string(),
        reasoning_effort: None,
        enabled: true,
        allow_streaming: true,
        request_timeout_seconds: 60,
        context_window_tokens: 128_000,
        max_output_tokens: 4096,
        temperature: 0.7,
        supports_parallel_tools: true,
        fallback: Some(ModelFallbackConfig {
            enabled: true,
            provider_name: "fallback-provider".to_string(),
            provider_base_url: "https://fallback.example/v1".to_string(),
            provider_api_key: "fallback-token".to_string(),
            default_model: "gpt-4.1-fallback".to_string(),
            allowed_models: vec!["gpt-4.1-fallback".to_string()],
            api_type: RESPONSES_API_TYPE.to_string(),
            reasoning_effort: None,
            allow_streaming: false,
            request_timeout_seconds: 30,
            context_window_tokens: 64_000,
            max_output_tokens: 1024,
            temperature: 0.2,
            supports_parallel_tools: false,
        }),
    });
    registry
        .replace(ModelConfig {
            config_kind: TITLE_MODEL_CONFIG_KIND.to_string(),
            provider_name: "primary-title-provider".to_string(),
            provider_base_url: "https://title-primary.example/v1".to_string(),
            provider_api_key: "title-primary-token".to_string(),
            default_model: "gpt-4.1-mini".to_string(),
            allowed_models: vec!["gpt-4.1-mini".to_string()],
            api_type: RESPONSES_API_TYPE.to_string(),
            reasoning_effort: None,
            enabled: true,
            allow_streaming: false,
            request_timeout_seconds: 60,
            context_window_tokens: 128_000,
            max_output_tokens: 4096,
            temperature: 0.7,
            supports_parallel_tools: true,
            fallback: Some(ModelFallbackConfig {
                enabled: true,
                provider_name: "fallback-title-provider".to_string(),
                provider_base_url: "https://title-fallback.example/v1".to_string(),
                provider_api_key: "title-fallback-token".to_string(),
                default_model: "gpt-4.1-title-fallback".to_string(),
                allowed_models: vec!["gpt-4.1-title-fallback".to_string()],
                api_type: RESPONSES_API_TYPE.to_string(),
                reasoning_effort: None,
                allow_streaming: false,
                request_timeout_seconds: 30,
                context_window_tokens: 64_000,
                max_output_tokens: 1024,
                temperature: 0.2,
                supports_parallel_tools: false,
            }),
        })
        .await
        .expect("title config can be saved");

    let asr_client = hermes_hub_backend::asr::default_asr_client(&config.speech_input);
    AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
        ),
        config,
        store: SessionStore::default(),
        channel_store: ChannelStore::default(),
        model_registry: registry,
        llm_provider: provider,
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: Default::default(),
        asr_client,
    }
}

#[tokio::test]
async fn title_generation_uses_fallback_model_when_primary_provider_returns_error_status() {
    let provider = SequenceLlmProviderClient::new(vec![
        Ok(LlmProviderResponse {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            content_type: Some("application/json".to_string()),
            body: br#"{"error":"primary failed"}"#.to_vec(),
        }),
        Ok(LlmProviderResponse {
            status: StatusCode::OK,
            content_type: Some("application/json".to_string()),
            body: "{\"output_text\":\"备用标题\"}".as_bytes().to_vec(),
        }),
    ]);
    let provider_requests = provider.clone();
    let state = test_state(provider.shared()).await;

    let title = model_generated_title(&state, "user-1", "请给我一个标题").await;

    assert_eq!(title, "备用标题");
    let requests = provider_requests.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].provider_base_url,
        "https://title-primary.example/v1"
    );
    assert_eq!(requests[0].authorization, "Bearer title-primary-token");
    assert_eq!(
        requests[1].provider_base_url,
        "https://title-fallback.example/v1"
    );
    assert_eq!(requests[1].authorization, "Bearer title-fallback-token");
    let fallback_body: serde_json::Value =
        serde_json::from_slice(&requests[1].body).expect("fallback body is json");
    assert_eq!(fallback_body["model"], "gpt-4.1-title-fallback");
    assert_eq!(fallback_body["max_output_tokens"], 24);
}
