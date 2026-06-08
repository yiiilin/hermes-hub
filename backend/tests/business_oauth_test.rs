use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use futures_util::StreamExt;
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::ChannelStore,
    docker_config_from_app,
    hermes::docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
    ldap::DefaultLdapAuthenticator,
    llm_proxy::InMemoryLlmProviderClient,
    model_config::{ModelConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE, LLM_MODEL_CONFIG_KIND},
    session::store::{
        CreatedIntegrationApp, IncomingIntegrationToolDefinition, NewIntegrationApp,
        SessionPurpose, SessionStore, StoreError, UpdateIntegrationApp,
    },
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

async fn request_form(app: &Router, method: Method, uri: &str, form: &str) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form.to_string()))
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

async fn configure_integration_app(state: &AppState) -> CreatedIntegrationApp {
    let created = state
        .store
        .create_integration_app(NewIntegrationApp {
            name: "Business CRM".to_string(),
            enabled: true,
            redirect_uri: "https://biz.example/callback".to_string(),
            scopes: "openid profile email".to_string(),
            authorization_code_ttl_seconds: Some(600),
            hidden_session_idle_timeout_seconds: Some(3600),
            default_tool_timeout_seconds: Some(60),
            max_tool_timeout_seconds: Some(300),
        })
        .await
        .expect("integration app can be created");
    state
        .store
        .replace_integration_tools(
            &created.app.integration_id,
            vec![IncomingIntegrationToolDefinition {
                name: "business-crm".to_string(),
                description: "Business CRM toolset".to_string(),
                parameters: json!({}),
            }],
        )
        .await
        .expect("integration tools can be saved");
    created
}

async fn oauth_authorization_code(app: &Router, admin_cookie: &str, client_id: &str) -> String {
    let authorize = request_empty(
        app,
        Method::GET,
        &format!(
            "/api/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&scope=openid%20profile%20email&state=state-1"
        ),
        Some(admin_cookie),
        None,
    )
    .await;
    assert_eq!(authorize.status(), StatusCode::FOUND);
    let location = authorize
        .headers()
        .get(header::LOCATION)
        .expect("authorize redirects")
        .to_str()
        .expect("location is ascii")
        .to_string();
    assert!(location.starts_with("https://biz.example/callback?"));
    assert!(location.contains("state=state-1"));
    let code = location
        .split('?')
        .nth(1)
        .expect("redirect has query")
        .split('&')
        .find_map(|part| part.strip_prefix("code="))
        .expect("redirect contains code");

    code.to_string()
}

async fn oauth_access_token(
    app: &Router,
    admin_cookie: &str,
    client_id: &str,
    client_secret: &str,
) -> String {
    let code = oauth_authorization_code(app, admin_cookie, client_id).await;

    let token = request_form(
        app,
        Method::POST,
        "/api/oauth/token",
        &format!(
            "grant_type=authorization_code&client_id={client_id}&client_secret={client_secret}&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&code={code}"
        ),
    )
    .await;
    let (status, body) = response_json(token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["token_type"], "Bearer");
    body["access_token"]
        .as_str()
        .expect("access token")
        .to_string()
}

async fn create_integration_session(app: &Router, token: &str) -> String {
    let created = request_json(
        app,
        Method::POST,
        "/api/integrations/sessions",
        json!({ "kind": "agent", "title": "CRM case" }),
        None,
        Some(token),
    )
    .await;
    let (status, body) = response_json(created).await;
    assert_eq!(status, StatusCode::CREATED);
    body["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string()
}

fn business_tool_request_content(
    request_id: &str,
    tool_name: &str,
    timeout_seconds: Option<u64>,
) -> String {
    business_tool_request_content_with_arguments(
        request_id,
        tool_name,
        json!({ "case_id": "CASE-42" }),
        timeout_seconds,
    )
}

fn business_tool_request_content_with_arguments(
    request_id: &str,
    tool_name: &str,
    arguments: Value,
    timeout_seconds: Option<u64>,
) -> String {
    let mut envelope = json!({
        "request_id": request_id,
        "tool_name": tool_name,
        "arguments": arguments
    });
    if let Some(timeout_seconds) = timeout_seconds {
        envelope["timeout_seconds"] = json!(timeout_seconds);
    }
    format!("<!-- hermes-hub:business-tool-request:v1 -->\n{}", envelope)
}

#[tokio::test]
async fn bearer_session_token_authenticates_me_and_userinfo() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;

    let me = request_empty(&app, Method::GET, "/api/auth/me", None, Some(&token)).await;
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);

    let userinfo =
        request_empty(&app, Method::GET, "/api/oauth/userinfo", None, Some(&token)).await;
    let (status, body) = response_json(userinfo).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["email"], "admin@example.com");
    assert_eq!(body["sub"], body["id"]);
    assert_eq!(body["integration_id"], integration_app.app.integration_id);
    assert_eq!(body["toolset_names"], json!(["business-crm"]));

    let bearer_logout =
        request_empty(&app, Method::POST, "/api/auth/logout", None, Some(&token)).await;
    assert_eq!(bearer_logout.status(), StatusCode::NO_CONTENT);
    let userinfo_after_logout =
        request_empty(&app, Method::GET, "/api/oauth/userinfo", None, Some(&token)).await;
    assert_eq!(userinfo_after_logout.status(), StatusCode::OK);

    let cookie_userinfo = request_empty(
        &app,
        Method::GET,
        "/api/oauth/userinfo",
        Some(&format!("hermes_hub_session={token}")),
        None,
    )
    .await;
    assert_eq!(cookie_userinfo.status(), StatusCode::UNAUTHORIZED);

    let admin_settings = request_empty(
        &app,
        Method::GET,
        "/api/admin/system-settings",
        None,
        Some(&token),
    )
    .await;
    assert_eq!(admin_settings.status(), StatusCode::UNAUTHORIZED);

    let password_update = request_json(
        &app,
        Method::PUT,
        "/api/auth/password",
        json!({"new_password": "new-password-123"}),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(password_update.status(), StatusCode::UNAUTHORIZED);

    let authorize_again = request_empty(
        &app,
        Method::GET,
        &format!(
            "/api/oauth/authorize?response_type=code&client_id={}&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&scope=openid%20profile%20email&state=state-2",
            integration_app.app.client_id
        ),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(authorize_again.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oauth_session_creation_requires_integration_id() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    configure_integration_app(&state).await;
    let user = state
        .store
        .user_by_session_cookie(&admin_cookie, "hermes_hub_session")
        .await
        .expect("admin cookie resolves to user");
    let error = state
        .store
        .create_session_with_purpose(&user.id, SessionPurpose::OAuth, None)
        .await
        .expect_err("OAuth session without integration id must be rejected");
    assert!(matches!(error, StoreError::InvalidSystemSettings));
}

#[tokio::test]
async fn oauth_authorization_code_is_one_time_use() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;

    let code = oauth_authorization_code(&app, &admin_cookie, &integration_app.app.client_id).await;
    assert!(!code.is_empty());

    let first_exchange = request_form(
        &app,
        Method::POST,
        "/api/oauth/token",
        &format!(
            "grant_type=authorization_code&client_id={}&client_secret={}&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&code={code}",
            integration_app.app.client_id, integration_app.client_secret
        ),
    )
    .await;
    assert_eq!(first_exchange.status(), StatusCode::OK);

    let reused = request_form(
        &app,
        Method::POST,
        "/api/oauth/token",
        &format!(
            "grant_type=authorization_code&client_id={}&client_secret={}&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&code={code}",
            integration_app.app.client_id, integration_app.client_secret
        ),
    )
    .await;
    assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oauth_bearer_cannot_use_public_channel_session_api() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;

    let web_channel_attempt = request_empty(
        &app,
        Method::GET,
        "/api/channels",
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(web_channel_attempt.status(), StatusCode::NOT_FOUND);

    let bearer_channel_attempt =
        request_empty(&app, Method::GET, "/api/channels", None, Some(&token)).await;
    assert_eq!(bearer_channel_attempt.status(), StatusCode::NOT_FOUND);

    let created = request_json(
        &app,
        Method::POST,
        "/api/channels/hermes-hub/sessions",
        json!({ "kind": "agent" }),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(created.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn oauth_bearer_uses_integration_sessions_without_channel_id() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;

    let web_attempt = request_empty(
        &app,
        Method::GET,
        "/api/integrations/sessions",
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(web_attempt.status(), StatusCode::UNAUTHORIZED);

    let empty_list = request_empty(
        &app,
        Method::GET,
        "/api/integrations/sessions",
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(empty_list).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["sessions"].as_array().expect("sessions array").len(),
        0
    );

    let created = request_json(
        &app,
        Method::POST,
        "/api/integrations/sessions",
        json!({ "kind": "agent", "title": "CRM case" }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(created).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["session"]["title"], "CRM case");
    assert_eq!(body["session"]["hidden_from_web"], true);
    assert!(body["session"].get("channel_id").is_none());
    let session_id = body["session"]["id"].as_str().expect("session id");

    let listed = request_empty(
        &app,
        Method::GET,
        "/api/integrations/sessions",
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(listed).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessions"][0]["id"], session_id);
    assert_eq!(body["sessions"][0]["hidden_from_web"], true);
    assert!(body["sessions"][0].get("channel_id").is_none());

    let message = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "user",
            "content": "hello from crm",
            "client_message_key": "crm-1"
        }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(message).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["message"]["content"], "hello from crm");

    let web_sessions = request_empty(
        &app,
        Method::GET,
        "/api/sessions",
        Some(&admin_cookie),
        None,
    )
    .await;
    let (status, body) = response_json(web_sessions).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["sessions"]
        .as_array()
        .expect("web sessions array")
        .iter()
        .all(|session| session["id"] != session_id));
}

#[tokio::test]
async fn oauth_integration_sessions_do_not_consume_web_session_limit() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let mut settings = state
        .store
        .system_settings()
        .await
        .expect("settings can be read");
    settings.max_sessions_per_user = 2;
    state
        .store
        .update_system_settings(settings)
        .await
        .expect("settings can be updated");

    let web_session = request_json(
        &app,
        Method::POST,
        "/api/sessions",
        json!({ "kind": "agent" }),
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(web_session.status(), StatusCode::CREATED);

    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;

    let integration_session = request_json(
        &app,
        Method::POST,
        "/api/integrations/sessions",
        json!({ "kind": "agent", "title": "not counted against web quota" }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(integration_session).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["session"]["hidden_from_web"], true);
    let integration_session_id = body["session"]["id"]
        .as_str()
        .expect("integration session id")
        .to_string();

    let second_web_session = request_json(
        &app,
        Method::POST,
        "/api/sessions",
        json!({ "kind": "agent" }),
        Some(&admin_cookie),
        None,
    )
    .await;
    let (status, body) = response_json(second_web_session).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["session"]["id"].as_str().is_some());

    let third_web_session = request_json(
        &app,
        Method::POST,
        "/api/sessions",
        json!({ "kind": "agent" }),
        Some(&admin_cookie),
        None,
    )
    .await;
    let (status, body) = response_json(third_web_session).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "session_limit_exceeded");
    assert_eq!(body["max_sessions_per_user"], 2);

    let events = request_empty(
        &app,
        Method::GET,
        &format!("/api/integrations/sessions/{integration_session_id}/events"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(events.status(), StatusCode::OK);
    let mut event_stream = events.into_body().into_data_stream();
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(1), event_stream.next())
        .await
        .expect("integration snapshot arrives")
        .expect("snapshot chunk exists")
        .expect("snapshot is readable");
    let snapshot_text = String::from_utf8_lossy(&snapshot);
    assert!(
        snapshot_text.contains("event: messages_snapshot"),
        "unexpected snapshot chunk: {snapshot_text}"
    );

    let appended = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{integration_session_id}/messages"),
        json!({
            "role": "user",
            "content": "still works after web quota is full",
            "client_message_key": "quota-proof-1"
        }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(appended).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body["message"]["content"],
        "still works after web quota is full"
    );

    let mut run_event_text = String::new();
    for _ in 0..4 {
        let next_event =
            tokio::time::timeout(std::time::Duration::from_secs(1), event_stream.next())
                .await
                .expect("session event arrives")
                .expect("event chunk exists")
                .expect("event is readable");
        let next_event_text = String::from_utf8_lossy(&next_event).to_string();
        if next_event_text.contains("event: run_updated") {
            run_event_text = next_event_text;
            break;
        }
    }
    assert!(
        !run_event_text.is_empty(),
        "expected run_updated event after integration user message"
    );
    assert!(
        run_event_text.contains("event: run_updated"),
        "unexpected run event chunk: {run_event_text}"
    );
    assert!(
        run_event_text.contains("\"status\":\"queued\"")
            || run_event_text.contains("\"status\":\"running\""),
        "unexpected run event chunk: {run_event_text}"
    );
}

#[tokio::test]
async fn business_tool_request_message_emits_typed_sse_event_with_effective_timeout() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let stream_response = request_empty(
        &app,
        Method::GET,
        &format!("/api/integrations/sessions/{session_id}/events"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(stream_response.status(), StatusCode::OK);
    let mut stream_body = stream_response.into_body().into_data_stream();
    let _snapshot = tokio::time::timeout(std::time::Duration::from_secs(1), stream_body.next())
        .await
        .expect("snapshot arrives")
        .expect("snapshot chunk exists")
        .expect("snapshot is readable");

    let created = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": business_tool_request_content("req-live-1", "business-crm", Some(999)),
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let event = tokio::time::timeout(std::time::Duration::from_secs(1), stream_body.next())
        .await
        .expect("business tool request event arrives")
        .expect("event chunk exists")
        .expect("event is readable");
    let event_text = String::from_utf8_lossy(&event);
    assert!(
        event_text.contains("event: business_tool_request"),
        "unexpected event chunk: {event_text}"
    );
    assert!(
        event_text.contains("\"type\":\"business_tool_request\""),
        "unexpected event chunk: {event_text}"
    );
    assert!(
        event_text.contains("\"request_id\":\"req-live-1\""),
        "unexpected event chunk: {event_text}"
    );
    assert!(
        event_text.contains("\"tool_name\":\"business-crm\""),
        "unexpected event chunk: {event_text}"
    );
    assert!(
        event_text.contains("\"timeout_seconds\":300"),
        "unexpected event chunk: {event_text}"
    );
    assert!(
        event_text.contains("\"expires_at\""),
        "unexpected event chunk: {event_text}"
    );
}

#[tokio::test]
async fn business_tool_request_snapshot_includes_typed_requests() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let created = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": business_tool_request_content("req-snapshot-1", "business-crm", Some(90)),
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let second_request = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": business_tool_request_content("req-snapshot-2", "business-crm", Some(120)),
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(second_request.status(), StatusCode::CREATED);

    let stream_response = request_empty(
        &app,
        Method::GET,
        &format!("/api/integrations/sessions/{session_id}/events"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(stream_response.status(), StatusCode::OK);
    let mut stream_body = stream_response.into_body().into_data_stream();
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(1), stream_body.next())
        .await
        .expect("snapshot arrives")
        .expect("snapshot chunk exists")
        .expect("snapshot is readable");
    let snapshot_text = String::from_utf8_lossy(&snapshot);
    let mut snapshot_lines = snapshot_text.lines();
    assert_eq!(
        snapshot_lines.next(),
        Some("event: messages_snapshot"),
        "unexpected snapshot chunk: {snapshot_text}"
    );
    assert!(
        snapshot_text.contains("\"business_tool_requests\""),
        "unexpected snapshot chunk: {snapshot_text}"
    );
    let snapshot_json = snapshot_lines
        .next()
        .and_then(|line| line.strip_prefix("data: "))
        .expect("snapshot chunk contains data payload");
    let snapshot_value: Value =
        serde_json::from_str(snapshot_json).expect("snapshot payload is valid json");
    let requests = snapshot_value["business_tool_requests"]
        .as_array()
        .expect("business tool requests are an array");
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .any(|request| { request["request"]["request_id"].as_str() == Some("req-snapshot-1") }));
    assert!(requests
        .iter()
        .any(|request| { request["request"]["request_id"].as_str() == Some("req-snapshot-2") }));
    for request in requests {
        assert_eq!(request["type"], "business_tool_request");
        assert_eq!(request["request"]["status"], "pending");
    }
}

#[tokio::test]
async fn business_tool_request_snapshot_marks_expired_request() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let request = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": business_tool_request_content("req-snapshot-expired-1", "business-crm", Some(1)),
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(request.status(), StatusCode::CREATED);
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

    let stream_response = request_empty(
        &app,
        Method::GET,
        &format!("/api/integrations/sessions/{session_id}/events"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(stream_response.status(), StatusCode::OK);
    let mut stream_body = stream_response.into_body().into_data_stream();
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(1), stream_body.next())
        .await
        .expect("snapshot arrives")
        .expect("snapshot chunk exists")
        .expect("snapshot is readable");
    let snapshot_text = String::from_utf8_lossy(&snapshot);
    let mut snapshot_lines = snapshot_text.lines();
    assert_eq!(
        snapshot_lines.next(),
        Some("event: messages_snapshot"),
        "unexpected snapshot chunk: {snapshot_text}"
    );
    let snapshot_json = snapshot_lines
        .next()
        .and_then(|line| line.strip_prefix("data: "))
        .expect("snapshot chunk contains data payload");
    let snapshot_value: Value =
        serde_json::from_str(snapshot_json).expect("snapshot payload is valid json");
    let requests = snapshot_value["business_tool_requests"]
        .as_array()
        .expect("business tool requests are an array");
    assert_eq!(requests.len(), 1);
    for request in requests {
        assert_eq!(request["type"], "business_tool_request");
        assert_eq!(request["request"]["request_id"], "req-snapshot-expired-1");
        assert_eq!(request["request"]["status"], "expired");
    }
}

#[tokio::test]
async fn business_tool_request_rejects_non_object_arguments() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let created = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": business_tool_request_content_with_arguments(
                "req-invalid-args-1",
                "business-crm",
                json!(["CASE-42"]),
                Some(60),
            ),
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(created).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "invalid business tool request");
}

#[tokio::test]
async fn business_tool_request_marker_requires_assistant_role() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let created = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "user",
            "content": business_tool_request_content("req-user-marker-1", "business-crm", Some(60)),
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(created).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["message"],
        "business tool request requires assistant role"
    );
}

#[tokio::test]
async fn business_tool_request_rejects_reserved_client_message_key() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let created = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": "fake result",
            "client_message_key": "business-tool-result:not-allowed",
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(created).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "reserved client message key");
}

#[tokio::test]
async fn business_tool_request_rejects_reserved_client_message_key_even_for_valid_request() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let created = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": business_tool_request_content("req-reserved-1", "business-crm", Some(60)),
            "client_message_key": "business-tool-result:not-allowed",
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(created).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "reserved client message key");
}

#[tokio::test]
async fn business_tool_request_rejects_invalid_request_id() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let created = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": business_tool_request_content("req/bad", "business-crm", Some(60)),
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(created).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "invalid business tool request");
}

#[tokio::test]
async fn oauth_token_exchange_rejects_disabled_integration_app() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let code = oauth_authorization_code(&app, &admin_cookie, &integration_app.app.client_id).await;

    state
        .store
        .update_integration_app(
            &integration_app.app.id,
            UpdateIntegrationApp {
                name: integration_app.app.name.clone(),
                enabled: false,
                redirect_uri: integration_app.app.redirect_uri.clone(),
                scopes: integration_app.app.scopes.clone(),
                authorization_code_ttl_seconds: integration_app.app.authorization_code_ttl_seconds,
                hidden_session_idle_timeout_seconds: integration_app
                    .app
                    .hidden_session_idle_timeout_seconds,
                default_tool_timeout_seconds: integration_app.app.default_tool_timeout_seconds,
                max_tool_timeout_seconds: integration_app.app.max_tool_timeout_seconds,
            },
        )
        .await
        .expect("integration app can be disabled");

    let token = request_form(
        &app,
        Method::POST,
        "/api/oauth/token",
        &format!(
            "grant_type=authorization_code&client_id={}&client_secret={}&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&code={code}",
            integration_app.app.client_id,
            integration_app.client_secret,
        ),
    )
    .await;
    let (status, body) = response_json(token).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["message"], "integration app is not enabled");
}

#[tokio::test]
async fn business_tool_result_callback_appends_assistant_message() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let request = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": business_tool_request_content("req-result-1", "business-crm", Some(60)),
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(request.status(), StatusCode::CREATED);

    let callback = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/integrations/sessions/{session_id}/business-tool-requests/req-result-1/result"
        ),
        json!({ "result": "CRM says the case is approved" }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(callback).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["message"]["role"], "assistant");
    assert_eq!(body["message"]["content"], "CRM says the case is approved");
    let client_message_key = body["message"]["client_message_key"]
        .as_str()
        .expect("client message key");
    assert!(client_message_key.starts_with("business-tool-result:"));
    assert_eq!(client_message_key.len(), "business-tool-result:".len() + 64);
}

#[tokio::test]
async fn business_tool_result_callback_is_idempotent() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let request = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": business_tool_request_content("req-idempotent-1", "business-crm", Some(60)),
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(request.status(), StatusCode::CREATED);

    let first = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/integrations/sessions/{session_id}/business-tool-requests/req-idempotent-1/result"
        ),
        json!({ "result": "first result wins" }),
        None,
        Some(&token),
    )
    .await;
    let (first_status, first_body) = response_json(first).await;
    assert_eq!(first_status, StatusCode::CREATED);

    let repeated = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/integrations/sessions/{session_id}/business-tool-requests/req-idempotent-1/result"
        ),
        json!({ "result": "second result is ignored" }),
        None,
        Some(&token),
    )
    .await;
    let (repeated_status, repeated_body) = response_json(repeated).await;
    assert_eq!(repeated_status, StatusCode::OK);
    assert_eq!(repeated_body["message"]["id"], first_body["message"]["id"]);
    assert_eq!(repeated_body["message"]["content"], "first result wins");
}

#[tokio::test]
async fn business_tool_result_callback_rejects_expired_request() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;

    let request = request_json(
        &app,
        Method::POST,
        &format!("/api/integrations/sessions/{session_id}/messages"),
        json!({
            "role": "assistant",
            "content": business_tool_request_content("req-expired-1", "business-crm", Some(1)),
            "attachments": []
        }),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(request.status(), StatusCode::CREATED);
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

    let callback = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/integrations/sessions/{session_id}/business-tool-requests/req-expired-1/result"
        ),
        json!({ "result": "too late" }),
        None,
        Some(&token),
    )
    .await;
    let (status, body) = response_json(callback).await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(body["error"], "gone");
    assert_eq!(body["message"], "business tool request expired");
}

#[tokio::test]
async fn internal_business_tool_request_endpoint_waits_for_result() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let integration_app = configure_integration_app(&state).await;
    let user_id = state
        .store
        .user_by_session_cookie(&admin_cookie, "hermes_hub_session")
        .await
        .expect("admin cookie resolves to user")
        .id;
    let token = oauth_access_token(
        &app,
        &admin_cookie,
        &integration_app.app.client_id,
        &integration_app.client_secret,
    )
    .await;
    let session_id = create_integration_session(&app, &token).await;
    let integration_channel = state
        .channel_store
        .ensure_integration_channel(&user_id, &integration_app.app.integration_id)
        .await
        .expect("integration channel exists");
    let instance = hermes_hub_backend::hermes::instance::HermesInstance::managed_docker(
        &user_id,
        "/tmp/workspace".to_string(),
        None,
        "/tmp/config".to_string(),
    );
    let instance_id = instance.id.clone();
    state
        .store
        .bind_hermes_instance(instance)
        .await
        .expect("instance can be bound");
    state
        .channel_store
        .bind_integration_channel_to_instance(
            &user_id,
            &integration_app.app.integration_id,
            &instance_id,
        )
        .await
        .expect("integration channel can be rebound");
    let instance_token = "integration-instance-token";
    state
        .model_registry
        .add_instance_token_for_instance(&instance_id, instance_token)
        .await
        .expect("instance token can be registered");

    let callback_app = app.clone();
    let callback_token = token.clone();
    let callback_session_id = session_id.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let callback_task = tokio::spawn(async move {
        let mut receiver = state.session_events.subscribe();
        let _ = ready_tx.send(());
        loop {
            match receiver.recv().await.expect("event exists") {
                hermes_hub_backend::channel::events::SessionEvent::BusinessToolRequest { request }
                    if request.session_id == callback_session_id
                        && request.status
                            == hermes_hub_backend::channel::events::BusinessToolRequestStatus::Pending =>
                {
                    let response = request_json(
                        &callback_app,
                        Method::POST,
                        &format!(
                            "/api/integrations/sessions/{}/business-tool-requests/{}/result",
                            callback_session_id, request.request_id
                        ),
                        json!({ "result": "CRM says the case is approved" }),
                        None,
                        Some(&callback_token),
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::CREATED);
                    break;
                }
                _ => {}
            }
        }
    });
    ready_rx.await.expect("callback subscriber is ready");

    let response = request_json(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{session_id}/business-tool-request"),
        json!({
            "args": {
                "tool_name": "business-crm",
                "arguments": { "ticket": "A-1" }
            }
        }),
        None,
        Some(instance_token),
    )
    .await;
    let (status, body) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "completed");
    assert_eq!(body["result"], "CRM says the case is approved");
    callback_task.await.expect("callback task finishes");
    let _ = integration_channel;
}
