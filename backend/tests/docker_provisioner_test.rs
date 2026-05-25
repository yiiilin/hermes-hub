use hermes_hub_backend::hermes::{
    docker_provisioner::{
        ContainerMount, DockerProvisioner, DockerProvisionerConfig, DockerRuntime,
        DockerRuntimeOutput, ManagedSkillsMountConfig,
    },
    instance::{HermesInstanceKind, HermesInstanceStatus},
    provisioner::HermesProvisioner,
};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Clone, Default)]
struct FakeDockerRuntime {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    container_exists: Arc<Mutex<bool>>,
}

#[async_trait::async_trait]
impl DockerRuntime for FakeDockerRuntime {
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
                success: false,
                stdout: String::new(),
                stderr: "network not found".to_string(),
            });
        }

        if args.get(0).map(String::as_str) == Some("container")
            && args.get(1).map(String::as_str) == Some("inspect")
        {
            let exists = *self.container_exists.lock().expect("exists lock");
            return Ok(DockerRuntimeOutput {
                success: exists,
                stdout: if exists {
                    "container-existing".to_string()
                } else {
                    String::new()
                },
                stderr: if exists {
                    String::new()
                } else {
                    "No such container".to_string()
                },
            });
        }

        if args.get(0).map(String::as_str) == Some("create") {
            *self.container_exists.lock().expect("exists lock") = true;
            return Ok(DockerRuntimeOutput {
                success: true,
                stdout: "container-created".to_string(),
                stderr: String::new(),
            });
        }

        if args.get(0).map(String::as_str) == Some("port") {
            let exists = *self.container_exists.lock().expect("exists lock");
            return Ok(DockerRuntimeOutput {
                success: exists,
                stdout: if exists {
                    "127.0.0.1:32080".to_string()
                } else {
                    String::new()
                },
                stderr: String::new(),
            });
        }

        if args.get(0).map(String::as_str) == Some("rm") {
            *self.container_exists.lock().expect("exists lock") = false;
        }

        Ok(DockerRuntimeOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

fn test_config() -> DockerProvisionerConfig {
    DockerProvisionerConfig {
        image: "nousresearch/hermes-agent:latest".to_string(),
        data_root: PathBuf::from("/tmp/hermes-hub-test/users"),
        network: "hermes-hub-net".to_string(),
        internal_port: 8000,
        hub_llm_base_url: "http://hermes-hub:8080/internal/llm/v1".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        image_model: "gpt-image-2-medium".to_string(),
        api_mode: "chat_completions".to_string(),
        memory_limit: Some("1g".to_string()),
        cpu_limit: Some("1.0".to_string()),
        docker_binary: "docker".to_string(),
        managed_skills: None,
    }
}

fn test_config_with_managed_skills() -> DockerProvisionerConfig {
    let mut config = test_config();
    config.managed_skills = Some(ManagedSkillsMountConfig {
        volume_name: "hermes-managed-skills-test".to_string(),
        addr: "127.0.0.1:12049".to_string(),
        export: "/skills".to_string(),
        container_path: "/hub-managed-skills".to_string(),
    });
    config
}

#[tokio::test]
async fn docker_provisioner_never_publishes_host_ports() {
    let runtime = FakeDockerRuntime::default();
    let provisioner = DockerProvisioner::new_with_runtime(test_config(), Arc::new(runtime.clone()));

    let instance = provisioner
        .ensure_instance("user-456", "instance-token")
        .await
        .expect("instance can be created");

    let _spec = provisioner
        .container_spec_for(&instance)
        .expect("container spec can be rendered");

    let calls = runtime.calls.lock().expect("calls lock").clone();
    let create_call = calls
        .iter()
        .find(|args| args.first().map(String::as_str) == Some("create"))
        .expect("container create command is issued");
    assert!(
        create_call
            .iter()
            .all(|arg| arg != "-p" && arg != "--publish"),
        "adapter-only Hermes containers must not publish host ports"
    );
    assert!(
        calls
            .iter()
            .all(|args| args.first().map(String::as_str) != Some("port")),
        "Hub no longer needs to resolve a published Hermes port"
    );
}

#[tokio::test]
async fn docker_provisioner_recreates_running_container_without_current_managed_spec_label() {
    let runtime = FakeDockerRuntime::default();
    let provisioner = DockerProvisioner::new_with_runtime(test_config(), Arc::new(runtime.clone()));

    // 第一次启动会写出当前 config.yaml 和 hermes_hub platform plugin；
    // 第二次启动时文件不再变化，只有容器规格标签能暴露旧容器缺少新插件行为的问题。
    provisioner
        .ensure_instance("user-spec-label", "instance-token")
        .await
        .expect("instance can be created");
    runtime.calls.lock().expect("calls lock").clear();

    provisioner
        .ensure_instance("user-spec-label", "instance-token")
        .await
        .expect("old container can be recreated");

    let calls = runtime.calls.lock().expect("calls lock").clone();
    assert!(
        calls.iter().any(|args| args
            == &vec![
                "rm".to_string(),
                "-f".to_string(),
                "hermes-user-user-spec-label".to_string(),
            ]),
        "old managed Hermes containers without the current spec label must be removed"
    );
    assert!(
        calls
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("create")),
        "old managed Hermes containers without the current spec label must be recreated"
    );
}

#[tokio::test]
async fn docker_provisioner_writes_codex_responses_api_mode_for_responses_models() {
    let runtime = FakeDockerRuntime::default();
    let mut config = test_config();
    config.api_mode = "responses".to_string();
    let provisioner = DockerProvisioner::new_with_runtime(config, Arc::new(runtime.clone()));

    provisioner
        .ensure_instance("user-responses", "instance-token")
        .await
        .expect("instance can be created");

    let managed_config =
        std::fs::read_to_string("/tmp/hermes-hub-test/users/user-responses/config/config.yaml")
            .expect("managed Hermes config is written");
    assert!(managed_config.contains("api_mode: \"codex_responses\""));
}

#[tokio::test]
async fn docker_provisioner_writes_configured_image_model_to_env_and_config() {
    let runtime = FakeDockerRuntime::default();
    let mut config = test_config();
    config.image_model = "gpt-image-1".to_string();
    let provisioner = DockerProvisioner::new_with_runtime(config, Arc::new(runtime.clone()));

    let instance = provisioner
        .ensure_instance("user-image-model", "instance-token")
        .await
        .expect("instance can be created");
    let spec = provisioner
        .container_spec_for(&instance)
        .expect("container spec can be rendered");

    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "OPENAI_IMAGE_MODEL=gpt-image-1"));
    let managed_config =
        std::fs::read_to_string("/tmp/hermes-hub-test/users/user-image-model/config/config.yaml")
            .expect("managed Hermes config is written");
    assert!(managed_config.contains("image_gen:"));
    assert!(managed_config.contains("model: \"gpt-image-1\""));
    assert!(!managed_config.contains("gpt-image-2-medium"));
}

#[tokio::test]
async fn docker_provisioner_derives_channel_base_url_from_hub_origin() {
    let runtime = FakeDockerRuntime::default();
    let mut config = test_config();
    config.hub_llm_base_url = "http://hermes-hub:8080".to_string();
    let provisioner = DockerProvisioner::new_with_runtime(config, Arc::new(runtime.clone()));

    let instance = provisioner
        .ensure_instance("user-root-hub-url", "instance-token")
        .await
        .expect("instance can be created");
    let spec = provisioner
        .container_spec_for(&instance)
        .expect("container spec can be rendered");

    assert!(
        spec.env
            .iter()
            .any(|entry| entry == "HERMES_HUB_CHANNEL_BASE_URL=http://hermes-hub:8080/internal/channel/v1"),
        "channel adapter must call the Hub internal channel API, even when the LLM base URL is configured as the Hub origin"
    );
    let managed_config =
        std::fs::read_to_string("/tmp/hermes-hub-test/users/user-root-hub-url/config/config.yaml")
            .expect("managed Hermes config is written");
    assert!(managed_config.contains("base_url: \"http://hermes-hub:8080/internal/channel/v1\""));
}

#[tokio::test]
async fn docker_provisioner_pre_approves_hermes_hub_pairing_for_managed_users() {
    let runtime = FakeDockerRuntime::default();
    let provisioner = DockerProvisioner::new_with_runtime(test_config(), Arc::new(runtime));
    let user_id = format!("user-pairing-{}", Uuid::new_v4());

    let instance = provisioner
        .ensure_instance(&user_id, "instance-token")
        .await
        .expect("instance can be created");

    let config_path = PathBuf::from(
        instance
            .host_config_path
            .expect("managed config path is set"),
    );
    for approved_path in [
        config_path.join("pairing/hermes_hub-approved.json"),
        config_path.join("platforms/pairing/hermes_hub-approved.json"),
    ] {
        let approved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&approved_path)
                .expect("Hermes Hub approved pairing is written"),
        )
        .expect("approved pairing is valid json");
        let user_entry = approved
            .get(&user_id)
            .expect("Hub user is pre-approved for hermes_hub");

        assert_eq!(user_entry["user_name"], "Hub user");
        assert!(
            user_entry["approved_at"].as_f64().unwrap_or_default() > 0.0,
            "Hermes pairing store expects an approval timestamp"
        );
    }
}

#[tokio::test]
async fn docker_provisioner_preserves_approved_pairing_and_clears_stale_pending_pairing() {
    let runtime = FakeDockerRuntime::default();
    let provisioner = DockerProvisioner::new_with_runtime(test_config(), Arc::new(runtime));
    let user_id = format!("user-pairing-state-{}", Uuid::new_v4());
    let prepared = provisioner.prepare_instance(&user_id);
    let config_path = PathBuf::from(
        prepared
            .host_config_path
            .as_ref()
            .expect("managed config path is set"),
    );
    let pairing_dir = config_path.join("pairing");
    std::fs::create_dir_all(&pairing_dir).expect("pairing directory can be created");
    let approved_path = pairing_dir.join("hermes_hub-approved.json");
    let pending_path = pairing_dir.join("hermes_hub-pending.json");

    // Hermes 自己的 pairing store 以 platform 分文件保存状态。Hub 重写托管配置时
    // 不能刷新既有 approved_at，否则升级/重建容器会造成无意义状态漂移。
    std::fs::write(
        &approved_path,
        format!(
            r#"{{
                "{user_id}": {{"user_name": "Existing Hub user", "approved_at": 12345.0}},
                "other-user": {{"user_name": "Other", "approved_at": 67890.0}}
            }}"#
        ),
    )
    .expect("approved pairing fixture can be written");
    std::fs::write(
        &pending_path,
        format!(
            r#"{{
                "STALE": {{"user_id": "{user_id}", "user_name": "Hub user", "created_at": 1.0}},
                "KEEP": {{"user_id": "other-user", "user_name": "Other", "created_at": 2.0}}
            }}"#
        ),
    )
    .expect("pending pairing fixture can be written");

    provisioner
        .ensure_instance(&user_id, "instance-token")
        .await
        .expect("instance can be created");

    let approved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&approved_path).expect("approved exists"))
            .expect("approved pairing is valid json");
    assert_eq!(approved[&user_id]["user_name"], "Existing Hub user");
    assert_eq!(approved[&user_id]["approved_at"], 12345.0);
    assert_eq!(approved["other-user"]["approved_at"], 67890.0);

    let pending: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pending_path).expect("pending exists"))
            .expect("pending pairing is valid json");
    assert!(
        pending.get("STALE").is_none(),
        "stale pending code for the already approved Hub user must be removed"
    );
    assert_eq!(pending["KEEP"]["user_id"], "other-user");
}

#[tokio::test]
async fn docker_provisioner_mounts_managed_skills_readonly_as_external_dir() {
    let runtime = FakeDockerRuntime::default();
    let provisioner = DockerProvisioner::new_with_runtime(
        test_config_with_managed_skills(),
        Arc::new(runtime.clone()),
    );

    let instance = provisioner
        .ensure_instance("user-managed-skills", "instance-token")
        .await
        .expect("instance can be created");
    let spec = provisioner
        .container_spec_for(&instance)
        .expect("container spec can be rendered");

    assert!(spec.mounts.iter().any(|mount| matches!(
        mount,
        ContainerMount::NfsVolume(volume)
            if volume.volume_name == "hermes-managed-skills-test"
                && volume.container_path == "/hub-managed-skills"
                && volume.read_only
                && volume.addr == "127.0.0.1:12049"
                && volume.export == "/skills"
    )));
    assert!(
        spec.mounts
            .iter()
            .all(|mount| !mount.container_path().starts_with("/config/skills")),
        "managed skills must not be mounted into Hermes curator's /config/skills tree"
    );

    let managed_config = std::fs::read_to_string(
        "/tmp/hermes-hub-test/users/user-managed-skills/config/config.yaml",
    )
    .expect("managed Hermes config is written");
    assert!(managed_config.contains("skills:"));
    assert!(managed_config.contains("external_dirs:"));
    assert!(managed_config.contains("- \"/hub-managed-skills\""));

    let calls = runtime.calls.lock().expect("calls lock").clone();
    assert!(
        calls.iter().any(|args| {
            args.first().map(String::as_str) == Some("volume")
                && args.get(1).map(String::as_str) == Some("create")
                && args.contains(&"type=nfs".to_string())
                && args.contains(
                    &"o=addr=127.0.0.1,port=12049,mountport=12049,vers=3,tcp,nolock,soft,ro"
                        .to_string(),
                )
                && args.contains(&"device=:/skills".to_string())
                && args.last().map(String::as_str) == Some("hermes-managed-skills-test")
        }),
        "managed skills NFS volume must be created before container create"
    );
    let create_call = calls
        .iter()
        .find(|args| args.first().map(String::as_str) == Some("create"))
        .expect("container create command is issued");
    assert!(
        create_call.windows(2).any(|args| {
            args[0] == "--mount"
                && args[1]
                    == "type=volume,src=hermes-managed-skills-test,dst=/hub-managed-skills,volume-driver=local,readonly"
        }),
        "managed skills must be mounted readonly into Hermes containers"
    );
}

#[tokio::test]
async fn docker_provisioner_test() {
    let runtime = FakeDockerRuntime::default();
    let provisioner = DockerProvisioner::new_with_runtime(test_config(), Arc::new(runtime.clone()));

    let instance = provisioner
        .ensure_instance("user-123", "instance-token")
        .await
        .expect("instance can be created");

    assert_eq!(instance.user_id, "user-123");
    assert_eq!(instance.kind, HermesInstanceKind::ManagedDocker);
    assert_eq!(instance.status, HermesInstanceStatus::Running);
    assert_eq!(
        instance.host_workspace_path.as_deref(),
        Some("/tmp/hermes-hub-test/users/user-123/workspace")
    );
    assert_eq!(
        instance.host_sandbox_path.as_deref(),
        Some("/tmp/hermes-hub-test/users/user-123/sandbox")
    );
    assert_eq!(
        instance.host_config_path.as_deref(),
        Some("/tmp/hermes-hub-test/users/user-123/config")
    );
    let workspace_mode = std::fs::metadata("/tmp/hermes-hub-test/users/user-123/workspace")
        .expect("workspace exists")
        .permissions()
        .mode()
        & 0o777;
    let sandbox_mode = std::fs::metadata("/tmp/hermes-hub-test/users/user-123/sandbox")
        .expect("sandbox exists")
        .permissions()
        .mode()
        & 0o777;
    let config_mode = std::fs::metadata("/tmp/hermes-hub-test/users/user-123/config")
        .expect("config exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(workspace_mode, 0o777);
    assert_eq!(sandbox_mode, 0o777);
    assert_eq!(config_mode, 0o777);

    let spec = provisioner
        .container_spec_for(&instance)
        .expect("container spec can be rendered");

    assert_eq!(spec.image, "nousresearch/hermes-agent:latest");
    assert_eq!(spec.network, "hermes-hub-net");
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "API_SERVER_ENABLED=true"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "API_SERVER_HOST=127.0.0.1"));
    assert!(spec.env.iter().any(|entry| entry == "API_SERVER_PORT=8000"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "API_SERVER_KEY=instance-token"));
    assert!(spec.env.iter().any(|entry| entry == "HERMES_HOME=/config"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "HERMES_INFERENCE_PROVIDER=custom"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "CUSTOM_BASE_URL=http://hermes-hub:8080/internal/llm/v1"));
    assert_eq!(spec.command, vec!["gateway".to_string()]);
    assert_eq!(spec.workdir.as_deref(), Some("/workspace"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "OPENAI_BASE_URL=http://hermes-hub:8080/internal/llm/v1"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "OPENAI_API_KEY=instance-token"));
    assert!(spec.env.iter().any(|entry| {
        entry == "HERMES_HUB_CHANNEL_BASE_URL=http://hermes-hub:8080/internal/channel/v1"
    }));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "HERMES_HUB_CHANNEL_TOKEN=instance-token"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == &format!("HERMES_HUB_INSTANCE_ID={}", instance.id)));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "HERMES_HUB_USER_ID=user-123"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "HERMES_HUB_INBOX_PATH=/internal/channel/v1/inbox"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "HERMES_HUB_INBOX_TIMEOUT_SECONDS=25"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "HERMES_HUB_INBOX_LIMIT=4"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "OPENAI_IMAGE_MODEL=gpt-image-2-medium"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "HERMES_TOOL_PROGRESS_MODE=verbose"));
    assert!(spec.env.iter().any(|entry| entry == "HERMES_YOLO_MODE=1"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "HERMES_ACCEPT_HOOKS=1"));
    assert!(spec.labels.iter().any(|(key, value)| {
        key == "hermes_hub_spec_version" && value == "2026-05-25-hermes-hub-run-context"
    }));
    assert!(spec
        .mounts
        .iter()
        .any(|mount| mount.container_path() == "/config" && !mount.read_only()));
    assert!(spec.mounts.iter().any(|mount| {
        matches!(
            mount,
            ContainerMount::Bind(bind)
                if bind.container_path == "/opt/data"
                    && bind.host_path == "/tmp/hermes-hub-test/users/user-123/sandbox"
                    && !bind.read_only
        )
    }));
    assert_eq!(instance.container_id.as_deref(), Some("container-created"));
    assert_eq!(
        instance.api_token_secret_ref.as_deref(),
        Some("instance-token")
    );
    let managed_config =
        std::fs::read_to_string("/tmp/hermes-hub-test/users/user-123/config/config.yaml")
            .expect("managed Hermes config is written");
    let plugin_yaml = std::fs::read_to_string(
        "/tmp/hermes-hub-test/users/user-123/config/plugins/platforms/hermes_hub/plugin.yaml",
    )
    .expect("hermes_hub platform plugin metadata is written");
    let plugin_init = std::fs::read_to_string(
        "/tmp/hermes-hub-test/users/user-123/config/plugins/platforms/hermes_hub/__init__.py",
    )
    .expect("hermes_hub platform plugin package marker is written");
    let plugin_adapter = std::fs::read_to_string(
        "/tmp/hermes-hub-test/users/user-123/config/plugins/platforms/hermes_hub/adapter.py",
    )
    .expect("hermes_hub platform adapter is written");
    assert!(managed_config.contains("provider: \"custom\""));
    assert!(managed_config.contains("default: \"gpt-4.1-mini\""));
    assert!(managed_config.contains("api_key: \"instance-token\""));
    assert!(managed_config.contains("plugins:"));
    assert!(managed_config.contains("enabled: [platforms/hermes_hub]"));
    assert!(managed_config.contains("gateway:"));
    assert!(managed_config.contains("platforms:"));
    assert!(managed_config.contains("hermes_hub:"));
    assert!(managed_config.contains("enabled: true"));
    assert!(managed_config.contains("extra:"));
    assert!(managed_config.contains("base_url: \"http://hermes-hub:8080/internal/channel/v1\""));
    assert!(managed_config.contains("inbox_path: \"/internal/channel/v1/inbox\""));
    assert!(managed_config.contains(&format!("instance_id: \"{}\"", instance.id)));
    assert!(managed_config.contains("user_id: \"user-123\""));
    assert!(managed_config.contains("timeout_seconds: 25"));
    assert!(managed_config.contains("limit: 4"));
    assert!(managed_config.contains("image_gen:"));
    assert!(managed_config.contains("provider: \"openai\""));
    assert!(managed_config.contains("model: \"gpt-image-2-medium\""));
    assert!(managed_config.contains("display:"));
    assert!(managed_config.contains("tool_progress: \"verbose\""));
    assert!(managed_config.contains("tool_progress_command: true"));
    assert!(managed_config.contains("auxiliary:"));
    assert!(managed_config.contains("session_search:"));
    assert!(managed_config.contains("provider: \"main\""));
    assert!(managed_config.contains("timeout: 60"));
    assert!(managed_config.contains("max_concurrency: 1"));
    assert!(managed_config.contains("approvals:"));
    assert!(managed_config.contains("mode: \"off\""));
    assert!(managed_config.contains("cron_mode: \"approve\""));
    assert!(managed_config.contains("mcp_reload_confirm: false"));
    assert!(managed_config.contains("destructive_slash_confirm: false"));
    assert!(plugin_yaml.contains("name: hermes-hub-platform"));
    assert!(plugin_yaml.contains("kind: platform"));
    assert!(plugin_yaml.contains("HERMES_HUB_CHANNEL_BASE_URL"));
    assert!(plugin_yaml.contains("HERMES_HUB_CHANNEL_TOKEN"));
    assert!(plugin_init.contains("Hermes Hub platform plugin"));
    assert!(plugin_init.contains("from .adapter import register"));
    assert!(plugin_adapter.contains("class HermesHubAdapter"));
    assert!(plugin_adapter.contains("/internal/channel/v1/inbox?timeout_seconds=25&limit=4"));
    assert!(plugin_adapter.contains("async def connect("));
    assert!(plugin_adapter.contains("MessageEvent("));
    assert!(plugin_adapter.contains("text=content"));
    assert!(plugin_adapter.contains("HERMES_HUB_HOME_CHANNEL"));
    assert!(plugin_adapter.contains("self.build_source("));
    assert!(plugin_adapter.contains("thread_id=session_id"));
    assert!(plugin_adapter.contains("raw_message[\"run_id\"] = run_id"));
    assert!(plugin_adapter.contains("await self.handle_message(event)"));
    assert!(plugin_adapter.contains("async def on_processing_start("));
    assert!(plugin_adapter.contains("async def on_processing_complete("));
    assert!(plugin_adapter.contains("ProcessingOutcome.CANCELLED"));
    assert!(plugin_adapter.contains("MAX_MESSAGE_LENGTH = 8000"));
    assert!(plugin_adapter.contains("self._last_output_messages: dict[str, dict[str, Any]] = {}"));
    assert!(plugin_adapter.contains("self._active_run_ids_by_session: dict[str, str] = {}"));
    assert!(plugin_adapter.contains("self._remember_output_message(metadata, message)"));
    assert!(plugin_adapter
        .contains("run_id = self._normalize_run_id(metadata.get(\"run_id\") or \"\")"));
    assert!(plugin_adapter.contains("return self._active_run_ids_by_session.get(session_id, \"\")"));
    assert!(!plugin_adapter
        .contains("run_id = metadata.get(\"run_id\") or metadata.get(\"thread_id\")"));
    assert!(
        !plugin_adapter.contains("metadata.get(\"message_id\")"),
        "adapter must not treat a Hermes message id as a Hub run id"
    );
    assert!(plugin_adapter.contains("def _session_id_from_metadata("));
    assert!(plugin_adapter.contains("def _forget_active_run("));
    assert!(plugin_adapter.contains("def _normalize_run_id(self, run_id: Any) -> str:"));
    assert!(plugin_adapter.contains("return f\"hub-run-{value}\""));
    assert!(plugin_adapter.contains("\"output_message_id\": output_message_id"));
    assert!(plugin_adapter.contains("payload[\"client_message_key\"] = client_message_key"));
    assert!(plugin_adapter.contains("payload[\"run_id\"] = run_id"));
    assert!(plugin_adapter.contains("await self._merge_attachment_into_last_output("));
    assert!(plugin_adapter.contains("def _content_with_attachment("));
    assert!(plugin_adapter.contains("def _merge_attachments("));
    assert!(plugin_adapter.contains("from urllib.parse import unquote, urlencode"));
    assert!(plugin_adapter.contains("upload_name = unquote(file_name or Path(file_path).name)"));
    assert!(plugin_adapter.contains("def _client_message_key("));
    assert!(plugin_adapter.contains("return f\"hermes-run:{run_id}\""));
    assert!(plugin_adapter.contains("f\"/inbox/{run_id}/ack\""));
    assert!(plugin_adapter.contains("media_types.append(content_type)"));
    assert!(plugin_adapter.contains("startswith(\"image/\")"));
    assert!(plugin_adapter.contains("async def send("));
    assert!(plugin_adapter.contains("async def edit_message("));
    assert!(plugin_adapter.contains("async def send_document("));
    assert!(plugin_adapter.contains("async def send_image_file("));
    assert!(plugin_adapter.contains("async def _wait_after_empty_poll("));
    assert!(
        plugin_adapter.contains("#"),
        "adapter.py must include Chinese comments explaining Hub queue behavior"
    );
    let compile_output = Command::new("python3")
        .args([
            "-m",
            "py_compile",
            "/tmp/hermes-hub-test/users/user-123/config/plugins/platforms/hermes_hub/adapter.py",
        ])
        .output()
        .expect("python3 is available to compile generated adapter");
    assert!(
        compile_output.status.success(),
        "generated adapter.py must be valid Python: {}",
        String::from_utf8_lossy(&compile_output.stderr)
    );

    let calls = runtime.calls.lock().expect("calls lock").clone();
    assert!(calls.iter().any(|args| args
        == &vec![
            "network".to_string(),
            "create".to_string(),
            "hermes-hub-net".to_string(),
        ]));
    let create_call = calls
        .iter()
        .find(|args| args.first().map(String::as_str) == Some("create"))
        .expect("container create command is issued");
    assert!(
        !create_call
            .iter()
            .any(|arg| arg == "-p" || arg == "--publish"),
        "managed Hermes must not publish host ports"
    );
    assert!(
        create_call.windows(2).any(|args| {
            args[0] == "--label"
                && args[1] == "hermes_hub_spec_version=2026-05-25-hermes-hub-run-context"
        }),
        "managed Hermes containers must carry the current spec label"
    );
    assert!(
        create_call.windows(2).any(|args| {
            args[0] == "--mount"
                && args[1]
                    == "type=bind,src=/tmp/hermes-hub-test/users/user-123/sandbox,dst=/opt/data"
        }),
        "managed Hermes must expose /opt/data through a Hub-owned host directory"
    );
    for path in [
        "/tmp/hermes-hub-test/users/user-123/workspace",
        "/tmp/hermes-hub-test/users/user-123/sandbox",
        "/tmp/hermes-hub-test/users/user-123/config",
    ] {
        let mode = std::fs::metadata(path)
            .expect("managed writable directory exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o777,
            "Hermes tool directories must be writable by the container user"
        );
    }

    provisioner
        .stop_instance(&instance)
        .await
        .expect("instance can be stopped");
    assert_eq!(
        provisioner
            .instance(&instance.id)
            .expect("instance is stored")
            .status,
        HermesInstanceStatus::Stopped
    );

    provisioner
        .start_instance(&instance)
        .await
        .expect("instance can be started");
    assert_eq!(
        provisioner
            .instance(&instance.id)
            .expect("instance is stored")
            .status,
        HermesInstanceStatus::Running
    );

    let rebuilt = provisioner
        .rebuild_instance(&instance, "rotated-token")
        .await
        .expect("instance can be rebuilt");

    assert_eq!(rebuilt.id, instance.id);
    assert_eq!(rebuilt.host_workspace_path, instance.host_workspace_path);
    assert_eq!(rebuilt.status, HermesInstanceStatus::Running);
    assert_eq!(
        rebuilt.api_token_secret_ref.as_deref(),
        Some("rotated-token")
    );
}
