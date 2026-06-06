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
    model_config::ModelRegistry,
    session::store::SessionStore,
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::Value;
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
        model_registry: ModelRegistry::default_for_tests(),
        llm_provider: InMemoryLlmProviderClient::default().shared(),
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: Default::default(),
    }
}

fn test_app(state: AppState) -> Router {
    build_router_with_state(state)
}

async fn request_empty(app: &Router, method: Method, uri: &str) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("router responds")
}

#[tokio::test]
async fn api_docs_are_hidden_until_enabled() {
    let app = test_app(test_state());

    assert_eq!(
        request_empty(&app, Method::GET, "/api/docs").await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request_empty(&app, Method::GET, "/api/docs/openapi.json")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn api_docs_are_exposed_when_api_management_is_enabled() {
    let state = test_state();
    let mut settings = state
        .store
        .system_settings()
        .await
        .expect("settings can be read");
    settings.api_management.enabled = true;
    state
        .store
        .update_system_settings(settings)
        .await
        .expect("api management can be enabled");
    let app = test_app(state);

    let html_response = request_empty(&app, Method::GET, "/api/docs").await;
    assert_eq!(html_response.status(), StatusCode::OK);
    assert!(html_response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html")));
    let html = String::from_utf8(
        to_bytes(html_response.into_body(), usize::MAX)
            .await
            .expect("docs body")
            .to_vec(),
    )
    .expect("docs html");
    assert!(html.contains("SwaggerUIBundle"));

    let json_response = request_empty(&app, Method::GET, "/api/docs/openapi.json").await;
    assert_eq!(json_response.status(), StatusCode::OK);
    let spec: Value = serde_json::from_slice(
        &to_bytes(json_response.into_body(), usize::MAX)
            .await
            .expect("openapi body"),
    )
    .expect("openapi json");
    assert_eq!(spec["openapi"], "3.0.3");
    assert!(spec["paths"]["/api/oauth/token"].is_object());
    assert!(spec["paths"]["/api/integrations/sessions"].is_object());
    assert!(spec["paths"]["/api/channels"].is_null());
    assert!(spec["paths"]["/api/auth/me"].is_null());
}
