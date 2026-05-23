use axum::{
    body::{to_bytes, Body},
    http::{header, HeaderMap, Method, Request, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use hermes_hub_backend::{build_router, AppConfig};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tower::ServiceExt;

fn test_app() -> Router {
    build_router(AppConfig::for_tests())
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

async fn request_empty_with_headers(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);

    for (name, value) in headers {
        builder = builder.header(*name, *value);
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

async fn bootstrap_admin(app: &Router) {
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
}

async fn login(app: &Router, email: &str, password: &str) -> String {
    let response = request_json(
        app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": email,
            "password": password
        }),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    cookie_from(&response)
}

#[tokio::test]
async fn oidc_start_redirects_with_configured_authorization_parameters() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "authorization_url": "https://idp.example.com/oauth2/v1/authorize",
                "token_url": "https://idp.example.com/oauth2/v1/token",
                "userinfo_url": "https://idp.example.com/oauth2/v1/userinfo",
                "scopes": "openid profile email",
                "email_claim": "email",
                "username_claim": "preferred_username",
                "allow_password_login": true,
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let response = request_empty(&app, Method::GET, "/api/auth/oidc/start", None).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("redirect location")
        .to_str()
        .expect("location is valid")
        .to_string();
    assert!(location.starts_with("https://idp.example.com/oauth2/v1/authorize?"));
    assert!(location.contains("client_id=hermes-hub"));
    assert!(location.contains("response_type=code"));
    assert!(location.contains("scope=openid%20profile%20email"));
    assert!(location.contains("redirect_uri=http%3A%2F%2Flocalhost%2Fapi%2Fauth%2Foidc%2Fcallback"));
    assert!(location.contains("state="));
    assert!(location.contains("nonce="));
}

#[tokio::test]
async fn oidc_start_uses_forwarded_origin_for_redirect_uri() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "authorization_url": "https://idp.example.com/oauth2/v1/authorize",
                "token_url": "https://idp.example.com/oauth2/v1/token",
                "userinfo_url": "https://idp.example.com/oauth2/v1/userinfo",
                "scopes": "openid profile email",
                "email_claim": "email",
                "username_claim": "preferred_username",
                "allow_password_login": true,
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let response = request_empty_with_headers(
        &app,
        Method::GET,
        "/api/auth/oidc/start",
        &[("host", "hub.example.com"), ("x-forwarded-proto", "https")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("redirect location")
        .to_str()
        .expect("location is valid");

    assert!(location
        .contains("redirect_uri=https%3A%2F%2Fhub.example.com%2Fapi%2Fauth%2Foidc%2Fcallback"));
}

#[tokio::test]
async fn oidc_callback_exchanges_code_creates_user_and_sets_session_cookie() {
    let provider_base_url = spawn_oidc_provider_server().await;
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 20,
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "authorization_url": format!("{provider_base_url}/authorize"),
                "token_url": format!("{provider_base_url}/token"),
                "userinfo_url": format!("{provider_base_url}/userinfo"),
                "scopes": "openid profile email",
                "email_claim": "email",
                "username_claim": "preferred_username",
                "allow_password_login": true,
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let start = request_empty(&app, Method::GET, "/api/auth/oidc/start", None).await;
    assert_eq!(start.status(), StatusCode::FOUND);
    let state_cookie = cookie_from(&start);
    let state = state_cookie
        .split_once('=')
        .map(|(_, value)| value)
        .expect("state cookie has a value");

    let callback = request_empty(
        &app,
        Method::GET,
        &format!("/api/auth/oidc/callback?code=auth-code&state={state}"),
        Some(&state_cookie),
    )
    .await;
    assert_eq!(callback.status(), StatusCode::FOUND);
    assert_eq!(
        callback
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/")
    );
    let session_cookie = cookie_from(&callback);

    let me = request_empty(&app, Method::GET, "/api/auth/me", Some(&session_cookie)).await;
    let (status, body) = response_json(me).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["email"], "oidc-user@example.com");
    assert_eq!(body["user"]["role"], "user");
}

async fn create_invite(app: &Router, admin_cookie: &str, expires_at: u64, max_uses: u32) -> Value {
    let response = request_json(
        app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": expires_at,
            "max_uses": max_uses
        }),
        Some(admin_cookie),
    )
    .await;
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::CREATED);
    value
}

async fn spawn_oidc_provider_server() -> String {
    let app = Router::new()
        .route("/token", post(oidc_token_handler))
        .route("/userinfo", get(oidc_userinfo_handler));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test OIDC provider can bind");
    let addr = listener.local_addr().expect("test OIDC provider addr");

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("test OIDC provider server runs");
    });

    format!("http://{addr}")
}

async fn oidc_token_handler() -> Json<Value> {
    Json(json!({
        "access_token": "access-token",
        "token_type": "Bearer"
    }))
}

async fn oidc_userinfo_handler(headers: HeaderMap) -> Json<Value> {
    assert_eq!(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer access-token")
    );
    Json(json!({
        "email": "oidc-user@example.com",
        "preferred_username": "oidc-user"
    }))
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
                "provider_base_url": "https://ready-provider.example/v1",
                "provider_api_key": "ready-provider-key",
                "default_model": model,
                "allowed_models": [model],
                "allow_streaming": config_kind == "llm",
                "request_timeout_seconds": 30
            }),
            Some(admin_cookie),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}

async fn redeem_invite(app: &Router, token: &str, email: &str) -> Response<Body> {
    request_json(
        app,
        Method::POST,
        "/api/auth/register",
        json!({
            "invite_token": token,
            "email": email,
            "password": "user-password-123"
        }),
        None,
    )
    .await
}

#[tokio::test]
async fn first_user_bootstrap_registers_admin_and_blocks_second_bootstrap() {
    let app = test_app();

    let response = request_empty(&app, Method::GET, "/api/auth/bootstrap-status", None).await;
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["bootstrap_open"], true);

    let response = request_json(
        &app,
        Method::POST,
        "/api/auth/bootstrap-register",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123"
        }),
        None,
    )
    .await;
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(value["user"]["email"], "admin@example.com");
    assert_eq!(value["user"]["role"], "admin");

    let response = request_json(
        &app,
        Method::POST,
        "/api/auth/bootstrap-register",
        json!({
            "email": "second-admin@example.com",
            "password": "admin-password-456"
        }),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);

    let response = request_empty(&app, Method::GET, "/api/auth/bootstrap-status", None).await;
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["bootstrap_open"], false);
}

#[tokio::test]
async fn password_hashing_accepts_right_password_and_rejects_wrong_password() {
    let hash = hermes_hub_backend::domain::user::hash_password("correct-password")
        .expect("password can be hashed");

    assert_ne!(hash, "correct-password");
    assert!(
        hermes_hub_backend::domain::user::verify_password(&hash, "correct-password")
            .expect("stored hash can be verified")
    );
    assert!(
        !hermes_hub_backend::domain::user::verify_password(&hash, "wrong-password")
            .expect("stored hash can reject mismatches")
    );
}

#[tokio::test]
async fn login_sets_cookie_me_reads_user_and_logout_clears_session() {
    let app = test_app();
    bootstrap_admin(&app).await;

    let cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let response = request_empty(&app, Method::GET, "/api/auth/me", Some(&cookie)).await;
    let (status, value) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["user"]["email"], "admin@example.com");
    assert_eq!(value["user"]["role"], "admin");

    let response = request_json(
        &app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "admin@example.com",
            "password": "wrong-password"
        }),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = request_empty(&app, Method::POST, "/api/auth/logout", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response
        .headers()
        .get(header::SET_COOKIE)
        .expect("logout clears cookie")
        .to_str()
        .expect("cookie is ascii")
        .contains("Max-Age=0"));

    let response = request_empty(&app, Method::GET, "/api/auth/me", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn invite_creation_requires_expiry_and_max_uses() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let missing_expiry = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({ "max_uses": 1 }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(missing_expiry.status(), StatusCode::BAD_REQUEST);

    let missing_max_uses = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({ "expires_at": unix_now() + 86_400 }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(missing_max_uses.status(), StatusCode::BAD_REQUEST);

    configure_required_model_configs(&app, &admin_cookie).await;
    let created = create_invite(&app, &admin_cookie, unix_now() + 86_400, 1).await;

    assert!(created["token"].as_str().expect("token exists").len() >= 32);
    assert_eq!(created["invite"]["max_uses"], 1);
    assert_eq!(created["invite"]["used_count"], 0);
}

#[tokio::test]
async fn invite_creation_requires_ready_llm_and_title_model_configs() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

    let not_ready = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": unix_now() + 86_400,
            "max_uses": 1
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(not_ready.status(), StatusCode::CONFLICT);

    let llm_only = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "config_kind": "llm",
            "provider_name": "openai-compatible",
            "provider_base_url": "https://ready-provider.example/v1",
            "provider_api_key": "ready-provider-key",
            "default_model": "gpt-4.1-mini",
            "allowed_models": ["gpt-4.1-mini"],
            "allow_streaming": true,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(llm_only.status(), StatusCode::NO_CONTENT);

    let still_not_ready = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": unix_now() + 86_400,
            "max_uses": 1
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(still_not_ready.status(), StatusCode::CONFLICT);

    let title = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "config_kind": "title",
            "provider_name": "openai-compatible",
            "provider_base_url": "https://ready-provider.example/v1",
            "provider_api_key": "ready-provider-key",
            "default_model": "gpt-4.1-mini",
            "allowed_models": ["gpt-4.1-mini"],
            "allow_streaming": false,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(title.status(), StatusCode::NO_CONTENT);

    let ready = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": unix_now() + 86_400,
            "max_uses": 1
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(ready.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn invite_redemption_obeys_expiry_and_max_uses() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;

    let invite = create_invite(&app, &admin_cookie, unix_now() + 86_400, 2).await;
    let token = invite["token"].as_str().expect("token exists");

    let first = redeem_invite(&app, token, "first@example.com").await;
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = redeem_invite(&app, token, "second@example.com").await;
    assert_eq!(second.status(), StatusCode::CREATED);

    let third = redeem_invite(&app, token, "third@example.com").await;
    assert_eq!(third.status(), StatusCode::CONFLICT);

    let expired = create_invite(&app, &admin_cookie, unix_now() - 1, 1).await;
    let expired_token = expired["token"].as_str().expect("token exists");
    let response = redeem_invite(&app, expired_token, "late@example.com").await;

    assert_eq!(response.status(), StatusCode::GONE);
}

#[tokio::test]
async fn revoked_invites_cannot_be_redeemed() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;
    let invite = create_invite(&app, &admin_cookie, unix_now() + 86_400, 1).await;
    let invite_id = invite["invite"]["id"].as_str().expect("invite id exists");
    let token = invite["token"].as_str().expect("token exists");

    let response = request_empty(
        &app,
        Method::POST,
        &format!("/api/invites/{invite_id}/revoke"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = redeem_invite(&app, token, "revoked@example.com").await;
    assert_eq!(response.status(), StatusCode::GONE);
}

#[tokio::test]
async fn invite_registration_provisions_a_managed_hermes_instance() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;
    configure_required_model_configs(&app, &admin_cookie).await;
    let invite = create_invite(&app, &admin_cookie, unix_now() + 86_400, 1).await;
    let token = invite["token"].as_str().expect("token exists");

    let response = redeem_invite(&app, token, "test@example.com").await;
    let (status, registered) = response_json(response).await;
    assert_eq!(status, StatusCode::CREATED);
    let user_id = registered["user"]["id"]
        .as_str()
        .expect("registered user id");

    let instances = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-instances",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(instances).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instances"][0]["user_id"], user_id);
    assert_eq!(body["hermes_instances"][0]["kind"], "managed_docker");
    assert_eq!(body["hermes_instances"][0]["status"], "running");
}
