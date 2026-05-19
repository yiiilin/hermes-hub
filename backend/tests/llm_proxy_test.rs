use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::ChannelStore,
    llm_proxy::{InMemoryLlmProviderClient, LlmProviderResponse},
    model_config::{ModelConfig, ModelRegistry},
    session::store::SessionStore,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use tower::ServiceExt;

fn test_state(provider: InMemoryLlmProviderClient, registry: ModelRegistry) -> AppState {
    AppState {
        config: AppConfig::for_tests(),
        store: SessionStore::default(),
        channel_store: ChannelStore::default(),
        hermes_proxy: Default::default(),
        model_registry: registry,
        llm_provider: provider,
    }
}

fn test_app(provider: InMemoryLlmProviderClient, registry: ModelRegistry) -> Router {
    build_router_with_state(test_state(provider, registry))
}

fn test_registry() -> ModelRegistry {
    ModelRegistry::new(ModelConfig {
        provider_name: "openai-compatible".to_string(),
        provider_base_url: "https://provider.example/v1".to_string(),
        provider_api_key: "provider-secret".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string(), "gpt-4.1".to_string()],
        allow_streaming: true,
        request_timeout_seconds: 60,
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
}
