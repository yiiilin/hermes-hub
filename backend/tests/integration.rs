use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::ChannelStore,
    docker_config_from_app,
    hermes::docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
    ldap::DefaultLdapAuthenticator,
    llm_proxy::{InMemoryLlmProviderClient, LlmProviderResponse},
    model_config::ModelRegistry,
    session::store::SessionStore,
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

fn test_state(provider: InMemoryLlmProviderClient) -> AppState {
    let config = AppConfig::for_tests();
    AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
        ),
        config,
        store: SessionStore::default(),
        channel_store: ChannelStore::default(),
        model_registry: ModelRegistry::default_for_tests(),
        llm_provider: provider.shared(),
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: Default::default(),
    }
}

fn test_app(state: AppState) -> Router {
    build_router_with_state(state)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}

async fn request_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Value,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }

    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
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
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }

    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
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

fn cookie_from(response: &Response<Body>) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("response sets a cookie")
        .to_str()
        .expect("cookie is valid ascii")
        .split(';')
        .next()
        .expect("cookie has name and value")
        .to_string()
}

async fn bootstrap_admin(app: &Router) -> String {
    let created = request_json(
        app,
        Method::POST,
        "/api/auth/bootstrap-register",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123"
        }),
        None,
        None,
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let login = request_json(
        app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123"
        }),
        None,
        None,
    )
    .await;
    assert_eq!(login.status(), StatusCode::OK);
    cookie_from(&login)
}

async fn configure_required_model_configs(app: &Router, admin_cookie: &str) {
    for (config_kind, model) in [("llm", "gpt-4.1-mini"), ("title", "gpt-4.1-mini")] {
        let response = request_json(
            app,
            Method::PUT,
            "/api/admin/model-config",
            json!({
                "config_kind": config_kind,
                "provider_name": "openai-compatible",
                "provider_base_url": "https://provider-ready.example/v1",
                "provider_api_key": "provider-ready-key",
                "default_model": model,
                "allowed_models": [model],
                "allow_streaming": config_kind == "llm",
                "request_timeout_seconds": 30
            }),
            Some(admin_cookie),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}

async fn invite_user(app: &Router, admin_cookie: &str) -> String {
    let invite = request_json(
        app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": unix_now() + 24 * 60 * 60,
            "max_uses": 1
        }),
        Some(admin_cookie),
        None,
    )
    .await;
    let (status, body) = response_json(invite).await;
    assert_eq!(status, StatusCode::CREATED);
    let token = body["token"].as_str().expect("invite token");

    let registered = request_json(
        app,
        Method::POST,
        "/api/auth/register",
        json!({
            "invite_token": token,
            "email": "user@example.com",
            "password": "user-password-123"
        }),
        None,
        None,
    )
    .await;
    assert_eq!(registered.status(), StatusCode::CREATED);

    let login = request_json(
        app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "user@example.com",
            "password": "user-password-123"
        }),
        None,
        None,
    )
    .await;
    assert_eq!(login.status(), StatusCode::OK);
    cookie_from(&login)
}

#[tokio::test]
async fn integration_test() {
    let provider = InMemoryLlmProviderClient::new(LlmProviderResponse {
        status: StatusCode::OK,
        content_type: Some("application/json".to_string()),
        body: br#"{"id":"provider-response"}"#.to_vec(),
    });
    let state = test_state(provider.clone());
    let app = test_app(state.clone());

    let admin_cookie = bootstrap_admin(&app).await;
    configure_required_model_configs(&app, &admin_cookie).await;
    let user_cookie = invite_user(&app, &admin_cookie).await;

    let update_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "openai-compatible",
            "provider_base_url": "https://provider-one.example/v1",
            "provider_api_key": "provider-key-one",
            "default_model": "gpt-4.1-mini",
            "allowed_models": ["gpt-4.1-mini", "gpt-4.1"],
            "api_type": "responses",
            "reasoning_effort": "low",
            "allow_streaming": true,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(update_model.status(), StatusCode::NO_CONTENT);

    let test_llm_model = request_json(
        &app,
        Method::POST,
        "/api/admin/model-config/llm/test",
        json!({
            "provider_name": "openai-compatible",
            "provider_base_url": "https://provider-one.example/v1",
            "provider_api_key": "provider-key-one",
            "default_model": "gpt-4.1-mini",
            "allowed_models": ["gpt-4.1-mini", "gpt-4.1"],
            "api_type": "responses",
            "reasoning_effort": "low",
            "allow_streaming": true,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
        None,
    )
    .await;
    let (status, body) = response_json(test_llm_model).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["status_code"], 200);
    let forwarded = provider.last_request().expect("llm test provider request");
    assert_eq!(forwarded.path, "/responses");
    assert_eq!(forwarded.authorization, "Bearer provider-key-one");
    let forwarded_body: Value = serde_json::from_slice(&forwarded.body).expect("provider json");
    assert_eq!(forwarded_body["model"], "gpt-4.1-mini");
    assert_eq!(forwarded_body["reasoning"]["effort"], "low");

    let test_image_model = request_json(
        &app,
        Method::POST,
        "/api/admin/model-config/image/test",
        json!({
            "provider_name": "openai-compatible",
            "provider_base_url": "https://provider-image.example/v1",
            "provider_api_key": "provider-image-key",
            "default_model": "gpt-image-1",
            "allowed_models": ["gpt-image-1"],
            "allow_streaming": false,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
        None,
    )
    .await;
    let (status, body) = response_json(test_image_model).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    let forwarded = provider
        .last_request()
        .expect("image test provider request");
    assert_eq!(forwarded.path, "/images/generations");
    assert_eq!(forwarded.authorization, "Bearer provider-image-key");
    assert_eq!(forwarded.timeout_seconds, 180);

    let ensured = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, body) = response_json(ensured).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["kind"], "managed_docker");
    assert_eq!(body["hermes_instance"]["status"], "running");
    assert!(
        body.get("instance_llm_token").is_none(),
        "实例 token 不应该通过浏览器 API 暴露"
    );
    let user = state
        .store
        .user_by_session_cookie(&user_cookie, "hermes_hub_session")
        .await
        .expect("user can be read from session");
    let instance_llm_token = state
        .store
        .hermes_instance_for_user(&user.id)
        .await
        .expect("user has hermes instance")
        .llm_api_key
        .expect("instance token is stored for the managed hermes runtime");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&user_cookie), None).await;
    let (status, body) = response_json(channel).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["channels"][0]["name"], "hermes-hub");
    let channel_id = body["channels"][0]["id"].as_str().expect("channel id");

    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({
            "kind": "agent"
        }),
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, body) = response_json(session).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["session"]["kind"], "agent");

    // 用户只能通过 Hub 访问自己的 Hermes；浏览器不会拿到 Hermes 的直接入口。
    let hermes = request_json(
        &app,
        Method::POST,
        "/api/removed-hermes/v1/runs?stream=true",
        json!({ "prompt": "hello" }),
        Some(&user_cookie),
        None,
    )
    .await;
    assert_eq!(hermes.status(), StatusCode::NOT_FOUND);

    // Hermes 容器调用 Hub 内部 LLM Gateway，使用 Hub 下发的实例 token。
    let llm_response = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/responses",
        json!({
            "input": "hello"
        }),
        None,
        Some(&instance_llm_token),
    )
    .await;
    assert_eq!(llm_response.status(), StatusCode::OK);
    let forwarded = provider.last_request().expect("provider request");
    assert_eq!(
        forwarded.provider_base_url,
        "https://provider-one.example/v1"
    );
    assert_eq!(forwarded.authorization, "Bearer provider-key-one");
    let forwarded_body: Value = serde_json::from_slice(&forwarded.body).expect("provider json");
    assert_eq!(forwarded_body["model"], "gpt-4.1-mini");
    assert_eq!(forwarded_body["reasoning"]["effort"], "low");

    let rotate_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "openai-compatible",
            "provider_base_url": "https://provider-two.example/v1",
            "provider_api_key": "provider-key-two",
            "default_model": "gpt-4.1",
            "allowed_models": ["gpt-4.1"],
            "allow_streaming": true,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(rotate_model.status(), StatusCode::NO_CONTENT);

    let rotated = request_json(
        &app,
        Method::POST,
        "/internal/llm/v1/chat/completions",
        json!({
            "messages": []
        }),
        None,
        Some(&instance_llm_token),
    )
    .await;
    assert_eq!(rotated.status(), StatusCode::OK);
    let forwarded = provider.last_request().expect("rotated provider request");
    assert_eq!(
        forwarded.provider_base_url,
        "https://provider-two.example/v1"
    );
    assert_eq!(forwarded.authorization, "Bearer provider-key-two");
    let forwarded_body: Value = serde_json::from_slice(&forwarded.body).expect("provider json");
    assert_eq!(forwarded_body["model"], "gpt-4.1");
}
