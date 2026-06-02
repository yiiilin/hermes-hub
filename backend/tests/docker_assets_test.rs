use std::path::Path;
use std::process::Command;

// 从 Dockerfile 读取当前 tag，避免把 Hermes Agent 升级入口扩散到多处配置。
fn hermes_agent_image_from_dockerfile(dockerfile: &str) -> &str {
    dockerfile
        .lines()
        .find_map(|line| line.strip_prefix("ARG HERMES_AGENT_IMAGE="))
        .expect("Dockerfile pins Hermes Agent image with HERMES_AGENT_IMAGE")
}

fn assert_selected_hermes_agent_image(image: &str) {
    assert_eq!(
        image, "nousresearch/hermes-agent:v2026.5.29.2",
        "Hermes Agent image must use the selected release tag"
    );
    assert!(
        !image.contains(":latest"),
        "Hermes Agent image must not track latest"
    );
    assert!(
        !image.contains("@sha256:"),
        "Hermes Agent image should use tag-only selection"
    );
}

#[test]
fn backend_image_uses_modern_docker_cli_for_host_daemon_compatibility() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let dockerfile = std::fs::read_to_string(repo_root.join("docker/backend.Dockerfile"))
        .expect("backend Dockerfile is present");

    // backend 直接连接宿主机 Docker socket，客户端版本必须跟得上新 daemon。
    assert!(dockerfile.contains("FROM docker:29.1.3-cli AS docker-cli"));
    assert!(
        dockerfile.contains("COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker")
    );
    assert!(!dockerfile.contains("ca-certificates curl docker.io"));
}

#[test]
fn hermes_wrapper_image_uses_selected_official_hermes_agent_tag() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let dockerfile = std::fs::read_to_string(repo_root.join("docker/hermes/Dockerfile"))
        .expect("Hermes wrapper Dockerfile is present");
    let patch_path = repo_root.join("docker/hermes/patch_send_message_tool.py");
    let plugin_root = repo_root.join("docker/hermes/plugins/platforms/hermes_hub");
    let plugin_yaml = std::fs::read_to_string(plugin_root.join("plugin.yaml"))
        .expect("Hermes Hub bundled platform plugin manifest is present");
    let compose = std::fs::read_to_string(repo_root.join("deploy/compose.yml"))
        .expect("compose file is present");

    let hermes_agent_image = hermes_agent_image_from_dockerfile(&dockerfile);
    assert_selected_hermes_agent_image(hermes_agent_image);
    assert!(dockerfile.contains("ARG HERMES_AGENT_IMAGE="));
    assert!(dockerfile.contains("FROM ${HERMES_AGENT_IMAGE}"));
    assert!(!dockerfile.contains("FROM nousresearch/hermes-agent:${HERMES_VERSION}"));
    assert!(dockerfile.contains("COPY docker/hermes/hermes-hub-entrypoint.sh"));
    assert!(dockerfile.contains("COPY docker/hermes/patch_send_message_tool.py"));
    assert!(dockerfile.contains("COPY docker/hermes/plugins /opt/hermes/plugins"));
    assert!(dockerfile.contains("RUN python3 /opt/hermes-hub/patch_send_message_tool.py"));
    assert!(dockerfile.contains("ENTRYPOINT [\"/opt/hermes-hub/entrypoint.sh\"]"));
    assert!(plugin_yaml.contains("kind: platform"));
    assert!(plugin_yaml.contains("label: Hermes Hub"));
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
    assert!(compose.contains(
        "HERMES_DOCKER_IMAGE: ${HERMES_DOCKER_IMAGE:-ghcr.io/yiiilin/hermes-hub-hermes:latest}"
    ));
    assert!(!compose.contains("HERMES_AGENT_IMAGE:"));
    assert!(!compose.contains(hermes_agent_image));
    assert!(!compose.contains("HERMES_VERSION: ${HERMES_VERSION:-latest}"));
}

#[test]
fn hermes_send_message_patch_applies_to_target_image_when_docker_is_available() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let dockerfile = std::fs::read_to_string(repo_root.join("docker/hermes/Dockerfile"))
        .expect("Hermes wrapper Dockerfile is present");
    let pinned_hermes_agent_image = hermes_agent_image_from_dockerfile(&dockerfile);
    let patch_path = repo_root
        .join("docker/hermes/patch_send_message_tool.py")
        .canonicalize()
        .expect("send_message MEDIA bridge patch is present");
    let image = std::env::var("HERMES_HUB_HERMES_TEST_IMAGE")
        .unwrap_or_else(|_| pinned_hermes_agent_image.to_string());

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

    let mount = format!(
        "{}:/tmp/patch_send_message_tool.py:ro",
        patch_path.display()
    );
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
fn hermes_hub_adapter_extracts_unquoted_arbitrary_media_when_docker_is_available() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let dockerfile = std::fs::read_to_string(repo_root.join("docker/hermes/Dockerfile"))
        .expect("Hermes wrapper Dockerfile is present");
    let pinned_hermes_agent_image = hermes_agent_image_from_dockerfile(&dockerfile);
    let adapter_path = repo_root
        .join("docker/hermes/plugins/platforms/hermes_hub/adapter.py")
        .canonicalize()
        .expect("Hermes Hub adapter is present");
    let image = std::env::var("HERMES_HUB_HERMES_TEST_IMAGE")
        .unwrap_or_else(|_| pinned_hermes_agent_image.to_string());

    let docker_available = Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !docker_available {
        eprintln!(
            "skipping Hermes Hub adapter behavior verification because Docker is unavailable"
        );
        return;
    }

    let image_available = Command::new("docker")
        .args(["image", "inspect", &image])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !image_available {
        eprintln!(
            "skipping Hermes Hub adapter behavior verification because {image} is unavailable"
        );
        return;
    }

    let mount = format!("{}:/tmp/hermes_hub_adapter.py:ro", adapter_path.display());
    let script = r#"
PYTHONPATH=/opt/hermes /opt/hermes/.venv/bin/python - <<'PY'
import importlib.util
from gateway.platforms.base import BasePlatformAdapter

spec = importlib.util.spec_from_file_location("hermes_hub_adapter", "/tmp/hermes_hub_adapter.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

content = "caption\nMEDIA:/workspace/start_ntp.sh"
base_media, base_cleaned = BasePlatformAdapter.extract_media(content)
assert base_media == [], (base_media, base_cleaned)
assert base_cleaned == content, (base_media, base_cleaned)

media, cleaned = module.HermesHubAdapter.extract_media(content)
assert media == [("/workspace/start_ntp.sh", False)], (media, cleaned)
assert cleaned == "caption", (media, cleaned)

voice_media, voice_cleaned = module.HermesHubAdapter.extract_media(
    "[[audio_as_voice]]\nMEDIA:/workspace/clip.custom"
)
assert voice_media == [("/workspace/clip.custom", True)], (voice_media, voice_cleaned)
assert voice_cleaned == "", (voice_media, voice_cleaned)

paths, caption = module.HermesHubAdapter._extract_hub_media_directives(
    "x\nMEDIA:'/workspace/a.sh'\nMEDIA:`/workspace/b.noext`"
)
assert paths == ["/workspace/a.sh", "/workspace/b.noext"], (paths, caption)
assert caption == "x", (paths, caption)
PY
"#;
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--entrypoint",
            "sh",
            "-v",
            &mount,
            &image,
            "-lc",
            script,
        ])
        .output()
        .expect("docker can run Hermes Hub adapter behavior verification");
    assert!(
        output.status.success(),
        "Hermes Hub adapter must extract arbitrary MEDIA lines on {image}: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hermes_wrapper_exposes_standard_hub_adapter_surface() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let adapter_path = repo_root.join("docker/hermes/plugins/platforms/hermes_hub/adapter.py");
    let adapter =
        std::fs::read_to_string(&adapter_path).expect("Hermes Hub adapter source is present");
    let provisioner =
        std::fs::read_to_string(repo_root.join("backend/src/hermes/docker_provisioner.rs"))
            .expect("Docker provisioner source is present");

    assert!(adapter.contains("async def send_document("));
    assert!(adapter.contains("async def send_image_file("));
    assert!(adapter.contains("def extract_media(content: str):"));
    assert!(adapter.contains("HUB_MEDIA_DIRECTIVE_RE"));
    assert!(adapter.contains("BasePlatformAdapter.extract_media(content)"));
    assert!(adapter.contains("async def _send_media_directives("));
    assert!(adapter.contains("def _media_directives_from_content("));
    assert!(adapter.contains("def _extract_hub_media_directives("));
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
    assert!(!adapter.contains("outside allowed media directories"));
    assert!(adapter.contains("return await super().send_clarify("));
    assert!(adapter.contains("item_metadata[\"media_sequence\"] = index"));
    assert!(adapter.contains("media_metadata.setdefault(\"media_source_url\", raw_url)"));
    assert!(adapter.contains("def _image_extension_from_file("));
    assert!(adapter.contains("os.replace(cached_path, renamed_path)"));
    assert!(adapter.contains(
        "explicit_client_message_key = str(metadata.get(\"client_message_key\") or \"\")"
    ));
    assert!(adapter.contains("if explicit_client_message_key and media_sequence is None:"));
    assert!(adapter.contains("\"remote\""));
    assert!(adapter.contains("\"local\""));
    assert!(!adapter.contains("platform_hint="));
    assert!(!adapter.contains("attachments=['/workspace/report.pdf']"));
    assert!(!adapter.contains("send_message_with_attachments"));
    assert!(!adapter.contains("platform_name == \"origin\""));
    assert!(!adapter.contains("async def delete_message("));
    assert!(!adapter.contains("async def play_tts("));
    assert!(!adapter.contains("async def send_private_notice("));
    assert!(!adapter.contains("async def create_handoff_thread("));
    assert!(!adapter.contains("async def send_draft("));
    assert!(!adapter.contains("def supports_draft_streaming("));
    assert!(adapter.contains("def register(ctx: Any) -> None:"));
    assert!(adapter.contains("name=\"hermes_hub\""));
    assert!(provisioner
        .contains("remove_path_if_exists(&config_path.join(\"plugins/platforms/hermes_hub\"))"));
    assert!(!provisioner.contains("HERMES_HUB_ADAPTER_PY"));

    let compile_output = Command::new("python3")
        .args([
            "-m",
            "py_compile",
            adapter_path.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("python3 can compile Hermes Hub adapter");
    assert!(
        compile_output.status.success(),
        "Hermes Hub adapter must compile: {}",
        String::from_utf8_lossy(&compile_output.stderr)
    );
}

#[test]
fn hermes_wrapper_entrypoint_links_managed_profile_from_nfs() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let entrypoint =
        std::fs::read_to_string(repo_root.join("docker/hermes/hermes-hub-entrypoint.sh"))
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
