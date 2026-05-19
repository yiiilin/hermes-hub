use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use hermes_hub_backend::{build_router, AppConfig};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
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

    let created = create_invite(&app, &admin_cookie, unix_now() + 86_400, 1).await;

    assert!(created["token"].as_str().expect("token exists").len() >= 32);
    assert_eq!(created["invite"]["max_uses"], 1);
    assert_eq!(created["invite"]["used_count"], 0);
}

#[tokio::test]
async fn invite_redemption_obeys_expiry_and_max_uses() {
    let app = test_app();
    bootstrap_admin(&app).await;
    let admin_cookie = login(&app, "admin@example.com", "admin-password-123").await;

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
