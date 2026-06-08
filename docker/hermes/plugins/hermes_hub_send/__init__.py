"""Hermes Hub 当前会话发送工具。

本文件以 Hermes Agent 官方 ``tools/send_message_tool.py`` 为蓝本：
保留官方的 MEDIA 提取、路径过滤、错误脱敏、中断检查、分块发送流程，
只把“目标解析/跨平台投递”收窄成 Hermes Hub 当前会话投递。
"""

from __future__ import annotations

import json
import logging
import os
import re
from typing import Any, Optional

from gateway.platforms.base import BasePlatformAdapter
from gateway.session_context import get_session_env

try:
    from agent.redact import redact_sensitive_text
except Exception:  # pragma: no cover - Hermes 运行时应内置 agent.redact；测试桩可走兜底。
    def redact_sensitive_text(text: Any) -> str:
        return str(text)

logger = logging.getLogger(__name__)

_IMAGE_EXTS = {".jpg", ".jpeg", ".png", ".webp", ".gif"}
_VIDEO_EXTS = {".mp4", ".mov", ".avi", ".mkv", ".3gp", ".webm"}
_AUDIO_EXTS = {".ogg", ".opus", ".mp3", ".wav", ".m4a", ".flac"}
_VOICE_EXTS = {".ogg", ".opus"}
_URL_SECRET_QUERY_RE = re.compile(
    r"([?&](?:access_token|api[_-]?key|auth[_-]?token|token|signature|sig)=)([^&#\s]+)",
    re.IGNORECASE,
)
_GENERIC_SECRET_ASSIGN_RE = re.compile(
    r"\b(access_token|api[_-]?key|auth[_-]?token|signature|sig)\s*=\s*([^\s,;]+)",
    re.IGNORECASE,
)
_MEDIA_LINE_RE = re.compile(
    r"""^\s*[`"']?MEDIA:\s*(?P<path>`[^`\n]+`|"[^"\n]+"|'[^'\n]+'|(?:~/|/).+?)\s*[`"']?\s*$"""
)


HERMES_HUB_SEND_SCHEMA = {
    "name": "hermes_hub_send",
    "description": (
        "Send a message to the current Hermes Hub conversation. When the user "
        "asks you to send, deliver, attach, upload, or share a local file or "
        "image, call this tool and include MEDIA:<local_path> in the message, "
        "for example 'Here is the file\\nMEDIA:/workspace/report.txt'."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "description": (
                    "Message text to send to the current Hermes Hub conversation. "
                    "Use official MEDIA:<absolute_path> directives for local files "
                    "that should be delivered as native Hermes Hub attachments."
                ),
            }
        },
        "required": ["message"],
    },
}

HERMES_HUB_BUSINESS_TOOL_REQUEST_SCHEMA = {
    "name": "business_tool_request",
    "description": (
        "Call a Hermes Hub integration business tool. Use this when the current "
        "conversation exposes third-party tools such as save_note or search_notes."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "tool_name": {
                "type": "string",
                "description": "Exact business tool name provided by the current integration.",
            },
            "arguments": {
                "type": "object",
                "description": "JSON object arguments for the selected business tool.",
                "additionalProperties": True,
            },
            "timeout_seconds": {
                "type": "integer",
                "description": "Optional timeout hint in seconds for the business tool request.",
            },
        },
        "required": ["tool_name", "arguments"],
        "additionalProperties": False,
    },
}


def _sanitize_error_text(text: Any) -> str:
    # 对齐官方 send_message_tool：所有面向模型/用户的错误都先做敏感字段脱敏。
    redacted = redact_sensitive_text(text)
    redacted = _URL_SECRET_QUERY_RE.sub(lambda match: f"{match.group(1)}***", redacted)
    return _GENERIC_SECRET_ASSIGN_RE.sub(lambda match: f"{match.group(1)}=***", redacted)


def _error(message: str) -> dict[str, str]:
    return {"error": _sanitize_error_text(message)}


def _tool_error(message: str) -> str:
    return json.dumps(_error(str(message)), ensure_ascii=False)


def _tool_success(message_id: str = "", warnings: Optional[list[str]] = None) -> str:
    payload: dict[str, Any] = {"success": True}
    if message_id:
        payload["message_id"] = message_id
    if warnings:
        payload["warnings"] = warnings
    return json.dumps(payload, ensure_ascii=False)


def _current_hub_session_id() -> str:
    # Hub adapter 在 MessageEvent 里把 chat_id/thread_id 都设为 Hub session_id。
    return (
        get_session_env("HERMES_SESSION_THREAD_ID", "").strip()
        or get_session_env("HERMES_SESSION_CHAT_ID", "").strip()
    )


def _current_run_id_for(adapter: Any, session_id: str) -> str:
    active = getattr(adapter, "_active_run_ids_by_session", {}) or {}
    return str(active.get(session_id) or "")


def _live_hub_adapter() -> Any:
    try:
        from gateway.config import Platform
        from gateway.run import _gateway_runner_ref

        runner = _gateway_runner_ref()
        if runner is None:
            return None
        return runner.adapters.get(Platform("hermes_hub"))
    except Exception:
        return None


def _check_hub_send_available() -> bool:
    if get_session_env("HERMES_SESSION_PLATFORM", "") == "hermes_hub":
        return True
    return _live_hub_adapter() is not None


def _check_business_tool_request_available() -> bool:
    return _check_hub_send_available()


def hermes_hub_send_tool(args: dict[str, Any], **_kw: Any) -> str:
    """Hermes Hub 当前会话版 send_message。

    结构上对齐官方 tools/send_message_tool.py：
    _handle_send 负责解析/过滤，_send_to_platform 负责分块和平台投递。
    """
    return _handle_send(args or {})


def business_tool_request_tool(args: dict[str, Any], **_kw: Any) -> str:
    """通过 Hub 内部 API 发起第三方业务工具调用。"""
    return _handle_business_tool_request(args or {})


def _handle_send(args: dict[str, Any]) -> str:
    message = _normalize_message_line_breaks(str((args or {}).get("message") or ""))
    if not message.strip():
        return _tool_error("message is required")

    try:
        from tools.interrupt import is_interrupted

        if is_interrupted():
            return _tool_error("Interrupted")
    except Exception:
        # 中断工具不可用不应影响 Hub 发送；官方路径也把中断作为发送前的轻量检查。
        pass

    session_id = _current_hub_session_id()
    if not session_id:
        return _tool_error("Hermes Hub session context is unavailable")

    media_files, cleaned_message = _extract_media_with_hub_fallback(message)
    media_files = BasePlatformAdapter.filter_media_delivery_paths(media_files)
    force_document = "[[as_document]]" in message
    if not cleaned_message.strip() and not media_files:
        return _tool_error("No deliverable text or media remained after processing MEDIA tags")

    try:
        from model_tools import _run_async

        result = _run_async(
            _send_to_platform(
                session_id,
                cleaned_message,
                media_files=media_files,
                force_document=force_document,
            )
        )
    except Exception as error:
        logger.warning("Hermes Hub send tool failed: %s", error, exc_info=True)
        return _tool_error(f"Hermes Hub send failed: {error}")

    if isinstance(result, dict) and result.get("success"):
        return _tool_success(str(result.get("message_id") or ""), result.get("warnings"))
    if isinstance(result, dict) and result.get("error"):
        return json.dumps(_error(str(result["error"])), ensure_ascii=False)
    return _tool_error("Hermes Hub send returned an invalid result")


def _handle_business_tool_request(args: dict[str, Any]) -> str:
    tool_name = str((args or {}).get("tool_name") or "").strip()
    arguments = (args or {}).get("arguments")
    timeout_seconds = (args or {}).get("timeout_seconds")
    if not tool_name:
        return _tool_error("tool_name is required")
    if not isinstance(arguments, dict):
        return _tool_error("arguments must be an object")
    if timeout_seconds is not None:
        try:
            timeout_seconds = int(timeout_seconds)
        except Exception:
            return _tool_error("timeout_seconds must be an integer")
        if timeout_seconds <= 0:
            return _tool_error("timeout_seconds must be positive")

    session_id = _current_hub_session_id()
    if not session_id:
        return _tool_error("Hermes Hub session context is unavailable")

    adapter = _live_hub_adapter()
    if adapter is None:
        return _tool_error("Hermes Hub adapter is not connected")

    payload = {
        "args": {
            "tool_name": tool_name,
            "arguments": arguments,
        }
    }
    if timeout_seconds is not None:
        payload["timeout_seconds"] = timeout_seconds

    try:
        from model_tools import _run_async

        response = _run_async(
            adapter._request_json(
                "POST",
                f"/sessions/{session_id}/business-tool-request",
                json=payload,
            )
        )
    except Exception as error:
        logger.warning("Hermes Hub business tool request failed: %s", error, exc_info=True)
        return _tool_error(f"Hermes Hub business tool request failed: {error}")

    if isinstance(response, dict):
        result = response.get("result")
        if isinstance(result, str):
            return result
        if response.get("error"):
            return _tool_error(str(response["error"]))
    return _tool_error("Hermes Hub business tool request returned an invalid result")


def _normalize_message_line_breaks(message: str) -> str:
    if "MEDIA:" not in message:
        return message
    # Hermes 某些模型/tool-call 路径会把 JSON 字符串里的换行保留成字面量 \n。
    # MEDIA 必须独占一行才能按官方语义解析，因此只在含 MEDIA 指令时归一化换行。
    return (
        message.replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\r", "\n")
    )


def _extract_media_with_hub_fallback(message: str) -> tuple[list[tuple[str, bool]], str]:
    message = _normalize_message_line_breaks(message)
    # 正文清理优先使用官方整段 extract_media，保留它对空行/缩进的处理；
    # 附件顺序则逐行扫描原文，确保 fallback 的 .sh/.noext 不会被追加到末尾。
    official_media, official_cleaned = BasePlatformAdapter.extract_media(message)
    fallback_media, cleaned_message = _extract_remaining_media_lines(official_cleaned, message)
    if not fallback_media:
        return official_media, official_cleaned
    return _extract_media_in_original_order(message), cleaned_message


def _extract_media_in_original_order(message: str) -> list[tuple[str, bool]]:
    has_voice_tag = "[[audio_as_voice]]" in message
    media_files: list[tuple[str, bool]] = []

    for line in str(message or "").splitlines():
        official_line = f"[[audio_as_voice]]\n{line}" if has_voice_tag else line
        official_media, _official_cleaned = BasePlatformAdapter.extract_media(official_line)
        if official_media:
            media_files.extend(official_media)
            continue

        match = _MEDIA_LINE_RE.match(line)
        if not match:
            continue
        media_path = _clean_media_path(match.group("path"))
        if media_path:
            media_files.append((media_path, has_voice_tag))
    return media_files


def _extract_remaining_media_lines(
    cleaned_message: str,
    original_message: str,
) -> tuple[list[tuple[str, bool]], str]:
    has_voice_tag = "[[audio_as_voice]]" in original_message
    media_files: list[tuple[str, bool]] = []
    text_lines: list[str] = []
    for line in str(cleaned_message or "").splitlines():
        match = _MEDIA_LINE_RE.match(line)
        if not match:
            text_lines.append(line)
            continue
        media_path = _clean_media_path(match.group("path"))
        if media_path:
            media_files.append((media_path, has_voice_tag))
    cleaned = re.sub(r"\n{3,}", "\n\n", "\n".join(text_lines)).strip()
    return media_files, cleaned


def _clean_media_path(raw_path: str) -> str:
    media_path = str(raw_path or "").strip()
    if len(media_path) >= 2 and media_path[0] == media_path[-1] and media_path[0] in "`\"'":
        media_path = media_path[1:-1].strip()
    else:
        media_path = media_path.lstrip("`\"'").rstrip("`\"',.;:)}]")
    return os.path.expanduser(media_path)


async def _send_to_platform(
    chat_id: str,
    message: str,
    *,
    thread_id: Optional[str] = None,
    media_files: Optional[list[tuple[str, bool]]] = None,
    force_document: bool = False,
) -> dict[str, Any]:
    """按官方 _send_to_platform 的方式分块发送到 Hermes Hub 平台。

    官方工具在这里区分 Telegram/Discord/Matrix 等平台；Hub 只保留插件平台
    的 live adapter 路径，并补上官方插件路径缺失的媒体附件投递。
    """
    media_files = media_files or []

    # Platform message length limits: 对齐官方逻辑，优先读取平台注册时声明的长度。
    max_length = None
    try:
        from gateway.platform_registry import platform_registry

        entry = platform_registry.get("hermes_hub")
        if entry and getattr(entry, "max_message_length", 0) > 0:
            max_length = int(entry.max_message_length)
    except Exception:
        max_length = None

    adapter = _live_hub_adapter()
    if adapter is not None and not max_length:
        max_length = int(getattr(adapter, "MAX_MESSAGE_LENGTH", 0) or 0)

    if max_length:
        chunks = BasePlatformAdapter.truncate_message(str(message or "").strip(), max_length)
    else:
        chunks = [str(message or "").strip()]
    if not chunks:
        chunks = [""]

    last_result = None
    for index, chunk in enumerate(chunks):
        is_last = index == len(chunks) - 1
        result = await _send_via_adapter(
            chat_id,
            chunk,
            thread_id=thread_id,
            media_files=media_files if is_last else [],
            force_document=force_document,
        )
        if isinstance(result, dict) and result.get("error"):
            return result
        last_result = result
    return last_result or {"success": True, "message_id": ""}


def _message_chunks(adapter: Any, message: str) -> list[str]:
    """测试和排障用的官方式分块 helper。

    真实发送路径通过 _send_to_platform 读取 platform_registry；这个函数保留
    官方 adapter.MAX_MESSAGE_LENGTH 的简化入口，方便验证分块行为。
    """
    cleaned = str(message or "").strip()
    if not cleaned:
        return [""]
    max_length = int(getattr(adapter, "MAX_MESSAGE_LENGTH", 8000) or 8000)
    return BasePlatformAdapter.truncate_message(cleaned, max_length)


async def _send_via_adapter(
    chat_id: str,
    chunk: str,
    *,
    thread_id: Optional[str] = None,
    media_files: Optional[list[tuple[str, bool]]] = None,
    force_document: bool = False,
) -> dict[str, Any]:
    """官方 _send_via_adapter 的 Hermes Hub 专用版。

    官方插件平台分支只调用 ``adapter.send(...)``；Hub 在这里沿用同一个 live
    adapter 入口，但当 MEDIA 已被解析时转交给 adapter 的标准媒体方法。
    """
    adapter = _live_hub_adapter()
    if adapter is None:
        return {"error": "Hermes Hub adapter is not connected"}

    session_id = chat_id
    metadata = _hub_metadata(adapter, session_id, thread_id)
    media_files = media_files or []
    if media_files:
        # 这是官方插件平台路径缺失的能力：把 MEDIA 变成原生附件，而不是作为文本泄露。
        result = await _send_media_files(
            adapter,
            session_id,
            media_files,
            caption=str(chunk or "").strip(),
            metadata=metadata,
            force_document=force_document,
        )
    else:
        result = await _send_text_chunk(adapter, session_id, chunk, metadata)

    if isinstance(result, dict):
        return result
    if getattr(result, "success", False):
        return {"success": True, "message_id": getattr(result, "message_id", "")}
    return {"error": f"Adapter send failed: {getattr(result, 'error', '')}"}


def _hub_metadata(adapter: Any, session_id: str, thread_id: Optional[str] = None) -> dict[str, Any]:
    metadata = {
        "channel_id": session_id,
        "thread_id": thread_id or session_id,
    }
    run_id = _current_run_id_for(adapter, session_id)
    if run_id:
        metadata["run_id"] = run_id
    return metadata

async def _send_text_chunk(
    adapter: Any,
    session_id: str,
    chunk: str,
    metadata: dict[str, Any],
) -> Any:
    if not str(chunk or "").strip():
        return {"success": True, "message_id": ""}
    return await adapter.send(
        chat_id=session_id,
        content=chunk,
        metadata=dict(metadata),
    )


async def _send_media_files(
    adapter: Any,
    session_id: str,
    media_files: list[tuple[str, bool]],
    *,
    caption: str,
    metadata: dict[str, Any],
    force_document: bool,
) -> Any:
    last_result = None
    for index, (media_path, is_voice) in enumerate(media_files):
        item_metadata = dict(metadata)
        item_metadata["media_sequence"] = index
        item_caption = caption if index == 0 else None
        extension = os.path.splitext(str(media_path))[1].lower()
        if is_voice and extension in _VOICE_EXTS and hasattr(adapter, "send_voice"):
            last_result = await adapter.send_voice(
                chat_id=session_id,
                audio_path=media_path,
                caption=item_caption,
                metadata=item_metadata,
            )
        elif extension in _VIDEO_EXTS and hasattr(adapter, "send_video"):
            last_result = await adapter.send_video(
                chat_id=session_id,
                video_path=media_path,
                caption=item_caption,
                metadata=item_metadata,
            )
        elif not force_document and extension in _IMAGE_EXTS and hasattr(adapter, "send_image_file"):
            last_result = await adapter.send_image_file(
                chat_id=session_id,
                image_path=media_path,
                caption=item_caption,
                metadata=item_metadata,
            )
        elif hasattr(adapter, "send_document"):
            last_result = await adapter.send_document(
                chat_id=session_id,
                file_path=media_path,
                caption=item_caption,
                metadata=item_metadata,
            )
        else:
            return {"error": "Hermes Hub adapter does not support media attachments"}
        if not getattr(last_result, "success", False):
            return {"error": f"Adapter send failed: {getattr(last_result, 'error', '')}"}
    return last_result


def register(ctx: Any) -> None:
    ctx.register_tool(
        name="hermes_hub_send",
        toolset="hermes_hub",
        schema=HERMES_HUB_SEND_SCHEMA,
        handler=hermes_hub_send_tool,
        check_fn=_check_hub_send_available,
        emoji="",
    )
    ctx.register_tool(
        name="business_tool_request",
        toolset="hermes_hub",
        schema=HERMES_HUB_BUSINESS_TOOL_REQUEST_SCHEMA,
        handler=business_tool_request_tool,
        check_fn=_check_business_tool_request_available,
        emoji="",
    )
