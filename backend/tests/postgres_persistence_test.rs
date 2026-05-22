use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::{ChannelSessionKind, ChannelStore},
    db::migrations::run_migrations,
    docker_config_from_app,
    hermes::{
        docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
        event_streams::HermesEventStreamRegistry,
        proxy_client::InMemoryHermesProxyClient,
    },
    llm_proxy::{InMemoryLlmProviderClient, LlmProviderResponse},
    model_config::{ModelConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE, LLM_MODEL_CONFIG_KIND},
    security::crypto::SecretCipher,
    session::store::SessionStore,
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;
use uuid::Uuid;

const TEST_SECRET_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

async fn postgres_pool() -> Option<PgPool> {
    let database_url = std::env::var("HERMES_HUB_TEST_DATABASE_URL").ok()?;
    let schema = format!("persistence_{}", Uuid::new_v4().simple());
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("postgres test database is reachable");

    sqlx::query(&format!(r#"create schema "{schema}""#))
        .execute(&pool)
        .await
        .expect("test schema can be created");
    sqlx::query(&format!(r#"set search_path to "{schema}", public"#))
        .execute(&pool)
        .await
        .expect("test schema can be selected");

    Some(pool)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}

async fn test_state(pool: PgPool, provider: InMemoryLlmProviderClient) -> AppState {
    let cipher = SecretCipher::from_master_key(TEST_SECRET_KEY).expect("test cipher is valid");
    let config = AppConfig::for_tests();
    let default_config = ModelConfig {
        config_kind: LLM_MODEL_CONFIG_KIND.to_string(),
        provider_name: "openai-compatible".to_string(),
        provider_base_url: "https://provider-default.example/v1".to_string(),
        provider_api_key: "provider-default-key".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string()],
        api_type: CHAT_COMPLETIONS_API_TYPE.to_string(),
        reasoning_effort: None,
        allow_streaming: true,
        request_timeout_seconds: 60,
    };
    let model_registry = ModelRegistry::postgres(pool.clone(), cipher.clone(), default_config)
        .await
        .expect("model registry can be initialized");

    AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
        ),
        config,
        store: SessionStore::postgres(pool.clone(), cipher),
        channel_store: ChannelStore::postgres(pool),
        hermes_proxy: InMemoryHermesProxyClient::default().shared(),
        hermes_event_streams: HermesEventStreamRegistry::default(),
        model_registry,
        llm_provider: provider.shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
    }
}

fn test_app(state: AppState) -> Router {
    build_router_with_state(state)
}

async fn request_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Value,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }

    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
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
    bearer: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }

    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
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

#[tokio::test(flavor = "multi_thread")]
async fn postgres_state_survives_recreated_router_state() {
    let Some(pool) = postgres_pool().await else {
        eprintln!("skipping postgres persistence test: HERMES_HUB_TEST_DATABASE_URL is not set");
        return;
    };
    run_migrations(&pool).await.expect("migrations can run");

    let provider = InMemoryLlmProviderClient::new(LlmProviderResponse {
        status: StatusCode::OK,
        content_type: Some("application/json".to_string()),
        body: br#"{"id":"provider-response"}"#.to_vec(),
    });
    let state = test_state(pool.clone(), provider.clone()).await;
    let app = test_app(state.clone());

    let created = request_json(
        &app,
        Method::POST,
        "/api/auth/bootstrap-register",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123"
        }),
        None,
        None,
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let admin_login = request_json(
        &app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "admin@example.com",
            "password": "admin-password-123"
        }),
        None,
        None,
    )
    .await;
    assert_eq!(admin_login.status(), StatusCode::OK);
    let admin_cookie = cookie_from(&admin_login);

    let invite = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": unix_now() + 24 * 60 * 60,
            "max_uses": 1
        }),
        Some(&admin_cookie),
        None,
    )
    .await;
    let (status, invite_body) = response_json(invite).await;
    assert_eq!(status, StatusCode::CREATED);
    let invite_token = invite_body["token"].as_str().expect("invite token");

    let registered = request_json(
        &app,
        Method::POST,
        "/api/auth/register",
        json!({
            "invite_token": invite_token,
            "email": "user@example.com",
            "password": "user-password-123"
        }),
        None,
        None,
    )
    .await;
    assert_eq!(registered.status(), StatusCode::CREATED);

    let user_login = request_json(
        &app,
        Method::POST,
        "/api/auth/login",
        json!({
            "email": "user@example.com",
            "password": "user-password-123"
        }),
        None,
        None,
    )
    .await;
    assert_eq!(user_login.status(), StatusCode::OK);
    let user_cookie = cookie_from(&user_login);

    let update_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "openai-compatible",
            "provider_base_url": "https://provider-persisted.example/v1",
            "provider_api_key": "provider-persisted-key",
            "default_model": "gpt-4.1",
            "allowed_models": ["gpt-4.1"],
            "allow_streaming": true,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(update_model.status(), StatusCode::NO_CONTENT);

    let ensured = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, ensured_body) = response_json(ensured).await;
    assert_eq!(status, StatusCode::OK);
    let instance_id = ensured_body["hermes_instance"]["id"]
        .as_str()
        .expect("instance id")
        .to_string();
    state
        .model_registry
        .add_instance_token_for_instance(&instance_id, "persisted-instance-token")
        .await
        .expect("instance token can be stored");

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&user_cookie), None).await;
    let (status, channel_body) = response_json(channel).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(channel_body["channels"][0]["name"], "hermes-hub");
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id");

    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({
            "kind": "agent"
        }),
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, session_body) = response_json(session).await;
    assert_eq!(status, StatusCode::CREATED);
    let session_id = session_body["session"]["id"].as_str().expect("session id");

    let title = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/title"),
        json!({
            "prompt": "persisted session"
        }),
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, title_body) = response_json(title).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(title_body["session"]["title"], "persisted session");

    let message = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        json!({
            "role": "user",
            "content": "persisted message",
            "attachments": [
                {
                    "id": "attachment-json-only",
                    "name": "note.txt",
                    "content_type": "text/plain",
                    "kind": "file",
                    "size": 12
                }
            ]
        }),
        Some(&user_cookie),
        None,
    )
    .await;
    assert_eq!(message.status(), StatusCode::CREATED);

    let restarted_state = test_state(pool, provider.clone()).await;
    let restarted_app = test_app(restarted_state);

    let me = request_empty(
        &restarted_app,
        Method::GET,
        "/api/auth/me",
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, me_body) = response_json(me).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me_body["user"]["email"], "user@example.com");

    let invites = request_empty(
        &restarted_app,
        Method::GET,
        "/api/invites",
        Some(&admin_cookie),
        None,
    )
    .await;
    let (status, invites_body) = response_json(invites).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(invites_body["invites"][0]["used_count"], 1);

    let channels = request_empty(
        &restarted_app,
        Method::GET,
        "/api/channels",
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, channels_body) = response_json(channels).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(channels_body["channels"][0]["name"], "hermes-hub");

    let sessions = request_empty(
        &restarted_app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions"),
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, sessions_body) = response_json(sessions).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sessions_body["sessions"][0]["title"], "persisted session");

    let messages = request_empty(
        &restarted_app,
        Method::GET,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, messages_body) = response_json(messages).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(messages_body["messages"][0]["content"], "persisted message");
    assert_eq!(
        messages_body["messages"][0]["attachments"][0]["name"],
        "note.txt"
    );

    let workspace = request_empty(
        &restarted_app,
        Method::GET,
        "/api/workspace/status",
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, workspace_body) = response_json(workspace).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(workspace_body["hermes_instance"]["id"], instance_id);

    let llm = request_json(
        &restarted_app,
        Method::POST,
        "/internal/llm/v1/responses",
        json!({ "input": "hello" }),
        None,
        Some("persisted-instance-token"),
    )
    .await;
    assert_eq!(llm.status(), StatusCode::OK);
    let forwarded = provider.last_request().expect("provider request");
    assert_eq!(
        forwarded.provider_base_url,
        "https://provider-persisted.example/v1"
    );
    assert_eq!(forwarded.authorization, "Bearer provider-persisted-key");
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_channel_session_persists_hermes_anchors() {
    let Some(pool) = postgres_pool().await else {
        eprintln!("skipping postgres persistence test: HERMES_HUB_TEST_DATABASE_URL is not set");
        return;
    };
    run_migrations(&pool).await.expect("migrations can run");

    let cipher = SecretCipher::from_master_key(TEST_SECRET_KEY).expect("test cipher is valid");
    let store = SessionStore::postgres(pool.clone(), cipher.clone());
    let channel_store = ChannelStore::postgres(pool);

    let user = store
        .create_bootstrap_admin("adapter@example.com", "adapter-password-123")
        .await
        .expect("bootstrap admin can be created");
    let channel = channel_store
        .ensure_hub_channel(&user.id)
        .await
        .expect("hub channel can be ensured");
    let session = channel_store
        .create_session(&user.id, &channel.id, ChannelSessionKind::Agent, None)
        .await
        .expect("session can be created");

    let updated = channel_store
        .update_session_hermes_anchors(
            &user.id,
            &channel.id,
            &session.id,
            Some("hermes-session-1"),
            Some("hermes-response-1"),
            Some("hermes-run-1"),
        )
        .await
        .expect("hermes anchors can be persisted");

    assert_eq!(
        updated.hermes_session_id.as_deref(),
        Some("hermes-session-1")
    );
    assert_eq!(
        updated.hermes_response_id.as_deref(),
        Some("hermes-response-1")
    );
    assert_eq!(updated.hermes_run_id.as_deref(), Some("hermes-run-1"));

    let fetched = channel_store
        .get_session(&user.id, &channel.id, &session.id)
        .await
        .expect("session can be reloaded");
    assert_eq!(
        fetched.hermes_session_id.as_deref(),
        Some("hermes-session-1")
    );
    assert_eq!(
        fetched.hermes_response_id.as_deref(),
        Some("hermes-response-1")
    );
    assert_eq!(fetched.hermes_run_id.as_deref(), Some("hermes-run-1"));

    let cleared = channel_store
        .clear_session_hermes_run_id(&user.id, &channel.id, &session.id)
        .await
        .expect("hermes run anchor can be cleared");
    assert!(cleared.hermes_run_id.is_none());
    assert_eq!(
        cleared.hermes_session_id.as_deref(),
        Some("hermes-session-1")
    );
    assert_eq!(
        cleared.hermes_response_id.as_deref(),
        Some("hermes-response-1")
    );
}
