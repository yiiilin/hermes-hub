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
    hermes::docker_provisioner::{DockerRuntime, DockerRuntimeOutput},
    ldap::DefaultLdapAuthenticator,
    llm_proxy::InMemoryLlmProviderClient,
    model_config::{
        ModelConfig, ModelRegistry, CHAT_COMPLETIONS_API_TYPE, LLM_MODEL_CONFIG_KIND,
        TITLE_MODEL_CONFIG_KIND,
    },
    session::store::{SessionStore, SystemSettings},
    storage::InMemoryObjectStorage,
    AppConfig, AppState,
};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

fn test_app() -> Router {
    hermes_hub_backend::build_router(AppConfig::for_tests())
}

#[derive(Clone, Default)]
struct RecordingDockerRuntime {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    fail_next_create: Arc<Mutex<bool>>,
}

impl RecordingDockerRuntime {
    fn fail_next_create(&self) {
        *self.fail_next_create.lock().expect("fail flag lock") = true;
    }
}

#[async_trait::async_trait]
impl DockerRuntime for RecordingDockerRuntime {
    async fn run(
        &self,
        args: Vec<String>,
    ) -> Result<DockerRuntimeOutput, hermes_hub_backend::hermes::provisioner::ProvisionerError>
    {
        self.calls.lock().expect("calls lock").push(args.clone());
        if args.get(0).map(String::as_str) == Some("network")
            && args.get(1).map(String::as_str) == Some("inspect")
        {
            return Ok(DockerRuntimeOutput {
                success: true,
                stdout: "network-existing".to_string(),
                stderr: String::new(),
            });
        }
        if args.get(0).map(String::as_str) == Some("image")
            && args.get(1).map(String::as_str) == Some("inspect")
        {
            return Ok(DockerRuntimeOutput {
                success: true,
                stdout: "image-existing".to_string(),
                stderr: String::new(),
            });
        }
        if args.get(0).map(String::as_str) == Some("create") {
            let mut fail_next_create = self.fail_next_create.lock().expect("fail flag lock");
            if *fail_next_create {
                *fail_next_create = false;
                return Err(
                    hermes_hub_backend::hermes::provisioner::ProvisionerError::DockerRuntime(
                        "create failed once".to_string(),
                    ),
                );
            }
            return Ok(DockerRuntimeOutput {
                success: true,
                stdout: "container-created".to_string(),
                stderr: String::new(),
            });
        }
        Ok(DockerRuntimeOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

fn ready_model_config(kind: &str) -> ModelConfig {
    ModelConfig {
        config_kind: kind.to_string(),
        enabled: true,
        provider_name: "openai-compatible".to_string(),
        provider_base_url: "https://models.example/v1".to_string(),
        provider_api_key: "real-secret".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        allowed_models: vec!["gpt-4.1-mini".to_string()],
        api_type: CHAT_COMPLETIONS_API_TYPE.to_string(),
        reasoning_effort: None,
        allow_streaming: true,
        request_timeout_seconds: 60,
        context_window_tokens: 128_000,
        max_output_tokens: 4096,
        temperature: 0.7,
        supports_parallel_tools: true,
        fallback: None,
    }
}

async fn app_state_with_recording_docker_runtime() -> (AppState, RecordingDockerRuntime) {
    let mut config = AppConfig::for_tests();
    config.skills_fs.mount_enabled = true;
    config.managed_profile.enabled = true;
    let model_registry = ModelRegistry::new(ready_model_config(LLM_MODEL_CONFIG_KIND));
    model_registry
        .replace(ready_model_config(TITLE_MODEL_CONFIG_KIND))
        .await
        .expect("title model config is ready");
    let runtime = RecordingDockerRuntime::default();
    let object_storage = InMemoryObjectStorage::new(config.object_storage.bucket.clone()).shared();
    let docker_provisioner = hermes_hub_backend::hermes::docker_provisioner::DockerProvisioner::new_with_runtime_and_object_storage(
        docker_config_from_app(&config, &ready_model_config(LLM_MODEL_CONFIG_KIND)),
        Arc::new(runtime.clone()),
        object_storage.clone(),
    );
    let state = AppState {
        object_storage,
        config,
        store: SessionStore::default(),
        channel_store: ChannelStore::default(),
        model_registry,
        llm_provider: InMemoryLlmProviderClient::default().shared(),
        ldap_authenticator: DefaultLdapAuthenticator::default().shared(),
        docker_provisioner,
        session_events: hermes_hub_backend::channel::events::SessionEventHub::default(),
    };
    (state, runtime)
}

async fn app_with_recording_docker_runtime() -> (Router, RecordingDockerRuntime) {
    let (state, runtime) = app_state_with_recording_docker_runtime().await;
    (build_router_with_state(state), runtime)
}

#[tokio::test]
async fn admin_model_config_update_refreshes_managed_config_without_gateway_restart_control() {
    let (state, _runtime) = app_state_with_recording_docker_runtime().await;
    let app = build_router_with_state(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let admin = state
        .store
        .user_by_session_cookie(&admin_cookie, "hermes_hub_session")
        .await
        .expect("admin can be read from session");

    let created = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let instance = state
        .store
        .hermes_instance_for_user(&admin.id)
        .await
        .expect("managed instance exists");
    let instance_token = instance
        .api_token_secret_ref
        .clone()
        .expect("managed instance token is stored");
    let config_key = format!("config/users/{}/config.yaml", admin.id);
    let before = state
        .object_storage
        .get(&config_key)
        .await
        .expect("initial managed config is written");
    assert!(String::from_utf8(before.to_vec())
        .expect("config is utf-8")
        .contains("default: \"gpt-4.1-mini\""));

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "secret-v2",
            "default_model": "gpt-4.1",
            "allowed_models": ["gpt-4.1"],
            "api_type": "chat_completions",
            "allow_streaming": true,
            "request_timeout_seconds": 30,
            "context_window_tokens": 200000,
            "max_output_tokens": 8192,
            "temperature": 0.3,
            "supports_parallel_tools": true,
            "fallback": {
                "enabled": true,
                "provider_name": "fallback-custom",
                "provider_base_url": "https://fallback-models.example/v1",
                "provider_api_key": "fallback-secret-v2",
                "default_model": "gpt-4.1-fallback",
                "allowed_models": ["gpt-4.1-fallback"],
                "api_type": "responses",
                "reasoning_effort": "low",
                "allow_streaming": true,
                "request_timeout_seconds": 45,
                "context_window_tokens": 100000,
                "max_output_tokens": 2048,
                "temperature": 0.2,
                "supports_parallel_tools": false
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let after = state
        .object_storage
        .get(&config_key)
        .await
        .expect("updated managed config is written to object storage");
    let content = String::from_utf8(after.to_vec()).expect("config is utf-8");
    assert!(content.contains("default: \"gpt-4.1\""));
    assert!(content.contains("context_window_tokens: 200000"));
    assert!(content.contains("max_output_tokens: 8192"));
    assert!(content.contains("temperature: 0.3"));

    let inbox = request_raw_with_bearer(
        &app,
        Method::GET,
        "/internal/channel/v1/inbox?timeout_seconds=0&limit=4",
        "application/json",
        Vec::new(),
        &instance_token,
    )
    .await;
    let (status, body) = response_json(inbox).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["items"].as_array().map(Vec::len),
        Some(0),
        "Docker-managed config refresh already restarts the container, so no gateway control restart is queued"
    );
}

#[tokio::test]
async fn model_config_refresh_rebuilds_ordinary_container_when_legacy_sandbox_policy_changes() {
    let (state, runtime) = app_state_with_recording_docker_runtime().await;
    let app = build_router_with_state(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let admin = state
        .store
        .user_by_session_cookie(&admin_cookie, "hermes_hub_session")
        .await
        .expect("admin can be read from session");

    let created = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let mut legacy_instance = state
        .store
        .hermes_instance_for_user(&admin.id)
        .await
        .expect("managed instance exists");
    legacy_instance.host_sandbox_path = Some("/tmp/hermes-hub-legacy-sandbox".to_string());
    state
        .store
        .bind_hermes_instance(legacy_instance)
        .await
        .expect("legacy sandbox instance can be rebound");

    runtime.calls.lock().expect("calls lock").clear();
    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "secret-v2",
            "default_model": "gpt-4.1",
            "allowed_models": ["gpt-4.1"],
            "api_type": "chat_completions",
            "allow_streaming": true,
            "request_timeout_seconds": 30,
            "context_window_tokens": 200000,
            "max_output_tokens": 8192,
            "temperature": 0.3,
            "supports_parallel_tools": true
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let stored_after_update = state
        .store
        .hermes_instance_for_user(&admin.id)
        .await
        .expect("managed instance remains stored");
    assert_eq!(
        stored_after_update.host_sandbox_path, None,
        "model config refresh must remove legacy public sandbox paths from ordinary instances"
    );
    let config_key = format!("config/users/{}/config.yaml", admin.id);
    let after = state
        .object_storage
        .get(&config_key)
        .await
        .expect("updated managed config is written");
    let content = String::from_utf8(after.to_vec()).expect("config is utf-8");
    assert!(!content.contains("- \"/sandbox\""));
    assert!(!content.contains("- \"/opt/data\""));

    let calls = runtime.calls.lock().expect("calls lock").clone();
    assert!(
        calls.iter().any(|args| {
            args.first().map(String::as_str) == Some("create")
                && args.windows(2).all(|pair| {
                    pair[0] != "--mount"
                        || (!pair[1].contains("dst=/sandbox") && !pair[1].contains("dst=/opt/data"))
                })
        }),
        "path policy changes must rebuild the ordinary container without public sandbox mounts"
    );
}

#[tokio::test]
async fn model_config_refresh_retries_legacy_sandbox_rebuild_after_transient_failure() {
    let (state, runtime) = app_state_with_recording_docker_runtime().await;
    let app = build_router_with_state(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let admin = state
        .store
        .user_by_session_cookie(&admin_cookie, "hermes_hub_session")
        .await
        .expect("admin can be read from session");

    let created = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let mut legacy_instance = state
        .store
        .hermes_instance_for_user(&admin.id)
        .await
        .expect("managed instance exists");
    legacy_instance.host_sandbox_path = Some("/tmp/hermes-hub-legacy-sandbox".to_string());
    state
        .store
        .bind_hermes_instance(legacy_instance)
        .await
        .expect("legacy sandbox instance can be rebound");

    runtime.calls.lock().expect("calls lock").clear();
    runtime.fail_next_create();
    let failed_update = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "secret-v2",
            "default_model": "gpt-4.1",
            "allowed_models": ["gpt-4.1"],
            "api_type": "chat_completions",
            "allow_streaming": true,
            "request_timeout_seconds": 30,
            "context_window_tokens": 200000,
            "max_output_tokens": 8192,
            "temperature": 0.3,
            "supports_parallel_tools": true
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(failed_update.status(), StatusCode::BAD_GATEWAY);
    let stored_after_failure = state
        .store
        .hermes_instance_for_user(&admin.id)
        .await
        .expect("managed instance remains stored");
    assert!(
        stored_after_failure.host_sandbox_path.is_some(),
        "failed rebuild must not clear legacy sandbox in storage before Docker has converged"
    );

    runtime.calls.lock().expect("calls lock").clear();
    let retried_update = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "secret-v2",
            "default_model": "gpt-4.1",
            "allowed_models": ["gpt-4.1"],
            "api_type": "chat_completions",
            "allow_streaming": true,
            "request_timeout_seconds": 30,
            "context_window_tokens": 200000,
            "max_output_tokens": 8192,
            "temperature": 0.3,
            "supports_parallel_tools": true
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(retried_update.status(), StatusCode::NO_CONTENT);
    let stored_after_retry = state
        .store
        .hermes_instance_for_user(&admin.id)
        .await
        .expect("managed instance remains stored");
    assert_eq!(stored_after_retry.host_sandbox_path, None);
    let calls = runtime.calls.lock().expect("calls lock").clone();
    assert!(
        calls.iter().any(|args| {
            args.first().map(String::as_str) == Some("create")
                && args.windows(2).all(|pair| {
                    pair[0] != "--mount"
                        || (!pair[1].contains("dst=/sandbox") && !pair[1].contains("dst=/opt/data"))
                })
        }),
        "retry must still rebuild the ordinary container without public sandbox mounts"
    );
}

#[tokio::test]
async fn model_config_refresh_skips_disabled_public_platform_instance() {
    let (state, runtime) = app_state_with_recording_docker_runtime().await;
    let app = build_router_with_state(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;
    let public_user = state
        .store
        .ensure_public_platform_user()
        .await
        .expect("public platform user can be created");
    let public_instance = state.docker_provisioner.prepare_instance(&public_user.id);
    state
        .store
        .bind_hermes_instance(public_instance)
        .await
        .expect("disabled public instance can be stored");

    runtime.calls.lock().expect("calls lock").clear();
    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "secret-v2",
            "default_model": "gpt-4.1",
            "allowed_models": ["gpt-4.1"],
            "api_type": "chat_completions",
            "allow_streaming": true,
            "request_timeout_seconds": 30,
            "context_window_tokens": 200000,
            "max_output_tokens": 8192,
            "temperature": 0.3,
            "supports_parallel_tools": true
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);
    let calls = runtime.calls.lock().expect("calls lock").clone();
    assert!(
        calls
            .iter()
            .all(|args| args.first().map(String::as_str) != Some("create")),
        "disabled public platform instances must not be rebuilt by config refresh"
    );
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

async fn request_raw(
    app: &Router,
    method: Method,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    cookie: Option<&str>,
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type);

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
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

async fn request_raw_with_bearer(
    app: &Router,
    method: Method,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    bearer: &str,
) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}

fn assert_managed_profile_uses_hub_fs(calls: &[Vec<String>], context: &str) {
    let create_call = calls
        .iter()
        .find(|args| args.first().map(String::as_str) == Some("create"))
        .unwrap_or_else(|| panic!("{context}: container create command is issued"));

    assert!(
        create_call
            .windows(2)
            .all(|pair| { pair[0] != "--mount" || !pair[1].contains("dst=/hub-managed-profile") }),
        "{context}: managed profile must not create a second NFS mount"
    );

    // wrapper entrypoint 负责把同一个 Hub FS 根目录里的 profile 文件链接到 Hermes 会读取的位置；
    // Hub 后端只负责挂载 /nfs 并启动 gateway。
    let command = create_call.join(" ");
    assert!(
        !command.contains("ln -sfn"),
        "{context}: profile files must be linked by the wrapper entrypoint, not Hub backend"
    );
    assert!(command.contains("exec /opt/hermes/.venv/bin/hermes gateway"));
}

fn multipart_text(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    body.extend_from_slice(
        format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"{name}\"\r\n\r\n\
             {value}\r\n"
        )
        .as_bytes(),
    );
}

fn multipart_file(
    body: &mut Vec<u8>,
    boundary: &str,
    name: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) {
    body.extend_from_slice(
        format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n\
             Content-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
}

fn finish_multipart(body: &mut Vec<u8>, boundary: &str) {
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
}

fn sample_zip_bytes() -> Vec<u8> {
    // 这个最小合法 ZIP 样本避免测试继续依赖后端已移除的 zip crate。
    vec![
        80, 75, 3, 4, 20, 0, 0, 0, 0, 0, 112, 56, 187, 92, 187, 86, 240, 253, 12, 0, 0, 0, 12, 0,
        0, 0, 18, 0, 0, 0, 97, 115, 115, 105, 115, 116, 97, 110, 116, 47, 83, 75, 73, 76, 76, 46,
        109, 100, 35, 32, 65, 115, 115, 105, 115, 116, 97, 110, 116, 10, 80, 75, 3, 4, 20, 0, 0, 0,
        0, 0, 112, 56, 187, 92, 134, 215, 146, 156, 11, 0, 0, 0, 11, 0, 0, 0, 28, 0, 0, 0, 97, 115,
        115, 105, 115, 116, 97, 110, 116, 47, 114, 101, 102, 101, 114, 101, 110, 99, 101, 115, 47,
        116, 111, 110, 101, 46, 109, 100, 66, 101, 32, 100, 105, 114, 101, 99, 116, 46, 10, 80, 75,
        1, 2, 20, 3, 20, 0, 0, 0, 0, 0, 112, 56, 187, 92, 187, 86, 240, 253, 12, 0, 0, 0, 12, 0, 0,
        0, 18, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 1, 0, 0, 0, 0, 97, 115, 115, 105, 115, 116,
        97, 110, 116, 47, 83, 75, 73, 76, 76, 46, 109, 100, 80, 75, 1, 2, 20, 3, 20, 0, 0, 0, 0, 0,
        112, 56, 187, 92, 134, 215, 146, 156, 11, 0, 0, 0, 11, 0, 0, 0, 28, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 128, 1, 60, 0, 0, 0, 97, 115, 115, 105, 115, 116, 97, 110, 116, 47, 114, 101, 102,
        101, 114, 101, 110, 99, 101, 115, 47, 116, 111, 110, 101, 46, 109, 100, 80, 75, 5, 6, 0, 0,
        0, 0, 2, 0, 2, 0, 138, 0, 0, 0, 129, 0, 0, 0, 0, 0,
    ]
}

fn tree_child<'a>(node: &'a Value, name: &str) -> &'a Value {
    node["children"]
        .as_array()
        .expect("tree node has children")
        .iter()
        .find(|child| child["name"] == name)
        .unwrap_or_else(|| panic!("missing tree child {name}"))
}

async fn bootstrap_admin(app: &Router) -> String {
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

#[tokio::test]
async fn admin_can_manage_unified_hermes_profile() {
    let (state, _runtime) = app_state_with_recording_docker_runtime().await;
    let app = build_router_with_state(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;

    let initial = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-profile",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(initial).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["profile"]["soul_md"], "");
    assert!(
        body["profile"].get("agents_md").is_none(),
        "AGENTS.md must not be exposed through the Hermes profile API"
    );

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/hermes-profile",
        json!({
            "agents_md": "# AGENTS\n\nLegacy clients may still send this.\n",
            "soul_md": "# SOUL\n\nBe direct and careful.\n"
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let agents = state
        .object_storage
        .get("managed-profile/current/AGENTS.md")
        .await;
    assert!(
        agents.is_err(),
        "Hermes profile API must ignore AGENTS.md instead of writing it"
    );
    let soul = state
        .object_storage
        .get("managed-profile/current/SOUL.md")
        .await
        .expect("SOUL.md is written to object storage");
    assert_eq!(
        String::from_utf8(soul.to_vec()).expect("SOUL.md is utf-8"),
        "# SOUL\n\nBe direct and careful.\n"
    );

    let saved = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-profile",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(saved).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["profile"]["soul_md"],
        "# SOUL\n\nBe direct and careful.\n"
    );
    assert!(
        body["profile"].get("agents_md").is_none(),
        "saved Hermes profile response must stay SOUL-only"
    );
}

#[tokio::test]
async fn admin_hermes_gets_writable_global_skills_mount_but_regular_users_do_not() {
    let (app, runtime) = app_with_recording_docker_runtime().await;
    let admin_cookie = bootstrap_admin(&app).await;

    let users = request_empty(&app, Method::GET, "/api/admin/users", Some(&admin_cookie)).await;
    let (_, users_body) = response_json(users).await;
    let admin_id = users_body["users"][0]["id"].as_str().expect("admin id");

    let admin_create = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/create-managed"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(admin_create.status(), StatusCode::OK);
    let admin_calls = runtime.calls.lock().expect("calls lock").clone();
    assert_managed_profile_uses_hub_fs(&admin_calls, "admin create");
    assert!(
        admin_calls.iter().any(|args| {
            args.first().map(String::as_str) == Some("create")
                && args.windows(2).any(|pair| {
                    pair[0] == "--mount"
                        && pair[1].contains("dst=/nfs")
                        && !pair[1].contains("readonly")
                })
        }),
        "admin Hermes must mount global skills read-write"
    );

    runtime.calls.lock().expect("calls lock").clear();
    let admin_rebuild = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/rebuild-managed"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(admin_rebuild.status(), StatusCode::OK);
    let admin_rebuild_calls = runtime.calls.lock().expect("calls lock").clone();
    assert_managed_profile_uses_hub_fs(&admin_rebuild_calls, "admin rebuild");
    assert!(
        admin_rebuild_calls.iter().any(|args| {
            args.first().map(String::as_str) == Some("create")
                && args.windows(2).any(|pair| {
                    pair[0] == "--mount"
                        && pair[1].contains("dst=/nfs")
                        && !pair[1].contains("readonly")
                })
        }),
        "rebuilt admin Hermes must keep global skills read-write"
    );

    runtime.calls.lock().expect("calls lock").clear();
    let invite = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": unix_now() + 86_400,
            "max_uses": 1
        }),
        Some(&admin_cookie),
    )
    .await;
    let (status, invite_body) = response_json(invite).await;
    assert_eq!(status, StatusCode::CREATED);
    let register = request_json(
        &app,
        Method::POST,
        "/api/auth/register",
        json!({
            "invite_token": invite_body["token"],
            "email": "user@example.com",
            "password": "user-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(register.status(), StatusCode::CREATED);

    let user_calls = runtime.calls.lock().expect("calls lock").clone();
    assert_managed_profile_uses_hub_fs(&user_calls, "regular create");
    assert!(
        user_calls.iter().any(|args| {
            args.first().map(String::as_str) == Some("create")
                && args.windows(2).any(|pair| {
                    pair[0] == "--mount"
                        && pair[1]
                            == "type=volume,src=hermes-hub-managed-skills-test-ro-nosharecache,dst=/nfs,volume-driver=local,readonly"
                })
        }),
        "regular Hermes must keep global skills readonly"
    );
}

#[tokio::test]
async fn admin_rebuild_managed_hermes_keeps_global_skills_writable() {
    let (state, runtime) = app_state_with_recording_docker_runtime().await;
    let app = build_router_with_state(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;

    let users = request_empty(&app, Method::GET, "/api/admin/users", Some(&admin_cookie)).await;
    let (_, users_body) = response_json(users).await;
    let admin_id = users_body["users"][0]["id"].as_str().expect("admin id");

    let created = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/create-managed"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);

    // Postgres 不持久化这个运行时权限位；测试里主动清掉，复现从存储重新读取后的实例形态。
    let mut stored_instance = state
        .store
        .hermes_instance_for_user(admin_id)
        .await
        .expect("admin Hermes instance is stored");
    stored_instance.global_skills_write_enabled = false;
    state
        .store
        .bind_hermes_instance(stored_instance)
        .await
        .expect("stored instance can be rebound");

    runtime.calls.lock().expect("calls lock").clear();
    let rebuilt = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/rebuild-managed"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(rebuilt.status(), StatusCode::OK);

    let calls = runtime.calls.lock().expect("calls lock").clone();
    assert!(
        calls.iter().any(|args| {
            args.first().map(String::as_str) == Some("create")
                && args.windows(2).any(|pair| {
                    pair[0] == "--mount"
                        && pair[1]
                            == "type=volume,src=hermes-hub-managed-skills-test-rw-nosharecache,dst=/nfs,volume-driver=local"
                })
        }),
        "admin rebuild must preserve writable global skills mount"
    );
}

#[tokio::test]
async fn regular_user_rebuild_managed_hermes_keeps_global_skills_readonly() {
    let (state, runtime) = app_state_with_recording_docker_runtime().await;
    let app = build_router_with_state(state.clone());
    let admin_cookie = bootstrap_admin(&app).await;

    let invite = request_json(
        &app,
        Method::POST,
        "/api/invites",
        json!({
            "expires_at": unix_now() + 86_400,
            "max_uses": 1
        }),
        Some(&admin_cookie),
    )
    .await;
    let (status, invite_body) = response_json(invite).await;
    assert_eq!(status, StatusCode::CREATED);
    let registered = request_json(
        &app,
        Method::POST,
        "/api/auth/register",
        json!({
            "invite_token": invite_body["token"],
            "email": "regular-rebuild@example.com",
            "password": "user-password-123"
        }),
        None,
    )
    .await;
    assert_eq!(registered.status(), StatusCode::CREATED);

    let users = request_empty(&app, Method::GET, "/api/admin/users", Some(&admin_cookie)).await;
    let (_, users_body) = response_json(users).await;
    let user_id = users_body["users"]
        .as_array()
        .expect("users list")
        .iter()
        .find(|user| user["email"] == "regular-rebuild@example.com")
        .and_then(|user| user["id"].as_str())
        .expect("regular user id");

    let mut legacy_instance = state
        .store
        .hermes_instance_for_user(user_id)
        .await
        .expect("regular Hermes instance exists");
    legacy_instance.host_sandbox_path = Some("/tmp/hermes-hub-legacy-sandbox".to_string());
    state
        .store
        .bind_hermes_instance(legacy_instance)
        .await
        .expect("legacy instance can be rebound");

    runtime.calls.lock().expect("calls lock").clear();
    let rebuilt = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{user_id}/hermes-instance/rebuild-managed"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(rebuilt.status(), StatusCode::OK);

    let stored_after_rebuild = state
        .store
        .hermes_instance_for_user(user_id)
        .await
        .expect("rebuilt regular Hermes instance exists");
    assert_eq!(
        stored_after_rebuild.host_sandbox_path, None,
        "ordinary user rebuild must remove legacy public sandbox paths from storage"
    );
    let config_key = format!("config/users/{user_id}/config.yaml");
    let managed_config = state
        .object_storage
        .get(&config_key)
        .await
        .expect("rebuilt managed config is written");
    let managed_config = String::from_utf8(managed_config.to_vec()).expect("config is utf-8");
    assert!(!managed_config.contains("- \"/sandbox\""));
    assert!(!managed_config.contains("- \"/opt/data\""));

    let calls = runtime.calls.lock().expect("calls lock").clone();
    assert!(
        calls.iter().all(|args| {
            args.first().map(String::as_str) != Some("create")
                || args.windows(2).all(|pair| {
                    pair[0] != "--mount"
                        || (!pair[1].contains("dst=/sandbox") && !pair[1].contains("dst=/opt/data"))
                })
        }),
        "ordinary user rebuild must not mount public sandbox paths"
    );
    assert!(
        calls.iter().any(|args| {
            args.first().map(String::as_str) == Some("create")
                && args.windows(2).any(|pair| {
                    pair[0] == "--mount"
                        && pair[1]
                            == "type=volume,src=hermes-hub-managed-skills-test-ro-nosharecache,dst=/nfs,volume-driver=local,readonly"
                })
        }),
        "regular user rebuild must keep global skills readonly"
    );
}

#[tokio::test]
async fn admin_workspace_test() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let users = request_empty(&app, Method::GET, "/api/admin/users", Some(&admin_cookie)).await;
    let (status, body) = response_json(users).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["users"][0]["email"], "admin@example.com");
    let admin_id = body["users"][0]["id"].as_str().expect("admin id");

    let disable = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/disable"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(disable.status(), StatusCode::CONFLICT);

    let update_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "secret-v2",
            "default_model": "gpt-4.1",
            "allowed_models": ["gpt-4.1"],
            "api_type": "responses",
            "reasoning_effort": "medium",
            "allow_streaming": true,
            "request_timeout_seconds": 30,
            "context_window_tokens": 200000,
            "max_output_tokens": 8192,
            "temperature": 0.3,
            "supports_parallel_tools": true,
            "fallback": {
                "enabled": true,
                "provider_name": "fallback-custom",
                "provider_base_url": "https://fallback-models.example/v1",
                "provider_api_key": "fallback-secret-v2",
                "default_model": "gpt-4.1-fallback",
                "allowed_models": ["gpt-4.1-fallback"],
                "api_type": "responses",
                "reasoning_effort": "low",
                "allow_streaming": true,
                "request_timeout_seconds": 45,
                "context_window_tokens": 100000,
                "max_output_tokens": 2048,
                "temperature": 0.2,
                "supports_parallel_tools": false
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_model.status(), StatusCode::NO_CONTENT);

    let model = request_empty(
        &app,
        Method::GET,
        "/api/admin/model-config",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(model).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["model_config"]["provider_name"], "custom");
    assert_eq!(body["model_config"]["default_model"], "gpt-4.1");
    assert_eq!(body["model_config"]["provider_api_key"], "secret-v2");
    assert_eq!(body["model_config"]["api_type"], "responses");
    assert_eq!(body["model_config"]["reasoning_effort"], "medium");
    assert_eq!(body["model_config"]["context_window_tokens"], 200000);
    assert_eq!(body["model_config"]["max_output_tokens"], 8192);
    assert_eq!(body["model_config"]["temperature"], 0.3);
    assert_eq!(body["model_config"]["supports_parallel_tools"], true);
    assert_eq!(body["model_config"]["fallback"]["enabled"], true);
    assert_eq!(
        body["model_config"]["fallback"]["provider_name"],
        "fallback-custom"
    );
    assert_eq!(
        body["model_config"]["fallback"]["provider_base_url"],
        "https://fallback-models.example/v1"
    );
    assert_eq!(
        body["model_config"]["fallback"]["provider_api_key"],
        "fallback-secret-v2"
    );
    assert_eq!(
        body["model_config"]["fallback"]["default_model"],
        "gpt-4.1-fallback"
    );
    assert_eq!(body["model_config"]["fallback"]["api_type"], "responses");
    assert_eq!(body["model_config"]["fallback"]["reasoning_effort"], "low");
    assert_eq!(
        body["model_config"]["fallback"]["supports_parallel_tools"],
        false
    );

    let status_response = request_empty(
        &app,
        Method::GET,
        "/api/workspace/status",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(status_response).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["hermes_instance"].is_null());

    let blocked_without_title_model = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(blocked_without_title_model.status(), StatusCode::CONFLICT);

    let blocked_admin_create_without_title_model = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/create-managed"),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(
        blocked_admin_create_without_title_model.status(),
        StatusCode::CONFLICT
    );

    let update_title_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "config_kind": "title",
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "title-secret-v2",
            "default_model": "gpt-4.1-mini",
            "allowed_models": ["gpt-4.1-mini"],
            "allow_streaming": false,
            "request_timeout_seconds": 30
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_title_model.status(), StatusCode::NO_CONTENT);

    let update_image_model = request_json(
        &app,
        Method::PUT,
        "/api/admin/model-config",
        json!({
            "config_kind": "image",
            "enabled": true,
            "provider_name": "custom",
            "provider_base_url": "https://models.example/v1",
            "provider_api_key": "image-secret-v2",
            "default_model": "gpt-image-1",
            "allowed_models": ["gpt-image-1"],
            "allow_streaming": false,
            "request_timeout_seconds": 180
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update_image_model.status(), StatusCode::NO_CONTENT);

    let created_by_admin = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/create-managed"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(created_by_admin).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["kind"], "managed_docker");
    assert_eq!(body["hermes_instance"]["status"], "running");
    assert!(
        body["hermes_instance"]["last_started_at"]
            .as_u64()
            .is_some(),
        "managed Hermes create response must expose its start time"
    );
    assert!(
        body["hermes_instance"]["last_user_activity_at"]
            .as_u64()
            .is_some(),
        "managed Hermes create response must expose the activity timestamp used for idle stop"
    );
    assert_eq!(body["hermes_instance"]["last_stopped_at"], Value::Null);
    let managed_config = std::fs::read_to_string(format!(
        "/tmp/hermes-hub/users/{admin_id}/config/config.yaml"
    ))
    .expect("managed Hermes config is written");
    assert!(managed_config.contains("model: \"gpt-image-1\""));
    assert!(!managed_config.contains("gpt-image-2-medium"));

    let ensured = request_empty(
        &app,
        Method::POST,
        "/api/workspace/ensure-hermes",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(ensured).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["kind"], "managed_docker");
    assert_eq!(body["hermes_instance"]["status"], "running");

    let stop = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/stop"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(stop).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["status"], "stopped");
    assert!(
        body["hermes_instance"]["last_stopped_at"]
            .as_u64()
            .is_some(),
        "managed Hermes stop response must expose its stopped time"
    );
    assert_eq!(body["hermes_instance"]["stopped_reason"], "manual");

    let start = request_empty(
        &app,
        Method::POST,
        &format!("/api/admin/users/{admin_id}/hermes-instance/start"),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(start).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instance"]["status"], "running");
    assert!(
        body["hermes_instance"]["last_started_at"]
            .as_u64()
            .is_some(),
        "managed Hermes start response must expose its latest start time"
    );
    assert_eq!(body["hermes_instance"]["stopped_reason"], Value::Null);

    let instances = request_empty(
        &app,
        Method::GET,
        "/api/admin/hermes-instances",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(instances).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hermes_instances"][0]["user_id"], admin_id);
    assert!(
        body["hermes_instances"][0]["last_started_at"]
            .as_u64()
            .is_some(),
        "Hermes management list must include lifecycle timestamps"
    );

    let removed_legacy_config = request_json(
        &app,
        Method::PUT,
        &format!("/api/admin/users/{admin_id}/hermes-instance/external-config"),
        json!({
            "name": "legacy runtime",
            "api_token": "external-token"
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(removed_legacy_config.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_can_configure_per_user_session_limit() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let settings = request_empty(
        &app,
        Method::GET,
        "/api/admin/system-settings",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(settings).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["max_sessions_per_user"], 20);
    assert_eq!(
        body["settings"]["max_attachment_upload_bytes"],
        200 * 1024 * 1024
    );
    assert_eq!(body["settings"]["attachment_retention_days"], 7);
    assert_eq!(body["settings"]["speech_input"]["enabled"], false);
    assert_eq!(body["settings"]["oidc"]["enabled"], false);
    assert_eq!(body["settings"]["oidc"]["display_name"], "OpenID Connect");
    assert_eq!(body["settings"]["ldap"]["enabled"], false);
    assert_eq!(body["settings"]["ldap"]["display_name"], "LDAP");

    let update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 2,
            "max_attachment_upload_bytes": 64 * 1024 * 1024,
            "attachment_retention_days": 30,
            "speech_input": {
                "enabled": true
            },
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "issuer_url": "https://idp.example.com",
                "authorization_url": "https://idp.example.com/oauth2/v1/authorize",
                "token_url": "https://idp.example.com/oauth2/v1/token",
                "userinfo_url": "https://idp.example.com/oauth2/v1/userinfo",
                "logout_url": "https://idp.example.com/logout",
                "scopes": "openid profile email",
                "username_claim": "preferred_username",
                "email_claim": "email",
                "allow_password_login": true,
                "auto_create_users": true
            },
            "ldap": {
                "enabled": true,
                "display_name": "Corporate LDAP",
                "url": "ldaps://ldap.example.com:636",
                "bind_dn": "cn=hub,ou=apps,dc=example,dc=com",
                "bind_password": "ldap-bind-secret",
                "base_dn": "ou=people,dc=example,dc=com",
                "user_filter": "(&(objectClass=person)(mail={email}))",
                "email_attribute": "mail",
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(update.status(), StatusCode::NO_CONTENT);

    let large_attachment_update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings",
        json!({
            "max_sessions_per_user": 2,
            "max_attachment_upload_bytes": 15_000_u64 * 1024 * 1024,
            "attachment_retention_days": 30,
            "speech_input": {
                "enabled": true
            },
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "issuer_url": "https://idp.example.com",
                "authorization_url": "https://idp.example.com/oauth2/v1/authorize",
                "token_url": "https://idp.example.com/oauth2/v1/token",
                "userinfo_url": "https://idp.example.com/oauth2/v1/userinfo",
                "logout_url": "https://idp.example.com/logout",
                "scopes": "openid profile email",
                "username_claim": "preferred_username",
                "email_claim": "email",
                "allow_password_login": true,
                "auto_create_users": true
            },
            "ldap": {
                "enabled": true,
                "display_name": "Corporate LDAP",
                "url": "ldaps://ldap.example.com:636",
                "bind_dn": "cn=hub,ou=apps,dc=example,dc=com",
                "bind_password": "ldap-bind-secret",
                "base_dn": "ou=people,dc=example,dc=com",
                "user_filter": "(&(objectClass=person)(mail={email}))",
                "email_attribute": "mail",
                "auto_create_users": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(
        large_attachment_update.status(),
        StatusCode::NO_CONTENT,
        "15000 MB must be a valid per-file attachment upload limit"
    );

    let settings = request_empty(
        &app,
        Method::GET,
        "/api/admin/system-settings",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(settings).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["max_sessions_per_user"], 2);
    assert_eq!(
        body["settings"]["max_attachment_upload_bytes"],
        15_000_u64 * 1024 * 1024
    );
    assert_eq!(body["settings"]["attachment_retention_days"], 30);
    assert_eq!(body["settings"]["speech_input"]["enabled"], true);
    assert_eq!(body["settings"]["oidc"]["enabled"], true);
    assert_eq!(body["settings"]["oidc"]["display_name"], "Acme SSO");
    assert_eq!(body["settings"]["oidc"]["client_id"], "hermes-hub");
    assert_eq!(body["settings"]["oidc"]["client_secret"], "oidc-secret");
    assert_eq!(
        body["settings"]["oidc"]["authorization_url"],
        "https://idp.example.com/oauth2/v1/authorize"
    );
    assert_eq!(body["settings"]["ldap"]["enabled"], true);
    assert_eq!(body["settings"]["ldap"]["display_name"], "Corporate LDAP");
    assert_eq!(
        body["settings"]["ldap"]["url"],
        "ldaps://ldap.example.com:636"
    );
    assert_eq!(
        body["settings"]["ldap"]["bind_password"],
        "ldap-bind-secret"
    );

    let public_oidc = request_empty(&app, Method::GET, "/api/auth/oidc/config", None).await;
    let (status, body) = response_json(public_oidc).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["oidc"]["enabled"], true);
    assert_eq!(body["oidc"]["display_name"], "Acme SSO");
    assert!(body["oidc"].get("client_secret").is_none());

    let public_ldap = request_empty(&app, Method::GET, "/api/auth/ldap/config", None).await;
    let (status, body) = response_json(public_ldap).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ldap"]["enabled"], true);
    assert_eq!(body["ldap"]["display_name"], "Corporate LDAP");
    assert!(body["ldap"].get("bind_password").is_none());

    for _ in 0..2 {
        let created = request_json(
            &app,
            Method::POST,
            "/api/sessions",
            json!({ "kind": "agent" }),
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    let blocked = request_json(
        &app,
        Method::POST,
        "/api/sessions",
        json!({ "kind": "agent" }),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(blocked).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "session_limit_exceeded");
    assert_eq!(body["message"], "session limit exceeded");
    assert_eq!(body["max_sessions_per_user"], 2);

    let public_blocked = request_json(
        &app,
        Method::POST,
        "/api/sessions",
        json!({ "kind": "agent" }),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(public_blocked).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "session_limit_exceeded");
    assert_eq!(body["message"], "session limit exceeded");
    assert_eq!(body["max_sessions_per_user"], 2);
}

#[tokio::test]
async fn admin_can_manage_hub_skills() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let save = request_json(
        &app,
        Method::PUT,
        "/api/admin/managed-skills/writing/SKILL.md",
        json!({
            "content": "# Writing\n\nUse concise prose.\n"
        }),
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(save).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["skill"]["path"], "writing/SKILL.md");
    assert_eq!(
        body["skill"]["content"],
        "# Writing\n\nUse concise prose.\n"
    );

    let list = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(list).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["skills"][0]["path"], "writing/SKILL.md");
    assert_eq!(body["skills"][0]["size"], 30);

    let read = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/writing/SKILL.md",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(read).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["skill"]["content"],
        "# Writing\n\nUse concise prose.\n"
    );

    let hidden = request_json(
        &app,
        Method::PUT,
        "/api/admin/managed-skills/.curator_state/state.json",
        json!({ "content": "{}" }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(hidden.status(), StatusCode::BAD_REQUEST);

    let delete = request_empty(
        &app,
        Method::DELETE,
        "/api/admin/managed-skills/writing/SKILL.md",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);

    let read_deleted = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/writing/SKILL.md",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(read_deleted.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_can_view_managed_skills_as_a_file_tree() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    for (path, content) in [
        ("writing/SKILL.md", "# Writing\n"),
        ("writing/references/style.md", "Be precise.\n"),
        ("image/SKILL.md", "# Image\n"),
    ] {
        let save = request_json(
            &app,
            Method::PUT,
            &format!("/api/admin/managed-skills/{path}"),
            json!({ "content": content }),
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(save.status(), StatusCode::OK);
    }

    let tree = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/tree",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(tree).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tree"]["kind"], "dir");
    assert_eq!(body["tree"]["path"], "");

    let image = tree_child(&body["tree"], "image");
    assert_eq!(image["kind"], "dir");
    assert_eq!(tree_child(image, "SKILL.md")["kind"], "file");

    let writing = tree_child(&body["tree"], "writing");
    assert_eq!(writing["kind"], "dir");
    assert_eq!(tree_child(writing, "SKILL.md")["size"], 10);
    let references = tree_child(writing, "references");
    assert_eq!(
        tree_child(references, "style.md")["path"],
        "writing/references/style.md"
    );
}

#[tokio::test]
async fn admin_can_create_empty_managed_skill_directories() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let create = request_empty(
        &app,
        Method::POST,
        "/api/admin/managed-skills/directories/research/references",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(create.status(), StatusCode::CREATED);

    let list = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(list).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["skills"].as_array().expect("skills").is_empty());

    let tree = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/tree",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(tree).await;
    assert_eq!(status, StatusCode::OK);
    let research = tree_child(&body["tree"], "research");
    assert_eq!(research["kind"], "dir");
    assert_eq!(tree_child(research, "references")["kind"], "dir");

    let hidden = request_empty(
        &app,
        Method::POST,
        "/api/admin/managed-skills/directories/.curator_state",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(hidden.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_can_delete_managed_skill_directories_recursively() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    for path in [
        "writing/SKILL.md",
        "writing/references/style.md",
        "image/SKILL.md",
    ] {
        let save = request_json(
            &app,
            Method::PUT,
            &format!("/api/admin/managed-skills/{path}"),
            json!({ "content": path }),
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(save.status(), StatusCode::OK);
    }

    let delete = request_empty(
        &app,
        Method::DELETE,
        "/api/admin/managed-skills/writing",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);

    let list = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(list).await;
    assert_eq!(status, StatusCode::OK);
    let paths = body["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .map(|skill| skill["path"].as_str().expect("skill path"))
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["image/SKILL.md"]);

    let delete_missing = request_empty(
        &app,
        Method::DELETE,
        "/api/admin/managed-skills/writing",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(delete_missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_can_delete_binary_managed_skill_without_reading_utf8_content() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;
    let boundary = "managed-skills-binary-delete-boundary";
    let mut upload_body = Vec::new();
    multipart_file(
        &mut upload_body,
        boundary,
        "files",
        "mindoc-search.tgz",
        "application/gzip",
        &[0x1f, 0x8b, 0xff, 0x00],
    );
    finish_multipart(&mut upload_body, boundary);

    let upload = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(upload.status(), StatusCode::CREATED);

    let read = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/mindoc-search.tgz",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(read.status(), StatusCode::BAD_GATEWAY);

    let delete = request_empty(
        &app,
        Method::DELETE,
        "/api/admin/managed-skills/mindoc-search.tgz",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn admin_can_upload_managed_skill_files_and_folders() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;
    let boundary = "managed-skills-upload-boundary";
    let mut upload_body = Vec::new();
    multipart_text(&mut upload_body, boundary, "target_path", "packs");
    multipart_file(
        &mut upload_body,
        boundary,
        "files",
        "research/SKILL.md",
        "text/markdown",
        b"# Research\n",
    );
    multipart_file(
        &mut upload_body,
        boundary,
        "files",
        "research/references/paper.md",
        "text/markdown",
        b"Read primary sources.\n",
    );
    finish_multipart(&mut upload_body, boundary);

    let upload = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["skills"].as_array().expect("uploaded skills").len(), 2);

    let read = request_empty(
        &app,
        Method::GET,
        "/api/admin/managed-skills/packs/research/references/paper.md",
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(read).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["skill"]["content"], "Read primary sources.\n");
}

#[tokio::test]
async fn admin_can_update_system_settings_by_section() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let system_update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings/system",
        json!({
            "max_sessions_per_user": 3,
            "max_attachment_upload_bytes": 128 * 1024 * 1024,
            "attachment_retention_days": 14,
            "empty_chat_prompt": "Ask Hermes anything",
            "speech_input": {
                "enabled": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(system_update.status(), StatusCode::NO_CONTENT);

    let auth_update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings/auth",
        json!({
            "oidc": {
                "enabled": true,
                "display_name": "Acme SSO",
                "client_id": "hermes-hub",
                "client_secret": "oidc-secret",
                "issuer_url": "https://idp.example.com",
                "authorization_url": "https://idp.example.com/oauth2/v1/authorize",
                "token_url": "https://idp.example.com/oauth2/v1/token",
                "userinfo_url": "https://idp.example.com/oauth2/v1/userinfo",
                "logout_url": "https://idp.example.com/logout",
                "scopes": "openid profile email",
                "username_claim": "preferred_username",
                "email_claim": "email",
                "allow_password_login": true,
                "auto_create_users": true
            },
            "ldap": {
                "enabled": true,
                "display_name": "Corporate LDAP",
                "url": "ldaps://ldap.example.com:636",
                "bind_dn": "cn=hub,ou=apps,dc=example,dc=com",
                "bind_password": "ldap-bind-secret",
                "base_dn": "ou=people,dc=example,dc=com",
                "user_filter": "(&(objectClass=person)(mail={email}))",
                "email_attribute": "mail",
                "auto_create_users": true
            },
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(auth_update.status(), StatusCode::NO_CONTENT);

    let public_platform_update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings/public-platform",
        json!({
            "public_platform": {
                "enabled": false,
                "temporary_session_retention_hours": 48
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(public_platform_update.status(), StatusCode::NO_CONTENT);

    let api_management_update = request_json(
        &app,
        Method::PUT,
        "/api/admin/system-settings/api-management",
        json!({
            "api_management": {
                "enabled": true
            }
        }),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(api_management_update.status(), StatusCode::NO_CONTENT);

    let settings = request_empty(
        &app,
        Method::GET,
        "/api/admin/system-settings",
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(
        settings
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-cache, no-store, no-transform")
    );
    let (status, body) = response_json(settings).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["max_sessions_per_user"], 3);
    assert_eq!(
        body["settings"]["max_attachment_upload_bytes"],
        128 * 1024 * 1024
    );
    assert_eq!(body["settings"]["attachment_retention_days"], 14);
    assert_eq!(body["settings"]["empty_chat_prompt"], "Ask Hermes anything");
    assert_eq!(body["settings"]["speech_input"]["enabled"], true);
    assert_eq!(body["settings"]["oidc"]["enabled"], true);
    assert_eq!(body["settings"]["oidc"]["client_id"], "hermes-hub");
    assert_eq!(body["settings"]["ldap"]["enabled"], true);
    assert_eq!(body["settings"]["ldap"]["display_name"], "Corporate LDAP");
    assert!(body["settings"].get("business_oauth").is_none());
    assert_eq!(body["settings"]["public_platform"]["enabled"], false);
    assert_eq!(
        body["settings"]["public_platform"]["temporary_session_retention_hours"],
        48
    );
    assert_eq!(body["settings"]["api_management"]["enabled"], true);
}

#[tokio::test]
async fn managed_skill_upload_can_exceed_default_json_body_limit() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;
    let boundary = "managed-skills-large-upload-boundary";
    let large_skill = vec![b'a'; 9 * 1024 * 1024];
    let mut upload_body = Vec::new();
    multipart_file(
        &mut upload_body,
        boundary,
        "files",
        "large/SKILL.md",
        "text/markdown",
        &large_skill,
    );
    finish_multipart(&mut upload_body, boundary);

    let upload = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&admin_cookie),
    )
    .await;
    let (status, body) = response_json(upload).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["skills"][0]["path"], "large/SKILL.md");
    assert_eq!(body["skills"][0]["size"], large_skill.len());
}

#[tokio::test]
async fn managed_skill_upload_obeys_configured_size_limit_while_streaming() {
    let (state, _runtime) = app_state_with_recording_docker_runtime().await;
    let mut settings = SystemSettings::default();
    settings.max_attachment_upload_bytes = 1024;
    state
        .store
        .update_system_settings(settings)
        .await
        .expect("system settings can be lowered");
    let app = build_router_with_state(state);
    let admin_cookie = bootstrap_admin(&app).await;
    let boundary = "managed-skills-size-limit-boundary";
    let oversized_skill = vec![b'a'; 2048];
    let mut upload_body = Vec::new();
    multipart_file(
        &mut upload_body,
        boundary,
        "files",
        "large/SKILL.md",
        "text/markdown",
        &oversized_skill,
    );
    finish_multipart(&mut upload_body, boundary);

    let upload = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(upload.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn managed_skill_upload_rejects_zip_archives() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;
    let boundary = "managed-skills-zip-boundary";
    let archive = sample_zip_bytes();
    let mut upload_body = Vec::new();
    multipart_text(&mut upload_body, boundary, "target_path", "bundles");
    multipart_file(
        &mut upload_body,
        boundary,
        "file",
        "skills.zip",
        "application/zip",
        &archive,
    );
    finish_multipart(&mut upload_body, boundary);

    let upload = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        upload_body,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(upload.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn managed_skill_upload_rejects_unsafe_paths() {
    let app = test_app();
    let admin_cookie = bootstrap_admin(&app).await;

    let boundary = "managed-skills-unsafe-folder-boundary";
    let mut unsafe_folder_body = Vec::new();
    multipart_file(
        &mut unsafe_folder_body,
        boundary,
        "files",
        "../SKILL.md",
        "text/markdown",
        b"escaped",
    );
    finish_multipart(&mut unsafe_folder_body, boundary);
    let unsafe_folder = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        unsafe_folder_body,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(unsafe_folder.status(), StatusCode::BAD_REQUEST);

    let boundary = "managed-skills-hidden-upload-boundary";
    let mut hidden_body = Vec::new();
    multipart_file(
        &mut hidden_body,
        boundary,
        "files",
        ".curator_state/state.json",
        "application/json",
        b"{}",
    );
    finish_multipart(&mut hidden_body, boundary);
    let hidden = request_raw(
        &app,
        Method::POST,
        "/api/admin/managed-skills/upload",
        &format!("multipart/form-data; boundary={boundary}"),
        hidden_body,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(hidden.status(), StatusCode::BAD_REQUEST);

    for (case_name, filename, content_type) in [
        ("nested-hidden-file", "writing/.hidden.md", "text/markdown"),
        ("hidden-directory", ".cache/file.md", "text/markdown"),
    ] {
        let boundary = format!("managed-skills-dot-path-boundary-{case_name}");
        let mut dot_path_body = Vec::new();
        multipart_file(
            &mut dot_path_body,
            &boundary,
            "files",
            filename,
            content_type,
            b"hidden",
        );
        finish_multipart(&mut dot_path_body, &boundary);
        let dot_path = request_raw(
            &app,
            Method::POST,
            "/api/admin/managed-skills/upload",
            &format!("multipart/form-data; boundary={boundary}"),
            dot_path_body,
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(dot_path.status(), StatusCode::BAD_REQUEST);
    }
}
