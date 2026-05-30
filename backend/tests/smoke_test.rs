use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use hermes_hub_backend::{app_config::AppConfig, build_router};
use tower::ServiceExt;

#[test]
fn loads_default_config_for_tests() {
    let config = AppConfig::for_tests();

    assert_eq!(config.bind_addr.to_string(), "127.0.0.1:0");
    assert_eq!(config.cookie_name, "hermes_hub_session");
}

#[tokio::test]
async fn builds_router_for_smoke_tests() {
    let config = AppConfig::for_tests();
    let router = build_router(config);

    let _ = router;
}

#[tokio::test]
async fn normal_api_routes_keep_small_body_limit() {
    let router = build_router(AppConfig::for_tests());
    let oversized_json = format!("{{\"kind\":\"{}\"}}", "x".repeat(9 * 1024 * 1024));

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/sessions")
                .header("content-type", "application/json")
                .body(Body::from(oversized_json))
                .expect("request can be built"),
        )
        .await
        .expect("router can handle request");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
