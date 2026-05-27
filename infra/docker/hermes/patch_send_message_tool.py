from pathlib import Path


tool_path = Path("/opt/hermes/tools/send_message_tool.py")
source = tool_path.read_text()

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
                # Plugin 平台的 live adapter 才知道如何把本地文件变成平台附件；
                # send_message 的 MEDIA: 路径必须交给 adapter 的媒体方法，而不是丢给纯文本 send。
                metadata = {"thread_id": thread_id or chat_id}
                if media_files:
                    last_result = None
                    for index, (media_path, _is_voice) in enumerate(media_files):
                        caption = chunk if index == 0 else None
                        extension = os.path.splitext(str(media_path))[1].lower()
                        if (
                            not force_document
                            and extension in _IMAGE_EXTS
                            and hasattr(adapter, "send_image_file")
                        ):
                            last_result = await adapter.send_image_file(
                                chat_id=chat_id,
                                image_path=media_path,
                                caption=caption,
                                metadata=metadata,
                            )
                        elif hasattr(adapter, "send_document"):
                            last_result = await adapter.send_document(
                                chat_id=chat_id,
                                file_path=media_path,
                                caption=caption,
                                metadata=metadata,
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

if old_live_adapter not in source:
    raise SystemExit("send_message live adapter block not found")
source = source.replace(old_live_adapter, new_live_adapter, 1)

if old_media_guard not in source:
    raise SystemExit("send_message media guard block not found")
source = source.replace(old_media_guard, new_media_guard, 1)

tool_path.write_text(source)
