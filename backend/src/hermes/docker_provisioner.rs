#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::Bytes;
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::process::Command;

use crate::{
    model_config::RESPONSES_API_TYPE,
    storage::{DynObjectStorage, ObjectStorageError},
};

use super::{
    instance::{HermesInstance, HermesInstanceKind, HermesInstanceStatus},
    provisioner::{HermesProvisioner, ProvisionerError},
};

/// Hub 托管 Hermes 容器规格版本。只要 env、挂载、工作目录或安全策略有变化，
/// 就提升这个值，确保已存在的旧容器会被重建并拿到新行为。
const MANAGED_CONTAINER_SPEC_VERSION: &str = "2026-05-28-managed-nfs-soul-only";
const MANAGED_CONTAINER_SPEC_LABEL: &str = "hermes_hub_spec_version";
const HUB_INBOX_PATH: &str = "/internal/channel/v1/inbox";
const HUB_INBOX_TIMEOUT_SECONDS: u16 = 25;
const HUB_INBOX_LIMIT: u16 = 4;
const MANAGED_SKILLS_EXTERNAL_DIR: &str = "/nfs/skills";
const MANAGED_PROFILE_SOUL_FILE: &str = "SOUL.md";
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
import hashlib
import importlib.metadata
import json
import logging
import mimetypes
import os
import re
import time
from pathlib import Path
from typing import Any, Dict, Optional
from urllib.parse import unquote, urlencode

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
        # Hub 追踪最后一条输出消息，便于图片/文件回传时更新同一气泡。
        self._last_output_messages: dict[str, dict[str, Any]] = {}
        # thread_id 在 Hub 中稳定等于 session_id，不能当作每轮 run_id 使用；
        # 处理开始/结束钩子用这个表把当前 session 映射到真实 Hub run。
        self._active_run_ids_by_session: dict[str, str] = {}
        self._runtime_status_reported = False
        self._last_scheduler_snapshot_hash = ""
        self._last_scheduler_snapshot_reported_at = 0.0

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
        if self._session is None or self._session.closed:
            self._session = self._new_client_session()
        if self._poll_task is None or self._poll_task.done():
            self._closed.clear()
            await self._report_runtime_status_once()
            await self._report_scheduler_snapshot(force=True)
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
        run_id = self._run_id(metadata)
        if run_id and (metadata or {}).get("notify"):
            merged = await self._merge_text_into_last_attachment_output(
                chat_id, content, metadata
            )
            if merged is not None:
                return merged
        payload = {
            "role": "assistant",
            "content": content,
            "attachments": (metadata or {}).get("attachments") or [],
        }
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
        await self._report_scheduler_snapshot()
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

    async def _report_runtime_status_once(self) -> None:
        if self._runtime_status_reported:
            return
        payload = self._runtime_status_payload()
        if not payload:
            return
        try:
            await self._request_json("POST", "/instance/status", json=payload)
            self._runtime_status_reported = True
        except Exception as error:
            # 版本上报不应阻断 adapter 长轮询；下一次重连时再补报。
            logger.warning("Hermes Hub runtime status report failed: %s", error)

    def _runtime_status_payload(self) -> dict[str, str]:
        payload: dict[str, str] = {}
        runtime_version = self._runtime_version()
        if runtime_version:
            payload["runtime_version"] = runtime_version
        runtime_image = os.getenv("HERMES_RUNTIME_IMAGE", "").strip()
        if runtime_image:
            payload["runtime_image"] = runtime_image
        return payload

    def _runtime_version(self) -> str:
        for package_name in ("hermes-agent", "hermes", "gateway"):
            try:
                version = importlib.metadata.version(package_name).strip()
            except importlib.metadata.PackageNotFoundError:
                continue
            if self._usable_runtime_version(version):
                return version
        fallback = (
            os.getenv("HERMES_RUNTIME_VERSION")
            or os.getenv("HERMES_VERSION")
            or ""
        ).strip()
        return fallback if self._usable_runtime_version(fallback) else ""

    def _usable_runtime_version(self, value: str) -> bool:
        return bool(value and value.strip() and value.strip() != "latest")

    async def _report_scheduler_snapshot(self, force: bool = False) -> None:
        now = time.monotonic()
        if not force and now - self._last_scheduler_snapshot_reported_at < 60:
            return
        payload = self._scheduler_snapshot_payload()
        snapshot = payload.get("scheduler_snapshot") or {}
        snapshot_hash = str(snapshot.get("snapshot_hash") or "")
        if (
            not force
            and snapshot_hash
            and snapshot_hash == self._last_scheduler_snapshot_hash
        ):
            self._last_scheduler_snapshot_reported_at = now
            return
        try:
            await self._request_json("POST", "/instance/status", json=payload)
            self._last_scheduler_snapshot_hash = snapshot_hash
            self._last_scheduler_snapshot_reported_at = now
        except Exception as error:
            # 定时任务快照只影响 Hub 生命周期调度，不能中断用户消息通道。
            logger.warning("Hermes Hub scheduler snapshot report failed: %s", error)

    def _scheduler_snapshot_payload(self) -> dict[str, Any]:
        jobs, source, load_error = self._load_cron_jobs()
        tasks = [self._scheduler_job_payload(job, index, source) for index, job in enumerate(jobs)]
        enabled_next_runs = [
            task["next_run_at"]
            for task in tasks
            if task.get("enabled") and task.get("next_run_at") is not None
        ]
        status = "ok" if load_error is None else "unavailable"
        stable_snapshot = {
            "status": status,
            "source": source,
            "jobs": tasks,
        }
        stable = json.dumps(stable_snapshot, sort_keys=True, default=str).encode("utf-8")
        snapshot = {
            **stable_snapshot,
            "scheduler_enabled": status == "ok",
            "running_jobs_count": sum(1 for task in tasks if task.get("status") == "running"),
            "generated_at": int(time.time()),
            "next_wake_at": min(enabled_next_runs) if enabled_next_runs else None,
            "snapshot_hash": hashlib.sha256(stable).hexdigest(),
        }
        if load_error:
            snapshot["error"] = str(load_error)[:512]
        return {"scheduler_snapshot": snapshot}

    def _load_cron_jobs(self) -> tuple[list[Any], str, Optional[Exception]]:
        try:
            from cron.jobs import list_jobs

            jobs = list_jobs(include_disabled=True)
            return self._jobs_list(jobs), "cron.jobs", None
        except Exception as error:
            file_jobs, file_error = self._load_cron_jobs_json()
            if file_error is None:
                return file_jobs, "jobs.json", None
            return [], "unavailable", error

    def _load_cron_jobs_json(self) -> tuple[list[Any], Optional[Exception]]:
        try:
            jobs_path = Path(os.getenv("HERMES_HOME", "/config")) / "cron" / "jobs.json"
            with jobs_path.open("r", encoding="utf-8") as file:
                return self._jobs_list(json.load(file)), None
        except Exception as error:
            return [], error

    def _jobs_list(self, value: Any) -> list[Any]:
        if isinstance(value, list):
            return value
        if isinstance(value, dict):
            jobs = value.get("jobs") or value.get("items") or value.get("tasks")
            if isinstance(jobs, list):
                return jobs
            return list(value.values())
        return []

    def _scheduler_job_payload(self, job: Any, index: int, source: str) -> dict[str, Any]:
        enabled = bool(self._job_value(job, "enabled", default=True))
        name = str(self._job_value(job, "name", "title", default="") or "")
        task_id = str(self._job_value(job, "id", "job_id", "name", default=f"task-{index}") or f"task-{index}")
        status = str(
            self._job_value(job, "status", "state", "last_status", default="")
            or ("scheduled" if enabled else "disabled")
        )
        return {
            "id": task_id,
            "name": name,
            "enabled": enabled,
            "schedule": str(self._job_value(job, "schedule", "cron", "cron_expr", "expression", default="") or ""),
            "timezone": str(self._job_value(job, "timezone", "tz", default="UTC") or "UTC"),
            "next_run_at": self._epoch_value(self._job_value(job, "next_run_at", "next_run", "next_at")),
            "last_run_at": self._epoch_value(self._job_value(job, "last_run_at", "last_run", "last_at")),
            "status": status,
            "source": source,
        }

    def _job_value(self, job: Any, *names: str, default: Any = None) -> Any:
        for name in names:
            if isinstance(job, dict) and name in job:
                return job.get(name)
            if hasattr(job, name):
                return getattr(job, name)
        return default

    def _epoch_value(self, value: Any) -> Optional[int]:
        if value is None:
            return None
        if isinstance(value, (int, float)):
            return int(value)
        if hasattr(value, "timestamp"):
            try:
                return int(value.timestamp())
            except Exception:
                return None
        try:
            text = str(value).strip()
            return int(float(text)) if text else None
        except Exception:
            return None

    async def _dispatch_inbox_item(self, item: dict[str, Any]) -> None:
        item_type = str(item.get("type") or item.get("kind") or "")
        if item_type == "control":
            await self._dispatch_control_item(item)
            return
        inbox_id = str(item.get("id") or item.get("message_id") or "")
        run_id = str(item.get("run_id") or inbox_id or "")
        session_id = str(item.get("session_id") or item.get("channel_session_id") or "")
        content = item.get("content") or item.get("text") or item.get("message") or ""
        if session_id:
            # send_message 工具通过 gateway config 读取 home_channel。Hub 会话的
            # “正确默认目标”是当前正在处理的 session，不能沿用上一次会话。
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
            # thread_id 必须稳定跟随 session_id，而不是每一轮的 run_id；
            # 这样 Hermes 才会把同一会话的历史连续接起来。
            thread_id=session_id,
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

    async def _dispatch_control_item(self, item: dict[str, Any]) -> None:
        action = str(item.get("action") or "")
        if action == "restart_gateway":
            # Hub 已经把新的 config.yaml 写入只读挂载源；adapter 退出 gateway，
            # Docker 的 restart: always 会拉起新进程并读取最新配置。
            logger.info("Hermes Hub requested gateway restart")
            self._closed.set()
            asyncio.create_task(self._exit_for_gateway_restart())
            return
        logger.warning("Hermes Hub ignored unknown control action: %s", action)

    async def _exit_for_gateway_restart(self) -> None:
        await asyncio.sleep(0.2)
        os._exit(0)

    async def on_processing_start(self, event: MessageEvent) -> None:
        run_id = self._run_id_from_event(event)
        session_id = self._session_id_from_event(event)
        if run_id and session_id:
            self._active_run_ids_by_session[session_id] = run_id

    async def on_processing_complete(self, event: MessageEvent, outcome) -> None:
        run_id = self._run_id_from_event(event)
        try:
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
        finally:
            self._forget_active_run(event, run_id)
            await self._report_scheduler_snapshot(force=True)

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
            merged = await self._merge_attachment_into_last_output(
                chat_id, file_path, caption, attachment, metadata
            )
            if merged is not None:
                return merged
            content = self._content_with_attachment("", caption, attachment, file_path)
            next_metadata = dict(metadata or {})
            next_metadata["attachments"] = [attachment]
            run_id = self._run_id(metadata)
            attachment_id = attachment.get("id")
            if run_id and attachment_id:
                # 如果 Hermes 先发文件再发最终文本，这条附件消息先独立落库；
                # 后续 notify 文本会用 _merge_text_into_last_attachment_output 合并回同一气泡。
                next_metadata["client_message_key"] = f"hermes-run:{run_id}:attachment:{attachment_id}"
            return await self.send(chat_id, content, metadata=next_metadata)
        except Exception as error:
            logger.warning("Hermes Hub attachment send failed: %s", error)
            return SendResult(success=False, error=str(error), retryable=True)

    async def _merge_attachment_into_last_output(
        self,
        chat_id: str,
        file_path: str,
        caption: Optional[str],
        attachment: dict[str, Any],
        metadata: Optional[Dict[str, Any]],
    ) -> Optional[SendResult]:
        run_id = self._run_id(metadata)
        if not run_id:
            return None
        existing = self._last_output_message(run_id)
        if not existing or not self._message_accepts_attachment_merge(run_id, existing):
            return None
        message_id = str(existing.get("id") or "")
        if not message_id:
            return None
        merged_content = self._content_with_attachment(
            str(existing.get("content") or ""), caption, attachment, file_path
        )
        merged_attachments = self._merge_attachments(
            existing.get("attachments") or [], [attachment]
        )
        next_metadata = dict(metadata or {})
        next_metadata["attachments"] = merged_attachments
        # 文件/图片是 run 输出的一部分时，更新同一条 Hub 消息；这样历史快照和实时
        # 事件都只有一个最终气泡，附件也天然嵌入在文本位置。
        return await self.edit_message(
            chat_id, message_id, merged_content, metadata=next_metadata
        )

    async def _merge_text_into_last_attachment_output(
        self,
        chat_id: str,
        content: str,
        metadata: Optional[Dict[str, Any]],
    ) -> Optional[SendResult]:
        run_id = self._run_id(metadata)
        existing = self._last_output_message(run_id) if run_id else None
        if not existing:
            return None
        existing_attachments = existing.get("attachments") or []
        message_id = str(existing.get("id") or "")
        if not message_id or not existing_attachments:
            return None
        merged_content = self._join_message_parts(content, str(existing.get("content") or ""))
        next_metadata = dict(metadata or {})
        next_metadata["attachments"] = existing_attachments
        return await self.edit_message(
            chat_id, message_id, merged_content, metadata=next_metadata
        )

    def _content_with_attachment(
        self,
        existing_content: str,
        caption: Optional[str],
        attachment: dict[str, Any],
        file_path: str,
    ) -> str:
        content = self._join_message_parts(existing_content, caption or "")
        download_url = str(attachment.get("download_url") or "")
        if not download_url:
            return content
        # Hermes 生图有时先输出空图片 markdown，再单独发送文件；这里把空地址补成
        # Hub 下载地址，前端才能在原文位置预览图片。
        if re.search(r"!\[[^\]]*\]\(\s*\)", content):
            return re.sub(
                r"!\[([^\]]*)\]\(\s*\)",
                lambda match: f"![{match.group(1)}]({download_url})",
                content,
                count=1,
            )
        if download_url in content:
            return content
        display_name = str(attachment.get("name") or Path(file_path).name)
        content_type = str(attachment.get("content_type") or "")
        kind = str(attachment.get("kind") or "")
        if kind == "image" or content_type.startswith("image/"):
            markdown = f"![{display_name}]({download_url})"
        else:
            markdown = f"[{display_name}]({download_url})"
        return self._join_message_parts(content, markdown)

    def _join_message_parts(self, *parts: str) -> str:
        cleaned = [str(part).strip() for part in parts if str(part or "").strip()]
        return "\n\n".join(cleaned)

    def _merge_attachments(
        self, existing: list[dict[str, Any]], incoming: list[dict[str, Any]]
    ) -> list[dict[str, Any]]:
        merged: list[dict[str, Any]] = []
        seen: set[str] = set()
        for attachment in [*existing, *incoming]:
            if not isinstance(attachment, dict):
                continue
            attachment_id = str(attachment.get("id") or "")
            dedupe_key = attachment_id or json.dumps(attachment, sort_keys=True, ensure_ascii=False)
            if dedupe_key in seen:
                continue
            seen.add(dedupe_key)
            merged.append(attachment)
        return merged

    def _message_accepts_attachment_merge(self, run_id: str, message: dict[str, Any]) -> bool:
        client_key = str(message.get("client_message_key") or "")
        if client_key == f"hermes-run:{run_id}":
            return True
        if message.get("attachments"):
            return True
        return bool(re.search(r"!\[[^\]]*\]\(\s*\)", str(message.get("content") or "")))

    async def _upload_attachment(
        self, session_id: str, file_path: str, file_name: Optional[str] = None
    ) -> dict[str, Any]:
        # Hermes 工具链里有时会把中文文件名先做 URL 编码；Hub 端应恢复成可读名称，
        # 否则最终下载名会变成一串百分号编码。
        upload_name = unquote(file_name or Path(file_path).name)
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
        if not self._session_matches_current_loop(session):
            async with self._new_client_session() as transient_session:
                return await self._download_attachment_with_session(transient_session, attachment_id)
        return await self._download_attachment_with_session(session, attachment_id)

    async def _download_attachment_with_session(
        self,
        session: aiohttp.ClientSession,
        attachment_id: str,
    ) -> tuple[bytes, str]:
        url = self._url(f"/attachments/{attachment_id}/download")
        async with session.get(url) as response:
            payload = await response.read()
            if response.status >= 400:
                raise RuntimeError(f"Hub attachment download failed {response.status}")
            return payload, response.headers.get("content-type", "application/octet-stream")

    async def _request_json(self, method: str, path: str, **kwargs: Any) -> Any:
        session = await self._ensure_session()
        if not self._session_matches_current_loop(session):
            # Hermes cron 可能在独立 event loop 里回调 platform.send；aiohttp session 不能跨 loop 复用。
            async with self._new_client_session() as transient_session:
                return await self._request_json_with_session(transient_session, method, path, **kwargs)
        return await self._request_json_with_session(session, method, path, **kwargs)

    async def _request_json_with_session(
        self,
        session: aiohttp.ClientSession,
        method: str,
        path: str,
        **kwargs: Any,
    ) -> Any:
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
        if self._session is None or self._session.closed:
            connected = await self.connect()
            if not connected:
                raise RuntimeError("Hermes Hub platform is not connected")
        return self._session

    def _new_client_session(self) -> aiohttp.ClientSession:
        timeout = aiohttp.ClientTimeout(total=self.timeout_seconds + 15)
        return aiohttp.ClientSession(
            timeout=timeout,
            headers=self._headers(),
            trust_env=True,
        )

    def _session_matches_current_loop(self, session: aiohttp.ClientSession) -> bool:
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            return False
        return getattr(session, "_loop", None) is loop and asyncio.current_task(loop=loop) is not None

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
        run_id = self._normalize_run_id(metadata.get("run_id") or "")
        if run_id:
            return run_id
        session_id = self._session_id_from_metadata(metadata)
        if not session_id:
            return ""
        # 这里只用 thread_id/session_id 查找当前运行中的 run，不能把它本身当 run_id；
        # 否则同一会话多轮回复会复用同一个 client_message_key。
        return self._active_run_ids_by_session.get(session_id, "")

    def _run_id_from_event(self, event: MessageEvent) -> str:
        raw = getattr(event, "raw_message", {}) or {}
        if isinstance(raw, dict) and raw.get("run_id"):
            return self._normalize_run_id(raw["run_id"])
        session_id = self._session_id_from_event(event)
        return self._active_run_ids_by_session.get(session_id, "") if session_id else ""

    def _session_id_from_event(self, event: MessageEvent) -> str:
        raw = getattr(event, "raw_message", {}) or {}
        if isinstance(raw, dict):
            session_id = raw.get("session_id") or raw.get("channel_session_id")
            if session_id:
                return str(session_id)
        source = getattr(event, "source", None)
        session_id = getattr(source, "chat_id", "") if source else ""
        return str(session_id or "")

    def _session_id_from_metadata(self, metadata: Optional[Dict[str, Any]]) -> str:
        metadata = metadata or {}
        session_id = (
            metadata.get("session_id")
            or metadata.get("channel_id")
            or metadata.get("thread_id")
            or ""
        )
        return str(session_id or "")

    def _forget_active_run(self, event: MessageEvent, run_id: str) -> None:
        session_id = self._session_id_from_event(event)
        if not session_id or not run_id:
            return
        if self._active_run_ids_by_session.get(session_id) == run_id:
            self._active_run_ids_by_session.pop(session_id, None)

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
        self._last_output_messages[run_id] = message

    def _last_output_message_id(self, run_id: str) -> str:
        message = self._last_output_message(run_id)
        return str((message or {}).get("id") or "")

    def _last_output_message(self, run_id: str) -> Optional[dict[str, Any]]:
        return self._last_output_messages.get(run_id)

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


def _env_enablement() -> Optional[dict[str, Any]]:
    seed: dict[str, Any] = {}
    home_channel = os.getenv("HERMES_HUB_HOME_CHANNEL", "").strip()
    if home_channel:
        # Hermes 的 send_message 工具只读取 gateway config 里的 home_channel；
        # adapter 在分发当前 run 前会写入这个环境变量，这里把它桥接成配置对象。
        seed["home_channel"] = {
            "chat_id": home_channel,
            "name": "Hermes Hub",
        }
    return seed


def register(ctx: Any) -> None:
    ctx.register_platform(
        name="hermes_hub",
        label="Hermes Hub",
        adapter_factory=lambda cfg: HermesHubAdapter(cfg),
        check_fn=_check_requirements,
        env_enablement_fn=_env_enablement,
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
#[derive(Clone, Debug, PartialEq)]
pub struct DockerProvisionerConfig {
    pub image: String,
    pub data_root: PathBuf,
    pub network: String,
    pub internal_port: u16,
    pub hub_llm_base_url: String,
    pub default_model: String,
    pub context_window_tokens: u64,
    pub max_output_tokens: u64,
    pub temperature: f64,
    pub supports_parallel_tools: bool,
    pub image_model_enabled: bool,
    pub image_model: String,
    pub api_mode: String,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    pub docker_binary: String,
    pub managed_skills: Option<ManagedSkillsMountConfig>,
    pub managed_profile: Option<ManagedProfileConfig>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeModelSettings {
    pub default_model: String,
    pub api_mode: String,
    pub context_window_tokens: u64,
    pub max_output_tokens: u64,
    pub temperature: f64,
    pub supports_parallel_tools: bool,
}

/// 容器挂载定义。测试和真实 Docker adapter 共用同一份 spec，避免部署行为漂移。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ContainerMount {
    Bind(ContainerBindMount),
    NfsVolume(ContainerNfsVolumeMount),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerBindMount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerNfsVolumeMount {
    pub volume_name: String,
    pub container_path: String,
    pub read_only: bool,
    pub addr: String,
    pub export: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedSkillsMountConfig {
    pub volume_name: String,
    pub addr: String,
    pub export: String,
    pub container_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedProfileConfig {
    pub container_path: String,
    pub object_prefix: String,
}

impl ContainerMount {
    pub fn bind(host_path: String, container_path: impl Into<String>, read_only: bool) -> Self {
        Self::Bind(ContainerBindMount {
            host_path,
            container_path: container_path.into(),
            read_only,
        })
    }

    pub fn nfs_volume(config: &ManagedSkillsMountConfig, read_only: bool) -> Self {
        Self::NfsVolume(ContainerNfsVolumeMount {
            volume_name: managed_nfs_volume_name(&config.volume_name, read_only),
            container_path: config.container_path.clone(),
            read_only,
            addr: config.addr.clone(),
            export: config.export.clone(),
        })
    }

    pub fn container_path(&self) -> &str {
        match self {
            Self::Bind(mount) => &mount.container_path,
            Self::NfsVolume(mount) => &mount.container_path,
        }
    }

    pub fn read_only(&self) -> bool {
        match self {
            Self::Bind(mount) => mount.read_only,
            Self::NfsVolume(mount) => mount.read_only,
        }
    }
}

/// 可渲染为 Docker create 参数的规范。adapter-only 托管 Hermes 不包含任何端口发布配置。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub network: String,
    pub internal_port: u16,
    pub env: Vec<String>,
    pub mounts: Vec<ContainerMount>,
    pub labels: Vec<(String, String)>,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    pub workdir: Option<String>,
    pub healthcheck: Option<ContainerHealthcheck>,
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContainerInspection {
    id: String,
    running: bool,
    health_status: Option<String>,
    health_error: Option<String>,
    spec_version: Option<String>,
    image: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerHealthcheck {
    pub command: String,
    pub interval: String,
    pub timeout: String,
    pub retries: u8,
    pub start_period: String,
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
            format!(
                r#"{{"Id":"noop-container-id","State":{{"Running":true}},"Config":{{"Labels":{{"{MANAGED_CONTAINER_SPEC_LABEL}":"{MANAGED_CONTAINER_SPEC_VERSION}"}}}}}}"#
            )
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
    object_storage: Option<DynObjectStorage>,
    instances: Arc<Mutex<HashMap<String, HermesInstance>>>,
}

impl DockerProvisioner {
    pub fn new(config: DockerProvisionerConfig) -> Self {
        let runtime = Arc::new(CommandDockerRuntime::new(config.docker_binary.clone()));
        Self::new_with_runtime(config, runtime)
    }

    pub fn new_with_runtime(config: DockerProvisionerConfig, runtime: DynDockerRuntime) -> Self {
        Self::new_with_runtime_and_optional_object_storage(config, runtime, None)
    }

    pub fn new_with_runtime_and_object_storage(
        config: DockerProvisionerConfig,
        runtime: DynDockerRuntime,
        object_storage: DynObjectStorage,
    ) -> Self {
        Self::new_with_runtime_and_optional_object_storage(config, runtime, Some(object_storage))
    }

    fn new_with_runtime_and_optional_object_storage(
        config: DockerProvisionerConfig,
        runtime: DynDockerRuntime,
        object_storage: Option<DynObjectStorage>,
    ) -> Self {
        Self {
            config,
            runtime,
            object_storage,
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
        let config_file = path_to_string(PathBuf::from(&config).join("config.yaml"));

        let mut mounts = vec![
            ContainerMount::bind(workspace, "/workspace", false),
            ContainerMount::bind(sandbox.clone(), "/sandbox", false),
            ContainerMount::bind(sandbox, "/opt/data", false),
            ContainerMount::bind(config, "/config", false),
            // Hermes 运行态目录保持可写，但 Hub 生成的配置文件本身必须只读挂载，
            // 这样管理员配置只能从 Hub/S3 更新，不会被容器内进程意外覆盖。
            ContainerMount::bind(config_file, "/config/config.yaml", true),
        ];
        if let Some(managed_skills) = &self.config.managed_skills {
            mounts.push(ContainerMount::nfs_volume(
                managed_skills,
                !instance.global_skills_write_enabled,
            ));
        }
        let mut env = vec![
            "API_SERVER_ENABLED=true".to_string(),
            // API server 仅作为容器内本地能力保留；Hub 通信全部由 adapter 主动连接。
            "API_SERVER_HOST=127.0.0.1".to_string(),
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
            "HERMES_HUB_NFS_DIR=/nfs".to_string(),
            format!("HERMES_HUB_INBOX_PATH={HUB_INBOX_PATH}"),
            format!("HERMES_HUB_INBOX_TIMEOUT_SECONDS={HUB_INBOX_TIMEOUT_SECONDS}"),
            format!("HERMES_HUB_INBOX_LIMIT={HUB_INBOX_LIMIT}"),
            format!("OPENAI_MODEL={}", self.config.default_model),
            format!("HERMES_RUNTIME_IMAGE={}", self.config.image),
            format!(
                "HERMES_RUNTIME_VERSION={}",
                runtime_version_from_image(&self.config.image).unwrap_or_default()
            ),
            "HERMES_TOOL_PROGRESS_MODE=verbose".to_string(),
            // Hub 托管 Hermes 已经运行在用户独立容器里，命令安全边界由容器承担；
            // 默认自动批准可以避免长任务卡在无人值守的 approval prompt。
            "HERMES_YOLO_MODE=1".to_string(),
            "HERMES_ACCEPT_HOOKS=1".to_string(),
        ];
        if self.config.image_model_enabled {
            // 只有管理员显式启用图片模型时，才把图片生成模型暴露给 Hermes。
            env.push(format!("OPENAI_IMAGE_MODEL={}", self.config.image_model));
        }

        Ok(ContainerSpec {
            name: instance.name.clone(),
            image: self.config.image.clone(),
            network: self.config.network.clone(),
            internal_port: self.config.internal_port,
            env,
            // Hermes gateway 会在 HERMES_HOME 下写入 sessions、logs、skills 等运行态文件；
            // 统一管理 skills 通过单独只读挂载提供，避免进入 /config/skills 的 curator 路径。
            mounts,
            labels: vec![
                ("app".to_string(), "hermes-hub".to_string()),
                ("user_id".to_string(), instance.user_id.clone()),
                ("instance_id".to_string(), instance.id.clone()),
                (
                    MANAGED_CONTAINER_SPEC_LABEL.to_string(),
                    MANAGED_CONTAINER_SPEC_VERSION.to_string(),
                ),
            ],
            memory_limit: self.config.memory_limit.clone(),
            cpu_limit: self.config.cpu_limit.clone(),
            workdir: Some("/workspace".to_string()),
            healthcheck: Some(ContainerHealthcheck {
                // healthcheck 同时验证 Hermes 本地 gateway 和 Hub 内网地址；
                // 这样“进程还在但 adapter 连不上 Hub”不会继续被误判为健康。
                command: format!(
                    "fetch() {{ if command -v curl >/dev/null 2>&1; then curl -fsS --max-time 5 \"$1\" >/dev/null; elif command -v wget >/dev/null 2>&1; then wget -q -T 5 -O /dev/null \"$1\"; else exit 1; fi; }}; fetch \"http://127.0.0.1:{}/health\" && hub=\"${{HERMES_HUB_CHANNEL_BASE_URL%/internal/channel/v1}}\" && fetch \"$hub/health\"",
                    self.config.internal_port
                ),
                interval: "10s".to_string(),
                timeout: "6s".to_string(),
                retries: 3,
                start_period: "20s".to_string(),
            }),
            command: hermes_gateway_command(self.config.managed_profile.as_ref()),
        })
    }

    pub async fn ensure_container(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        self.ensure_network().await?;
        self.ensure_managed_profile_files().await?;

        let mut next = instance.clone();
        next.llm_api_key = Some(llm_api_key.to_string());
        next.api_token_secret_ref = Some(llm_api_key.to_string());
        self.create_host_directories(&next)?;
        let config_changed = self.write_managed_config(&next).await?;

        if let Some(inspection) = self.inspect_container(&next.name).await? {
            if inspection.running
                && inspection.health_status.as_deref() != Some("unhealthy")
                && !config_changed
                && inspection.spec_version.as_deref() == Some(MANAGED_CONTAINER_SPEC_VERSION)
            {
                apply_inspection_status(&mut next, &inspection);
                self.remember(next.clone())?;
                return Ok(next);
            }

            // 旧版本可能创建了交互式 CLI、只读 /config 或发布宿主机端口的容器；
            // 模型配置变化时也需要重建，保证 gateway 读取 Hub 管理的 config.yaml。
            self.remove_container_if_exists(&next.name).await?;
        }

        let container_id = self.create_container(&next).await?;
        self.run_required(vec!["start".to_string(), next.name.clone()])
            .await?;
        next.container_id = Some(container_id);
        next.status = HermesInstanceStatus::Running;
        next.health_status = "starting".to_string();
        next.status_message = None;
        self.remember(next.clone())?;

        Ok(next)
    }

    pub async fn ensure_container_with_default_model(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
        model_settings: &RuntimeModelSettings,
        image_model: Option<&str>,
    ) -> Result<HermesInstance, ProvisionerError> {
        let mut provisioner = self.clone();
        provisioner.config.default_model = model_settings.default_model.clone();
        provisioner.config.api_mode = model_settings.api_mode.clone();
        provisioner.config.context_window_tokens = model_settings.context_window_tokens;
        provisioner.config.max_output_tokens = model_settings.max_output_tokens;
        provisioner.config.temperature = model_settings.temperature;
        provisioner.config.supports_parallel_tools = model_settings.supports_parallel_tools;
        provisioner.config.image_model_enabled = image_model.is_some();
        if let Some(image_model) = image_model {
            provisioner.config.image_model = image_model.to_string();
        }
        provisioner.ensure_container(instance, llm_api_key).await
    }

    pub async fn write_config_with_default_model(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
        model_settings: &RuntimeModelSettings,
        image_model: Option<&str>,
    ) -> Result<bool, ProvisionerError> {
        let mut provisioner = self.clone();
        provisioner.config.default_model = model_settings.default_model.clone();
        provisioner.config.api_mode = model_settings.api_mode.clone();
        provisioner.config.context_window_tokens = model_settings.context_window_tokens;
        provisioner.config.max_output_tokens = model_settings.max_output_tokens;
        provisioner.config.temperature = model_settings.temperature;
        provisioner.config.supports_parallel_tools = model_settings.supports_parallel_tools;
        provisioner.config.image_model_enabled = image_model.is_some();
        if let Some(image_model) = image_model {
            provisioner.config.image_model = image_model.to_string();
        }

        let mut next = instance.clone();
        next.llm_api_key = Some(llm_api_key.to_string());
        next.api_token_secret_ref = Some(llm_api_key.to_string());
        provisioner.create_host_directories(&next)?;
        // 只刷新 Hub 管理的配置文件，不重建 Docker 容器；gateway 由 adapter 控制项重启。
        provisioner.write_managed_config(&next).await
    }

    pub async fn refresh_instance_status(
        &self,
        instance: &HermesInstance,
    ) -> Result<HermesInstance, ProvisionerError> {
        self.ensure_managed(instance)?;
        let mut next = instance.clone();
        match self.inspect_container(&instance.name).await? {
            Some(inspection) => apply_inspection_status(&mut next, &inspection),
            None => {
                next.container_id = None;
                next.status = HermesInstanceStatus::Error;
                next.health_status = "missing".to_string();
                next.status_message = Some("Docker container is missing".to_string());
            }
        }
        self.remember(next.clone())?;
        Ok(next)
    }

    pub async fn rebuild_instance_with_default_model(
        &self,
        instance: &HermesInstance,
        llm_api_key: &str,
        model_settings: &RuntimeModelSettings,
        image_model: Option<&str>,
    ) -> Result<HermesInstance, ProvisionerError> {
        let mut provisioner = self.clone();
        provisioner.config.default_model = model_settings.default_model.clone();
        provisioner.config.api_mode = model_settings.api_mode.clone();
        provisioner.config.context_window_tokens = model_settings.context_window_tokens;
        provisioner.config.max_output_tokens = model_settings.max_output_tokens;
        provisioner.config.temperature = model_settings.temperature;
        provisioner.config.supports_parallel_tools = model_settings.supports_parallel_tools;
        provisioner.config.image_model_enabled = image_model.is_some();
        if let Some(image_model) = image_model {
            provisioner.config.image_model = image_model.to_string();
        }
        provisioner.rebuild_instance(instance, llm_api_key).await
    }

    fn build_instance(&self, user_id: &str) -> HermesInstance {
        let user_root = self.config.data_root.join(user_id);
        let workspace = user_root.join("workspace");
        let sandbox = user_root.join("sandbox");
        let config = user_root.join("config");

        let mut instance = HermesInstance::managed_docker(
            user_id,
            path_to_string(workspace),
            path_to_string(sandbox),
            path_to_string(config),
        );
        apply_runtime_image(&mut instance, &self.config.image);
        instance
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
                "{{json .}}".to_string(),
                name.to_string(),
            ])
            .await?;

        if output.success && !output.stdout.is_empty() {
            Ok(parse_container_inspection(&output.stdout))
        } else {
            Ok(None)
        }
    }

    async fn create_container(
        &self,
        instance: &HermesInstance,
    ) -> Result<String, ProvisionerError> {
        let spec = self.container_spec_for(instance)?;
        self.ensure_image_available(&spec.image).await?;
        self.ensure_nfs_volumes(&spec.mounts).await?;
        let mut args = vec![
            "create".to_string(),
            "--name".to_string(),
            spec.name.clone(),
            "--network".to_string(),
            spec.network.clone(),
            "--restart".to_string(),
            "always".to_string(),
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
            args.push(render_container_mount(&mount));
        }
        if let Some(memory_limit) = spec.memory_limit {
            args.push("--memory".to_string());
            args.push(memory_limit);
        }
        if let Some(cpu_limit) = spec.cpu_limit {
            args.push("--cpus".to_string());
            args.push(cpu_limit);
        }
        if let Some(healthcheck) = spec.healthcheck {
            args.push("--health-cmd".to_string());
            args.push(healthcheck.command);
            args.push("--health-interval".to_string());
            args.push(healthcheck.interval);
            args.push("--health-timeout".to_string());
            args.push(healthcheck.timeout);
            args.push("--health-retries".to_string());
            args.push(healthcheck.retries.to_string());
            args.push("--health-start-period".to_string());
            args.push(healthcheck.start_period);
        }
        args.push(spec.image);
        args.extend(spec.command);

        let output = self.run_required(args).await?;
        Ok(output.stdout.lines().next().unwrap_or_default().to_string())
    }

    async fn ensure_image_available(&self, image: &str) -> Result<(), ProvisionerError> {
        let inspected = self
            .runtime
            .run(vec![
                "image".to_string(),
                "inspect".to_string(),
                image.to_string(),
            ])
            .await?;
        if inspected.success {
            return Ok(());
        }

        // docker create 不会自动拉镜像；这里兜底拉取，避免管理员首次点击创建直接失败。
        self.run_required(vec!["pull".to_string(), image.to_string()])
            .await?;
        Ok(())
    }

    async fn ensure_nfs_volumes(&self, mounts: &[ContainerMount]) -> Result<(), ProvisionerError> {
        for mount in mounts {
            let ContainerMount::NfsVolume(mount) = mount else {
                continue;
            };
            self.run_required(vec![
                "volume".to_string(),
                "create".to_string(),
                "--driver".to_string(),
                "local".to_string(),
                "--opt".to_string(),
                "type=nfs".to_string(),
                "--opt".to_string(),
                format!("o={}", nfs_mount_options(&mount.addr, mount.read_only)),
                "--opt".to_string(),
                format!("device=:{}", normalize_nfs_export(&mount.export)),
                mount.volume_name.clone(),
            ])
            .await?;
        }
        Ok(())
    }

    async fn ensure_managed_profile_files(&self) -> Result<(), ProvisionerError> {
        let Some(profile) = &self.config.managed_profile else {
            return Ok(());
        };
        let Some(object_storage) = &self.object_storage else {
            return Ok(());
        };

        for file_name in [MANAGED_PROFILE_SOUL_FILE] {
            let key = managed_profile_object_key(&profile.object_prefix, file_name);
            match object_storage.get(&key).await {
                Ok(_) => {}
                Err(ObjectStorageError::NotFound) => {
                    // 首次创建用户 Hermes 前先放一个空文件，保证容器内符号链接可解析。
                    object_storage
                        .put(&key, Bytes::new())
                        .await
                        .map_err(|error| ProvisionerError::ObjectStorage(error.to_string()))?;
                }
                Err(error) => return Err(ProvisionerError::ObjectStorage(error.to_string())),
            }
        }
        Ok(())
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

    async fn write_managed_config(
        &self,
        instance: &HermesInstance,
    ) -> Result<bool, ProvisionerError> {
        let config_path = instance
            .host_config_path
            .as_ref()
            .ok_or(ProvisionerError::InvalidManagedInstance)?;
        let config_path = PathBuf::from(config_path);
        let model = yaml_string(&self.config.default_model)?;
        let image_gen_section = if self.config.image_model_enabled {
            let image_model = yaml_string(&self.config.image_model)?;
            format!(
                "image_gen:\n\
                 \x20\x20provider: \"openai\"\n\
                 \x20\x20model: {image_model}\n\
                 \x20\x20openai:\n\
                 \x20\x20\x20\x20model: {image_model}\n"
            )
        } else {
            String::new()
        };
        let base_url = yaml_string(&self.config.hub_llm_base_url)?;
        let channel_base_url = yaml_string(&hub_channel_base_url(&self.config.hub_llm_base_url))?;
        let api_key = yaml_string(instance.llm_api_key.as_deref().unwrap_or(""))?;
        let api_mode = yaml_string(normalize_hermes_api_mode(&self.config.api_mode))?;
        let instance_id = yaml_string(&instance.id)?;
        let user_id = yaml_string(&instance.user_id)?;
        let managed_skills_section = self
            .config
            .managed_skills
            .as_ref()
            .map(|_| {
                Ok(format!(
                    "skills:\n  external_dirs:\n    - {}\n",
                    yaml_string(MANAGED_SKILLS_EXTERNAL_DIR)?
                ))
            })
            .transpose()?
            .unwrap_or_default();
        let content = format!(
            "# Managed by Hermes Hub. Do not edit model settings inside this container.\n\
             {managed_skills_section}\
             memory:\n\
             \x20\x20provider: holographic\n\
             plugins:\n\
             \x20\x20enabled: [platforms/hermes_hub]\n\
             \x20\x20hermes-memory-store:\n\
             \x20\x20\x20\x20db_path: \"$HERMES_HOME/memory_store.db\"\n\
             \x20\x20\x20\x20default_trust: 0.5\n\
             \x20\x20\x20\x20hrr_dim: 1024\n\
             \x20\x20\x20\x20auto_extract: false\n\
             model:\n\
             \x20\x20default: {model}\n\
             \x20\x20provider: \"custom\"\n\
             \x20\x20base_url: {base_url}\n\
             \x20\x20api_key: {api_key}\n\
             \x20\x20api_mode: {api_mode}\n\
             \x20\x20context_window_tokens: {context_window_tokens}\n\
             \x20\x20max_output_tokens: {max_output_tokens}\n\
             \x20\x20temperature: {temperature}\n\
             \x20\x20parallel_tool_calls: {parallel_tool_calls}\n\
             {image_gen_section}\
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
             auxiliary:\n\
             \x20\x20session_search:\n\
             \x20\x20\x20\x20provider: \"main\"\n\
             \x20\x20\x20\x20timeout: 60\n\
             \x20\x20\x20\x20max_concurrency: 1\n\
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
             \x20\x20destructive_slash_confirm: false\n",
            context_window_tokens = self.config.context_window_tokens,
            max_output_tokens = self.config.max_output_tokens,
            temperature = self.config.temperature,
            parallel_tool_calls = self.config.supports_parallel_tools,
        );
        let config_file = config_path.join("config.yaml");
        let plugin_root = config_path.join("plugins/platforms/hermes_hub");

        let mut changed = write_file_if_changed(&config_file, &content)?;
        if let Some(object_storage) = &self.object_storage {
            object_storage
                .put(
                    &user_config_object_key(&instance.user_id),
                    Bytes::from(content.clone()),
                )
                .await
                .map_err(|error| ProvisionerError::ObjectStorage(error.to_string()))?;
        }
        changed |= write_file_if_changed(&plugin_root.join("plugin.yaml"), HERMES_HUB_PLUGIN_YAML)?;
        changed |= write_file_if_changed(&plugin_root.join("__init__.py"), HERMES_HUB_PLUGIN_INIT)?;
        changed |= write_file_if_changed(&plugin_root.join("adapter.py"), HERMES_HUB_ADAPTER_PY)?;
        // pairing 是 Hermes gateway 的运行态授权数据，不属于容器规格。
        // 写入失败必须阻断编排；写入成功不需要为了状态文件变化而重建正在运行的容器。
        ensure_hermes_hub_pairing(&config_path, &instance.user_id)?;
        Ok(changed)
    }
}

fn user_config_object_key(user_id: &str) -> String {
    // 用户 id 来自 Hub 数据库；这里只兜底去掉对象存储路径分隔符，避免非预期 key 层级。
    let safe_user_id = user_id
        .chars()
        .map(|ch| if ch == '/' || ch == '\\' { '_' } else { ch })
        .collect::<String>();
    format!("config/users/{safe_user_id}/config.yaml")
}

fn parse_container_inspection(raw: &str) -> Option<ContainerInspection> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let id = value
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if id.is_empty() {
        return None;
    }
    let state = value.get("State").and_then(Value::as_object);
    let running = state
        .and_then(|state| state.get("Running"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let health_status = state
        .and_then(|state| state.get("Health"))
        .and_then(|health| health.get("Status"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let health_error = state
        .and_then(|state| state.get("Health"))
        .and_then(docker_health_error);
    let spec_version = value
        .get("Config")
        .and_then(|config| config.get("Labels"))
        .and_then(|labels| labels.get(MANAGED_CONTAINER_SPEC_LABEL))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let image = value
        .get("Config")
        .and_then(|config| config.get("Image"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    Some(ContainerInspection {
        id,
        running,
        health_status,
        health_error,
        spec_version,
        image,
    })
}

fn docker_health_error(health: &Value) -> Option<String> {
    health
        .get("Log")
        .and_then(Value::as_array)
        .and_then(|logs| logs.iter().rev().find_map(docker_health_log_error))
}

fn docker_health_log_error(log: &Value) -> Option<String> {
    let output = log
        .get("Output")
        .and_then(Value::as_str)
        .map(clean_status_message)
        .filter(|output| !output.is_empty());
    if output.is_some() {
        return output;
    }
    log.get("ExitCode")
        .and_then(Value::as_i64)
        .filter(|code| *code != 0)
        .map(|code| format!("Docker healthcheck exited with code {code}"))
}

fn clean_status_message(message: &str) -> String {
    // Docker health log 可能包含多行 curl/wget 输出；前端只需要可读摘要，避免撑爆表格。
    message.trim().chars().take(1024).collect()
}

fn apply_inspection_status(instance: &mut HermesInstance, inspection: &ContainerInspection) {
    instance.container_id = Some(inspection.id.clone());
    if let Some(image) = &inspection.image {
        apply_runtime_image(instance, image);
    }
    if !inspection.running {
        instance.status = HermesInstanceStatus::Stopped;
        instance.health_status = "stopped".to_string();
        instance.status_message = None;
        return;
    }

    match inspection.health_status.as_deref() {
        Some("healthy") => {
            instance.status = HermesInstanceStatus::Running;
            instance.health_status = "healthy".to_string();
            instance.status_message = None;
        }
        Some("starting") => {
            instance.status = HermesInstanceStatus::Provisioning;
            instance.health_status = "starting".to_string();
            instance.status_message = None;
        }
        Some("unhealthy") => {
            instance.status = HermesInstanceStatus::Error;
            instance.health_status = "unhealthy".to_string();
            instance.status_message = inspection
                .health_error
                .clone()
                .or_else(|| Some("Docker healthcheck reported unhealthy".to_string()));
        }
        Some(other) => {
            instance.status = HermesInstanceStatus::Running;
            instance.health_status = other.to_string();
            instance.status_message = None;
        }
        None => {
            // 兼容旧容器：没有 Docker healthcheck 时只能确认进程运行。
            instance.status = HermesInstanceStatus::Running;
            instance.health_status = "running".to_string();
            instance.status_message = None;
        }
    }
}

fn apply_runtime_image(instance: &mut HermesInstance, image: &str) {
    // 镜像 tag 是 adapter 尚未上报前的兜底；一旦容器内上报了真实版本，就不再覆盖。
    let previous_image_version = instance
        .runtime_image
        .as_deref()
        .and_then(runtime_version_from_image);
    let next_image_version = runtime_version_from_image(image);
    instance.runtime_image = Some(image.to_string());
    if instance.runtime_version.is_none() || instance.runtime_version == previous_image_version {
        instance.runtime_version = next_image_version;
    }
}

fn runtime_version_from_image(image: &str) -> Option<String> {
    let image_without_digest = image.split('@').next().unwrap_or(image);
    let last_segment = image_without_digest
        .rsplit('/')
        .next()
        .unwrap_or(image_without_digest);
    last_segment
        .rsplit_once(':')
        .map(|(_, tag)| tag.trim())
        .filter(|tag| !tag.is_empty() && *tag != "latest")
        .map(ToOwned::to_owned)
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
        next.health_status = "starting".to_string();
        next.status_message = None;
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
        next.health_status = "stopped".to_string();
        next.status_message = None;
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
        self.ensure_managed_profile_files().await?;

        let mut next = instance.clone();
        next.llm_api_key = Some(llm_api_key.to_string());
        next.api_token_secret_ref = Some(llm_api_key.to_string());
        self.create_host_directories(&next)?;
        self.write_managed_config(&next).await?;
        self.remove_container_if_exists(&next.name).await?;
        let container_id = self.create_container(&next).await?;
        self.run_required(vec!["start".to_string(), next.name.clone()])
            .await?;

        next.container_id = Some(container_id);
        next.status = HermesInstanceStatus::Running;
        next.health_status = "starting".to_string();
        next.status_message = None;
        self.remember(next.clone())?;
        Ok(next)
    }
}

fn path_to_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

fn render_container_mount(mount: &ContainerMount) -> String {
    let mut value = match mount {
        ContainerMount::Bind(mount) => {
            format!(
                "type=bind,src={},dst={}",
                mount.host_path, mount.container_path
            )
        }
        ContainerMount::NfsVolume(mount) => {
            format!(
                "type=volume,src={},dst={},volume-driver=local",
                mount.volume_name, mount.container_path
            )
        }
    };
    if mount.read_only() {
        value.push_str(",readonly");
    }
    value
}

fn hermes_gateway_command(managed_profile: Option<&ManagedProfileConfig>) -> Vec<String> {
    if managed_profile.is_some() {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "exec /opt/hermes/.venv/bin/hermes gateway".to_string(),
        ]
    } else {
        vec!["gateway".to_string()]
    }
}

fn managed_profile_object_key(prefix: &str, file_name: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        file_name.to_string()
    } else {
        format!("{prefix}/{file_name}")
    }
}

fn nfs_mount_options(addr: &str, read_only: bool) -> String {
    let (host, port) = split_nfs_addr(addr);
    let mode = if read_only { "ro" } else { "rw" };
    format!(
        "addr={host},port={port},mountport={port},vers=3,tcp,nolock,soft,actimeo=0,lookupcache=none,{mode}"
    )
}

fn managed_nfs_volume_name(base_name: &str, read_only: bool) -> String {
    if read_only {
        // Docker 不会更新已有 volume 的 NFS options；换名确保新容器使用禁缓存挂载。
        format!("{base_name}-live")
    } else {
        // rw 挂载也要独立名称，避免复用普通用户 ro volume 以及旧缓存参数。
        format!("{base_name}-rw-live")
    }
}

fn split_nfs_addr(addr: &str) -> (&str, &str) {
    addr.rsplit_once(':').unwrap_or((addr, "2049"))
}

fn normalize_nfs_export(export: &str) -> String {
    let trimmed = export.trim();
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
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
