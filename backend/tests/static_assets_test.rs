use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
};
use hermes_hub_backend::{build_router, AppConfig};
use tempfile::tempdir;
use tower::ServiceExt;

#[tokio::test]
async fn backend_serves_frontend_static_assets_and_spa_fallback() {
    let static_dir = tempdir().expect("static dir can be created");
    std::fs::write(
        static_dir.path().join("index.html"),
        "<main>Hermes Hub App</main>",
    )
    .expect("index can be written");
    std::fs::create_dir_all(static_dir.path().join("assets")).expect("assets dir can be created");
    std::fs::write(
        static_dir.path().join("assets/app.js"),
        "console.log('hub')",
    )
    .expect("asset can be written");

    let mut config = AppConfig::for_tests();
    config.static_dir = static_dir.path().to_path_buf();
    let app = build_router(config);

    let asset = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/assets/app.js")
                .body(Body::empty())
                .expect("request can be built"),
        )
        .await
        .expect("asset response");
    assert_eq!(asset.status(), StatusCode::OK);
    let body = to_bytes(asset.into_body(), usize::MAX)
        .await
        .expect("asset body");
    assert_eq!(body, "console.log('hub')");

    let spa = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/channels/anything")
                .body(Body::empty())
                .expect("request can be built"),
        )
        .await
        .expect("spa response");
    assert_eq!(spa.status(), StatusCode::OK);
    let body = to_bytes(spa.into_body(), usize::MAX)
        .await
        .expect("spa body");
    assert_eq!(body, "<main>Hermes Hub App</main>");

    let api_miss = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/not-a-real-route")
                .body(Body::empty())
                .expect("request can be built"),
        )
        .await
        .expect("api miss response");
    assert_eq!(api_miss.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(api_miss.into_body(), usize::MAX)
        .await
        .expect("api miss body");
    assert!(
        String::from_utf8_lossy(&body).contains("api route not found"),
        "未知 API 路径必须返回 JSON 404，不能落到 SPA fallback 变成含糊的 405"
    );
}

#[tokio::test]
async fn backend_serves_pwa_manifest_and_service_worker_as_static_assets() {
    let mut config = AppConfig::for_tests();
    let repository_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate has a repository root");
    config.static_dir = repository_root.join("frontend/public");
    let app = build_router(config);

    for uri in ["/manifest.webmanifest", "/service-worker.js"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request can be built"),
            )
            .await
            .expect("pwa asset response");
        assert_eq!(response.status(), StatusCode::OK, "{uri} must be served");
    }
}
