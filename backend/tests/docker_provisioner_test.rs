use hermes_hub_backend::hermes::{
    docker_provisioner::{DockerProvisioner, DockerProvisionerConfig},
    instance::{HermesInstanceKind, HermesInstanceStatus},
    provisioner::HermesProvisioner,
};
use std::path::PathBuf;

fn test_config() -> DockerProvisionerConfig {
    DockerProvisionerConfig {
        image: "nousresearch/hermes-agent:latest".to_string(),
        data_root: PathBuf::from("/tmp/hermes-hub-test/users"),
        network: "hermes-hub-net".to_string(),
        internal_port: 8000,
        hub_llm_base_url: "http://hermes-hub:8080/internal/llm/v1".to_string(),
        default_model: "gpt-4.1-mini".to_string(),
        memory_limit: Some("1g".to_string()),
        cpu_limit: Some("1.0".to_string()),
    }
}

#[tokio::test]
async fn docker_provisioner_test() {
    let provisioner = DockerProvisioner::new(test_config());

    let instance = provisioner
        .ensure_instance("user-123")
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
        .any(|entry| entry == "LLM_BASE_URL=http://hermes-hub:8080/internal/llm/v1"));
    assert!(spec
        .mounts
        .iter()
        .any(|mount| mount.container_path == "/config" && mount.read_only));

    provisioner
        .stop_instance(&instance.id)
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
        .start_instance(&instance.id)
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
        .rebuild_instance(&instance.id)
        .await
        .expect("instance can be rebuilt");

    assert_eq!(rebuilt.id, instance.id);
    assert_eq!(rebuilt.host_workspace_path, instance.host_workspace_path);
    assert_eq!(rebuilt.status, HermesInstanceStatus::Running);
}
