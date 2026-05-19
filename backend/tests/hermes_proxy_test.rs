use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::ChannelStore,
    hermes::{
        instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
        proxy_client::{HermesProxyResponse, InMemoryHermesProxyClient},
    },
    llm_proxy::{InMemoryLlmProviderClient, LlmProviderResponse},
    model_config::ModelRegistry,
    session::store::SessionStore,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use tower::ServiceExt;

fn test_state(store: SessionStore, proxy: InMemoryHermesProxyClient) -> AppState {
    AppState {
        config: AppConfig::for_tests(),
        store,
        channel_store: ChannelStore::default(),
        hermes_proxy: proxy,
        model_registry: ModelRegistry::default_for_tests(),
        llm_provider: InMemoryLlmProviderClient::new(LlmProviderResponse {
            status: StatusCode::OK,
            content_type: Some("application/json".to_string()),
            body: b"{}".to_vec(),
        }),
    }
}

fn test_app(store: SessionStore, proxy: InMemoryHermesProxyClient) -> Router {
    build_router_with_state(test_state(store, proxy))
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

async fn bootstrap_and_login(app: &Router) -> String {
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

fn managed_instance_for(user_id: &str) -> HermesInstance {
    HermesInstance {
        id: "instance-1".to_string(),
        user_id: user_id.to_string(),
        kind: HermesInstanceKind::ManagedDocker,
        status: HermesInstanceStatus::Running,
        name: "hermes-user-admin".to_string(),
        base_url: "http://hermes-user-admin:8000".to_string(),
        api_token_secret_ref: Some("hermes-secret-token".to_string()),
        container_id: Some("container-1".to_string()),
        host_workspace_path: Some("/tmp/hermes/admin/workspace".to_string()),
        host_sandbox_path: Some("/tmp/hermes/admin/sandbox".to_string()),
        host_config_path: Some("/tmp/hermes/admin/config".to_string()),
        health_status: "healthy".to_string(),
    }
}

#[tokio::test]
async fn hermes_proxy_test() {
    let proxy = InMemoryHermesProxyClient::new(HermesProxyResponse {
        status: StatusCode::OK,
        content_type: Some("text/event-stream".to_string()),
        body: "event: message\ndata: hello\n\n".as_bytes().to_vec(),
    });
    let store = SessionStore::default();
    let app = test_app(store.clone(), proxy.clone());
    let cookie = bootstrap_and_login(&app).await;
    let user_id = store
        .user_by_session_cookie(&cookie, "hermes_hub_session")
        .expect("user can be read from session")
        .id;
    store
        .bind_hermes_instance(managed_instance_for(&user_id))
        .expect("instance can be bound");

    let create_channel = request_json(
        &app,
        Method::POST,
        "/api/channels",
        json!({ "name": "research", "description": "Research channel" }),
        Some(&cookie),
    )
    .await;
    let (status, channel_body) = response_json(create_channel).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(channel_body["channel"]["name"], "research");
    let channel_id = channel_body["channel"]["id"].as_str().expect("channel id");

    let create_session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent", "title": "first run" }),
        Some(&cookie),
    )
    .await;
    let (status, session_body) = response_json(create_session).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(session_body["session"]["kind"], "agent");

    let proxied = request_json(
        &app,
        Method::POST,
        "/api/hermes/v1/runs?stream=true",
        json!({ "prompt": "hello" }),
        Some(&cookie),
    )
    .await;
    assert_eq!(proxied.status(), StatusCode::OK);
    assert_eq!(
        proxied
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("stream content type")
            .to_str()
            .expect("header is ascii"),
        "text/event-stream"
    );
    let bytes = to_bytes(proxied.into_body(), usize::MAX)
        .await
        .expect("stream body can be read");
    assert_eq!(bytes, "event: message\ndata: hello\n\n");

    let forwarded = proxy.last_request().expect("request forwarded");
    assert_eq!(forwarded.method, Method::POST);
    assert_eq!(forwarded.path_and_query, "/v1/runs?stream=true");
    assert_eq!(forwarded.body, br#"{"prompt":"hello"}"#);
    assert_eq!(forwarded.instance_base_url, "http://hermes-user-admin:8000");
    assert_eq!(
        forwarded.authorization,
        Some("Bearer hermes-secret-token".to_string())
    );

    let denied = request_empty(&app, Method::GET, "/api/hermes/admin/config", Some(&cookie)).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
}
