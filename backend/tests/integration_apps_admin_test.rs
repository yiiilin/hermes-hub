use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::ChannelStore,
    docker_config_from_app,
    hermes::docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
    ldap::DefaultLdapAuthenticator,
    llm_proxy::InMemoryLlmProviderClient,
    model_config::{ModelConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE, LLM_MODEL_CONFIG_KIND},
    session::store::SessionStore,
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn test_state() -> AppState {
    let config = AppConfig::for_tests();
    AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
        ),
        config,
        store: SessionStore::in_memory_for_tests(),
        channel_store: ChannelStore::in_memory_for_tests(),
        model_registry: ModelRegistry::in_memory_for_tests(ready_test_model_config()),
        llm_provider: InMemoryLlmProviderClient::default().shared(),
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: Default::default(),
    }
}

fn ready_test_model_config() -> ModelConfig {
    ModelConfig {
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: "openai-compatible".to_string(),
        provider_base_url: "https://ready-provider.example/v1".to_string(),
        provider_api_key: "ready-provider-key".to_string(),
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
        fallback: None,
    }
}

fn test_app(state: AppState) -> Router {
    build_router_with_state(state)
}

async fn request_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Value,
    cookie: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
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
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }

    app.clone()
        .oneshot(builder.body(Body::empty()).expect("request can be built"))
        .await
        .expect("router responds")
}

async fn request_json_with_authorization(
    app: &Router,
    method: Method,
    uri: &str,
    body: Value,
    authorization: &str,
) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, authorization)
                .body(Body::from(body.to_string()))
                .expect("request can be built"),
        )
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

fn basic_authorization_value(client_id: &str, client_secret: &str) -> String {
    let encoded = BASE64_STANDARD.encode(format!("{client_id}:{client_secret}"));
    format!("Basic {encoded}")
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
    )
    .await;
    assert_eq!(login.status(), StatusCode::OK);
    cookie_from(&login)
}

#[tokio::test]
async fn admin_can_manage_integration_apps() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;

    let created = request_json(
        &app,
        Method::POST,
        "/api/admin/integration-apps",
        json!({
            "name": "CRM",
            "enabled": true,
            "redirect_uri": "https://crm.example/callback",
            "scopes": "openid profile email",
            "authorization_code_ttl_seconds": 600,
            "hidden_session_idle_timeout_seconds": 1800,
            "default_tool_timeout_seconds": 60,
            "max_tool_timeout_seconds": 300
        }),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(created).await;
    assert_eq!(status, StatusCode::CREATED);
    let app_id = body["app"]["id"].as_str().expect("app id").to_string();
    let client_id = body["app"]["client_id"]
        .as_str()
        .expect("client id")
        .to_string();
    let client_secret = body["client_secret"]
        .as_str()
        .expect("client secret")
        .to_string();
    assert_eq!(body["app"]["integration_id"], "crm");
    assert_eq!(body["app"]["redirect_uri"], "https://crm.example/callback");

    let invalid_create = request_json(
        &app,
        Method::POST,
        "/api/admin/integration-apps",
        json!({
            "name": "ERP",
            "enabled": true,
            "redirect_uri": "https://erp.example/callback",
            "scopes": "openid profile email",
            "authorization_code_ttl_seconds": 600,
            "hidden_session_idle_timeout_seconds": 1800,
            "default_tool_timeout_seconds": 300,
            "max_tool_timeout_seconds": 120
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(invalid_create.status(), StatusCode::BAD_REQUEST);

    let invalid_redirect_uri = request_json(
        &app,
        Method::POST,
        "/api/admin/integration-apps",
        json!({
            "name": "ERP",
            "enabled": true,
            "redirect_uri": "https://erp.example:99999/callback",
            "scopes": "openid profile email",
            "authorization_code_ttl_seconds": 600,
            "hidden_session_idle_timeout_seconds": 1800,
            "default_tool_timeout_seconds": 60,
            "max_tool_timeout_seconds": 300
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(invalid_redirect_uri.status(), StatusCode::BAD_REQUEST);

    let listed = request_empty(
        &app,
        Method::GET,
        "/api/admin/integration-apps",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(listed).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["integration_apps"].as_array().expect("apps").len(), 1);

    let updated = request_json(
        &app,
        Method::PUT,
        &format!("/api/admin/integration-apps/{app_id}"),
        json!({
            "name": "CRM Hub",
            "enabled": true,
            "redirect_uri": "https://crm.example/alt",
            "scopes": "openid profile email",
            "authorization_code_ttl_seconds": 900,
            "hidden_session_idle_timeout_seconds": 1200,
            "default_tool_timeout_seconds": 90,
            "max_tool_timeout_seconds": 300
        }),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(updated).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["integration_app"]["name"], "CRM Hub");
    assert_eq!(body["integration_app"]["enabled"], true);
    assert_eq!(
        body["integration_app"]["redirect_uri"],
        "https://crm.example/alt"
    );

    let admin_tools_update = request_json(
        &app,
        Method::PUT,
        &format!("/api/admin/integration-apps/{app_id}/tools"),
        json!({
            "tools": [
                {
                    "name": "business-crm",
                    "description": "Business CRM toolset",
                    "parameters": {}
                }
            ]
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(admin_tools_update.status(), StatusCode::METHOD_NOT_ALLOWED);

    let authorization = basic_authorization_value(&client_id, &client_secret);
    let tools = request_json_with_authorization(
        &app,
        Method::PUT,
        "/api/integrations/apps/self/tools",
        json!({
            "tools": [
                {
                    "name": "business-crm",
                    "description": "Business CRM toolset",
                    "parameters": {}
                }
            ]
        }),
        &authorization,
    )
    .await;
    let (status, body) = response_json(tools).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tools"].as_array().expect("tools").len(), 1);

    let duplicate_tools = request_json_with_authorization(
        &app,
        Method::PUT,
        "/api/integrations/apps/self/tools",
        json!({
            "tools": [
                {
                    "name": "business-crm",
                    "description": "Business CRM toolset",
                    "parameters": {}
                },
                {
                    "name": "business-crm",
                    "description": "Duplicate tool",
                    "parameters": {}
                }
            ]
        }),
        &authorization,
    )
    .await;
    assert_eq!(duplicate_tools.status(), StatusCode::BAD_REQUEST);

    let tools_list = request_empty(
        &app,
        Method::GET,
        &format!("/api/admin/integration-apps/{app_id}/tools"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(tools_list).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tools"][0]["name"], "business-crm");

    let rotated = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/integration-apps/{app_id}/secret/rotate"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(rotated).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["id"], app_id);
    assert_ne!(body["client_secret"], client_secret);

    let deleted = request_empty(
        &app,
        Method::DELETE,
        &format!("/api/admin/integration-apps/{app_id}"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let empty_list = request_empty(
        &app,
        Method::GET,
        "/api/admin/integration-apps",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(empty_list).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["integration_apps"].as_array().expect("apps").len(), 0);
}

#[tokio::test]
async fn integration_app_tool_sync_requires_valid_basic_auth() {
    let state = test_state();
    let app = test_app(state.clone());

    let created = state
        .store
        .create_integration_app(hermes_hub_backend::session::store::NewIntegrationApp {
            name: "CRM".to_string(),
            enabled: true,
            redirect_uri: "https://crm.example/callback".to_string(),
            scopes: "openid profile email".to_string(),
            authorization_code_ttl_seconds: Some(600),
            hidden_session_idle_timeout_seconds: Some(1800),
            default_tool_timeout_seconds: Some(60),
            max_tool_timeout_seconds: Some(300),
        })
        .await
        .expect("integration app can be created");

    let unauthorized = request_json(
        &app,
        Method::PUT,
        "/api/integrations/apps/self/tools",
        json!({
            "tools": []
        }),
        None,
    )
    .await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let invalid_secret = request_json_with_authorization(
        &app,
        Method::PUT,
        "/api/integrations/apps/self/tools",
        json!({
            "tools": []
        }),
        &basic_authorization_value(&created.app.client_id, "wrong-secret"),
    )
    .await;
    assert_eq!(invalid_secret.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn integration_app_tool_sync_rejects_non_object_parameters() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;

    let created = state
        .store
        .create_integration_app(hermes_hub_backend::session::store::NewIntegrationApp {
            name: "CRM".to_string(),
            enabled: true,
            redirect_uri: "https://crm.example/callback".to_string(),
            scopes: "openid profile email".to_string(),
            authorization_code_ttl_seconds: Some(600),
            hidden_session_idle_timeout_seconds: Some(1800),
            default_tool_timeout_seconds: Some(60),
            max_tool_timeout_seconds: Some(300),
        })
        .await
        .expect("integration app can be created");

    let invalid_parameters = request_json_with_authorization(
        &app,
        Method::PUT,
        "/api/integrations/apps/self/tools",
        json!({
            "tools": [
                {
                    "name": "business-crm",
                    "description": "Business CRM toolset",
                    "parameters": null
                }
            ]
        }),
        &basic_authorization_value(&created.app.client_id, &created.client_secret),
    )
    .await;
    assert_eq!(invalid_parameters.status(), StatusCode::BAD_REQUEST);

    let invalid_array_parameters = request_json_with_authorization(
        &app,
        Method::PUT,
        "/api/integrations/apps/self/tools",
        json!({
            "tools": [
                {
                    "name": "business-crm",
                    "description": "Business CRM toolset",
                    "parameters": []
                }
            ]
        }),
        &basic_authorization_value(&created.app.client_id, &created.client_secret),
    )
    .await;
    assert_eq!(invalid_array_parameters.status(), StatusCode::BAD_REQUEST);

    let listed = request_empty(
        &app,
        Method::GET,
        &format!("/api/admin/integration-apps/{}/tools", created.app.id),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(listed).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tools"].as_array().expect("tools").len(), 0);
}
