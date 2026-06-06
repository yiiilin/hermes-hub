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
    llm_proxy::InMemoryLlmProviderClient,
    model_config::{ModelConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE, LLM_MODEL_CONFIG_KIND},
    session::store::{BusinessOAuthSettings, SessionStore},
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
        store: SessionStore::default(),
        channel_store: ChannelStore::default(),
        model_registry: ModelRegistry::new(ready_test_model_config()),
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

async fn configure_business_oauth(state: &AppState) {
    let mut settings = state
        .store
        .system_settings()
        .await
        .expect("settings can be read");
    settings.business_oauth = BusinessOAuthSettings {
        enabled: true,
        client_id: "business-client".to_string(),
        client_secret: "business-secret".to_string(),
        allowed_redirect_uris: vec!["https://biz.example/callback".to_string()],
        scopes: "openid profile email".to_string(),
        authorization_code_ttl_seconds: 600,
        hidden_session_idle_timeout_seconds: 3600,
        toolset_names: vec!["business-crm".to_string()],
    };
    state
        .store
        .update_system_settings(settings)
        .await
        .expect("business OAuth settings can be saved");
}

async fn oauth_authorization_code(app: &Router, admin_cookie: &str) -> String {
    let authorize = request_empty(
        app,
        Method::GET,
        "/api/oauth/authorize?response_type=code&client_id=business-client&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&scope=openid%20profile%20email&state=state-1",
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

async fn oauth_access_token(app: &Router, admin_cookie: &str) -> String {
    let code = oauth_authorization_code(app, admin_cookie).await;

    let token = request_form(
        app,
        Method::POST,
        "/api/oauth/token",
        &format!(
            "grant_type=authorization_code&client_id=business-client&client_secret=business-secret&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&code={code}"
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

#[tokio::test]
async fn bearer_session_token_authenticates_me_and_userinfo() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    configure_business_oauth(&state).await;
    let token = oauth_access_token(&app, &admin_cookie).await;

    let me = request_empty(&app, Method::GET, "/api/auth/me", None, Some(&token)).await;
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);

    let userinfo =
        request_empty(&app, Method::GET, "/api/oauth/userinfo", None, Some(&token)).await;
    let (status, body) = response_json(userinfo).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["email"], "admin@example.com");
    assert_eq!(body["sub"], body["id"]);
    assert_eq!(body["integration_id"], "business-client");

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
        "/api/oauth/authorize?response_type=code&client_id=business-client&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&scope=openid%20profile%20email&state=state-2",
        None,
        Some(&token),
    )
    .await;
    assert_eq!(authorize_again.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oauth_userinfo_rejects_legacy_oauth_session_without_integration_id() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    configure_business_oauth(&state).await;
    let user = state
        .store
        .user_by_session_cookie(&admin_cookie, "hermes_hub_session")
        .await
        .expect("admin cookie resolves to user");
    let token = state
        .store
        .create_session_with_purpose(
            &user.id,
            hermes_hub_backend::session::store::SessionPurpose::OAuth,
            None,
        )
        .await
        .expect("legacy OAuth session can be created");

    let userinfo =
        request_empty(&app, Method::GET, "/api/oauth/userinfo", None, Some(&token)).await;

    assert_eq!(userinfo.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oauth_authorization_code_is_one_time_use() {
    let state = test_state();
    let app = test_app(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    configure_business_oauth(&state).await;

    let code = oauth_authorization_code(&app, &admin_cookie).await;
    assert!(!code.is_empty());

    let first_exchange = request_form(
        &app,
        Method::POST,
        "/api/oauth/token",
        &format!(
            "grant_type=authorization_code&client_id=business-client&client_secret=business-secret&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&code={code}"
        ),
    )
    .await;
    assert_eq!(first_exchange.status(), StatusCode::OK);

    let reused = request_form(
        &app,
        Method::POST,
        "/api/oauth/token",
        &format!(
            "grant_type=authorization_code&client_id=business-client&client_secret=business-secret&redirect_uri=https%3A%2F%2Fbiz.example%2Fcallback&code={code}"
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
    configure_business_oauth(&state).await;
    let token = oauth_access_token(&app, &admin_cookie).await;

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
    configure_business_oauth(&state).await;
    let token = oauth_access_token(&app, &admin_cookie).await;

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
