use std::path::Path;
use std::process::Command;

#[test]
fn hermes_wrapper_image_tracks_official_hermes_version() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let dockerfile = std::fs::read_to_string(repo_root.join("infra/docker/hermes/Dockerfile"))
        .expect("Hermes wrapper Dockerfile is present");
    let patch_path = repo_root.join("infra/docker/hermes/patch_send_message_tool.py");
    let compose = std::fs::read_to_string(repo_root.join("infra/docker/docker-compose.hub.yml"))
        .expect("deployment compose file is present");
    let dev_compose = std::fs::read_to_string(repo_root.join("infra/docker/docker-compose.yml"))
        .expect("development compose file is present");

    assert!(dockerfile.contains("ARG HERMES_VERSION=latest"));
    assert!(dockerfile.contains("FROM nousresearch/hermes-agent:${HERMES_VERSION}"));
    assert!(dockerfile.contains("COPY infra/docker/hermes/hermes-hub-entrypoint.sh"));
    assert!(dockerfile.contains("COPY infra/docker/hermes/patch_send_message_tool.py"));
    assert!(dockerfile.contains("RUN python3 /opt/hermes-hub/patch_send_message_tool.py"));
    assert!(dockerfile.contains("ENTRYPOINT [\"/opt/hermes-hub/entrypoint.sh\"]"));
    assert!(
        patch_path.exists(),
        "Hermes wrapper keeps a minimal MEDIA bridge for plugin send_message delivery"
    );
    let patch_source =
        std::fs::read_to_string(&patch_path).expect("send_message MEDIA bridge patch is present");
    assert!(patch_source.contains("Hermes Hub plugin media bridge"));
    assert!(patch_source.contains("send_image_file"));
    assert!(patch_source.contains("send_document"));
    assert!(patch_source.contains("send_voice"));
    assert!(patch_source.contains("send_video"));
    assert!(patch_source.contains("media_sequence"));
    assert!(patch_source.contains("media_files=media_files if is_last else []"));
    assert!(!patch_source.contains("attachments=['/workspace/report.pdf']"));
    assert!(!patch_source.contains("send_message_with_attachments"));
    assert!(!patch_source.contains("platform_name == \"origin\""));
    let compile_output = Command::new("python3")
        .args(["-m", "py_compile", patch_path.to_str().expect("utf-8 path")])
        .output()
        .expect("python3 can compile send_message patch");
    assert!(
        compile_output.status.success(),
        "send_message patch must compile: {}",
        String::from_utf8_lossy(&compile_output.stderr)
    );
    assert!(compose.contains("HERMES_VERSION: ${HERMES_VERSION:-latest}"));
    assert!(compose.contains(
        "HERMES_DOCKER_IMAGE: ${HERMES_DOCKER_IMAGE:-ghcr.io/yiiilin/hermes-hub-hermes:${HERMES_VERSION:-latest}}"
    ));
    assert!(dev_compose.contains(
        "HERMES_DOCKER_IMAGE: ${HERMES_DOCKER_IMAGE:-ghcr.io/yiiilin/hermes-hub-hermes:${HERMES_VERSION:-latest}}"
    ));
}

#[test]
fn hermes_send_message_patch_applies_to_target_image_when_docker_is_available() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let patch_path = repo_root
        .join("infra/docker/hermes/patch_send_message_tool.py")
        .canonicalize()
        .expect("send_message MEDIA bridge patch is present");
    let image = std::env::var("HERMES_HUB_HERMES_TEST_IMAGE")
        .unwrap_or_else(|_| "nousresearch/hermes-agent:latest".to_string());

    let docker_available = Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !docker_available {
        eprintln!("skipping target image patch verification because Docker is unavailable");
        return;
    }

    let image_available = Command::new("docker")
        .args(["image", "inspect", &image])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !image_available {
        eprintln!("skipping target image patch verification because {image} is unavailable");
        return;
    }

    let mount = format!("{}:/tmp/patch_send_message_tool.py:ro", patch_path.display());
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--entrypoint",
            "sh",
            "-u",
            "0:0",
            "-v",
            &mount,
            &image,
            "-lc",
            "python3 /tmp/patch_send_message_tool.py && python3 -m py_compile /opt/hermes/tools/send_message_tool.py && python3 /tmp/patch_send_message_tool.py",
        ])
        .output()
        .expect("docker can run target Hermes image patch verification");
    assert!(
        output.status.success(),
        "send_message patch must apply and compile on {image}: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hermes_wrapper_exposes_standard_hub_adapter_surface() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let adapter =
        std::fs::read_to_string(repo_root.join("backend/src/hermes/docker_provisioner.rs"))
            .expect("Hermes Hub adapter source is present");

    assert!(adapter.contains("async def send_document("));
    assert!(adapter.contains("async def send_image_file("));
    assert!(adapter.contains("async def send_image("));
    assert!(adapter.contains("async def send_multiple_images("));
    assert!(adapter.contains("async def send_typing("));
    assert!(adapter.contains("async def stop_typing("));
    assert!(adapter.contains("async def send_clarify("));
    assert!(!adapter.contains("async def send_slash_confirm("));
    assert!(adapter.contains("async def send_voice("));
    assert!(adapter.contains("async def send_video("));
    assert!(adapter.contains("async def send_animation("));
    assert!(adapter.contains("file_path=audio_path"));
    assert!(adapter.contains("file_path=video_path"));
    assert!(adapter.contains("image_url=animation_url"));
    assert!(adapter.contains("return await super().send_clarify("));
    assert!(adapter.contains("item_metadata[\"media_sequence\"] = index"));
    assert!(adapter.contains("media_metadata.setdefault(\"media_source_url\", raw_url)"));
    assert!(adapter.contains("def _image_extension_from_file("));
    assert!(adapter.contains("os.replace(cached_path, renamed_path)"));
    assert!(adapter.contains("explicit_client_message_key = str(metadata.get(\"client_message_key\") or \"\")"));
    assert!(adapter.contains("if explicit_client_message_key and media_sequence is None:"));
    assert!(adapter.contains("\"remote\""));
    assert!(adapter.contains("\"local\""));
    assert!(adapter.contains("MEDIA:/workspace/report.pdf"));
    assert!(!adapter.contains("attachments=['/workspace/report.pdf']"));
    assert!(!adapter.contains("send_message_with_attachments"));
    assert!(!adapter.contains("platform_name == \"origin\""));
    assert!(!adapter.contains("async def delete_message("));
    assert!(!adapter.contains("async def play_tts("));
    assert!(!adapter.contains("async def send_private_notice("));
    assert!(!adapter.contains("async def create_handoff_thread("));
    assert!(!adapter.contains("async def send_draft("));
    assert!(!adapter.contains("def supports_draft_streaming("));
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
    assert!(entrypoint.contains("ln -sfn \"$HERMES_HUB_NFS_DIR/SOUL.md\" \"/config/SOUL.md\""));
    assert!(entrypoint.contains("ln -sfn \"$HERMES_HUB_NFS_DIR/SOUL.md\" \"/workspace/SOUL.md\""));
    assert!(
        !entrypoint.contains("for file in AGENTS.md SOUL.md"),
        "entrypoint must not manage AGENTS.md anymore"
    );
    assert!(
        !entrypoint.contains("ln -sfn \"$HERMES_HUB_NFS_DIR/AGENTS.md\""),
        "entrypoint must not create AGENTS.md links from Hub FS"
    );
    assert!(entrypoint.contains("旧版 wrapper 曾经管理 AGENTS.md"));
    assert!(entrypoint.contains("exec /init /opt/hermes/docker/main-wrapper.sh \"$@\""));
    assert!(entrypoint.contains("exec /opt/hermes/docker/entrypoint.sh \"$@\""));
}
