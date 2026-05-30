use axum::{
    body::Body,
    extract::{multipart::Field as MultipartField, Multipart, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use percent_encoding::percent_decode_str;
use std::{
    path::Path as StdPath,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::{
    channel::service::{
        ChannelAttachment, ChannelAttachmentDirection, ChannelAttachmentKind, ChannelStoreError,
        NewChannelAttachment,
    },
    http::{auth::current_user, ApiError},
    storage::{session_object_key, ObjectStorageError},
    AppState,
};

const INTERNAL_ATTACHMENT_METADATA_MAX_BYTES: usize = 64 * 1024;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/attachments/{attachment_id}/download",
        get(download_attachment),
    )
}

pub async fn upload_session_attachments(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    session_id: &str,
    direction: ChannelAttachmentDirection,
    mut multipart: Multipart,
) -> Result<Vec<ChannelAttachment>, ApiError> {
    let mut attachments = Vec::new();
    let max_upload_bytes = effective_attachment_upload_limit(state).await?;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::BadRequest("multipart body is invalid"))?
    {
        let mut file =
            spool_multipart_file_to_temp_with_limit(field, Some(max_upload_bytes)).await?;
        let attachment = create_session_attachment_from_file(
            state,
            user_id,
            channel_id,
            session_id,
            direction.clone(),
            file.file_name.clone(),
            file.content_type.clone(),
            file.size,
            file.path(),
        )
        .await;
        file.cleanup().await;
        attachments.push(attachment?);
    }

    Ok(attachments)
}

fn normalized_multipart_file_name(file_name: Option<&str>) -> String {
    let raw_name = file_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("attachment.bin");
    let raw_name = raw_name
        .strip_prefix("UTF-8''")
        .or_else(|| raw_name.strip_prefix("utf-8''"))
        .unwrap_or(raw_name);
    // 有些客户端会把 multipart filename 里的中文做百分号编码；
    // Hub 在入库边界恢复成人可读文件名，下载头再统一按 RFC 5987 编码。
    let decoded = percent_decode_str(raw_name)
        .decode_utf8()
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| raw_name.to_string());
    let base_name = StdPath::new(decoded.trim())
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("attachment.bin");

    base_name.to_string()
}

pub struct SpooledMultipartFile {
    path: Option<std::path::PathBuf>,
    pub file_name: String,
    pub content_type: String,
    pub size: u64,
}

struct TempPathGuard {
    path: Option<std::path::PathBuf>,
}

impl TempPathGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(mut self) -> std::path::PathBuf {
        self.path
            .take()
            .expect("temporary path should exist until guard is disarmed")
    }
}

impl Drop for TempPathGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl SpooledMultipartFile {
    pub fn path(&self) -> &StdPath {
        self.path
            .as_deref()
            .expect("spooled multipart file path should exist until cleanup")
    }

    pub async fn cleanup(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
}

impl Drop for SpooledMultipartFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub async fn read_multipart_text_field_with_limit(
    mut field: MultipartField<'_>,
    max_bytes: usize,
    too_large_message: &'static str,
) -> Result<String, ApiError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|_| ApiError::BadRequest("multipart body is invalid"))?
    {
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(ApiError::BadRequest(too_large_message))?;
        if next_len > max_bytes {
            return Err(ApiError::BadRequest(too_large_message));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| ApiError::BadRequest("multipart text field is invalid"))
}

pub async fn drain_multipart_field_with_limit(
    mut field: MultipartField<'_>,
    max_bytes: usize,
    too_large_message: &'static str,
) -> Result<(), ApiError> {
    let mut size = 0usize;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|_| ApiError::BadRequest("multipart body is invalid"))?
    {
        size = size
            .checked_add(chunk.len())
            .ok_or(ApiError::BadRequest(too_large_message))?;
        if size > max_bytes {
            return Err(ApiError::BadRequest(too_large_message));
        }
    }
    Ok(())
}

pub async fn spool_multipart_file_to_temp_with_limit(
    mut field: MultipartField<'_>,
    max_upload_bytes: Option<usize>,
) -> Result<SpooledMultipartFile, ApiError> {
    let file_name = normalized_multipart_file_name(field.file_name());
    let content_type = field
        .content_type()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let path = std::env::temp_dir().join(format!("hermes-hub-attachment-{}.part", Uuid::new_v4()));
    let mut file = create_private_spool_file(&path).await?;
    let path_guard = TempPathGuard::new(path);
    let mut size = 0u64;

    let write_result = async {
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| ApiError::BadRequest("attachment body is invalid"))?
        {
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or(ApiError::BadRequest("attachment body is invalid"))?;
            if max_upload_bytes.is_some_and(|limit| size > limit as u64) {
                return Err(ApiError::BadRequest("attachment is too large"));
            }
            file.write_all(&chunk)
                .await
                .map_err(|_| ApiError::Internal)?;
        }
        file.flush().await.map_err(|_| ApiError::Internal)?;
        Ok::<(), ApiError>(())
    }
    .await;
    if let Err(error) = write_result {
        return Err(error);
    }

    Ok(SpooledMultipartFile {
        path: Some(path_guard.disarm()),
        file_name,
        content_type,
        size,
    })
}

async fn create_private_spool_file(path: &StdPath) -> Result<tokio::fs::File, ApiError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        // multipart 临时文件可能承载私密图片/文档，不能依赖进程 umask。
        options.mode(0o600);
    }
    options.open(path).await.map_err(|_| ApiError::Internal)
}

pub async fn create_session_attachment_from_file(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    session_id: &str,
    direction: ChannelAttachmentDirection,
    file_name: String,
    content_type: String,
    size: u64,
    file_path: &StdPath,
) -> Result<ChannelAttachment, ApiError> {
    // 调用方负责选择是否套用上传大小限制；这里统一先校验所有权，
    // 避免非法请求在对象存储留下孤儿对象。
    state
        .channel_store
        .get_session(user_id, channel_id, session_id)
        .await
        .map_err(map_channel_error)?;

    let attachment_id = Uuid::new_v4().to_string();
    let object_key = session_object_key(
        &state.config.object_storage.prefix,
        user_id,
        session_id,
        &attachment_id,
        &file_name,
    );
    state
        .object_storage
        .put_file(&object_key, file_path)
        .await
        .map_err(map_storage_error)?;

    let kind = if content_type.starts_with("image/") {
        ChannelAttachmentKind::Image
    } else {
        ChannelAttachmentKind::File
    };
    match state
        .channel_store
        .create_attachment(
            user_id,
            channel_id,
            session_id,
            NewChannelAttachment {
                direction,
                bucket: state.object_storage.bucket().to_string(),
                object_key: object_key.clone(),
                name: file_name,
                content_type,
                size,
                kind,
            },
        )
        .await
    {
        Ok(attachment) => Ok(attachment),
        Err(error) => {
            let _ = state.object_storage.delete(&object_key).await;
            Err(map_channel_error(error))
        }
    }
}

pub async fn upload_session_attachments_for_context(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    session_id: &str,
    direction: ChannelAttachmentDirection,
    multipart: Multipart,
) -> Result<Vec<ChannelAttachment>, ApiError> {
    upload_session_attachments(state, user_id, channel_id, session_id, direction, multipart).await
}

pub async fn upload_session_attachments_from_instance(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    session_id: &str,
    direction: ChannelAttachmentDirection,
    mut multipart: Multipart,
) -> Result<Vec<ChannelAttachment>, ApiError> {
    let mut attachments = Vec::new();
    let max_upload_bytes = effective_attachment_upload_limit(state).await?;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::BadRequest("multipart body is invalid"))?
    {
        if field.file_name().is_none() {
            // 内部 adapter 偶尔会附带轻量元数据字段；文件字段仍严格执行系统配置的单文件限制。
            drain_multipart_field_with_limit(
                field,
                INTERNAL_ATTACHMENT_METADATA_MAX_BYTES,
                "multipart field is too large",
            )
            .await?;
            continue;
        }
        let mut file =
            spool_multipart_file_to_temp_with_limit(field, Some(max_upload_bytes)).await?;
        let attachment = create_session_attachment_from_file(
            state,
            user_id,
            channel_id,
            session_id,
            direction.clone(),
            file.file_name.clone(),
            file.content_type.clone(),
            file.size,
            file.path(),
        )
        .await;
        file.cleanup().await;
        attachments.push(attachment?);
    }

    Ok(attachments)
}

async fn download_attachment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(attachment_id): Path<String>,
) -> Result<Response, ApiError> {
    let user = current_user(&state, &headers).await?;
    let attachment = state
        .channel_store
        .get_attachment(&user.id, &attachment_id)
        .await
        .map_err(map_channel_error)?;
    ensure_attachment_not_expired(&state, &attachment).await?;
    let stream = state
        .object_storage
        .get_stream(&attachment.object_key)
        .await
        .map_err(map_storage_error)?;

    let mut response = (StatusCode::OK, Body::from_stream(stream)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&attachment.content_type).map_err(|_| ApiError::Internal)?,
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition_for_attachment(&attachment.name))
            .map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}

pub async fn effective_attachment_upload_limit(state: &AppState) -> Result<usize, ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(settings.max_attachment_upload_bytes)
}

pub async fn ensure_attachment_not_expired(
    state: &AppState,
    attachment: &ChannelAttachment,
) -> Result<(), ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    if attachment_is_expired(attachment.created_at, settings.attachment_retention_days) {
        return Err(ApiError::NotFound("attachment not found"));
    }
    Ok(())
}

fn attachment_is_expired(created_at: u64, retention_days: u32) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let retention_seconds = u64::from(retention_days).saturating_mul(24 * 60 * 60);
    now.saturating_sub(created_at) > retention_seconds
}

pub fn map_channel_error(error: ChannelStoreError) -> ApiError {
    match error {
        ChannelStoreError::ChannelNotFound | ChannelStoreError::AttachmentNotFound => {
            ApiError::NotFound("resource not found")
        }
        ChannelStoreError::InvalidSessionKind
        | ChannelStoreError::InvalidMessageRole
        | ChannelStoreError::InvalidAttachment
        | ChannelStoreError::InvalidRunStatus => ApiError::BadRequest("invalid channel request"),
        ChannelStoreError::RunNotFound => ApiError::NotFound("resource not found"),
        ChannelStoreError::SessionLimitExceeded { .. } => {
            ApiError::Conflict("session limit exceeded")
        }
        ChannelStoreError::LockFailed | ChannelStoreError::DatabaseFailed => ApiError::Internal,
    }
}

fn map_storage_error(error: ObjectStorageError) -> ApiError {
    match error {
        ObjectStorageError::NotFound => ApiError::NotFound("attachment not found"),
        ObjectStorageError::NotConfigured => ApiError::Internal,
        ObjectStorageError::LockFailed | ObjectStorageError::OperationFailed => {
            ApiError::BadGateway("object storage request failed")
        }
    }
}

fn safe_header_filename(name: &str) -> String {
    let value = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .to_string();

    if value.is_empty() {
        "attachment.bin".to_string()
    } else {
        value
    }
}

fn content_disposition_for_attachment(name: &str) -> String {
    let encoded = rfc5987_encode_filename(name);
    if name.trim().is_empty() {
        return format!("attachment; filename=\"attachment.bin\"; filename*=UTF-8''{encoded}");
    }

    if name.is_ascii() {
        let fallback = safe_header_filename(name);
        format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
    } else {
        // 非 ASCII 文件名不要再提供会把中文等名称压成下划线的 fallback，
        // 让支持 RFC 5987 的客户端直接使用 filename*。
        format!("attachment; filename*=UTF-8''{encoded}")
    }
}

fn rfc5987_encode_filename(name: &str) -> String {
    let source = if name.trim().is_empty() {
        "attachment.bin"
    } else {
        name.trim()
    };
    let mut encoded = String::new();

    for byte in source.as_bytes() {
        if is_rfc5987_attr_char(*byte) {
            encoded.push(char::from(*byte));
        } else {
            // filename* 只能使用 ASCII；非 ASCII 文件名按 UTF-8 字节百分号编码。
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }

    encoded
}

fn is_rfc5987_attr_char(byte: u8) -> bool {
    matches!(
        byte,
        b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'!'
            | b'#'
            | b'$'
            | b'&'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~'
    )
}

#[cfg(test)]
mod tests {
    use super::{attachment_is_expired, create_private_spool_file};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn attachment_expiration_uses_retention_days() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_secs();

        assert!(!attachment_is_expired(now - 6 * 24 * 60 * 60, 7));
        assert!(attachment_is_expired(now - 8 * 24 * 60 * 60, 7));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn private_spool_file_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir can be created");
        let path = dir.path().join("attachment.part");
        let file = create_private_spool_file(&path)
            .await
            .expect("private spool file can be created");
        drop(file);

        let mode = std::fs::metadata(&path)
            .expect("spool file metadata can be read")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
