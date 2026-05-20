use hermes_hub_backend::hermes::{
    docker_provisioner::{
        DockerProvisioner, DockerProvisionerConfig, DockerRuntime, DockerRuntimeOutput,
        HermesContainerConnectMode,
    },
    instance::{HermesInstanceKind, HermesInstanceStatus},
    provisioner::HermesProvisioner,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

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
        connect_mode: HermesContainerConnectMode::Network,
        published_host_ip: "127.0.0.1".to_string(),
        published_base_url: "http://127.0.0.1".to_string(),
        hub_llm_base_url: "http://hermes-hub:8080/internal/llm/v1".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        memory_limit: Some("1g".to_string()),
        cpu_limit: Some("1.0".to_string()),
        docker_binary: "docker".to_string(),
    }
}

#[tokio::test]
async fn docker_provisioner_publishes_random_host_port_for_host_development() {
    let runtime = FakeDockerRuntime::default();
    let mut config = test_config();
    config.connect_mode = HermesContainerConnectMode::PublishedHost;
    config.published_host_ip = "127.0.0.1".to_string();
    config.published_base_url = "http://127.0.0.1".to_string();
    let provisioner = DockerProvisioner::new_with_runtime(config, Arc::new(runtime.clone()));

    let instance = provisioner
        .ensure_instance("user-456", "instance-token")
        .await
        .expect("instance can be created");

    assert_eq!(instance.base_url, "http://127.0.0.1:32080");

    let spec = provisioner
        .container_spec_for(&instance)
        .expect("container spec can be rendered");
    assert_eq!(spec.published_ports, vec!["127.0.0.1::8000".to_string()]);

    let calls = runtime.calls.lock().expect("calls lock").clone();
    let create_call = calls
        .iter()
        .find(|args| args.first().map(String::as_str) == Some("create"))
        .expect("container create command is issued");
    assert!(
        create_call
            .windows(2)
            .any(|args| args[0] == "--publish" && args[1] == "127.0.0.1::8000"),
        "host development mode must publish a random loopback port"
    );
    assert!(calls.iter().any(|args| args
        == &vec![
            "port".to_string(),
            "hermes-user-user-456".to_string(),
            "8000/tcp".to_string(),
        ]));
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
    assert_eq!(instance.base_url, "http://hermes-user-user-123:8000");
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

    let spec = provisioner
        .container_spec_for(&instance)
        .expect("container spec can be rendered");

    assert_eq!(spec.image, "nousresearch/hermes-agent:latest");
    assert_eq!(spec.network, "hermes-hub-net");
    assert!(
        spec.published_ports.is_empty(),
        "managed Hermes must not expose host ports"
    );
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "API_SERVER_ENABLED=true"));
    assert!(spec
        .env
        .iter()
        .any(|entry| entry == "API_SERVER_HOST=0.0.0.0"));
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
    assert!(spec
        .mounts
        .iter()
        .any(|mount| mount.container_path == "/config" && !mount.read_only));
    assert_eq!(instance.container_id.as_deref(), Some("container-created"));
    assert_eq!(
        instance.api_token_secret_ref.as_deref(),
        Some("instance-token")
    );
    let managed_config =
        std::fs::read_to_string("/tmp/hermes-hub-test/users/user-123/config/config.yaml")
            .expect("managed Hermes config is written");
    assert!(managed_config.contains("provider: \"custom\""));
    assert!(managed_config.contains("default: \"gpt-4.1-mini\""));
    assert!(managed_config.contains("api_key: \"instance-token\""));

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
