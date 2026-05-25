use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use hermes_hub_backend::{build_router, AppConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

fn test_app() -> Router {
    build_router(AppConfig::for_tests())
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
    let response = request_json(
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
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = request_json(
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
    assert_eq!(response.status(), StatusCode::OK);
    cookie_from(&response)
}

#[tokio::test]
async fn admin_workspace_test() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let users = request_empty(&app, Method::GET, "/api/admin/users", Some(&admin_cookie)).await;
    let (status, body) = response_json(users).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["users"][0]["email"], "admin@example.com");
    let admin_id = body["users"][0]["id"].as_str().expect("admin id");

    let disable = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/disable"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(disable.status(), StatusCode::CONFLICT);

    let update_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "secret-v2",
            "default_model": "gpt-4.1",
            "allowed_models": ["gpt-4.1"],
            "api_type": "responses",
            "reasoning_effort": "medium",
            "allow_streaming": true,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_model.status(), StatusCode::NO_CONTENT);

    let model = request_empty(
        &app,
        Method::GET,
        "/api/admin/model-config",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(model).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["model_config"]["provider_name"], "custom");
    assert_eq!(body["model_config"]["default_model"], "gpt-4.1");
    assert_eq!(body["model_config"]["provider_api_key"], "secret-v2");
    assert_eq!(body["model_config"]["api_type"], "responses");
    assert_eq!(body["model_config"]["reasoning_effort"], "medium");

    let status_response = request_empty(
        &app,
        Method::GET,
        "/api/workspace/status",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(status_response).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["hermes_instance"].is_null());

    let blocked_without_title_model = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(blocked_without_title_model.status(), StatusCode::CONFLICT);

    let blocked_admin_create_without_title_model = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/create-managed"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(
        blocked_admin_create_without_title_model.status(),
        StatusCode::CONFLICT
    );

    let update_title_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "config_kind": "title",
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "title-secret-v2",
            "default_model": "gpt-4.1-mini",
            "allowed_models": ["gpt-4.1-mini"],
            "allow_streaming": false,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_title_model.status(), StatusCode::NO_CONTENT);

    let update_image_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "config_kind": "image",
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "image-secret-v2",
            "default_model": "gpt-image-1",
            "allowed_models": ["gpt-image-1"],
            "allow_streaming": false,
            "request_timeout_seconds": 180
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_image_model.status(), StatusCode::NO_CONTENT);

    let created_by_admin = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/create-managed"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(created_by_admin).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["kind"], "managed_docker");
    assert_eq!(body["hermes_instance"]["status"], "running");
    let managed_config = std::fs::read_to_string(format!(
        "/tmp/hermes-hub/users/{admin_id}/config/config.yaml"
    ))
    .expect("managed Hermes config is written");
    assert!(managed_config.contains("model: \"gpt-image-1\""));
    assert!(!managed_config.contains("gpt-image-2-medium"));

    let ensured = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(ensured).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["kind"], "managed_docker");
    assert_eq!(body["hermes_instance"]["status"], "running");

    let stop = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/stop"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(stop).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["status"], "stopped");

    let start = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/start"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(start).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["status"], "running");

    let instances = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-instances",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(instances).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instances"][0]["user_id"], admin_id);

    let removed_legacy_config = request_json(
        &app,
        Method::PUT,
        &format!("/api/admin/users/{admin_id}/hermes-instance/external-config"),
        json!({
            "name": "legacy runtime",
            "api_token": "external-token"
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(removed_legacy_config.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_can_configure_per_user_session_limit() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let settings = request_empty(
        &app,
        Method::GET,
        "/api/admin/system-settings",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(settings).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["max_sessions_per_user"], 20);
    assert_eq!(body["settings"]["oidc"]["enabled"], false);
    assert_eq!(body["settings"]["oidc"]["display_name"], "OpenID Connect");

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 2,
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "issuer_url": "https://idp.example.com",
                "authorization_url": "https://idp.example.com/oauth2/v1/authorize",
                "token_url": "https://idp.example.com/oauth2/v1/token",
                "userinfo_url": "https://idp.example.com/oauth2/v1/userinfo",
                "logout_url": "https://idp.example.com/logout",
                "scopes": "openid profile email",
                "username_claim": "preferred_username",
                "email_claim": "email",
                "allow_password_login": true,
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let settings = request_empty(
        &app,
        Method::GET,
        "/api/admin/system-settings",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(settings).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["max_sessions_per_user"], 2);
    assert_eq!(body["settings"]["oidc"]["enabled"], true);
    assert_eq!(body["settings"]["oidc"]["display_name"], "Acme SSO");
    assert_eq!(body["settings"]["oidc"]["client_id"], "hermes-hub");
    assert_eq!(body["settings"]["oidc"]["client_secret"], "oidc-secret");
    assert_eq!(
        body["settings"]["oidc"]["authorization_url"],
        "https://idp.example.com/oauth2/v1/authorize"
    );

    let public_oidc = request_empty(&app, Method::GET, "/api/auth/oidc/config", None).await;
    let (status, body) = response_json(public_oidc).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["oidc"]["enabled"], true);
    assert_eq!(body["oidc"]["display_name"], "Acme SSO");
    assert!(body["oidc"].get("client_secret").is_none());

    let channels = request_empty(&app, Method::GET, "/api/channels", Some(&admin_cookie)).await;
    let (status, body) = response_json(channels).await;
    assert_eq!(status, StatusCode::OK);
    let channel_id = body["channels"][0]
        .as_object()
        .and_then(|channel| channel.get("id"))
        .and_then(Value::as_str)
        .expect("channel id");

    for _ in 0..2 {
        let created = request_json(
            &app,
            Method::POST,
            &format!("/api/channels/{channel_id}/sessions"),
            json!({ "kind": "agent" }),
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    let blocked = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(blocked).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "session_limit_exceeded");
    assert_eq!(body["message"], "session limit exceeded");
    assert_eq!(body["max_sessions_per_user"], 2);
}
