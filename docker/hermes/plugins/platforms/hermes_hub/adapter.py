from __future__ import annotations

import asyncio
import hashlib
import importlib.metadata
import json
import logging
import mimetypes
import os
import re
import stat as stat_module
import time
from pathlib import Path
from typing import Any, Dict, Optional
from urllib.parse import unquote, urlencode, urlsplit

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
    cache_image_from_url,
)


logger = logging.getLogger(__name__)

HUB_MEDIA_DIRECTIVE_RE = re.compile(
    r"""^\s*[`"']?MEDIA:\s*(?P<path>`[^`\n]+`|"[^"\n]+"|'[^'\n]+'|(?:~/|/).+?)\s*[`"']?\s*$"""
)


class AttachmentFileError(Exception):
    pass


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
        # Hub 只记录最后一条输出消息 id，用于 run 完成回调携带 output_message_id。
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

    @staticmethod
    def extract_media(content: str):
        # Hermes core 的最终回答路径会先调用 self.extract_media；这里补齐任意扩展的 MEDIA 行。
        media, cleaned = BasePlatformAdapter.extract_media(content)
        media_paths, cleaned = HermesHubAdapter._extract_hub_media_directives(cleaned)
        if media_paths:
            has_voice_tag = "[[audio_as_voice]]" in str(content or "")
            media.extend((path, has_voice_tag) for path in media_paths)
        return media, cleaned

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
        media_paths, cleaned_content = self._media_directives_from_content(content)
        if media_paths:
            return await self._send_media_directives(
                chat_id, media_paths, cleaned_content, metadata
            )

        session_id = self._session_id(chat_id, metadata)
        run_id = self._run_id(metadata)
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

    async def _send_media_directives(
        self,
        chat_id: str,
        media_paths: list[str],
        caption: str,
        metadata: Optional[Dict[str, Any]],
    ) -> SendResult:
        last_result: SendResult | None = None
        for index, media_path in enumerate(media_paths):
            media_metadata = dict(metadata or {})
            media_metadata["media_sequence"] = index
            last_result = await self._send_media_output(
                chat_id,
                media_path,
                caption if index == 0 else None,
                media_metadata,
                None,
            )
            if not last_result.success:
                return last_result
        return last_result or SendResult(success=True, message_id="")

    def _media_directives_from_content(self, content: str) -> tuple[list[str], str]:
        return self._extract_hub_media_directives(content)

    @staticmethod
    def _extract_hub_media_directives(content: str) -> tuple[list[str], str]:
        media_paths: list[str] = []
        text_lines: list[str] = []
        for line in str(content or "").splitlines():
            match = HUB_MEDIA_DIRECTIVE_RE.match(line)
            if not match:
                text_lines.append(line)
                continue
            media_path = HermesHubAdapter._clean_media_directive_path(match.group("path"))
            if media_path:
                media_paths.append(media_path)
        return media_paths, "\n".join(text_lines).strip()

    @staticmethod
    def _clean_media_directive_path(raw_path: str) -> str:
        media_path = str(raw_path or "").strip()
        if len(media_path) >= 2 and media_path[0] == media_path[-1] and media_path[0] in "`\"'":
            media_path = media_path[1:-1].strip()
        else:
            media_path = media_path.lstrip("`\"'").rstrip("`\"',.;:)}]")
        return os.path.expanduser(media_path)

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
        return await self._send_media_output(chat_id, file_path, caption, metadata, file_name)

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
        return await self._send_media_output(chat_id, image_path, caption, metadata, None)

    async def send_image(
        self,
        chat_id: str,
        image_url: str,
        caption: Optional[str] = None,
        reply_to: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
    ) -> SendResult:
        del reply_to
        raw_url = str(image_url or "").strip()
        if not raw_url:
            return SendResult(success=False, error="image url is empty", retryable=False)
        if raw_url.startswith("file://"):
            return await self.send_image_file(
                chat_id=chat_id,
                image_path=unquote(raw_url[7:]),
                caption=caption,
                metadata=metadata,
            )
        if raw_url.startswith("/") or raw_url.startswith("~/"):
            return await self.send_image_file(
                chat_id=chat_id,
                image_path=os.path.expanduser(raw_url),
                caption=caption,
                metadata=metadata,
            )
        try:
            media_metadata = dict(metadata or {})
            media_metadata.setdefault("media_source_url", raw_url)
            image_ext = self._image_extension_from_url(raw_url)
            cached_path = await cache_image_from_url(raw_url, ext=image_ext)
            detected_ext = self._image_extension_from_file(cached_path)
            if detected_ext and detected_ext != Path(cached_path).suffix.lower():
                renamed_path = str(Path(cached_path).with_suffix(detected_ext))
                os.replace(cached_path, renamed_path)
                cached_path = renamed_path
            return await self.send_image_file(
                chat_id=chat_id,
                image_path=cached_path,
                caption=caption,
                metadata=media_metadata,
            )
        except Exception as error:
            logger.warning("Hermes Hub remote image send failed: %s", error)
            return SendResult(success=False, error=str(error), retryable=True)

    async def send_multiple_images(
        self,
        chat_id: str,
        images: list[tuple[str, str]],
        metadata: Optional[Dict[str, Any]] = None,
        human_delay: float = 0.0,
    ) -> None:
        # Hub 不做批量打包，但必须给每个图片稳定编号，避免相同文件在同一 run 内被幂等键合并。
        for index, (image_url, alt_text) in enumerate(images):
            if human_delay > 0:
                await asyncio.sleep(human_delay)
            item_metadata = dict(metadata or {})
            item_metadata["media_sequence"] = index
            caption = alt_text if alt_text else None
            try:
                if image_url.startswith("file://"):
                    result = await self.send_image_file(
                        chat_id=chat_id,
                        image_path=unquote(image_url[7:]),
                        caption=caption,
                        metadata=item_metadata,
                    )
                elif image_url.startswith("/") or image_url.startswith("~/"):
                    result = await self.send_image_file(
                        chat_id=chat_id,
                        image_path=os.path.expanduser(image_url),
                        caption=caption,
                        metadata=item_metadata,
                    )
                else:
                    # 批量图片里的 GIF/动画也按图片交付，避免依赖上游内部动画判断。
                    result = await self.send_image(
                        chat_id=chat_id,
                        image_url=image_url,
                        caption=caption,
                        metadata=item_metadata,
                    )
                if not result.success:
                    logger.error("Hermes Hub image batch item failed: %s", result.error)
            except Exception as error:
                logger.error("Hermes Hub image batch item failed: %s", error, exc_info=True)

    async def send_voice(
        self,
        chat_id: str,
        audio_path: str,
        caption: Optional[str] = None,
        reply_to: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
        **kwargs: Any,
    ) -> SendResult:
        del kwargs
        # Hub 目前没有独立语音气泡，音频统一作为普通附件交付。
        return await self.send_document(
            chat_id=chat_id,
            file_path=audio_path,
            caption=caption,
            reply_to=reply_to,
            metadata=metadata,
        )

    async def send_video(
        self,
        chat_id: str,
        video_path: str,
        caption: Optional[str] = None,
        reply_to: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
        **kwargs: Any,
    ) -> SendResult:
        del kwargs
        # Hub 目前没有独立视频播放器消息，视频统一作为普通附件交付。
        return await self.send_document(
            chat_id=chat_id,
            file_path=video_path,
            caption=caption,
            reply_to=reply_to,
            metadata=metadata,
        )

    async def send_animation(
        self,
        chat_id: str,
        animation_url: str,
        caption: Optional[str] = None,
        reply_to: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
    ) -> SendResult:
        # GIF/动画在 Hub 侧按图片交付，保持和用户看到的图片预览一致。
        return await self.send_image(
            chat_id=chat_id,
            image_url=animation_url,
            caption=caption,
            reply_to=reply_to,
            metadata=metadata,
        )

    async def send_typing(self, chat_id: str, metadata=None) -> None:
        del chat_id, metadata
        # Hub 前端的“正在输入”由 active run 状态驱动；adapter typing 事件保持无副作用。
        return None

    async def stop_typing(self, chat_id: str) -> None:
        del chat_id
        # Hub 前端没有独立 typing 通道，停止输入同样由 run 完成/失败事件驱动。
        return None

    async def send_clarify(
        self,
        chat_id: str,
        question: str,
        choices: Optional[list],
        clarify_id: str,
        session_key: str,
        metadata: Optional[Dict[str, Any]] = None,
    ) -> SendResult:
        # 先复用 Hermes 基类文本回退；后续前端按钮可以在 Hub 协议层单独扩展。
        return await super().send_clarify(chat_id, question, choices, clarify_id, session_key, metadata)

    def _image_extension_from_url(self, image_url: str) -> str:
        suffix = Path(unquote(urlsplit(image_url).path)).suffix.lower()
        return suffix if suffix in {".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp"} else ".jpg"

    def _image_extension_from_file(self, image_path: str) -> str:
        try:
            with open(image_path, "rb") as handle:
                header = handle.read(16)
        except OSError:
            return Path(image_path).suffix.lower()
        if header.startswith(b"\xff\xd8\xff"):
            return ".jpg"
        if header.startswith(b"\x89PNG\r\n\x1a\n"):
            return ".png"
        if header[:6] in {b"GIF87a", b"GIF89a"}:
            return ".gif"
        if len(header) >= 12 and header.startswith(b"RIFF") and header[8:12] == b"WEBP":
            return ".webp"
        if header.startswith(b"BM"):
            return ".bmp"
        return Path(image_path).suffix.lower()

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
            if run_id:
                self._last_output_messages.pop(run_id, None)
            await self._report_scheduler_snapshot(force=True)

    async def _send_media_output(
        self,
        chat_id: str,
        file_path: str,
        caption: Optional[str],
        metadata: Optional[Dict[str, Any]],
        file_name: Optional[str],
    ) -> SendResult:
        session_id = self._session_id(chat_id, metadata)
        media_file = None
        try:
            media_file = self._open_validated_media_file(file_path)
            # 对齐 Telegram adapter 语义：一次 send_document/send_image_file 就是一条原生媒体输出。
            # Hub 端用原子接口完成上传、落库和附件绑定，不再靠本地 last-output 状态猜测合并目标。
            upload_name = unquote(file_name or media_file["upload_name"])
            content_type = mimetypes.guess_type(upload_name)[0] or "application/octet-stream"
            data = aiohttp.FormData()
            data.add_field("content", self._content_with_single_attachment_placeholder(caption))
            run_id = self._run_id(metadata)
            if run_id:
                data.add_field("run_id", run_id)
            client_message_key = self._media_client_message_key(metadata, media_file, upload_name, caption)
            if client_message_key:
                data.add_field("client_message_key", client_message_key)
            # 文件内容只在当前请求中流向 Hub；Hub 后端负责写对象存储并绑定消息。
            data.add_field(
                "file",
                media_file["handle"],
                filename=upload_name,
                content_type=content_type,
            )
            response = await self._request_json(
                "POST", f"/sessions/{session_id}/outputs/media", data=data
            )
            message = response.get("message", response)
            self._remember_output_message(metadata, message)
            return SendResult(
                success=True,
                message_id=str(message.get("id", "")),
                raw_response=message,
            )
        except AttachmentFileError as error:
            return SendResult(success=False, error=str(error), retryable=False)
        except Exception as error:
            logger.warning("Hermes Hub media output send failed: %s", error)
            return SendResult(success=False, error=str(error), retryable=True)
        finally:
            if media_file is not None:
                self._close_media_files([media_file])

    def _content_with_single_attachment_placeholder(self, caption: Optional[str]) -> str:
        content = str(caption or "").strip()
        if "{{attachment:" in content:
            return content
        return f"{content}\n\n{{{{attachment:0}}}}".strip()

    def _open_validated_media_file(self, file_path: str) -> dict[str, Any]:
        resolved = os.path.realpath(str(file_path or "").strip())
        if not resolved:
            raise AttachmentFileError("attachment file not found")
        flags = os.O_RDONLY
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        fd = None
        handle = None
        try:
            fd = os.open(resolved, flags)
            actual_path = self._opened_media_file_path(fd)
            file_stat = os.fstat(fd)
            if not stat_module.S_ISREG(file_stat.st_mode):
                raise AttachmentFileError("attachment path is not a readable file")
            handle = os.fdopen(fd, "rb")
            fd = None
            digest = self._file_sha256_from_handle(handle)
            handle.seek(0)
            return {
                "handle": handle,
                "path": resolved,
                "upload_name": unquote(Path(resolved).name),
                "size": int(file_stat.st_size),
                "mtime_ns": int(
                    getattr(file_stat, "st_mtime_ns", int(file_stat.st_mtime * 1_000_000_000))
                ),
                "sha256": digest,
            }
        except AttachmentFileError:
            if handle is not None:
                handle.close()
            raise
        except FileNotFoundError:
            if handle is not None:
                handle.close()
            raise AttachmentFileError("attachment file not found") from None
        except PermissionError:
            if handle is not None:
                handle.close()
            raise AttachmentFileError("attachment path is not a readable file") from None
        except OSError:
            if handle is not None:
                handle.close()
            raise AttachmentFileError("attachment path is not a readable file") from None
        finally:
            if fd is not None:
                os.close(fd)

    def _media_path_is_allowed(self, resolved: str) -> bool:
        del resolved
        # Hermes 的文件读取能力已经覆盖容器内路径；附件发送保持同一边界。
        return True

    def _opened_media_file_path(self, fd: int) -> str:
        try:
            return os.path.realpath(f"/proc/self/fd/{fd}")
        except OSError:
            raise AttachmentFileError("attachment file could not be verified") from None

    def _close_media_files(self, media_files: list[dict[str, Any]]) -> None:
        for media_file in media_files:
            handle = media_file.get("handle")
            if handle is not None:
                handle.close()

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

    def _media_client_message_key(
        self,
        metadata: Optional[Dict[str, Any]],
        media_file: dict[str, Any],
        upload_name: str,
        caption: Optional[str],
    ) -> str:
        metadata = metadata or {}
        explicit_client_message_key = str(metadata.get("client_message_key") or "")
        run_id = self._run_id(metadata)
        if not run_id and not explicit_client_message_key:
            return ""
        media_sequence = metadata.get("media_sequence")
        if explicit_client_message_key and media_sequence is None:
            return explicit_client_message_key
        media_source_url = str(metadata.get("media_source_url") or "")
        if media_source_url:
            # 远程图片会先缓存到随机文件名；幂等键必须基于来源和内容，而不是缓存路径。
            fingerprint_parts = [
                "remote",
                media_source_url,
                media_file["size"],
                media_file["sha256"],
                caption or "",
                media_sequence,
            ]
        else:
            fingerprint_parts = [
                "local",
                upload_name,
                media_file["size"],
                media_file["mtime_ns"],
                media_file["sha256"],
                caption or "",
                media_sequence,
            ]
        fingerprint = json.dumps(
            fingerprint_parts,
            ensure_ascii=False,
            separators=(",", ":"),
        )
        digest = hashlib.sha256(fingerprint.encode("utf-8")).hexdigest()[:20]
        key_prefix = explicit_client_message_key or f"hermes-run:{run_id}"
        return f"{key_prefix}:media:{digest}"

    def _file_sha256_from_handle(self, handle) -> str:
        digest = hashlib.sha256()
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
        return digest.hexdigest()

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
        # Hub 在容器启动前注入固定主会话，adapter 只把它桥接成平台配置。
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
    )
