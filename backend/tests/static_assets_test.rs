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
    std::fs::write(static_dir.path().join("index.html"), "<main>Hermes Hub App</main>")
        .expect("index can be written");
    std::fs::create_dir_all(static_dir.path().join("assets")).expect("assets dir can be created");
    std::fs::write(static_dir.path().join("assets/app.js"), "console.log('hub')")
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
}
