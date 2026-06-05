#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
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
    let dockerfile = std::fs::read_to_string(repo_root.join("docker/backend/Dockerfile"))
        .expect("backend Dockerfile is present");

    // backend 直接连接宿主机 Docker socket，客户端版本必须跟得上新 daemon。
    assert!(dockerfile.contains("FROM docker:29.1.3-cli AS docker-cli"));
    assert!(
        dockerfile.contains("COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker")
    );
    assert!(!dockerfile.contains("ca-certificates curl docker.io"));
}

#[test]
fn release_workflow_keeps_hub_image_on_numeric_release_tags() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let workflow = std::fs::read_to_string(repo_root.join(".github/workflows/release.yml"))
        .expect("release workflow is present");
    let hub_metadata = workflow
        .split("      - name: Hermes wrapper metadata")
        .next()
        .expect("Hub metadata block is present");
    let hub_build_step = workflow
        .split("      - name: Build and push Hub image")
        .nth(1)
        .expect("Hub build step is present")
        .split("      - name: Build and push Hermes wrapper image")
        .next()
        .expect("Hub build step terminator is present");

    // Hub 主镜像继续走发布 tag / semver，只有 Hermes wrapper 和 ASR 使用日期 tag。
    assert!(hub_metadata.contains("name: Docker metadata"));
    assert!(hub_metadata.contains("type=ref,event=tag"));
    assert!(hub_metadata.contains("type=semver,pattern={{version}}"));
    assert!(hub_metadata.contains("type=semver,pattern={{major}}.{{minor}}"));
    assert!(hub_metadata.contains("type=raw,value=latest"));
    assert!(!hub_metadata.contains("type=raw,value=${{ steps.runtime_tag.outputs.tag }}"));
    assert!(hub_build_step.contains("tags: ${{ steps.meta.outputs.tags }}"));
    assert!(hub_build_step.contains("labels: ${{ steps.meta.outputs.labels }}"));
    assert!(!hub_build_step.contains("steps.runtime_tag.outputs.tag"));
}

#[test]
fn release_workflow_uses_daily_runtime_release_count() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let workflow = std::fs::read_to_string(repo_root.join(".github/workflows/release.yml"))
        .expect("release workflow is present");
    let runtime_tag_step = workflow
        .split("      - name: Runtime image date tag")
        .nth(1)
        .expect("runtime tag step is present")
        .split("      - name: Detect image changes")
        .next()
        .expect("runtime tag step terminator is present");

    // 运行时镜像 tag 的尾号是当天第几次发版，不是 GitHub Actions 全局 run number。
    assert!(runtime_tag_step.contains("previous_releases_today"));
    assert!(runtime_tag_step.contains("release_index=$((previous_releases_today + 1))"));
    assert!(runtime_tag_step.contains("tag=\"${date_tag}.${release_index}\""));
    assert!(runtime_tag_step.contains("gh api --paginate"));
    assert!(runtime_tag_step.contains("select(.published_at | startswith(env.DATE_ISO))"));
    assert!(!runtime_tag_step.contains("GITHUB_RUN_NUMBER"));
}

#[test]
fn release_workflow_skips_unchanged_images_by_path_diff() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let workflow = std::fs::read_to_string(repo_root.join(".github/workflows/release.yml"))
        .expect("release workflow is present");

    // 发版镜像必须先按路径 diff 判断，避免未变更的运行时镜像被重复 build/push。
    assert!(workflow.contains("- name: Detect image changes"));
    assert!(workflow.contains("if: steps.image_changes.outputs.hub == 'true'"));
    assert!(workflow.contains("if: steps.image_changes.outputs.hermes == 'true'"));
    assert!(workflow.contains("if: steps.image_changes.outputs.asr == 'true'"));

    // Hub Dockerfile 放在独立目录后，diff 规则可以跟其他镜像一样按目录维护。
    assert!(workflow.contains("file: docker/backend/Dockerfile"));
    assert!(workflow.contains("docker/backend/*"));
    assert!(!workflow.contains("docker/backend.Dockerfile"));

    // Hub 只看会影响最终镜像的源码/构建输入；backend 测试变更不应触发发布镜像。
    assert!(workflow.contains("backend/src/*"));
    assert!(workflow.contains("backend/migrations/*"));
    assert!(!workflow.contains("backend/*|frontend/*"));

    assert!(workflow.contains("docker/hermes/*"));
    assert!(workflow.contains("deploy/asr/sherpa/*"));
    assert!(workflow.contains("Skipped: no Hub image input changed"));
    assert!(workflow.contains("Skipped: no Hermes wrapper image input changed"));
    assert!(workflow.contains("Skipped: no ASR image input changed"));
}

#[test]
fn release_automation_uses_script_and_annotated_tag_notes() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let script_path = repo_root.join("scripts/release.sh");
    let script = std::fs::read_to_string(&script_path).expect("release script is present");
    let workflow = std::fs::read_to_string(repo_root.join(".github/workflows/release.yml"))
        .expect("release workflow is present");

    // 本地脚本负责把“版本号 + 发版内容”转成完整发版动作。
    assert!(script.contains("npm version"));
    assert!(script.contains("cargo metadata --format-version 1 --no-deps"));
    assert!(script.contains("cargo test -p hermes-hub-backend"));
    assert!(script.contains("npm test"));
    assert!(script.contains("npm run build"));
    assert!(script.contains("git tag -a"));
    assert!(script.contains("gh run watch"));
    assert!(script.contains("--notes-file"));

    #[cfg(unix)]
    assert_ne!(
        std::fs::metadata(&script_path)
            .expect("release script metadata is readable")
            .permissions()
            .mode()
            & 0o111,
        0,
        "release script must be executable"
    );

    // GitHub Release notes 读取 annotated tag message，避免发版内容只停留在本地。
    assert!(workflow.contains("git cat-file -t \"${RELEASE_TAG}\""));
    assert!(workflow.contains("git for-each-ref \"refs/tags/${RELEASE_TAG}\""));
    assert!(workflow.contains("## Release notes"));
}

#[test]
fn hermes_wrapper_image_uses_selected_official_hermes_agent_tag() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let dockerfile = std::fs::read_to_string(repo_root.join("docker/hermes/Dockerfile"))
        .expect("Hermes wrapper Dockerfile is present");
    let platform_plugin_root = repo_root.join("docker/hermes/plugins/platforms/hermes_hub");
    let platform_plugin_yaml = std::fs::read_to_string(platform_plugin_root.join("plugin.yaml"))
        .expect("Hermes Hub bundled platform plugin manifest is present");
    let send_plugin_root = repo_root.join("docker/hermes/plugins/hermes_hub_send");
    let send_plugin_yaml = std::fs::read_to_string(send_plugin_root.join("plugin.yaml"))
        .expect("Hermes Hub send backend plugin manifest is present");
    let send_plugin_source = std::fs::read_to_string(send_plugin_root.join("__init__.py"))
        .expect("Hermes Hub send backend plugin source is present");
    let compose = std::fs::read_to_string(repo_root.join("deploy/compose.yml"))
        .expect("compose file is present");

    let hermes_agent_image = hermes_agent_image_from_dockerfile(&dockerfile);
    assert_selected_hermes_agent_image(hermes_agent_image);
    assert!(dockerfile.contains("ARG HERMES_AGENT_IMAGE="));
    assert!(dockerfile.contains("FROM ${HERMES_AGENT_IMAGE}"));
    assert!(!dockerfile.contains("FROM nousresearch/hermes-agent:${HERMES_VERSION}"));
    assert!(dockerfile.contains("COPY docker/hermes/hermes-hub-entrypoint.sh"));
    assert!(dockerfile.contains("COPY docker/hermes/plugins /opt/hermes/plugins"));
    assert!(!dockerfile.contains("patch_send_message_tool.py"));
    assert!(!dockerfile.contains("send_message_tool.py"));
    assert!(dockerfile.contains("ENTRYPOINT [\"/opt/hermes-hub/entrypoint.sh\"]"));
    assert!(platform_plugin_yaml.contains("kind: platform"));
    assert!(platform_plugin_yaml.contains("label: Hermes Hub"));
    assert!(send_plugin_yaml.contains("kind: backend"));
    assert!(send_plugin_yaml.contains("provides_tools:"));
    assert!(send_plugin_yaml.contains("hermes_hub_send"));
    assert!(send_plugin_source.contains("name=\"hermes_hub_send\""));
    assert!(send_plugin_source.contains("toolset=\"hermes_hub\""));
    assert!(send_plugin_source.contains("BasePlatformAdapter.extract_media(official_line)"));
    assert!(
        send_plugin_source.contains("BasePlatformAdapter.filter_media_delivery_paths(media_files)")
    );
    assert!(send_plugin_source.contains("HERMES_SESSION_THREAD_ID"));
    assert!(send_plugin_source.contains("HERMES_SESSION_CHAT_ID"));
    assert!(!send_plugin_source.contains("attachments=['/workspace/report.pdf']"));
    assert!(!send_plugin_source.contains("send_message_with_attachments"));
    let compile_output = Command::new("python3")
        .args([
            "-m",
            "py_compile",
            send_plugin_root
                .join("__init__.py")
                .to_str()
                .expect("utf-8 path"),
        ])
        .output()
        .expect("python3 can compile Hermes Hub send plugin");
    assert!(
        compile_output.status.success(),
        "Hermes Hub send plugin must compile: {}",
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
fn asr_runtime_uses_streaming_model_with_pinned_artifacts() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let dockerfile = std::fs::read_to_string(repo_root.join("deploy/asr/sherpa/Dockerfile"))
        .expect("ASR Dockerfile is present");
    let compose = std::fs::read_to_string(repo_root.join("deploy/compose.yml"))
        .expect("compose file is present");
    let env_example = std::fs::read_to_string(repo_root.join("deploy/.env.example"))
        .expect("env example is present");

    // ASR 运行时必须保持流式模型和固定校验，避免回退到旧 SenseVoice multipart 流程。
    let model_sha = "5462a1fce42693deae572af1e8c4687124b12aa85fe61ff4d3168bb5280e205f";
    assert!(compose.contains("ghcr.io/yiiilin/hermes-hub-asr:v2026.6.4.21"));
    assert!(!compose.contains("ghcr.io/yiiilin/hermes-hub-asr:latest"));
    assert!(compose.contains(&format!(
        "MODEL_SHA256: ${{HERMES_HUB_ASR_MODEL_SHA256:-{model_sha}}}"
    )));
    assert!(dockerfile.contains(&format!("MODEL_SHA256={model_sha}")));
    assert!(env_example
        .contains("HERMES_HUB_ASR_MODEL=sherpa-onnx-streaming-paraformer-bilingual-zh-en"));
    assert!(env_example.contains("HERMES_HUB_ASR_MODEL_FILE=encoder.int8.onnx"));
    assert!(env_example.contains("HERMES_HUB_ASR_DECODER_FILE=decoder.int8.onnx"));
    assert!(!env_example.contains("HERMES_HUB_ASR_TRANSCRIBE_PATH"));
    assert!(!env_example.contains("HERMES_HUB_ASR_MAX_UPLOAD_BYTES"));
    assert!(!env_example.contains("sensevoice"));
    assert!(!env_example.contains("SenseVoice"));
}

#[test]
fn hermes_hub_send_plugin_uses_official_media_policy_when_docker_is_available() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let dockerfile = std::fs::read_to_string(repo_root.join("docker/hermes/Dockerfile"))
        .expect("Hermes wrapper Dockerfile is present");
    let pinned_hermes_agent_image = hermes_agent_image_from_dockerfile(&dockerfile);
    let plugin_path = repo_root
        .join("docker/hermes/plugins/hermes_hub_send/__init__.py")
        .canonicalize()
        .expect("Hermes Hub send plugin source is present");
    let image = std::env::var("HERMES_HUB_HERMES_TEST_IMAGE")
        .unwrap_or_else(|_| pinned_hermes_agent_image.to_string());

    let docker_available = Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !docker_available {
        eprintln!("skipping Hermes Hub send plugin verification because Docker is unavailable");
        return;
    }

    let image_available = Command::new("docker")
        .args(["image", "inspect", &image])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !image_available {
        eprintln!("skipping Hermes Hub send plugin verification because {image} is unavailable");
        return;
    }

    let mount = format!("{}:/tmp/hermes_hub_send.py:ro", plugin_path.display());
    let script = r##"
PYTHONPATH=/opt/hermes HERMES_MEDIA_ALLOW_DIRS=/ /opt/hermes/.venv/bin/python - <<'PY'
import importlib.util
from pathlib import Path
from gateway.platforms.base import BasePlatformAdapter

Path("/tmp/start_ntp.sh").write_text("#!/bin/sh\ntrue\n", encoding="utf-8")
Path("/tmp/report.txt").write_text("report\n", encoding="utf-8")

spec = importlib.util.spec_from_file_location("hermes_hub_send", "/tmp/hermes_hub_send.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

media, cleaned = BasePlatformAdapter.extract_media("caption\nMEDIA:/tmp/start_ntp.sh")
assert media == [], (media, cleaned)
assert cleaned == "caption\nMEDIA:/tmp/start_ntp.sh", (media, cleaned)

tool_media, tool_cleaned = module._extract_media_with_hub_fallback(
    "[[as_document]]\nMEDIA:'/tmp/start_ntp.sh'"
)
assert tool_media == [("/tmp/start_ntp.sh", False)], (tool_media, tool_cleaned)
assert tool_cleaned == "", (tool_media, tool_cleaned)
assert BasePlatformAdapter.filter_media_delivery_paths(tool_media) == [("/tmp/start_ntp.sh", False)]

assert module.HERMES_HUB_SEND_SCHEMA["name"] == "hermes_hub_send"
assert "MEDIA:<local_path>" in module.HERMES_HUB_SEND_SCHEMA["description"]
assert module._extract_media_with_hub_fallback("caption\nMEDIA:/tmp/start_ntp.sh") == (
    [("/tmp/start_ntp.sh", False)],
    "caption",
)
assert module._extract_media_with_hub_fallback("caption\\nMEDIA:/tmp/start_ntp.sh") == (
    [("/tmp/start_ntp.sh", False)],
    "caption",
)
assert module._extract_media_with_hub_fallback(
    "caption\nMEDIA:/tmp/start_ntp.sh\nMEDIA:/tmp/report.txt"
) == (
    [("/tmp/start_ntp.sh", False), ("/tmp/report.txt", False)],
    "caption",
)
assert module._extract_media_with_hub_fallback(
    "    indented line\n\nMEDIA:/tmp/start_ntp.sh\n\n  second"
) == (
    [("/tmp/start_ntp.sh", False)],
    "indented line\n\n  second",
)
assert module._extract_media_with_hub_fallback(
    "    indented line\n\nMEDIA:/tmp/report.txt\n\n  second"
) == (
    [("/tmp/report.txt", False)],
    "indented line\n\n  second",
)
missing_media, missing_cleaned = module._extract_media_with_hub_fallback("MEDIA:/tmp/missing.sh")
assert missing_media == [("/tmp/missing.sh", False)], (missing_media, missing_cleaned)
assert BasePlatformAdapter.filter_media_delivery_paths(missing_media) == []
assert module._message_chunks(type("A", (), {"MAX_MESSAGE_LENGTH": 8000})(), "ok") == ["ok"]
PY
"##;
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
        .expect("docker can run Hermes Hub send plugin verification");
    assert!(
        output.status.success(),
        "Hermes Hub send plugin must use official media parsing on {image}: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hermes_hub_send_plugin_routes_sh_media_without_docker() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let plugin_path = repo_root
        .join("docker/hermes/plugins/hermes_hub_send/__init__.py")
        .canonicalize()
        .expect("Hermes Hub send plugin source is present");
    let script = r##"
import asyncio
import importlib.util
import json
import sys
import types

plugin_path = sys.argv[1]

gateway = types.ModuleType("gateway")
platforms = types.ModuleType("gateway.platforms")
base = types.ModuleType("gateway.platforms.base")

class BasePlatformAdapter:
    @staticmethod
    def extract_media(message):
        # 模拟官方 extract_media：txt 能解析，sh 会漏掉，后者必须靠 Hub fallback。
        if "MEDIA:/tmp/report.txt" in message:
            return [("/tmp/report.txt", False)], message.replace("MEDIA:/tmp/report.txt", "").strip()
        return [], message.replace("[[as_document]]", "").replace("[[audio_as_voice]]", "").strip()

    @staticmethod
    def filter_media_delivery_paths(media_files):
        return [item for item in media_files if item[0] != "/tmp/missing.sh"]

    @staticmethod
    def truncate_message(message, max_length):
        return [message]

base.BasePlatformAdapter = BasePlatformAdapter
session_context = types.ModuleType("gateway.session_context")
session_values = {
    "HERMES_SESSION_PLATFORM": "hermes_hub",
    "HERMES_SESSION_THREAD_ID": "session-1",
    "HERMES_SESSION_CHAT_ID": "session-1",
}
session_context.get_session_env = lambda name, default="": session_values.get(name, default)

config = types.ModuleType("gateway.config")
class Platform(str):
    def __new__(cls, value):
        return str.__new__(cls, value)
config.Platform = Platform

run = types.ModuleType("gateway.run")

class Result:
    def __init__(self, success=True, message_id="message-1", error=""):
        self.success = success
        self.message_id = message_id
        self.error = error

class Adapter:
    MAX_MESSAGE_LENGTH = 8000
    def __init__(self):
        self.documents = []
        self.messages = []
        self._active_run_ids_by_session = {"session-1": "run-1"}

    async def send(self, chat_id, content, metadata=None):
        self.messages.append((chat_id, content, metadata))
        return Result(message_id="text-1")

    async def send_document(self, chat_id, file_path, caption=None, metadata=None):
        self.documents.append((chat_id, file_path, caption, metadata))
        return Result(message_id="doc-1")

adapter = Adapter()
runner = types.SimpleNamespace(adapters={Platform("hermes_hub"): adapter})
run._gateway_runner_ref = lambda: runner

model_tools = types.ModuleType("model_tools")
model_tools._run_async = lambda coro: asyncio.run(coro)

sys.modules.update({
    "gateway": gateway,
    "gateway.platforms": platforms,
    "gateway.platforms.base": base,
    "gateway.session_context": session_context,
    "gateway.config": config,
    "gateway.run": run,
    "model_tools": model_tools,
})

spec = importlib.util.spec_from_file_location("hermes_hub_send", plugin_path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

result = json.loads(module.hermes_hub_send_tool({"message": "caption\\nMEDIA:/tmp/start_ntp.sh"}))
assert result == {"success": True, "message_id": "doc-1"}, result
assert adapter.documents == [
    ("session-1", "/tmp/start_ntp.sh", "caption", {
        "channel_id": "session-1",
        "thread_id": "session-1",
        "run_id": "run-1",
        "media_sequence": 0,
    })
], adapter.documents
adapter.documents.clear()

result = json.loads(module.hermes_hub_send_tool({
    "message": "caption\nMEDIA:/tmp/start_ntp.sh\nMEDIA:/tmp/report.txt"
}))
assert result == {"success": True, "message_id": "doc-1"}, result
assert adapter.documents == [
    ("session-1", "/tmp/start_ntp.sh", "caption", {
        "channel_id": "session-1",
        "thread_id": "session-1",
        "run_id": "run-1",
        "media_sequence": 0,
    }),
    ("session-1", "/tmp/report.txt", None, {
        "channel_id": "session-1",
        "thread_id": "session-1",
        "run_id": "run-1",
        "media_sequence": 1,
    }),
], adapter.documents

missing = json.loads(module.hermes_hub_send_tool({"message": "MEDIA:/tmp/missing.sh"}))
assert missing == {"error": "No deliverable text or media remained after processing MEDIA tags"}, missing
"##;
    let output = Command::new("python3")
        .args(["-c", script, plugin_path.to_str().expect("utf-8 path")])
        .output()
        .expect("python3 can run Hermes Hub send plugin stub test");
    assert!(
        output.status.success(),
        "Hermes Hub send plugin must route .sh MEDIA with local stubs: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hermes_hub_adapter_uses_official_media_extraction_when_docker_is_available() {
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

content = "caption\nMEDIA:/workspace/report.txt"
base_media, base_cleaned = BasePlatformAdapter.extract_media(content)
assert base_media == [("/workspace/report.txt", False)], (base_media, base_cleaned)
assert base_cleaned == "caption", (base_media, base_cleaned)

media, cleaned = module.HermesHubAdapter.extract_media(content)
assert media == [("/workspace/report.txt", False)], (media, cleaned)
assert cleaned == "caption", (media, cleaned)

voice_media, voice_cleaned = module.HermesHubAdapter.extract_media(
    "[[audio_as_voice]]\nMEDIA:/workspace/clip.ogg"
)
assert voice_media == [("/workspace/clip.ogg", True)], (voice_media, voice_cleaned)
assert voice_cleaned == "", (voice_media, voice_cleaned)

custom_media, custom_cleaned = module.HermesHubAdapter.extract_media("MEDIA:/workspace/b.noext")
assert custom_media == [], (custom_media, custom_cleaned)
assert custom_cleaned == "MEDIA:/workspace/b.noext", (custom_media, custom_cleaned)
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
        "Hermes Hub adapter must inherit official MEDIA extraction on {image}: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hermes_hub_adapter_disables_cron_tick_when_public_platform_env_is_set() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend crate lives under repo root");
    let adapter_path = repo_root
        .join("docker/hermes/plugins/platforms/hermes_hub/adapter.py")
        .canonicalize()
        .expect("Hermes Hub adapter is present");
    let script = r##"
import importlib.util
import os
import sys
import types

adapter_path = sys.argv[1]

gateway = types.ModuleType("gateway")
config = types.ModuleType("gateway.config")
platforms = types.ModuleType("gateway.platforms")
base = types.ModuleType("gateway.platforms.base")

class Platform(str):
    def __new__(cls, value):
        return str.__new__(cls, value)

class BasePlatformAdapter:
    def __init__(self, *args, **kwargs):
        pass

class SendResult:
    def __init__(self, success=True, message_id="", error="", retryable=False):
        self.success = success
        self.message_id = message_id
        self.error = error
        self.retryable = retryable

config.Platform = Platform
base.BasePlatformAdapter = BasePlatformAdapter
base.MessageEvent = object
base.MessageType = types.SimpleNamespace(TEXT="text", IMAGE="image", FILE="file")
base.ProcessingOutcome = types.SimpleNamespace(COMPLETED="completed", FAILED="failed")
base.SendResult = SendResult
base.cache_document_from_bytes = lambda *args, **kwargs: "/tmp/document"
base.cache_image_from_bytes = lambda *args, **kwargs: "/tmp/image"
base.cache_image_from_url = lambda *args, **kwargs: "/tmp/image"

cron = types.ModuleType("cron")
cron_scheduler = types.ModuleType("cron.scheduler")
cron_scheduler.tick = lambda *args, **kwargs: "original"

sys.modules.update({
    "gateway": gateway,
    "gateway.config": config,
    "gateway.platforms": platforms,
    "gateway.platforms.base": base,
    "cron": cron,
    "cron.scheduler": cron_scheduler,
})

spec = importlib.util.spec_from_file_location("hermes_hub_adapter", adapter_path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class Ctx:
    def __init__(self):
        self.platforms = []

    def register_platform(self, **kwargs):
        self.platforms.append(kwargs)

os.environ.pop("HERMES_HUB_DISABLE_CRON", None)
cron_scheduler.tick = lambda *args, **kwargs: "original"
module.register(Ctx())
assert cron_scheduler.tick() == "original"

os.environ["HERMES_HUB_DISABLE_CRON"] = "1"
cron_scheduler.tick = lambda *args, **kwargs: "original"
ctx = Ctx()
module.register(ctx)
assert len(ctx.platforms) == 1
assert ctx.platforms[0]["name"] == "hermes_hub"
assert cron_scheduler.tick("job") is None
"##;
    let output = Command::new("python3")
        .args(["-c", script, adapter_path.to_str().expect("utf-8 path")])
        .output()
        .expect("python3 can run Hermes Hub adapter cron hook stub test");
    assert!(
        output.status.success(),
        "Hermes Hub adapter must disable cron tick when requested: stdout={}, stderr={}",
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
    assert!(!adapter.contains("HUB_MEDIA_DIRECTIVE_RE"));
    assert!(!adapter.contains("BasePlatformAdapter.extract_media(content)"));
    assert!(!adapter.contains("async def _send_media_directives("));
    assert!(!adapter.contains("def _media_directives_from_content("));
    assert!(!adapter.contains("def _extract_hub_media_directives("));
    assert!(adapter.contains("async def send_image("));
    assert!(adapter.contains("async def send_multiple_images("));
    assert!(adapter.contains("async def send_typing("));
    assert!(adapter.contains("async def stop_typing("));
    assert!(adapter.contains("async def send_clarify("));
    assert!(adapter.contains("def _disable_cron_scheduler_if_requested("));
    assert!(adapter.contains("HERMES_HUB_DISABLE_CRON"));
    assert!(adapter.contains("cron_scheduler.tick = _disabled_tick"));
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
