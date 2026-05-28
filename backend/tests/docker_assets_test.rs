use std::path::Path;

#[test]
fn hermes_wrapper_image_tracks_official_hermes_version() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let dockerfile = std::fs::read_to_string(repo_root.join("infra/docker/hermes/Dockerfile"))
        .expect("Hermes wrapper Dockerfile is present");
    let compose = std::fs::read_to_string(repo_root.join("infra/docker/docker-compose.hub.yml"))
        .expect("deployment compose file is present");
    let dev_compose = std::fs::read_to_string(repo_root.join("infra/docker/docker-compose.yml"))
        .expect("development compose file is present");

    assert!(dockerfile.contains("ARG HERMES_VERSION=latest"));
    assert!(dockerfile.contains("FROM nousresearch/hermes-agent:${HERMES_VERSION}"));
    assert!(dockerfile.contains("patch_send_message_tool.py"));
    assert!(dockerfile.contains("COPY infra/docker/hermes/hermes-hub-entrypoint.sh"));
    assert!(dockerfile.contains("ENTRYPOINT [\"/opt/hermes-hub/entrypoint.sh\"]"));
    assert!(compose.contains("HERMES_VERSION: ${HERMES_VERSION:-latest}"));
    assert!(compose.contains(
        "HERMES_DOCKER_IMAGE: ${HERMES_DOCKER_IMAGE:-ghcr.io/yiiilin/hermes-hub-hermes:${HERMES_VERSION:-latest}}"
    ));
    assert!(dev_compose.contains(
        "HERMES_DOCKER_IMAGE: ${HERMES_DOCKER_IMAGE:-ghcr.io/yiiilin/hermes-hub-hermes:${HERMES_VERSION:-latest}}"
    ));
}

#[test]
fn hermes_wrapper_patches_plugin_media_delivery() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let patch =
        std::fs::read_to_string(repo_root.join("infra/docker/hermes/patch_send_message_tool.py"))
            .expect("Hermes send_message patch is present");

    assert!(patch.contains("adapter.send_document"));
    assert!(patch.contains("adapter.send_image_file"));
    assert!(patch.contains("is_plugin_platform"));
}

#[test]
fn hermes_wrapper_entrypoint_links_managed_profile_from_nfs() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let entrypoint =
        std::fs::read_to_string(repo_root.join("infra/docker/hermes/hermes-hub-entrypoint.sh"))
            .expect("Hermes Hub wrapper entrypoint is present");

    assert!(entrypoint.contains("HERMES_HUB_NFS_DIR=\"${HERMES_HUB_NFS_DIR:-/nfs}\""));
    assert!(entrypoint.contains("chown hermes:hermes /config /workspace"));
    assert!(entrypoint.contains("ln -sfn \"$HERMES_HUB_NFS_DIR/$file\" \"/config/$file\""));
    assert!(entrypoint.contains("ln -sfn \"$HERMES_HUB_NFS_DIR/$file\" \"/workspace/$file\""));
    assert!(entrypoint.contains("exec /init /opt/hermes/docker/main-wrapper.sh \"$@\""));
    assert!(entrypoint.contains("exec /opt/hermes/docker/entrypoint.sh \"$@\""));
}
