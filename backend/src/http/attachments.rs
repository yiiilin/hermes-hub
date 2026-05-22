use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use bytes::Bytes;
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

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::BadRequest("multipart body is invalid"))?
    {
        let file_name = field
            .file_name()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "attachment.bin".to_string());
        let content_type = field
            .content_type()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let bytes = field
            .bytes()
            .await
            .map_err(|_| ApiError::BadRequest("attachment body is invalid"))?;

        if bytes.len() > state.config.object_storage.max_upload_bytes {
            return Err(ApiError::BadRequest("attachment is too large"));
        }

        let attachment = create_session_attachment_from_bytes(
            state,
            user_id,
            channel_id,
            session_id,
            direction.clone(),
            file_name,
            content_type,
            bytes,
        )
        .await?;
        attachments.push(attachment);
    }

    Ok(attachments)
}

pub async fn create_session_attachment_from_bytes(
    state: &AppState,
    user_id: &str,
    channel_id: &str,
    session_id: &str,
    direction: ChannelAttachmentDirection,
    file_name: String,
    content_type: String,
    bytes: Bytes,
) -> Result<ChannelAttachment, ApiError> {
    if bytes.len() > state.config.object_storage.max_upload_bytes {
        return Err(ApiError::BadRequest("attachment is too large"));
    }
    // 先校验 session 所有权，再写对象存储，避免非法请求在 S3/RustFS 留下孤儿对象。
    state
        .channel_store
        .get_session(user_id, channel_id, session_id)
        .await
        .map_err(map_channel_error)?;

    // 所有附件统一进入对象存储，消息表只保存附件元数据和下载入口。
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
        .put(&object_key, Bytes::copy_from_slice(&bytes))
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
                size: bytes.len() as u64,
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
    let bytes = state
        .object_storage
        .get(&attachment.object_key)
        .await
        .map_err(map_storage_error)?;

    let mut response = (StatusCode::OK, Body::from(bytes)).into_response();
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
