use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, HeaderMap, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::ChannelStore,
    docker_config_from_app,
    hermes::docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
    llm_proxy::{
        DynLlmProviderClient, InMemoryLlmProviderClient, LlmProviderResponse,
        ReqwestLlmProviderClient,
    },
    model_config::{
        ModelConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE, IMAGES_GENERATIONS_API_TYPE,
        IMAGE_MODEL_CONFIG_KIND, LLM_MODEL_CONFIG_KIND, RESPONSES_API_TYPE,
    },
    session::store::SessionStore,
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tower::ServiceExt;

fn test_state(provider: InMemoryLlmProviderClient, registry: ModelRegistry) -> AppState {
    test_state_with_provider(provider.shared(), registry)
}

fn test_state_with_provider(provider: DynLlmProviderClient, registry: ModelRegistry) -> AppState {
    let config = AppConfig::for_tests();
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
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: Default::default(),
    }
}

fn test_app(provider: InMemoryLlmProviderClient, registry: ModelRegistry) -> Router {
    build_router_with_state(test_state(provider, registry))
}

fn test_registry() -> ModelRegistry {
    ModelRegistry::new(ModelConfig {
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: "openai-compatible".to_string(),
        provider_base_url: "https://provider.example/v1".to_string(),
        provider_api_key: "provider-secret".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string(), "gpt-4.1".to_string()],
        api_type: CHAT_COMPLETIONS_API_TYPE.to_string(),
        reasoning_effort: None,
        enabled: true,
        allow_streaming: true,
        request_timeout_seconds: 60,
        context_window_tokens: 128_000,
        max_output_tokens: 4096,
        temperature: 0.7,
        supports_parallel_tools: true,
    })
}

async fn request_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Value,
    token: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");

    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }

    app.clone()
        .oneshot(
            builder
                .body(Body::from(body.to_string()))
                .expect("request can be built"),
        )
        .await
        .expect("router responds")
}

async fn request_empty(
    app: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);

    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }

    app.clone()
        .oneshot(builder.body(Body::empty()).expect("request can be built"))
        .await
        .expect("router responds")
}

async fn response_json(response: Response<Body>) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body can be read");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response is json")
    };

    (status, value)
}

#[derive(Clone, Default)]
struct CapturedProviderRequest {
    authorization: Arc<Mutex<Option<String>>>,
    body: Arc<Mutex<Option<Value>>>,
}

async fn provider_handler(
    State(captured): State<CapturedProviderRequest>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    let bytes = to_bytes(body, usize::MAX)
        .await
        .expect("provider body can be read");
    *captured.authorization.lock().expect("auth lock") = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    *captured.body.lock().expect("body lock") = serde_json::from_slice::<Value>(&bytes).ok();

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        "data: real-provider\n\n",
    )
}

async fn slow_stream_provider_handler() -> impl IntoResponse {
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        "data: slow-provider\n\n",
    )
}

async fn spawn_provider_server(captured: CapturedProviderRequest) -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(provider_handler))
        .route("/v1/responses", post(provider_handler))
        .with_state(captured);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test provider can bind");
    let addr = listener.local_addr().expect("test provider addr");

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("test provider server runs");
    });

    format!("http://{addr}")
}

async fn spawn_slow_stream_provider_server() -> String {
    let app = Router::new().route("/v1/chat/completions", post(slow_stream_provider_handler));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test provider can bind");
    let addr = listener.local_addr().expect("test provider addr");

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("test provider server runs");
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn llm_proxy_test() {
    let provider = InMemoryLlmProviderClient::new(LlmProviderResponse {
        status: StatusCode::OK,
        content_type: Some("text/event-stream".to_string()),
        body: "data: ok\n\n".as_bytes().to_vec(),
    });
    let registry = test_registry();
    registry.add_instance_token("instance-token");
    let app = test_app(provider.clone(), registry);

    let unauthorized = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/chat/completions",
        json!({ "messages": [] }),
        None,
    )
    .await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let models = request_empty(
        &app,
        Method::GET,
        "/internal/llm/v1/models",
        Some("instance-token"),
    )
    .await;
    let (status, body) = response_json(models).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"][0]["id"], "gpt-4.1-mini");
    assert_eq!(body["data"][1]["id"], "gpt-4.1");

    let chat = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/chat/completions",
        json!({ "messages": [], "stream": true }),
        Some("instance-token"),
    )
    .await;
    assert_eq!(chat.status(), StatusCode::OK);
    assert_eq!(
        chat.headers()
            .get(header::CONTENT_TYPE)
            .expect("content type")
            .to_str()
            .expect("ascii"),
        "text/event-stream"
    );
    let bytes = to_bytes(chat.into_body(), usize::MAX)
        .await
        .expect("body can be read");
    assert_eq!(bytes, "data: ok\n\n");

    let forwarded = provider.last_request().expect("provider request");
    assert_eq!(forwarded.path, "/chat/completions");
    assert_eq!(forwarded.authorization, "Bearer provider-secret");
    let forwarded_body: Value = serde_json::from_slice(&forwarded.body).expect("json forwarded");
    assert_eq!(forwarded_body["model"], "gpt-4.1-mini");
    assert_eq!(forwarded_body["max_tokens"], 4096);
    assert_eq!(forwarded_body["temperature"], 0.7);
    assert_eq!(forwarded_body["parallel_tool_calls"], true);

    let denied = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/responses",
        json!({ "model": "not-allowed", "input": "hello" }),
        Some("instance-token"),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    let responses = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/responses",
        json!({ "model": "gpt-4.1", "input": "hello" }),
        Some("instance-token"),
    )
    .await;
    assert_eq!(responses.status(), StatusCode::OK);
    let forwarded = provider.last_request().expect("provider request");
    assert_eq!(forwarded.path, "/responses");
    let forwarded_body: Value = serde_json::from_slice(&forwarded.body).expect("json forwarded");
    assert_eq!(forwarded_body["max_output_tokens"], 4096);
    assert_eq!(forwarded_body["temperature"], 0.7);
    assert_eq!(forwarded_body["parallel_tool_calls"], true);
}

#[tokio::test]
async fn llm_proxy_caps_requested_output_tokens_at_model_config_limit() {
    let provider = InMemoryLlmProviderClient::new(LlmProviderResponse {
        status: StatusCode::OK,
        content_type: Some("application/json".to_string()),
        body: b"{}".to_vec(),
    });
    let registry = ModelRegistry::new(ModelConfig {
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: "openai-compatible".to_string(),
        provider_base_url: "https://provider.example/v1".to_string(),
        provider_api_key: "provider-secret".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string()],
        api_type: CHAT_COMPLETIONS_API_TYPE.to_string(),
        reasoning_effort: None,
        enabled: true,
        allow_streaming: true,
        request_timeout_seconds: 60,
        context_window_tokens: 64_000,
        max_output_tokens: 512,
        temperature: 0.2,
        supports_parallel_tools: false,
    });
    registry.add_instance_token("instance-token");
    let app = test_app(provider.clone(), registry);

    let chat = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/chat/completions",
        json!({
            "messages": [],
            "max_tokens": 4096,
            "temperature": 1.2,
            "parallel_tool_calls": true
        }),
        Some("instance-token"),
    )
    .await;
    assert_eq!(chat.status(), StatusCode::OK);
    let forwarded = provider.last_request().expect("provider request");
    let forwarded_body: Value = serde_json::from_slice(&forwarded.body).expect("json forwarded");
    assert_eq!(forwarded_body["max_tokens"], 512);
    assert_eq!(forwarded_body["temperature"], 1.2);
    assert!(forwarded_body.get("parallel_tool_calls").is_none());
}

#[tokio::test]
async fn llm_proxy_injects_reasoning_for_chat_and_responses_requests() {
    let provider = InMemoryLlmProviderClient::new(LlmProviderResponse {
        status: StatusCode::OK,
        content_type: Some("application/json".to_string()),
        body: b"{}".to_vec(),
    });
    let registry = ModelRegistry::new(ModelConfig {
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: "openai-compatible".to_string(),
        provider_base_url: "https://provider.example/v1".to_string(),
        provider_api_key: "provider-secret".to_string(),
        default_model: "gpt-5.5".to_string(),
        allowed_models: vec!["gpt-5.5".to_string()],
        api_type: RESPONSES_API_TYPE.to_string(),
        reasoning_effort: Some("high".to_string()),
        enabled: true,
        allow_streaming: true,
        request_timeout_seconds: 60,
        context_window_tokens: 128_000,
        max_output_tokens: 4096,
        temperature: 0.7,
        supports_parallel_tools: true,
    });
    registry.add_instance_token("instance-token");
    let app = test_app(provider.clone(), registry);

    let responses = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/responses",
        json!({ "input": "hello", "stream": true }),
        Some("instance-token"),
    )
    .await;
    assert_eq!(responses.status(), StatusCode::OK);
    let forwarded = provider.last_request().expect("provider request");
    assert_eq!(forwarded.path, "/responses");
    let forwarded_body: Value = serde_json::from_slice(&forwarded.body).expect("json forwarded");
    assert_eq!(forwarded_body["model"], "gpt-5.5");
    assert_eq!(forwarded_body["reasoning"]["effort"], "high");

    let chat = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/chat/completions",
        json!({ "messages": [] }),
        Some("instance-token"),
    )
    .await;
    assert_eq!(chat.status(), StatusCode::OK);
    let forwarded = provider.last_request().expect("provider request");
    assert_eq!(forwarded.path, "/chat/completions");
    let forwarded_body: Value = serde_json::from_slice(&forwarded.body).expect("json forwarded");
    assert_eq!(forwarded_body["reasoning_effort"], "high");
}

#[tokio::test]
async fn llm_proxy_uses_image_config_for_image_generation_requests() {
    let provider = InMemoryLlmProviderClient::new(LlmProviderResponse {
        status: StatusCode::OK,
        content_type: Some("application/json".to_string()),
        body: br#"{"data":[]}"#.to_vec(),
    });
    let registry = test_registry();
    registry
        .replace(ModelConfig {
            config_kind: IMAGE_MODEL_CONFIG_KIND.to_string(),
            provider_name: "openai-compatible-image".to_string(),
            provider_base_url: "https://image-provider.example/v1".to_string(),
            provider_api_key: "image-secret".to_string(),
            default_model: "gpt-image-2-medium".to_string(),
            allowed_models: vec!["gpt-image-2-medium".to_string()],
            api_type: IMAGES_GENERATIONS_API_TYPE.to_string(),
            reasoning_effort: None,
            enabled: true,
            allow_streaming: false,
            request_timeout_seconds: 180,
            context_window_tokens: 128_000,
            max_output_tokens: 4096,
            temperature: 0.7,
            supports_parallel_tools: true,
        })
        .await
        .expect("image config can be replaced");
    registry.add_instance_token("instance-token");
    let app = test_app(provider.clone(), registry);

    let response = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/images/generations",
        json!({
            "model": "gpt-image-2",
            "prompt": "画一张架构图",
            "size": "1024x1024",
            "quality": "medium",
            "background": "opaque",
            "output_format": "png",
            "moderation": "low"
        }),
        Some("instance-token"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = provider.last_request().expect("provider request");
    assert_eq!(forwarded.path, "/images/generations");
    assert_eq!(forwarded.authorization, "Bearer image-secret");
    assert_eq!(forwarded.timeout_seconds, 180);
    let forwarded_body: Value = serde_json::from_slice(&forwarded.body).expect("json forwarded");
    // Hermes 的 OpenAI 图片插件可能写死一个模型名；Hub 以管理员配置的图片模型为准。
    assert_eq!(forwarded_body["model"], "gpt-image-2-medium");
    // 管理员明确允许的图片输出参数要透传，让 Hermes 可以控制输出质量和格式。
    assert_eq!(forwarded_body["quality"], "medium");
    assert_eq!(forwarded_body["background"], "opaque");
    assert_eq!(forwarded_body["output_format"], "png");
    // moderation 仍属于供应商策略参数，当前保持清洗，避免兼容网关误判。
    assert!(forwarded_body.get("moderation").is_none());
    assert!(forwarded_body.get("reasoning").is_none());
    assert!(forwarded_body.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn llm_proxy_rejects_image_generation_when_image_model_is_disabled() {
    let provider = InMemoryLlmProviderClient::default();
    let registry = test_registry();
    registry
        .replace(ModelConfig {
            config_kind: IMAGE_MODEL_CONFIG_KIND.to_string(),
            provider_name: "openai-compatible-image".to_string(),
            provider_base_url: "https://image-provider.example/v1".to_string(),
            provider_api_key: "image-secret".to_string(),
            default_model: "gpt-image-2-medium".to_string(),
            allowed_models: vec!["gpt-image-2-medium".to_string()],
            api_type: IMAGES_GENERATIONS_API_TYPE.to_string(),
            reasoning_effort: None,
            enabled: false,
            allow_streaming: false,
            request_timeout_seconds: 180,
            context_window_tokens: 128_000,
            max_output_tokens: 4096,
            temperature: 0.7,
            supports_parallel_tools: true,
        })
        .await
        .expect("disabled image config can be saved");
    registry.add_instance_token("instance-token");
    let app = test_app(provider.clone(), registry);

    let response = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/images/generations",
        json!({
            "model": "gpt-image-2-medium",
            "prompt": "画一张架构图"
        }),
        Some("instance-token"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(
        provider.last_request().is_none(),
        "disabled image generation must not reach the provider"
    );
}

#[tokio::test]
async fn llm_proxy_uses_real_http_provider_and_records_usage() {
    let captured = CapturedProviderRequest::default();
    let provider_base_url = format!("{}/v1", spawn_provider_server(captured.clone()).await);
    let registry = ModelRegistry::new(ModelConfig {
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: "local-provider".to_string(),
        provider_base_url,
        provider_api_key: "real-provider-token".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string()],
        api_type: CHAT_COMPLETIONS_API_TYPE.to_string(),
        reasoning_effort: None,
        enabled: true,
        allow_streaming: true,
        request_timeout_seconds: 5,
        context_window_tokens: 128_000,
        max_output_tokens: 4096,
        temperature: 0.7,
        supports_parallel_tools: true,
    });
    registry
        .add_instance_token_for_instance("instance-for-usage", "instance-token")
        .await
        .expect("memory token can be registered");
    let state = test_state_with_provider(ReqwestLlmProviderClient::default().shared(), registry);
    let app = build_router_with_state(state.clone());

    let response = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/chat/completions",
        json!({ "messages": [], "stream": true }),
        Some("instance-token"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content type")
            .to_str()
            .expect("ascii"),
        "text/event-stream"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body can be read");
    assert_eq!(bytes, "data: real-provider\n\n");
    assert_eq!(
        captured.authorization.lock().expect("auth lock").as_deref(),
        Some("Bearer real-provider-token")
    );
    assert_eq!(
        captured
            .body
            .lock()
            .expect("body lock")
            .as_ref()
            .expect("body")["model"],
        "gpt-4.1-mini"
    );
    assert_eq!(state.store.llm_usage_count().await.expect("usage count"), 1);
}

#[tokio::test]
async fn llm_proxy_allows_longer_timeout_for_streaming_provider_requests() {
    let provider_base_url = format!("{}/v1", spawn_slow_stream_provider_server().await);
    let registry = ModelRegistry::new(ModelConfig {
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: "slow-provider".to_string(),
        provider_base_url,
        provider_api_key: "slow-provider-token".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string()],
        api_type: CHAT_COMPLETIONS_API_TYPE.to_string(),
        reasoning_effort: None,
        enabled: true,
        allow_streaming: true,
        request_timeout_seconds: 1,
        context_window_tokens: 128_000,
        max_output_tokens: 4096,
        temperature: 0.7,
        supports_parallel_tools: true,
    });
    registry
        .add_instance_token_for_instance("instance-for-slow-stream", "instance-token")
        .await
        .expect("memory token can be registered");
    let state = test_state_with_provider(ReqwestLlmProviderClient::default().shared(), registry);
    let app = build_router_with_state(state);

    let response = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/chat/completions",
        json!({ "messages": [], "stream": true }),
        Some("instance-token"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("streaming response body can be read");
    assert_eq!(bytes, "data: slow-provider\n\n");
}
