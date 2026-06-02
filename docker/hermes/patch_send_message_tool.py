from pathlib import Path


tool_path = Path("/opt/hermes/tools/send_message_tool.py")
source = tool_path.read_text()

def replace_once(source: str, old: str, new: str, label: str) -> str:
    if old not in source:
        raise SystemExit(f"{label} not found")
    return source.replace(old, new, 1)

old_live_adapter = """        if adapter is not None:
            try:
                metadata = {"thread_id": thread_id} if thread_id else None
                result = await adapter.send(chat_id=chat_id, content=chunk, metadata=metadata)
            except asyncio.CancelledError:
                raise
            except Exception as e:
                return {"error": f"Plugin platform send failed: {e}"}
            if result.success:
                return {"success": True, "message_id": result.message_id}
            return {"error": f"Adapter send failed: {result.error}"}
"""

new_live_adapter = """        if adapter is not None:
            try:
                # Hermes Hub plugin media bridge:
                # 上游 send_message 已能解析 MEDIA:/path，但 plugin live adapter 分支只会调用
                # adapter.send(...)。这里只把已解析的 media_files 转给标准 adapter 媒体接口，
                # 不扩展 send_message schema，也不恢复旧 attachments/origin 协议。
                metadata = {"channel_id": chat_id}
                if thread_id:
                    metadata["thread_id"] = thread_id
                if media_files:
                    last_result = None
                    if chunk.strip():
                        last_result = await adapter.send(
                            chat_id=chat_id,
                            content=chunk,
                            metadata=metadata,
                        )
                        if not last_result.success:
                            return {"error": f"Adapter send failed: {last_result.error}"}
                    for index, (media_path, is_voice) in enumerate(media_files):
                        media_metadata = dict(metadata)
                        media_metadata["media_sequence"] = index
                        extension = os.path.splitext(str(media_path))[1].lower()
                        if (
                            is_voice
                            and extension in _VOICE_EXTS
                            and hasattr(adapter, "send_voice")
                        ):
                            last_result = await adapter.send_voice(
                                chat_id=chat_id,
                                audio_path=media_path,
                                metadata=media_metadata,
                            )
                        elif extension in _VIDEO_EXTS and hasattr(adapter, "send_video"):
                            last_result = await adapter.send_video(
                                chat_id=chat_id,
                                video_path=media_path,
                                metadata=media_metadata,
                            )
                        elif (
                            not force_document
                            and extension in _IMAGE_EXTS
                            and hasattr(adapter, "send_image_file")
                        ):
                            last_result = await adapter.send_image_file(
                                chat_id=chat_id,
                                image_path=media_path,
                                metadata=media_metadata,
                            )
                        elif hasattr(adapter, "send_document"):
                            last_result = await adapter.send_document(
                                chat_id=chat_id,
                                file_path=media_path,
                                metadata=media_metadata,
                            )
                        else:
                            return {"error": f"Plugin platform '{platform.value}' does not support media attachments"}
                        if not last_result.success:
                            return {"error": f"Adapter send failed: {last_result.error}"}
                    return {"success": True, "message_id": last_result.message_id if last_result else ""}
                result = await adapter.send(chat_id=chat_id, content=chunk, metadata=metadata)
            except asyncio.CancelledError:
                raise
            except Exception as e:
                return {"error": f"Plugin platform send failed: {e}"}
            if result.success:
                return {"success": True, "message_id": result.message_id}
            return {"error": f"Adapter send failed: {result.error}"}
"""

old_media_guard = """    # --- Non-media platforms ---
    if media_files and not message.strip():
        return {
            "error": (
                f"send_message MEDIA delivery is currently only supported for telegram, discord, matrix, weixin, signal, yuanbao and feishu; "
                f"target {platform.value} had only media attachments"
            )
        }
    warning = None
    if media_files:
        warning = (
            f"MEDIA attachments were omitted for {platform.value}; "
            "native send_message media delivery is currently only supported for telegram, discord, matrix, weixin, signal, yuanbao and feishu"
        )
"""

new_media_guard = """    # --- Non-media platforms ---
    plugin_entry = None
    try:
        from gateway.platform_registry import platform_registry
        plugin_entry = platform_registry.get(platform.value)
    except Exception:
        plugin_entry = None
    is_plugin_platform = plugin_entry is not None
    if media_files and not message.strip() and not is_plugin_platform:
        return {
            "error": (
                f"send_message MEDIA delivery is currently only supported for telegram, discord, matrix, weixin, signal, yuanbao and feishu; "
                f"target {platform.value} had only media attachments"
            )
        }
    warning = None
    if media_files and not is_plugin_platform:
        warning = (
            f"MEDIA attachments were omitted for {platform.value}; "
            "native send_message media delivery is currently only supported for telegram, discord, matrix, weixin, signal, yuanbao and feishu"
        )
"""

old_generic_loop = """    last_result = None
    for chunk in chunks:
"""

new_generic_loop = """    last_result = None
    for i, chunk in enumerate(chunks):
        is_last = i == len(chunks) - 1
"""

old_plugin_call = """            result = await _send_via_adapter(
                platform,
                pconfig,
                chat_id,
                chunk,
                thread_id=thread_id,
                media_files=media_files,
                force_document=force_document,
            )
"""

new_plugin_call = """            result = await _send_via_adapter(
                platform,
                pconfig,
                chat_id,
                chunk,
                thread_id=thread_id,
                media_files=media_files if is_last else [],
                force_document=force_document,
            )
"""

if "Hermes Hub plugin media bridge" not in source:
    for old, new, label in [
        (old_live_adapter, new_live_adapter, "send_message live adapter block"),
        (old_media_guard, new_media_guard, "send_message media guard block"),
        (old_generic_loop, new_generic_loop, "send_message generic chunk loop"),
        (old_plugin_call, new_plugin_call, "send_message plugin media call"),
    ]:
        source = replace_once(source, old, new, label)
    tool_path.write_text(source)
