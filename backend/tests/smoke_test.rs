use hermes_hub_backend::{app_config::AppConfig, build_router};

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
