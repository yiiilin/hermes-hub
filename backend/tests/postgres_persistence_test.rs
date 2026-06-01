use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use hermes_hub_backend::{
    build_router_with_state,
    channel::service::{ChannelMessageRole, ChannelRunStatus, ChannelSessionKind, ChannelStore},
    db::migrations::run_migrations,
    docker_config_from_app,
    hermes::{
        docker_provisioner::{DockerProvisioner, NoopDockerRuntime},
        instance::{HermesInstance, HermesInstanceStatus},
    },
    ldap::DefaultLdapAuthenticator,
    llm_proxy::{InMemoryLlmProviderClient, LlmProviderResponse},
    model_config::{ModelConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE, LLM_MODEL_CONFIG_KIND},
    security::crypto::SecretCipher,
    session::store::{
        LdapSettings, OidcSettings, SessionStore, SpeechInputSettings, SystemSettings,
    },
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
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
        enabled: true,
        allow_streaming: true,
        request_timeout_seconds: 60,
        context_window_tokens: 128_000,
        max_output_tokens: 4096,
        temperature: 0.7,
        supports_parallel_tools: true,
        fallback: None,
    };
    let model_registry = ModelRegistry::postgres(pool.clone(), cipher.clone(), default_config)
        .await
        .expect("model registry can be initialized");

    let asr_client = hermes_hub_backend::asr::default_asr_client(&config.speech_input);
    AppState {
        docker_provisioner: DockerProvisioner::new_with_runtime(
            docker_config_from_app(&config, &config.initial_model_config),
            Arc::new(NoopDockerRuntime),
        ),
        config,
        store: SessionStore::postgres(pool.clone(), cipher),
        channel_store: ChannelStore::postgres(pool),
        model_registry,
        llm_provider: provider.shared(),
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        object_storage: InMemoryObjectStorage::default().shared(),
        session_events: Default::default(),
        asr_client,
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

async fn request_raw(
    app: &Router,
    method: Method,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type);

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }

    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }

    app.clone()
        .oneshot(
            builder
                .body(Body::from(body))
                .expect("request can be built"),
        )
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
async fn postgres_user_password_update_persists() {
    let Some(pool) = postgres_pool().await else {
        eprintln!("skipping postgres persistence test: HERMES_HUB_TEST_DATABASE_URL is not set");
        return;
    };
    run_migrations(&pool).await.expect("migrations can run");

    let cipher = SecretCipher::from_master_key(TEST_SECRET_KEY).expect("test cipher is valid");
    let store = SessionStore::postgres(pool.clone(), cipher.clone());
    store
        .create_bootstrap_admin("admin@example.com", "admin-password-123")
        .await
        .expect("admin can be created");

    let admin = store
        .login("admin@example.com", "admin-password-123")
        .await
        .expect("old password initially works");
    store
        .update_user_password(&admin.id, "new-password-456")
        .await
        .expect("postgres password hash can be updated");

    assert!(store
        .login("admin@example.com", "admin-password-123")
        .await
        .is_err());

    // 重新创建 store，确保新密码不是只写在内存对象里。
    let reloaded_store = SessionStore::postgres(pool, cipher);
    let reloaded_admin = reloaded_store
        .login("admin@example.com", "new-password-456")
        .await
        .expect("new password survives store recreation");
    assert_eq!(reloaded_admin.id, admin.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_external_auth_users_share_password_login_by_email() {
    let Some(pool) = postgres_pool().await else {
        eprintln!("skipping postgres persistence test: HERMES_HUB_TEST_DATABASE_URL is not set");
        return;
    };
    run_migrations(&pool).await.expect("migrations can run");

    let cipher = SecretCipher::from_master_key(TEST_SECRET_KEY).expect("test cipher is valid");
    let store = SessionStore::postgres(pool.clone(), cipher.clone());
    let admin = store
        .create_bootstrap_admin("admin@example.com", "admin-password-123")
        .await
        .expect("admin can be created");

    let linked_oidc = store
        .get_or_create_oidc_user("ADMIN@example.com", false)
        .await
        .expect("OIDC login links existing user by email");
    assert!(!linked_oidc.created);
    assert_eq!(linked_oidc.user.id, admin.id);

    let linked_ldap = store
        .get_or_create_ldap_user("admin@example.com", false)
        .await
        .expect("LDAP login links existing user by email");
    assert!(!linked_ldap.created);
    assert_eq!(linked_ldap.user.id, admin.id);

    let oidc_user = store
        .get_or_create_oidc_user("oidc-user@example.com", true)
        .await
        .expect("OIDC user can be auto-created");
    store
        .update_user_password(&oidc_user.user.id, "oidc-local-password")
        .await
        .expect("OIDC-created user can set local password");

    let ldap_user = store
        .get_or_create_ldap_user("ldap-user@example.com", true)
        .await
        .expect("LDAP user can be auto-created");
    store
        .update_user_password(&ldap_user.user.id, "ldap-local-password")
        .await
        .expect("LDAP-created user can set local password");

    // 重新创建 store，确保外部认证用户补充的本地密码持久化在 PostgreSQL 中。
    let reloaded_store = SessionStore::postgres(pool, cipher);
    let reloaded_oidc = reloaded_store
        .login("oidc-user@example.com", "oidc-local-password")
        .await
        .expect("OIDC-created user can later use password login");
    assert_eq!(reloaded_oidc.id, oidc_user.user.id);

    let reloaded_ldap = reloaded_store
        .login("ldap-user@example.com", "ldap-local-password")
        .await
        .expect("LDAP-created user can later use password login");
    assert_eq!(reloaded_ldap.id, ldap_user.user.id);
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
            "request_timeout_seconds": 30,
            "fallback": {
                "enabled": true,
                "provider_name": "fallback-openai-compatible",
                "provider_base_url": "https://provider-fallback.example/v1",
                "provider_api_key": "provider-fallback-key",
                "default_model": "gpt-4.1-fallback",
                "allowed_models": ["gpt-4.1-fallback"],
                "api_type": "chat_completions",
                "reasoning_effort": null,
                "allow_streaming": true,
                "request_timeout_seconds": 45,
                "context_window_tokens": 64000,
                "max_output_tokens": 2048,
                "temperature": 0.2,
                "supports_parallel_tools": false
            }
        }),
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(update_model.status(), StatusCode::NO_CONTENT);
    let stored_fallback_key: Option<String> = sqlx::query_scalar(
        "select fallback_config->>'provider_api_key' from model_configs where config_kind = 'llm'",
    )
    .fetch_one(&pool)
    .await
    .expect("fallback config can be queried");
    assert_ne!(
        stored_fallback_key.as_deref(),
        Some("provider-fallback-key")
    );

    let saved_model = request_empty(
        &app,
        Method::GET,
        "/api/admin/model-config",
        Some(&admin_cookie),
        None,
    )
    .await;
    let (status, saved_model_body) = response_json(saved_model).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        saved_model_body["model_config"]["fallback"]["provider_api_key"],
        "provider-fallback-key"
    );

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

    let boundary = "postgres-persistence-attachment-boundary";
    let upload_body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"note.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         persisted note\r\n\
         --{boundary}--\r\n"
    )
    .into_bytes();
    let upload = request_raw(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/attachments"),
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&user_cookie),
        None,
    )
    .await;
    let (status, upload_body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);

    let message = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/messages"),
        json!({
            "role": "user",
            "content": "persisted message",
            "attachments": upload_body["attachments"].clone()
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
async fn postgres_adapter_can_poll_when_channel_predates_instance() {
    let Some(pool) = postgres_pool().await else {
        eprintln!(
            "skipping postgres adapter binding test: HERMES_HUB_TEST_DATABASE_URL is not set"
        );
        return;
    };
    run_migrations(&pool).await.expect("migrations can run");

    let provider = InMemoryLlmProviderClient::new(LlmProviderResponse {
        status: StatusCode::OK,
        content_type: Some("application/json".to_string()),
        body: br#"{"id":"provider-response"}"#.to_vec(),
    });
    let state = test_state(pool, provider).await;
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
    let logged_in = request_json(
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
    assert_eq!(logged_in.status(), StatusCode::OK);
    let cookie = cookie_from(&logged_in);

    let channel = request_empty(&app, Method::GET, "/api/channels", Some(&cookie), None).await;
    let (status, channel_body) = response_json(channel).await;
    assert_eq!(status, StatusCode::OK);
    let channel_id = channel_body["channels"][0]["id"]
        .as_str()
        .expect("channel id")
        .to_string();

    let session = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions"),
        json!({ "kind": "agent" }),
        Some(&cookie),
        None,
    )
    .await;
    let (status, session_body) = response_json(session).await;
    assert_eq!(status, StatusCode::CREATED);
    let session_id = session_body["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();

    // 生产里用户可能先进入会话页生成 channel，之后 ensure Hermes 才创建实例。
    // ensure 成功后必须反向补齐 channel.hermes_instance_id，否则 adapter 按实例 token 拉不到队列。
    let ensured = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&cookie),
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
        .add_instance_token_for_instance(&instance_id, "late-bound-instance-token")
        .await
        .expect("instance token can be stored");

    let created_run = request_json(
        &app,
        Method::POST,
        &format!("/api/channels/{channel_id}/sessions/{session_id}/runs"),
        json!({
            "content": "late bound channel",
            "attachments": [],
            "client_message_key": "late-bound-turn"
        }),
        Some(&cookie),
        None,
    )
    .await;
    let (status, created_run_body) = response_json(created_run).await;
    assert_eq!(status, StatusCode::CREATED);
    let run_id = created_run_body["run"]["run_id"]
        .as_str()
        .expect("run id")
        .to_string();

    let inbox = request_empty(
        &app,
        Method::GET,
        "/internal/channel/v1/inbox?timeout_seconds=0&limit=4",
        None,
        Some("late-bound-instance-token"),
    )
    .await;
    let (status, inbox_body) = response_json(inbox).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inbox_body["items"].as_array().expect("items").len(), 1);
    assert_eq!(inbox_body["items"][0]["id"], run_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_adapter_releases_stale_running_runs() {
    let Some(pool) = postgres_pool().await else {
        eprintln!(
            "skipping postgres stale run recovery test: HERMES_HUB_TEST_DATABASE_URL is not set"
        );
        return;
    };
    run_migrations(&pool).await.expect("migrations can run");

    let provider = InMemoryLlmProviderClient::new(LlmProviderResponse {
        status: StatusCode::OK,
        content_type: Some("application/json".to_string()),
        body: br#"{"id":"provider-response"}"#.to_vec(),
    });
    let state = test_state(pool.clone(), provider).await;
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
    let logged_in = request_json(
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
    assert_eq!(logged_in.status(), StatusCode::OK);
    let cookie = cookie_from(&logged_in);

    let ensured = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&cookie),
        None,
    )
    .await;
    let (status, ensured_body) = response_json(ensured).await;
    assert_eq!(status, StatusCode::OK);
    let instance_id = ensured_body["hermes_instance"]["id"]
        .as_str()
        .expect("instance id")
        .to_string();

    let channel = state
        .channel_store
        .ensure_hub_channel(
            ensured_body["hermes_instance"]["user_id"]
                .as_str()
                .expect("user id"),
        )
        .await
        .expect("hub channel can be ensured");
    let session = state
        .channel_store
        .create_session(
            ensured_body["hermes_instance"]["user_id"]
                .as_str()
                .expect("user id"),
            &channel.id,
            ChannelSessionKind::Agent,
            None,
        )
        .await
        .expect("session can be created");
    let user_message = state
        .channel_store
        .append_session_message(
            ensured_body["hermes_instance"]["user_id"]
                .as_str()
                .expect("user id"),
            &channel.id,
            &session.id,
            hermes_hub_backend::channel::service::ChannelMessageRole::User,
            Some("stale-running-turn".to_string()),
            "recover me".to_string(),
            json!([]),
        )
        .await
        .expect("message can be appended");
    let run = state
        .channel_store
        .create_channel_run(
            ensured_body["hermes_instance"]["user_id"]
                .as_str()
                .expect("user id"),
            &channel.id,
            &session.id,
            &user_message.id,
            "recover me".to_string(),
            json!([]),
        )
        .await
        .expect("run can be created");
    state
        .channel_store
        .update_run_status_for_session(
            &session.id,
            &run.run_id,
            ChannelRunStatus::Running,
            None,
            None,
        )
        .await
        .expect("run can be marked running");
    sqlx::query(
        "update channel_runs set updated_at = now() - interval '11 minutes' where id = $1::uuid",
    )
    .bind(&run.id)
    .execute(&pool)
    .await
    .expect("run timestamp can be aged");

    let leased = state
        .channel_store
        .lease_runs_for_instance(Some(&instance_id), 4)
        .await
        .expect("stale running run can be leased");
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].run_id, run.run_id);
    assert_eq!(leased[0].status, ChannelRunStatus::Leased);
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_adapter_output_heartbeats_running_runs() {
    let Some(pool) = postgres_pool().await else {
        eprintln!(
            "skipping postgres running run heartbeat test: HERMES_HUB_TEST_DATABASE_URL is not set"
        );
        return;
    };
    run_migrations(&pool).await.expect("migrations can run");

    let provider = InMemoryLlmProviderClient::new(LlmProviderResponse {
        status: StatusCode::OK,
        content_type: Some("application/json".to_string()),
        body: br#"{"id":"provider-response"}"#.to_vec(),
    });
    let state = test_state(pool.clone(), provider).await;
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
    let logged_in = request_json(
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
    assert_eq!(logged_in.status(), StatusCode::OK);
    let cookie = cookie_from(&logged_in);

    let ensured = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&cookie),
        None,
    )
    .await;
    let (status, ensured_body) = response_json(ensured).await;
    assert_eq!(status, StatusCode::OK);
    let user_id = ensured_body["hermes_instance"]["user_id"]
        .as_str()
        .expect("user id")
        .to_string();
    let instance_id = ensured_body["hermes_instance"]["id"]
        .as_str()
        .expect("instance id")
        .to_string();
    state
        .model_registry
        .add_instance_token_for_instance(&instance_id, "heartbeat-instance-token")
        .await
        .expect("instance token can be stored");

    let channel = state
        .channel_store
        .ensure_hub_channel(&user_id)
        .await
        .expect("hub channel can be ensured");
    let session = state
        .channel_store
        .create_session(&user_id, &channel.id, ChannelSessionKind::Agent, None)
        .await
        .expect("session can be created");
    let user_message = state
        .channel_store
        .append_session_message(
            &user_id,
            &channel.id,
            &session.id,
            ChannelMessageRole::User,
            Some("heartbeat-turn".to_string()),
            "long task".to_string(),
            json!([]),
        )
        .await
        .expect("message can be appended");
    let run = state
        .channel_store
        .create_channel_run(
            &user_id,
            &channel.id,
            &session.id,
            &user_message.id,
            "long task".to_string(),
            json!([]),
        )
        .await
        .expect("run can be created");
    let initial_lease = state
        .channel_store
        .lease_runs_for_instance(Some(&instance_id), 4)
        .await
        .expect("queued run can be leased");
    assert_eq!(initial_lease.len(), 1);
    assert_eq!(initial_lease[0].run_id, run.run_id);
    assert_eq!(initial_lease[0].attempt_count, 1);
    state
        .channel_store
        .update_run_status_for_session(
            &session.id,
            &run.run_id,
            ChannelRunStatus::Running,
            None,
            None,
        )
        .await
        .expect("run can be marked running");
    sqlx::query(
        "update channel_runs set updated_at = now() - interval '11 minutes' where id = $1::uuid",
    )
    .bind(&run.id)
    .execute(&pool)
    .await
    .expect("run timestamp can be aged");

    // Hermes 长任务只要还在向 Hub 输出进度，就不是失联任务；否则 10 分钟恢复逻辑会重复投递。
    let progress = request_json(
        &app,
        Method::POST,
        &format!("/internal/channel/v1/sessions/{}/messages", session.id),
        json!({
            "role": "assistant",
            "content": "🔧 terminal([\"command\"])\n{\"command\":\"node build.js\"}",
            "attachments": [],
            "run_id": run.run_id
        }),
        None,
        Some("heartbeat-instance-token"),
    )
    .await;
    assert_eq!(progress.status(), StatusCode::CREATED);
    let (_, progress_body) = response_json(progress).await;
    let progress_message_id = progress_body["message"]["id"]
        .as_str()
        .expect("progress message id")
        .to_string();

    let leased = state
        .channel_store
        .lease_runs_for_instance(Some(&instance_id), 4)
        .await
        .expect("active running run can be checked");
    assert!(
        leased.is_empty(),
        "recent adapter output must refresh running run heartbeat and prevent duplicate leasing"
    );

    let active = state
        .channel_store
        .get_active_run_for_session(&user_id, &channel.id, &session.id)
        .await
        .expect("active run can be loaded")
        .expect("run remains visible");
    assert_eq!(active.run_id, run.run_id);
    assert_eq!(active.status, ChannelRunStatus::Running);
    assert_eq!(active.attempt_count, 1);

    sqlx::query(
        "update channel_runs set updated_at = now() - interval '11 minutes' where id = $1::uuid",
    )
    .bind(&run.id)
    .execute(&pool)
    .await
    .expect("run timestamp can be aged again");

    let progress_edit = request_json(
        &app,
        Method::PUT,
        &format!(
            "/internal/channel/v1/sessions/{}/messages/{progress_message_id}",
            session.id
        ),
        json!({
            "content": "🔧 terminal([\"command\"])\n{\"command\":\"node build.js\"}\n✅ terminal completed",
            "attachments": [],
            "run_id": run.run_id
        }),
        None,
        Some("heartbeat-instance-token"),
    )
    .await;
    assert_eq!(progress_edit.status(), StatusCode::OK);

    let leased = state
        .channel_store
        .lease_runs_for_instance(Some(&instance_id), 4)
        .await
        .expect("edited active running run can be checked");
    assert!(
        leased.is_empty(),
        "adapter message edits must also refresh running run heartbeat"
    );
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

#[tokio::test(flavor = "multi_thread")]
async fn postgres_can_bind_stopped_hermes_instance_without_activity_timestamp() {
    let Some(pool) = postgres_pool().await else {
        eprintln!(
            "skipping postgres stopped Hermes bind test: HERMES_HUB_TEST_DATABASE_URL is not set"
        );
        return;
    };
    run_migrations(&pool).await.expect("migrations can run");

    let cipher = SecretCipher::from_master_key(TEST_SECRET_KEY).expect("test cipher is valid");
    let store = SessionStore::postgres(pool.clone(), cipher);
    let user = store
        .create_bootstrap_admin("stopped-hermes@example.com", "password")
        .await
        .expect("test user can be created");
    let mut instance = HermesInstance::managed_docker(
        &user.id,
        "/tmp/hermes-hub-test/workspace".to_string(),
        "/tmp/hermes-hub-test/sandbox".to_string(),
        "/tmp/hermes-hub-test/config".to_string(),
    );
    instance.status = HermesInstanceStatus::Stopped;
    instance.health_status = "stopped".to_string();
    instance.api_token_secret_ref = Some("persisted-instance-token".to_string());

    // 本地事故来自 stopped 实例第一次 upsert 时 last_user_activity_at 写入 NULL。
    store
        .bind_hermes_instance(instance)
        .await
        .expect("stopped instance can be inserted without violating lifecycle constraints");
    let stored = store
        .hermes_instance_for_user(&user.id)
        .await
        .expect("stopped instance can be loaded back");

    assert_eq!(stored.status, HermesInstanceStatus::Stopped);
    assert!(
        stored.last_user_activity_at.is_some(),
        "stopped instances still need an activity baseline for idle-stop UI"
    );
    assert!(
        stored.last_stopped_at.is_some(),
        "newly stored stopped instances should expose a stopped timestamp"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_system_settings_persist_session_limit() {
    let Some(pool) = postgres_pool().await else {
        eprintln!(
            "skipping postgres system settings test: HERMES_HUB_TEST_DATABASE_URL is not set"
        );
        return;
    };
    run_migrations(&pool).await.expect("migrations can run");

    let cipher = SecretCipher::from_master_key(TEST_SECRET_KEY).expect("test cipher is valid");
    let store = SessionStore::postgres(pool.clone(), cipher.clone());
    assert_eq!(
        store
            .system_settings()
            .await
            .expect("settings can be read")
            .max_sessions_per_user,
        20
    );

    store
        .update_system_settings(SystemSettings {
            max_sessions_per_user: 7,
            max_attachment_upload_bytes: 128 * 1024 * 1024,
            attachment_retention_days: 14,
            speech_input: SpeechInputSettings { enabled: true },
            oidc: OidcSettings {
                enabled: true,
                display_name: "Acme SSO".to_string(),
                client_id: "hermes-hub".to_string(),
                client_secret: "oidc-secret".to_string(),
                issuer_url: "https://idp.example.com".to_string(),
                authorization_url: "https://idp.example.com/oauth2/v1/authorize".to_string(),
                token_url: "https://idp.example.com/oauth2/v1/token".to_string(),
                userinfo_url: "https://idp.example.com/oauth2/v1/userinfo".to_string(),
                logout_url: "https://idp.example.com/logout".to_string(),
                scopes: "openid profile email".to_string(),
                username_claim: "preferred_username".to_string(),
                email_claim: "email".to_string(),
                allow_password_login: true,
                auto_create_users: true,
            },
            ldap: LdapSettings {
                enabled: true,
                display_name: "Corporate LDAP".to_string(),
                url: "ldaps://ldap.example.com:636".to_string(),
                bind_dn: "cn=hub,ou=apps,dc=example,dc=com".to_string(),
                bind_password: "ldap-bind-secret".to_string(),
                base_dn: "ou=people,dc=example,dc=com".to_string(),
                user_filter: "(&(objectClass=person)(mail={email}))".to_string(),
                email_attribute: "mail".to_string(),
                auto_create_users: true,
            },
        })
        .await
        .expect("settings can be updated");

    let stored_oidc: String = sqlx::query("select value from system_settings where key = 'oidc'")
        .fetch_one(&pool)
        .await
        .expect("stored oidc settings can be read")
        .try_get("value")
        .expect("stored oidc settings include value");
    assert!(!stored_oidc.contains("oidc-secret"));
    assert!(stored_oidc.contains(r#""client_secret":"v1."#));
    let stored_ldap: String = sqlx::query("select value from system_settings where key = 'ldap'")
        .fetch_one(&pool)
        .await
        .expect("stored ldap settings can be read")
        .try_get("value")
        .expect("stored ldap settings include value");
    assert!(!stored_ldap.contains("ldap-bind-secret"));
    assert!(stored_ldap.contains(r#""bind_password":"v1."#));

    let reloaded = SessionStore::postgres(pool, cipher)
        .system_settings()
        .await
        .expect("settings can be reloaded");
    assert_eq!(reloaded.max_sessions_per_user, 7);
    assert_eq!(reloaded.max_attachment_upload_bytes, 128 * 1024 * 1024);
    assert_eq!(reloaded.attachment_retention_days, 14);
    assert!(reloaded.speech_input.enabled);
    assert_eq!(reloaded.oidc.client_secret, "oidc-secret");
    assert_eq!(reloaded.ldap.bind_password, "ldap-bind-secret");
}
