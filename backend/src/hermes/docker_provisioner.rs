#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::process::Command;

use crate::model_config::RESPONSES_API_TYPE;

use super::{
    instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
    provisioner::{HermesProvisioner, ProvisionerError},
};

/// Hub 托管 Hermes 容器规格版本。只要 env、挂载、工作目录或安全策略有变化，
/// 就提升这个值，确保已存在的旧容器会被重建并拿到新行为。
const MANAGED_CONTAINER_SPEC_VERSION: &str = "2026-05-22-hermes-hub-image-model";
const MANAGED_CONTAINER_SPEC_LABEL: &str = "hermes_hub_spec_version";
const HUB_INBOX_PATH: &str = "/internal/channel/v1/inbox";
const HUB_INBOX_TIMEOUT_SECONDS: u16 = 25;
const HUB_INBOX_LIMIT: u16 = 4;
const HERMES_HUB_PLUGIN_YAML: &str = r#"name: hermes-hub-platform
label: Hermes Hub
kind: platform
version: 1
description: Hermes Hub 长轮询平台适配器。
requires_env:
  - name: HERMES_HUB_CHANNEL_BASE_URL
    description: Hermes Hub internal channel API base URL
  - name: HERMES_HUB_CHANNEL_TOKEN
    description: Hermes Hub instance token
    password: true
optional_env:
  - name: HERMES_HUB_INSTANCE_ID
    description: Hermes Hub managed instance id
  - name: HERMES_HUB_USER_ID
    description: Hermes Hub user id
"#;
const HERMES_HUB_PLUGIN_INIT: &str = r#""""Hermes Hub platform plugin."""

from .adapter import register

__all__ = ["register"]
"#;
const HERMES_HUB_ADAPTER_PY: &str = r#"
from __future__ import annotations

import asyncio
import json
import logging
import mimetypes
import os
from pathlib import Path
from typing import Any, Dict, Optional
from urllib.parse import urlencode

try:
    import aiohttp
except ImportError:  # pragma: no cover - 容器镜像应内置 aiohttp。
    aiohttp = None

from gateway.config import Platform
from gateway.platforms.base import (
    BasePlatformAdapter,
    MessageEvent,
    MessageType,
    ProcessingOutcome,
    SendResult,
    cache_document_from_bytes,
    cache_image_from_bytes,
)


logger = logging.getLogger(__name__)


class HermesHubAdapter(BasePlatformAdapter):
    """通过 Hub 队列长轮询收发消息的 Hermes platform adapter。"""
    MAX_MESSAGE_LENGTH = 8000

    def __init__(self, config, **kwargs: Any):
        del kwargs
        platform = Platform("hermes_hub")
        super().__init__(config=config, platform=platform)
        extra = getattr(config, "extra", {}) or {}
        self.base_url = (
            os.getenv("HERMES_HUB_CHANNEL_BASE_URL")
            or extra.get("base_url")
            or "http://hermes-hub:8080/internal/channel/v1"
        ).rstrip("/")
        self.origin_url = self.base_url.rsplit("/internal/channel/v1", 1)[0]
        self.token = os.getenv("HERMES_HUB_CHANNEL_TOKEN") or extra.get("token") or ""
        self.instance_id = os.getenv("HERMES_HUB_INSTANCE_ID") or extra.get("instance_id") or ""
        self.user_id = os.getenv("HERMES_HUB_USER_ID") or extra.get("user_id") or ""
        self.inbox_path = os.getenv("HERMES_HUB_INBOX_PATH") or extra.get(
            "inbox_path", "/internal/channel/v1/inbox"
        )
        self.timeout_seconds = int(
            os.getenv("HERMES_HUB_INBOX_TIMEOUT_SECONDS")
            or extra.get("timeout_seconds", 25)
        )
        self.limit = int(os.getenv("HERMES_HUB_INBOX_LIMIT") or extra.get("limit", 4))
        self._last_empty_poll_delay = 1.0
        self._session: aiohttp.ClientSession | None = None
        self._poll_task: asyncio.Task | None = None
        self._closed = asyncio.Event()
        # Hermes 会把 thread_id 作为通用路由 metadata 传给所有输出；
        # Hub 只用它追踪最后一条输出消息，不在 send() 阶段结束 run。
        self._last_output_messages: dict[str, str] = {}

    @property
    def name(self) -> str:
        return "Hermes Hub"

    async def connect(self) -> bool:
        if aiohttp is None:
            self._set_fatal_error("missing_dependency", "aiohttp is required", retryable=False)
            return False
        if not self.token:
            self._set_fatal_error(
                "config_missing",
                "HERMES_HUB_CHANNEL_TOKEN is required",
                retryable=False,
            )
            return False
        if self._session is None:
            timeout = aiohttp.ClientTimeout(total=self.timeout_seconds + 15)
            self._session = aiohttp.ClientSession(
                timeout=timeout,
                headers=self._headers(),
                trust_env=True,
            )
        if self._poll_task is None or self._poll_task.done():
            self._closed.clear()
            self._poll_task = asyncio.create_task(self._poll_loop())
        self._mark_connected()
        return True

    async def disconnect(self) -> None:
        self._closed.set()
        if self._poll_task is not None:
            self._poll_task.cancel()
            try:
                await self._poll_task
            except asyncio.CancelledError:
                pass
            self._poll_task = None
        if self._session is not None:
            await self._session.close()
            self._session = None
        self._mark_disconnected()

    async def send(
        self,
        chat_id: str,
        content: str,
        reply_to: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
    ) -> SendResult:
        del reply_to
        session_id = self._session_id(chat_id, metadata)
        payload = {
            "role": "assistant",
            "content": content,
            "attachments": (metadata or {}).get("attachments") or [],
        }
        run_id = self._run_id(metadata)
        if run_id:
            payload["run_id"] = run_id
        client_message_key = self._client_message_key(metadata)
        if client_message_key:
            payload["client_message_key"] = client_message_key
        try:
            response = await self._request_json(
                "POST", f"/sessions/{session_id}/messages", json=payload
            )
            message = response.get("message", response)
            self._remember_output_message(metadata, message)
            return SendResult(
                success=True,
                message_id=str(message.get("id", "")),
                raw_response=message,
            )
        except Exception as error:
            logger.warning("Hermes Hub send failed: %s", error)
            return SendResult(success=False, error=str(error), retryable=True)

    async def edit_message(
        self,
        chat_id: str,
        message_id: str,
        content: str,
        *,
        finalize: bool = False,
        metadata: Optional[Dict[str, Any]] = None,
    ) -> SendResult:
        del finalize
        session_id = self._session_id(chat_id, metadata)
        payload = {
            "content": content,
            "attachments": (metadata or {}).get("attachments") or [],
        }
        run_id = self._run_id(metadata)
        if run_id:
            payload["run_id"] = run_id
        try:
            response = await self._request_json(
                "PUT", f"/sessions/{session_id}/messages/{message_id}", json=payload
            )
            message = response.get("message", response)
            self._remember_output_message(metadata, message)
            return SendResult(
                success=True,
                message_id=str(message.get("id", message_id)),
                raw_response=message,
            )
        except Exception as error:
            logger.warning("Hermes Hub edit failed: %s", error)
            return SendResult(success=False, error=str(error), retryable=True)

    async def send_document(
        self,
        chat_id: str,
        file_path: str,
        caption: Optional[str] = None,
        file_name: Optional[str] = None,
        reply_to: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
        **kwargs: Any,
    ) -> SendResult:
        del reply_to, kwargs
        return await self._upload_and_send(chat_id, file_path, caption, metadata, file_name)

    async def send_image_file(
        self,
        chat_id: str,
        image_path: str,
        caption: Optional[str] = None,
        reply_to: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
        **kwargs: Any,
    ) -> SendResult:
        del reply_to, kwargs
        return await self._upload_and_send(chat_id, image_path, caption, metadata, None)

    async def _poll_loop(self) -> None:
        while not self._closed.is_set():
            try:
                items = await self._fetch_inbox()
                if not items:
                    await self._wait_after_empty_poll()
                    continue
                self._last_empty_poll_delay = 1.0
                for item in items:
                    await self._dispatch_inbox_item(item)
            except asyncio.CancelledError:
                raise
            except Exception as error:
                # Hub 队列是长轮询入口，短暂网络错误不能让 adapter 退出。
                logger.warning("Hermes Hub poll failed: %s", error)
                await asyncio.sleep(2)

    async def _wait_after_empty_poll(self) -> None:
        # Hub 后端也会等待；这里保留退避，防止代理或配置异常时空队列忙轮询。
        await asyncio.sleep(self._last_empty_poll_delay)
        self._last_empty_poll_delay = min(self._last_empty_poll_delay * 2, 5.0)

    async def _fetch_inbox(self) -> list[dict[str, Any]]:
        path = f"/internal/channel/v1/inbox?timeout_seconds=25&limit=4"
        if (
            self.inbox_path != "/internal/channel/v1/inbox"
            or self.timeout_seconds != 25
            or self.limit != 4
        ):
            query = urlencode({"timeout_seconds": self.timeout_seconds, "limit": self.limit})
            path = f"{self.inbox_path}?{query}"
        response = await self._request_json("GET", path)
        if isinstance(response, list):
            return response
        return response.get("messages") or response.get("items") or response.get("inbox") or []

    async def _dispatch_inbox_item(self, item: dict[str, Any]) -> None:
        inbox_id = str(item.get("id") or item.get("message_id") or "")
        run_id = str(item.get("run_id") or inbox_id or "")
        session_id = str(item.get("session_id") or item.get("channel_session_id") or "")
        content = item.get("content") or item.get("text") or item.get("message") or ""
        if session_id and not os.getenv("HERMES_HUB_HOME_CHANNEL"):
            # Hub 托管场景不要求用户手动执行 /sethome；首个会话可作为 Hermes
            # cron/跨平台通知的默认 home channel，同时避免初始化提示污染对话。
            os.environ["HERMES_HUB_HOME_CHANNEL"] = session_id
        run_id = self._normalize_run_id(run_id)
        if run_id:
            # 先把 run 标成 running，再下载用户附件；附件下载较慢时也不会被重复租约消费。
            await self._request_json("POST", f"/runs/{run_id}/status", json={"status": "running"})
        media_urls, media_types, message_type = await self._media_from_attachments(item)
        source = self.build_source(
            chat_id=session_id,
            chat_name=item.get("channel_name") or "Hermes Hub",
            chat_type=item.get("chat_type") or "dm",
            user_id=str(item.get("user_id") or self.user_id or "hub-user"),
            user_name=item.get("user_name") or "Hub user",
            thread_id=run_id,
            message_id=inbox_id or None,
        )
        raw_message = dict(item)
        raw_message["run_id"] = run_id
        event = MessageEvent(
            text=content,
            message_type=message_type,
            source=source,
            raw_message=raw_message,
            message_id=inbox_id,
            media_urls=media_urls,
            media_types=media_types,
        )
        await self.handle_message(event)

    async def on_processing_complete(self, event: MessageEvent, outcome) -> None:
        run_id = self._run_id_from_event(event)
        if not run_id:
            return
        if outcome == ProcessingOutcome.CANCELLED:
            status = "cancelled"
        elif getattr(outcome, "value", str(outcome)) == "success":
            status = "completed"
        else:
            status = "failed"
        try:
            if status == "completed":
                output_message_id = self._last_output_message_id(run_id)
                payload = {"output_message_id": output_message_id} if output_message_id else {}
                await self._request_json("POST", f"/inbox/{run_id}/ack", json=payload)
            else:
                output_message_id = self._last_output_message_id(run_id)
                payload = {"status": status, "error": self._outcome_text(outcome)}
                if output_message_id:
                    payload["output_message_id"] = output_message_id
                await self._request_json(
                    "POST",
                    f"/runs/{run_id}/status",
                    json=payload,
                )
        except Exception as error:
            logger.warning("Hermes Hub run completion callback failed: %s", error)

    async def _upload_and_send(
        self,
        chat_id: str,
        file_path: str,
        caption: Optional[str],
        metadata: Optional[Dict[str, Any]],
        file_name: Optional[str],
    ) -> SendResult:
        session_id = self._session_id(chat_id, metadata)
        try:
            attachment = await self._upload_attachment(session_id, file_path, file_name)
            content = caption or ""
            download_url = attachment.get("download_url")
            if download_url:
                display_name = attachment.get("name") or Path(file_path).name
                content = f"{content}\n\n[{display_name}]({download_url})".strip()
            next_metadata = dict(metadata or {})
            next_metadata["attachments"] = [attachment]
            run_id = self._run_id(metadata)
            attachment_id = attachment.get("id")
            if run_id and attachment_id:
                next_metadata["client_message_key"] = f"hermes-run:{run_id}:attachment:{attachment_id}"
            return await self.send(chat_id, content, metadata=next_metadata)
        except Exception as error:
            logger.warning("Hermes Hub attachment send failed: %s", error)
            return SendResult(success=False, error=str(error), retryable=True)

    async def _upload_attachment(
        self, session_id: str, file_path: str, file_name: Optional[str] = None
    ) -> dict[str, Any]:
        upload_name = file_name or Path(file_path).name
        content_type = mimetypes.guess_type(upload_name)[0] or "application/octet-stream"
        data = aiohttp.FormData()
        # 附件只由 adapter 主动上传到 Hub，Hub 不读取容器挂载路径。
        with open(file_path, "rb") as handle:
            data.add_field("file", handle, filename=upload_name, content_type=content_type)
            response = await self._request_json(
                "POST", f"/sessions/{session_id}/attachments", data=data
            )
        attachments = response.get("attachments") or []
        return attachments[0] if attachments else {}

    async def _media_from_attachments(
        self, item: dict[str, Any]
    ) -> tuple[list[str], list[str], MessageType]:
        media_urls: list[str] = []
        media_types: list[str] = []
        for raw in item.get("attachments") or []:
            attachment_id = raw.get("attachment_id") or raw.get("id")
            if not attachment_id:
                continue
            try:
                payload, content_type = await self._download_attachment(str(attachment_id))
                name = raw.get("name") or raw.get("filename") or f"{attachment_id}.bin"
                if (content_type or "").startswith("image/"):
                    ext = Path(name).suffix or mimetypes.guess_extension(content_type) or ".jpg"
                    media_urls.append(cache_image_from_bytes(payload, ext=ext))
                    media_types.append(content_type)
                else:
                    media_urls.append(cache_document_from_bytes(payload, name))
                    media_types.append(content_type)
            except Exception as error:
                logger.warning("Hermes Hub inbound attachment download failed: %s", error)
        if media_urls:
            if all((media_type or "").startswith("image/") for media_type in media_types):
                return media_urls, media_types, MessageType.PHOTO
            return media_urls, media_types, MessageType.DOCUMENT
        return media_urls, media_types, MessageType.TEXT

    async def _download_attachment(self, attachment_id: str) -> tuple[bytes, str]:
        session = await self._ensure_session()
        url = self._url(f"/attachments/{attachment_id}/download")
        async with session.get(url) as response:
            payload = await response.read()
            if response.status >= 400:
                raise RuntimeError(f"Hub attachment download failed {response.status}")
            return payload, response.headers.get("content-type", "application/octet-stream")

    async def _request_json(self, method: str, path: str, **kwargs: Any) -> Any:
        session = await self._ensure_session()
        url = self._url(path)
        async with session.request(method, url, **kwargs) as response:
            text = await response.text()
            if response.status >= 400:
                raise RuntimeError(f"Hub channel request failed {response.status}: {text}")
            if not text:
                return {}
            try:
                return json.loads(text)
            except json.JSONDecodeError:
                return {"text": text}

    async def _ensure_session(self) -> aiohttp.ClientSession:
        if self._session is None:
            connected = await self.connect()
            if not connected:
                raise RuntimeError("Hermes Hub platform is not connected")
        return self._session

    def _url(self, path: str) -> str:
        if path.startswith("http://") or path.startswith("https://"):
            return path
        if path.startswith("/internal/channel/v1/"):
            return f"{self.origin_url}{path}"
        return f"{self.base_url}/{path.lstrip('/')}"

    def _headers(self) -> dict[str, str]:
        headers = {"User-Agent": "hermes-hub-platform/1"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        if self.instance_id:
            headers["X-Hermes-Hub-Instance-Id"] = self.instance_id
        return headers

    def _session_id(self, chat_id: str, metadata: Optional[Dict[str, Any]]) -> str:
        metadata = metadata or {}
        session_id = metadata.get("session_id") or metadata.get("channel_id") or chat_id
        if not session_id:
            raise ValueError("Hermes Hub send requires a session id target")
        return str(session_id)

    def _run_id(self, metadata: Optional[Dict[str, Any]]) -> str:
        metadata = metadata or {}
        run_id = metadata.get("run_id") or metadata.get("thread_id") or metadata.get("message_id") or ""
        return self._normalize_run_id(run_id)

    def _run_id_from_event(self, event: MessageEvent) -> str:
        raw = getattr(event, "raw_message", {}) or {}
        if isinstance(raw, dict) and raw.get("run_id"):
            return self._normalize_run_id(raw["run_id"])
        source = getattr(event, "source", None)
        thread_id = getattr(source, "thread_id", "") if source else ""
        normalized_thread_id = self._normalize_run_id(thread_id)
        if normalized_thread_id:
            return normalized_thread_id
        message_id = getattr(event, "message_id", "") or ""
        return self._normalize_run_id(message_id)

    def _normalize_run_id(self, run_id: Any) -> str:
        value = str(run_id or "").strip()
        if value.startswith("hub-run-"):
            return value
        if len(value) == 36 and value.count("-") == 4:
            return f"hub-run-{value}"
        return ""

    def _remember_output_message(self, metadata: Optional[Dict[str, Any]], message: dict[str, Any]) -> None:
        run_id = self._run_id(metadata)
        message_id = str(message.get("id") or "")
        if not run_id or not message_id:
            return
        self._last_output_messages[run_id] = message_id

    def _last_output_message_id(self, run_id: str) -> str:
        return self._last_output_messages.get(run_id, "")

    def _client_message_key(self, metadata: Optional[Dict[str, Any]]) -> str:
        metadata = metadata or {}
        if metadata.get("client_message_key"):
            return str(metadata["client_message_key"])
        run_id = self._run_id(metadata)
        if not run_id:
            return ""
        attachments = metadata.get("attachments") or []
        # Hub 的最终回答和文件/图片输出必须幂等；工具过程消息通常没有附件，
        # 且可能多次追加/编辑，不能和最终回答共用同一个 key。
        if attachments:
            return f"hermes-run:{run_id}"
        if metadata.get("notify"):
            return f"hermes-run:{run_id}"
        return ""

    def _outcome_text(self, outcome) -> str:
        return getattr(outcome, "value", str(outcome))

    async def get_chat_info(self, chat_id: str) -> dict[str, Any]:
        return {"id": chat_id, "name": "Hermes Hub", "type": "dm"}


def _check_requirements() -> bool:
    return aiohttp is not None


def register(ctx: Any) -> None:
    ctx.register_platform(
        name="hermes_hub",
        label="Hermes Hub",
        adapter_factory=lambda cfg: HermesHubAdapter(cfg),
        check_fn=_check_requirements,
        required_env=["HERMES_HUB_CHANNEL_BASE_URL", "HERMES_HUB_CHANNEL_TOKEN"],
        max_message_length=8000,
        emoji="",
        pii_safe=True,
        allow_update_command=True,
        platform_hint=(
            "You are running inside Hermes Hub. Replies are persisted to the "
            "Hub session, and files must be sent as attachments rather than "
            "by referring to container-local paths."
        ),
    )
"#;

/// Docker 托管 Hermes 的运行配置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DockerProvisionerConfig {
    pub image: String,
    pub data_root: PathBuf,
    pub network: String,
    pub internal_port: u16,
    pub connect_mode: HermesContainerConnectMode,
    pub published_host_ip: String,
    pub published_base_url: String,
    pub hub_llm_base_url: String,
    pub default_model: String,
    pub image_model: String,
    pub api_mode: String,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    pub docker_binary: String,
}

/// Hub 连接托管 Hermes 容器的方式。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HermesContainerConnectMode {
    /// backend 与 Hermes 容器位于同一个 Docker 网络，直接使用容器名访问。
    Network,
    /// backend 跑在宿主机时，Hermes 随机发布宿主机端口，Hub 通过该端口访问。
    PublishedHost,
}

impl HermesContainerConnectMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "published-host" | "published_host" | "host" => Self::PublishedHost,
            _ => Self::Network,
        }
    }
}

/// 容器挂载定义。测试和真实 Docker adapter 共用同一份 spec，避免部署行为漂移。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerMount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

/// 可渲染为 Docker create 参数的规范。这里显式保存 published_ports，
/// 用测试保证托管 Hermes 不暴露宿主机端口。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub network: String,
    pub internal_port: u16,
    pub env: Vec<String>,
    pub mounts: Vec<ContainerMount>,
    pub labels: Vec<(String, String)>,
    pub published_ports: Vec<String>,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    pub workdir: Option<String>,
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContainerInspection {
    id: String,
    running: bool,
    spec_version: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DockerRuntimeOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait DockerRuntime: Send + Sync {
    async fn run(&self, args: Vec<String>) -> Result<DockerRuntimeOutput, ProvisionerError>;
}

pub type DynDockerRuntime = Arc<dyn DockerRuntime>;

/// 生产 Docker runtime。它通过 Docker CLI 与本机 Docker daemon 交互，
/// 这样第一版不需要引入更重的 Docker API 客户端，也方便运维复用现有 docker 权限。
#[derive(Clone)]
pub struct CommandDockerRuntime {
    docker_binary: String,
}

impl CommandDockerRuntime {
    pub fn new(docker_binary: String) -> Self {
        Self { docker_binary }
    }
}

#[async_trait]
impl DockerRuntime for CommandDockerRuntime {
    async fn run(&self, args: Vec<String>) -> Result<DockerRuntimeOutput, ProvisionerError> {
        let output = Command::new(&self.docker_binary)
            .args(&args)
            .output()
            .await
            .map_err(|error| ProvisionerError::DockerRuntime(error.to_string()))?;

        Ok(DockerRuntimeOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// 单元测试和内存演示模式使用的 Docker runtime。它不碰真实 Docker daemon，
/// 只返回稳定成功结果；真实启动路径不会使用它。
#[derive(Clone, Default)]
pub struct NoopDockerRuntime;

#[async_trait]
impl DockerRuntime for NoopDockerRuntime {
    async fn run(&self, args: Vec<String>) -> Result<DockerRuntimeOutput, ProvisionerError> {
        let stdout = if args.get(0).map(String::as_str) == Some("container")
            && args.get(1).map(String::as_str) == Some("inspect")
        {
            "noop-container-id".to_string()
        } else if args.first().map(String::as_str) == Some("create") {
            "noop-container-id".to_string()
        } else {
            String::new()
        };

        Ok(DockerRuntimeOutput {
            success: true,
            stdout,
            stderr: String::new(),
        })
    }
}

/// Docker provisioner 会真实创建/启动/停止容器；内存 map 只用于测试和
/// handler 当前进程内快速读取最近一次编排结果，权威状态仍写入数据库。
#[derive(Clone)]
pub struct DockerProvisioner {
    config: DockerProvisionerConfig,
    runtime: DynDockerRuntime,
    instances: Arc<Mutex<HashMap<String, HermesInstance>>>,
}

impl DockerProvisioner {
    pub fn new(config: DockerProvisionerConfig) -> Self {
        let runtime = Arc::new(CommandDockerRuntime::new(config.docker_binary.clone()));
        Self::new_with_runtime(config, runtime)
    }

    pub fn new_with_runtime(config: DockerProvisionerConfig, runtime: DynDockerRuntime) -> Self {
        Self {
            config,
            runtime,
            instances: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn instance(&self, instance_id: &str) -> Option<HermesInstance> {
        self.instances.lock().ok()?.get(instance_id).cloned()
    }

    pub fn prepare_instance(&self, user_id: &str) -> HermesInstance {
        self.build_instance(user_id)
    }

    pub fn container_spec_for(
        &self,
        instance: &HermesInstance,
    ) -> Result<ContainerSpec, ProvisionerError> {
        let workspace = instance
            .host_workspace_path
            .clone()
            .ok_or(ProvisionerError::InvalidManagedInstance)?;
        let sandbox = instance
            .host_sandbox_path
            .clone()
            .ok_or(ProvisionerError::InvalidManagedInstance)?;
        let config = instance
            .host_config_path
            .clone()
            .ok_or(ProvisionerError::InvalidManagedInstance)?;

        Ok(ContainerSpec {
            name: instance.name.clone(),
            image: self.config.image.clone(),
            network: self.config.network.clone(),
            internal_port: self.config.internal_port,
            env: vec![
                "API_SERVER_ENABLED=true".to_string(),
                "API_SERVER_HOST=0.0.0.0".to_string(),
                format!("API_SERVER_PORT={}", self.config.internal_port),
                format!(
                    "API_SERVER_KEY={}",
                    instance.llm_api_key.as_deref().unwrap_or("unissued")
                ),
                "HERMES_HOME=/config".to_string(),
                "HERMES_INFERENCE_PROVIDER=custom".to_string(),
                format!("CUSTOM_BASE_URL={}", self.config.hub_llm_base_url),
                format!("OPENAI_BASE_URL={}", self.config.hub_llm_base_url),
                format!(
                    "OPENAI_API_KEY={}",
                    instance.llm_api_key.as_deref().unwrap_or("unissued")
                ),
                format!(
                    "HERMES_HUB_CHANNEL_BASE_URL={}",
                    hub_channel_base_url(&self.config.hub_llm_base_url)
                ),
                format!(
                    "HERMES_HUB_CHANNEL_TOKEN={}",
                    instance.llm_api_key.as_deref().unwrap_or("unissued")
                ),
                format!("HERMES_HUB_INSTANCE_ID={}", instance.id),
                format!("HERMES_HUB_USER_ID={}", instance.user_id),
                format!("HERMES_HUB_INBOUND_PORT={}", self.config.internal_port),
                format!("HERMES_HUB_INBOX_PATH={HUB_INBOX_PATH}"),
                format!("HERMES_HUB_INBOX_TIMEOUT_SECONDS={HUB_INBOX_TIMEOUT_SECONDS}"),
                format!("HERMES_HUB_INBOX_LIMIT={HUB_INBOX_LIMIT}"),
                format!("OPENAI_MODEL={}", self.config.default_model),
                format!("OPENAI_IMAGE_MODEL={}", self.config.image_model),
                "HERMES_TOOL_PROGRESS_MODE=verbose".to_string(),
                // Hub 托管 Hermes 已经运行在用户独立容器里，命令安全边界由容器承担；
                // 默认自动批准可以避免长任务卡在无人值守的 approval prompt。
                "HERMES_YOLO_MODE=1".to_string(),
                "HERMES_ACCEPT_HOOKS=1".to_string(),
            ],
            mounts: vec![
                ContainerMount {
                    host_path: workspace,
                    container_path: "/workspace".to_string(),
                    read_only: false,
                },
                ContainerMount {
                    host_path: sandbox.clone(),
                    container_path: "/sandbox".to_string(),
                    read_only: false,
                },
                ContainerMount {
                    host_path: sandbox,
                    container_path: "/opt/data".to_string(),
                    read_only: false,
                },
                ContainerMount {
                    host_path: config,
                    container_path: "/config".to_string(),
                    // Hermes gateway 会在 HERMES_HOME 下写入 sessions、logs、skills 等运行态文件。
                    read_only: false,
                },
            ],
            labels: vec![
                ("app".to_string(), "hermes-hub".to_string()),
                ("user_id".to_string(), instance.user_id.clone()),
                ("instance_id".to_string(), instance.id.clone()),
                (
                    MANAGED_CONTAINER_SPEC_LABEL.to_string(),
                    MANAGED_CONTAINER_SPEC_VERSION.to_string(),
                ),
            ],
            published_ports: self.published_ports(),
            memory_limit: self.config.memory_limit.clone(),
            cpu_limit: self.config.cpu_limit.clone(),
            workdir: Some("/workspace".to_string()),
            command: vec!["gateway".to_string()],
        })
    }

    pub async fn ensure_container(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        self.ensure_network().await?;

        let mut next = instance.clone();
        next.llm_api_key = Some(llm_api_key.to_string());
        next.api_token_secret_ref = Some(llm_api_key.to_string());
        self.create_host_directories(&next)?;
        let config_changed = self.write_managed_config(&next)?;

        if let Some(inspection) = self.inspect_container(&next.name).await? {
            if inspection.running
                && !config_changed
                && inspection.spec_version.as_deref() == Some(MANAGED_CONTAINER_SPEC_VERSION)
            {
                if let Some(base_url) = self.resolve_running_base_url(&next.name).await? {
                    next.base_url = base_url;
                    next.container_id = Some(inspection.id);
                    next.status = HermesInstanceStatus::Running;
                    self.remember(next.clone())?;
                    return Ok(next);
                }
            }

            // 旧版本可能创建了交互式 CLI、只读 /config 或未发布端口的容器；
            // 模型配置变化时也需要重建，保证 gateway 读取 Hub 管理的 config.yaml。
            self.remove_container_if_exists(&next.name).await?;
        }

        let container_id = self.create_container(&next).await?;
        self.run_required(vec!["start".to_string(), next.name.clone()])
            .await?;
        next.container_id = Some(container_id);
        next.status = HermesInstanceStatus::Running;
        next.base_url = self
            .running_base_url(&next.name)
            .await?
            .unwrap_or_else(|| self.network_base_url(&next.name));
        self.remember(next.clone())?;

        Ok(next)
    }

    pub async fn ensure_container_with_default_model(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
        default_model: &str,
        image_model: &str,
        api_mode: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        let mut provisioner = self.clone();
        provisioner.config.default_model = default_model.to_string();
        provisioner.config.image_model = image_model.to_string();
        provisioner.config.api_mode = api_mode.to_string();
        provisioner.ensure_container(instance, llm_api_key).await
    }

    pub async fn rebuild_instance_with_default_model(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
        default_model: &str,
        image_model: &str,
        api_mode: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        let mut provisioner = self.clone();
        provisioner.config.default_model = default_model.to_string();
        provisioner.config.image_model = image_model.to_string();
        provisioner.config.api_mode = api_mode.to_string();
        provisioner.rebuild_instance(instance, llm_api_key).await
    }

    fn build_instance(&self, user_id: &str) -> HermesInstance {
        let container_name = managed_container_name(user_id);
        let user_root = self.config.data_root.join(user_id);
        let workspace = user_root.join("workspace");
        let sandbox = user_root.join("sandbox");
        let config = user_root.join("config");

        HermesInstance::managed_docker(
            user_id,
            self.network_base_url(&container_name),
            path_to_string(workspace),
            path_to_string(sandbox),
            path_to_string(config),
        )
    }

    fn ensure_managed(&self, instance: &HermesInstance) -> Result<(), ProvisionerError> {
        if instance.kind != HermesInstanceKind::ManagedDocker {
            return Err(ProvisionerError::InvalidManagedInstance);
        }

        Ok(())
    }

    fn create_host_directories(&self, instance: &HermesInstance) -> Result<(), ProvisionerError> {
        for path in [
            instance.host_workspace_path.as_deref(),
            instance.host_sandbox_path.as_deref(),
            instance.host_config_path.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            std::fs::create_dir_all(path)
                .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
        }

        for path in [
            instance.host_workspace_path.as_deref(),
            instance.host_sandbox_path.as_deref(),
            instance.host_config_path.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            set_directory_mode_for_container_tools(path)?;
        }

        Ok(())
    }

    async fn ensure_network(&self) -> Result<(), ProvisionerError> {
        let inspected = self
            .runtime
            .run(vec![
                "network".to_string(),
                "inspect".to_string(),
                self.config.network.clone(),
            ])
            .await?;

        if inspected.success {
            return Ok(());
        }

        self.run_required(vec![
            "network".to_string(),
            "create".to_string(),
            self.config.network.clone(),
        ])
        .await?;
        Ok(())
    }

    async fn inspect_container(
        &self,
        name: &str,
    ) -> Result<Option<ContainerInspection>, ProvisionerError> {
        let output = self
            .runtime
            .run(vec![
                "container".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                format!(
                    "{{{{.Id}}}} {{{{.State.Running}}}} {{{{ index .Config.Labels \"{MANAGED_CONTAINER_SPEC_LABEL}\" }}}}"
                ),
                name.to_string(),
            ])
            .await?;

        if output.success && !output.stdout.is_empty() {
            let mut parts = output.stdout.split_whitespace();
            let id = parts.next().unwrap_or_default().to_string();
            let running = parts
                .next()
                .and_then(|value| value.parse::<bool>().ok())
                .unwrap_or(true);
            let spec_version = parts.next().map(ToOwned::to_owned);
            Ok(Some(ContainerInspection {
                id,
                running,
                spec_version,
            }))
        } else {
            Ok(None)
        }
    }

    async fn create_container(
        &self,
        instance: &HermesInstance,
    ) -> Result<String, ProvisionerError> {
        let spec = self.container_spec_for(instance)?;
        let mut args = vec![
            "create".to_string(),
            "--name".to_string(),
            spec.name.clone(),
            "--network".to_string(),
            spec.network.clone(),
        ];

        if let Some(workdir) = spec.workdir {
            args.push("--workdir".to_string());
            args.push(workdir);
        }

        for (key, value) in spec.labels {
            args.push("--label".to_string());
            args.push(format!("{key}={value}"));
        }
        for env in spec.env {
            args.push("--env".to_string());
            args.push(env);
        }
        for mount in spec.mounts {
            args.push("--mount".to_string());
            let mut value = format!(
                "type=bind,src={},dst={}",
                mount.host_path, mount.container_path
            );
            if mount.read_only {
                value.push_str(",readonly");
            }
            args.push(value);
        }
        if let Some(memory_limit) = spec.memory_limit {
            args.push("--memory".to_string());
            args.push(memory_limit);
        }
        if let Some(cpu_limit) = spec.cpu_limit {
            args.push("--cpus".to_string());
            args.push(cpu_limit);
        }
        for published_port in spec.published_ports {
            args.push("--publish".to_string());
            args.push(published_port);
        }

        args.push(spec.image);
        args.extend(spec.command);

        let output = self.run_required(args).await?;
        Ok(output.stdout.lines().next().unwrap_or_default().to_string())
    }

    async fn run_required(
        &self,
        args: Vec<String>,
    ) -> Result<DockerRuntimeOutput, ProvisionerError> {
        let output = self.runtime.run(args).await?;

        if output.success {
            Ok(output)
        } else {
            Err(ProvisionerError::DockerCommand(
                if output.stderr.is_empty() {
                    output.stdout
                } else {
                    output.stderr
                },
            ))
        }
    }

    async fn remove_container_if_exists(&self, name: &str) -> Result<(), ProvisionerError> {
        let output = self
            .runtime
            .run(vec!["rm".to_string(), "-f".to_string(), name.to_string()])
            .await?;

        if output.success || output.stderr.contains("No such container") {
            Ok(())
        } else {
            Err(ProvisionerError::DockerCommand(
                if output.stderr.is_empty() {
                    output.stdout
                } else {
                    output.stderr
                },
            ))
        }
    }

    fn remember(&self, instance: HermesInstance) -> Result<(), ProvisionerError> {
        self.instances
            .lock()
            .map_err(|_| ProvisionerError::LockFailed)?
            .insert(instance.id.clone(), instance);
        Ok(())
    }

    fn write_managed_config(&self, instance: &HermesInstance) -> Result<bool, ProvisionerError> {
        let config_path = instance
            .host_config_path
            .as_ref()
            .ok_or(ProvisionerError::InvalidManagedInstance)?;
        let config_path = PathBuf::from(config_path);
        let model = yaml_string(&self.config.default_model)?;
        let image_model = yaml_string(&self.config.image_model)?;
        let base_url = yaml_string(&self.config.hub_llm_base_url)?;
        let channel_base_url = yaml_string(&hub_channel_base_url(&self.config.hub_llm_base_url))?;
        let api_key = yaml_string(instance.llm_api_key.as_deref().unwrap_or(""))?;
        let api_mode = yaml_string(normalize_hermes_api_mode(&self.config.api_mode))?;
        let instance_id = yaml_string(&instance.id)?;
        let user_id = yaml_string(&instance.user_id)?;
        let content = format!(
            "# Managed by Hermes Hub. Do not edit model settings inside this container.\n\
             plugins:\n\
             \x20\x20enabled: [platforms/hermes_hub]\n\
             model:\n\
             \x20\x20default: {model}\n\
             \x20\x20provider: \"custom\"\n\
             \x20\x20base_url: {base_url}\n\
             \x20\x20api_key: {api_key}\n\
             \x20\x20api_mode: {api_mode}\n\
             image_gen:\n\
             \x20\x20provider: \"openai\"\n\
             \x20\x20model: {image_model}\n\
             \x20\x20openai:\n\
             \x20\x20\x20\x20model: {image_model}\n\
             display:\n\
             \x20\x20tool_progress: \"verbose\"\n\
             \x20\x20tool_progress_command: true\n\
             \x20\x20platforms:\n\
             \x20\x20\x20\x20api_server:\n\
             \x20\x20\x20\x20\x20\x20tool_progress: \"verbose\"\n\
             \x20\x20\x20\x20\x20\x20tool_preview_length: 0\n\
             \x20\x20\x20\x20hermes_hub:\n\
             \x20\x20\x20\x20\x20\x20tool_progress: \"verbose\"\n\
             \x20\x20\x20\x20\x20\x20tool_preview_length: 0\n\
             gateway:\n\
             \x20\x20platforms:\n\
             \x20\x20\x20\x20hermes_hub:\n\
             \x20\x20\x20\x20\x20\x20enabled: true\n\
             \x20\x20\x20\x20\x20\x20extra:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20base_url: {channel_base_url}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20inbox_path: \"{HUB_INBOX_PATH}\"\n\
             \x20\x20\x20\x20\x20\x20\x20\x20instance_id: {instance_id}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20user_id: {user_id}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20timeout_seconds: {HUB_INBOX_TIMEOUT_SECONDS}\n\
             \x20\x20\x20\x20\x20\x20\x20\x20limit: {HUB_INBOX_LIMIT}\n\
             approvals:\n\
             \x20\x20mode: \"off\"\n\
             \x20\x20timeout: 3600\n\
             \x20\x20cron_mode: \"approve\"\n\
             \x20\x20mcp_reload_confirm: false\n\
             \x20\x20destructive_slash_confirm: false\n"
        );
        let config_file = config_path.join("config.yaml");
        let plugin_root = config_path.join("plugins/platforms/hermes_hub");

        let mut changed = write_file_if_changed(&config_file, &content)?;
        changed |= write_file_if_changed(&plugin_root.join("plugin.yaml"), HERMES_HUB_PLUGIN_YAML)?;
        changed |= write_file_if_changed(&plugin_root.join("__init__.py"), HERMES_HUB_PLUGIN_INIT)?;
        changed |= write_file_if_changed(&plugin_root.join("adapter.py"), HERMES_HUB_ADAPTER_PY)?;
        // pairing 是 Hermes gateway 的运行态授权数据，不属于容器规格。
        // 写入失败必须阻断编排；写入成功不需要为了状态文件变化而重建正在运行的容器。
        ensure_hermes_hub_pairing(&config_path, &instance.user_id)?;
        Ok(changed)
    }

    fn published_ports(&self) -> Vec<String> {
        if self.config.connect_mode != HermesContainerConnectMode::PublishedHost {
            return Vec::new();
        }

        vec![format!(
            "{}::{}",
            self.config.published_host_ip, self.config.internal_port
        )]
    }

    fn network_base_url(&self, container_name: &str) -> String {
        format!("http://{container_name}:{}", self.config.internal_port)
    }

    async fn resolve_running_base_url(
        &self,
        container_name: &str,
    ) -> Result<Option<String>, ProvisionerError> {
        match self.config.connect_mode {
            HermesContainerConnectMode::Network => Ok(Some(self.network_base_url(container_name))),
            HermesContainerConnectMode::PublishedHost => {
                self.running_base_url(container_name).await
            }
        }
    }

    async fn running_base_url(
        &self,
        container_name: &str,
    ) -> Result<Option<String>, ProvisionerError> {
        if self.config.connect_mode != HermesContainerConnectMode::PublishedHost {
            return Ok(Some(self.network_base_url(container_name)));
        }

        let output = self
            .runtime
            .run(vec![
                "port".to_string(),
                container_name.to_string(),
                format!("{}/tcp", self.config.internal_port),
            ])
            .await?;

        if !output.success || output.stdout.trim().is_empty() {
            return Ok(None);
        }

        let Some(port) = output
            .stdout
            .lines()
            .find_map(|line| line.rsplit(':').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let base = self.config.published_base_url.trim_end_matches('/');

        Ok(Some(format!("{base}:{port}")))
    }
}

#[async_trait]
impl HermesProvisioner for DockerProvisioner {
    async fn ensure_instance(
        &self,
        user_id: &str,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        let instance = self.build_instance(user_id);
        self.ensure_container(&instance, llm_api_key).await
    }

    async fn start_instance(
        &self,
        instance: &HermesInstance,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        let Some(inspection) = self.inspect_container(&instance.name).await? else {
            return Err(ProvisionerError::InstanceNotFound);
        };
        self.run_required(vec!["start".to_string(), instance.name.clone()])
            .await?;

        let mut next = instance.clone();
        next.container_id = Some(inspection.id);
        next.status = HermesInstanceStatus::Running;
        if let Some(base_url) = self.resolve_running_base_url(&next.name).await? {
            next.base_url = base_url;
        }
        self.remember(next.clone())?;
        Ok(next)
    }

    async fn stop_instance(
        &self,
        instance: &HermesInstance,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        if self.inspect_container(&instance.name).await?.is_none() {
            return Err(ProvisionerError::InstanceNotFound);
        }
        self.run_required(vec!["stop".to_string(), instance.name.clone()])
            .await?;

        let mut next = instance.clone();
        next.status = HermesInstanceStatus::Stopped;
        self.remember(next.clone())?;
        Ok(next)
    }

    async fn rebuild_instance(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        self.ensure_network().await?;

        let mut next = instance.clone();
        next.llm_api_key = Some(llm_api_key.to_string());
        next.api_token_secret_ref = Some(llm_api_key.to_string());
        self.create_host_directories(&next)?;
        self.write_managed_config(&next)?;
        self.remove_container_if_exists(&next.name).await?;
        let container_id = self.create_container(&next).await?;
        self.run_required(vec!["start".to_string(), next.name.clone()])
            .await?;

        next.container_id = Some(container_id);
        next.status = HermesInstanceStatus::Running;
        next.base_url = self
            .running_base_url(&next.name)
            .await?
            .unwrap_or_else(|| self.network_base_url(&next.name));
        self.remember(next.clone())?;
        Ok(next)
    }
}

fn managed_container_name(user_id: &str) -> String {
    format!("hermes-user-{user_id}")
}

fn path_to_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

fn normalize_hermes_api_mode(api_mode: &str) -> &str {
    if api_mode == RESPONSES_API_TYPE {
        // Hermes 内部用 codex_responses 表示 OpenAI Responses API。
        "codex_responses"
    } else {
        api_mode
    }
}

fn hub_channel_base_url(hub_llm_base_url: &str) -> String {
    let trimmed = hub_llm_base_url.trim_end_matches('/');
    if trimmed.ends_with("/internal/channel/v1") {
        return trimmed.to_string();
    }

    if let Some(base) = trimmed.strip_suffix("/internal/llm/v1") {
        return format!("{base}/internal/channel/v1");
    }

    if let Ok(mut url) = reqwest::Url::parse(trimmed) {
        // channel API 是 Hub 自己的内部协议，始终挂在 Hub origin 下；
        // 即便管理员只配置了 http://host:port，也要派生出完整 internal channel 前缀。
        url.set_path("/internal/channel/v1");
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string().trim_end_matches('/').to_string();
    }

    trimmed.to_string()
}

fn write_file_if_changed(path: &Path, content: &str) -> Result<bool, ProvisionerError> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(content) {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    }
    std::fs::write(path, content)
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    Ok(true)
}

fn ensure_hermes_hub_pairing(config_path: &Path, user_id: &str) -> Result<(), ProvisionerError> {
    let approved_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default();

    // Hermes 当前通过 get_hermes_dir("platforms/pairing", "pairing") 解析目录：
    // 旧目录存在时继续使用 /pairing，新安装则偏向 /platforms/pairing。
    // Hub 同时写两份，保证已有用户和后续 Hermes 目录升级都不会重新触发配对。
    for pairing_dir in [
        config_path.join("pairing"),
        config_path.join("platforms/pairing"),
    ] {
        ensure_approved_pairing(
            &pairing_dir.join("hermes_hub-approved.json"),
            user_id,
            approved_at,
        )?;
        clear_pending_pairing_for_user(&pairing_dir.join("hermes_hub-pending.json"), user_id)?;
    }

    Ok(())
}

fn ensure_approved_pairing(
    approved_path: &Path,
    user_id: &str,
    approved_at: f64,
) -> Result<(), ProvisionerError> {
    let mut approved = read_json_object(approved_path)?;
    if approved.contains_key(user_id) {
        return Ok(());
    }

    approved.insert(
        user_id.to_string(),
        json!({
            "user_name": "Hub user",
            "approved_at": approved_at,
        }),
    );
    write_json_object_if_changed(approved_path, &approved)?;
    Ok(())
}

fn clear_pending_pairing_for_user(
    pending_path: &Path,
    user_id: &str,
) -> Result<(), ProvisionerError> {
    if !pending_path.exists() {
        return Ok(());
    }

    let mut pending = read_json_object(pending_path)?;
    let before_len = pending.len();
    pending.retain(|_, entry| entry.get("user_id").and_then(Value::as_str) != Some(user_id));

    if pending.len() != before_len {
        write_json_object_if_changed(pending_path, &pending)?;
    }

    Ok(())
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>, ProvisionerError> {
    if !path.exists() {
        return Ok(Map::new());
    }

    let content = std::fs::read_to_string(path)
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;

    if content.trim().is_empty() {
        return Ok(Map::new());
    }

    match serde_json::from_str::<Value>(&content) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) | Err(_) => Ok(Map::new()),
    }
}

fn write_json_object_if_changed(
    path: &Path,
    object: &Map<String, Value>,
) -> Result<bool, ProvisionerError> {
    let content = serde_json::to_string_pretty(object)
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))?;
    write_file_if_changed(path, &content)
}

#[cfg(unix)]
fn set_directory_mode_for_container_tools(path: &str) -> Result<(), ProvisionerError> {
    // Hermes gateway 进程以容器内 hermes 用户运行，Hub 在宿主机创建的目录默认是 root:root。
    // 第一版先把工具输出目录设为可写，避免 npm/pip/文件生成类任务卡在 EACCES。
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))
        .map_err(|error| ProvisionerError::Filesystem(error.to_string()))
}

#[cfg(not(unix))]
fn set_directory_mode_for_container_tools(_path: &str) -> Result<(), ProvisionerError> {
    Ok(())
}

fn yaml_string(value: &str) -> Result<String, ProvisionerError> {
    serde_json::to_string(value).map_err(|error| ProvisionerError::Filesystem(error.to_string()))
}
