use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use hermes_hub_backend::{
    app_config::AppConfig,
    build_router_from_config, build_router_with_state,
    channel::service::ChannelStore,
    docker_config_from_app,
    hermes::docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
    ldap::DefaultLdapAuthenticator,
    llm_proxy::InMemoryLlmProviderClient,
    model_config::ModelRegistry,
    session::store::SessionStore,
    storage::InMemoryObjectStorage,
    AppInitError, AppState,
};
use std::sync::Arc;
use tower::ServiceExt;

fn test_router(config: AppConfig) -> axum::Router {
    let object_storage = InMemoryObjectStorage::new(config.object_storage.bucket.clone()).shared();
    let docker_provisioner = DockerProvisioner::new_with_runtime_and_object_storage(
        docker_config_from_app(&config, &config.initial_model_config),
        Arc::new(NoopDockerRuntime),
        object_storage.clone(),
    );
    let state = AppState {
        model_registry: ModelRegistry::in_memory_for_tests(config.initial_model_config.clone()),
        config,
        store: SessionStore::in_memory_for_tests(),
        channel_store: ChannelStore::in_memory_for_tests(),
        llm_provider: InMemoryLlmProviderClient::default().shared(),
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        docker_provisioner,
        object_storage,
        session_events: Default::default(),
    };

    build_router_with_state(state)
}

#[test]
fn loads_default_config_for_tests() {
    let config = AppConfig::for_tests();

    assert_eq!(config.bind_addr.to_string(), "127.0.0.1:0");
    assert_eq!(config.cookie_name, "hermes_hub_session");
}

#[tokio::test]
async fn builds_router_for_smoke_tests() {
    let config = AppConfig::for_tests();
    let router = test_router(config);

    let _ = router;
}

#[tokio::test]
async fn normal_api_routes_keep_small_body_limit() {
    let router = test_router(AppConfig::for_tests());
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

#[tokio::test]
async fn runtime_startup_requires_database_url() {
    let config = AppConfig::for_tests();
    let error = build_router_from_config(config)
        .await
        .expect_err("runtime startup without database must fail");

    assert!(matches!(error, AppInitError::MissingDatabaseUrl));
}

#[tokio::test]
async fn runtime_startup_requires_secret_master_key() {
    let mut config = AppConfig::for_tests();
    config.database_url =
        Some("postgres://hermes_hub:hermes_hub@127.0.0.1:55432/hermes_hub".into());
    let error = build_router_from_config(config)
        .await
        .expect_err("runtime startup without secret master key must fail");

    assert!(matches!(error, AppInitError::MissingSecretMasterKey));
}

#[tokio::test]
async fn runtime_startup_requires_object_storage_configuration() {
    let mut config = AppConfig::for_tests();
    config.database_url =
        Some("postgres://hermes_hub:hermes_hub@127.0.0.1:55432/hermes_hub".into());
    config.secret_master_key = Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into());
    let error = build_router_from_config(config)
        .await
        .expect_err("runtime startup without object storage config must fail");

    assert!(matches!(
        error,
        AppInitError::ObjectStorage(hermes_hub_backend::storage::ObjectStorageError::NotConfigured)
    ));
}

#[tokio::test]
async fn runtime_startup_requires_absolute_hermes_data_root() {
    let mut config = AppConfig::for_tests();
    config.database_url =
        Some("postgres://hermes_hub:hermes_hub@127.0.0.1:55432/hermes_hub".into());
    config.secret_master_key = Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into());
    config.hermes_docker.data_root = "$(pwd)/data/hub/users".into();
    config.object_storage.endpoint = Some("http://127.0.0.1:9000".into());
    config.object_storage.access_key = Some("minio".into());
    config.object_storage.secret_key = Some("minio123".into());

    let error = build_router_from_config(config)
        .await
        .expect_err("runtime startup with relative Hermes data root must fail");

    assert!(matches!(error, AppInitError::InvalidHermesDataRoot));
}

#[test]
fn test_config_keeps_absolute_hermes_data_root_unchanged() {
    let config = AppConfig::for_tests();

    assert_eq!(
        config.hermes_docker.data_root,
        std::path::PathBuf::from("/tmp/hermes-hub/users")
    );
}
