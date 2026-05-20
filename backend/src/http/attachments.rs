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
        let attachment = state
            .channel_store
            .create_attachment(
                user_id,
                channel_id,
                session_id,
                NewChannelAttachment {
                    direction: direction.clone(),
                    bucket: state.object_storage.bucket().to_string(),
                    object_key,
                    name: file_name,
                    content_type,
                    size: bytes.len() as u64,
                    kind,
                },
            )
            .await
            .map_err(map_channel_error)?;
        attachments.push(attachment);
    }

    Ok(attachments)
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
        HeaderValue::from_str(&format!(
            "attachment; filename=\"{}\"",
            safe_header_filename(&attachment.name)
        ))
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
        | ChannelStoreError::InvalidAttachment => ApiError::BadRequest("invalid channel request"),
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
