use super::{AppState, update_book};
use crate::api::models::UpdateBookRequest;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{Result, TingError};
use crate::core::storage::local_paths::{
    ensure_path_inside_root, path_to_display_string, resolve_existing_local_library_root,
};
use crate::db::repository::Repository;
use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use image::ImageDecoder;
use std::io::Cursor;

const MAX_COVER_BYTES: usize = 10 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024;
pub(crate) const COVER_BODY_LIMIT: usize = MAX_COVER_BYTES + MAX_METADATA_BYTES + 16 * 1024;

/// Save an uploaded cover and the accompanying metadata in one editor action.
pub async fn upload_book_cover(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: AuthUser,
    mut multipart: Multipart,
) -> Result<Response> {
    if user.role != "admin" {
        return Err(TingError::PermissionDenied(
            "Only administrators can upload book covers".to_string(),
        ));
    }
    let book = state
        .book_repo
        .find_by_id(&id)
        .await?
        .ok_or_else(|| TingError::NotFound(format!("Book with id {id} not found")))?;
    let library = state
        .library_repo
        .find_by_id(&book.library_id)
        .await?
        .ok_or_else(|| TingError::NotFound("Book library not found".to_string()))?;

    let mut image_bytes = None;
    let mut metadata_bytes = None;
    while let Some(mut field) = match multipart.next_field().await {
        Ok(field) => field,
        Err(error) => return Ok(error.into_response()),
    } {
        let (limit, destination) = match field.name() {
            Some("file") if image_bytes.is_none() => (MAX_COVER_BYTES, &mut image_bytes),
            Some("metadata") if metadata_bytes.is_none() => {
                (MAX_METADATA_BYTES, &mut metadata_bytes)
            }
            _ => {
                return Err(TingError::InvalidRequest(
                    "Expected one cover file and optional metadata".to_string(),
                ));
            }
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = match field.chunk().await {
            Ok(chunk) => chunk,
            Err(error) => return Ok(error.into_response()),
        } {
            if bytes.len() + chunk.len() > limit {
                return Ok((
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(serde_json::json!({
                        "error": "PayloadTooLarge",
                        "message": "Uploaded cover or metadata exceeds the size limit",
                    })),
                )
                    .into_response());
            }
            bytes.extend_from_slice(&chunk);
        }
        *destination = Some(bytes);
    }
    let bytes = image_bytes
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(|| TingError::InvalidRequest("A cover image is required".to_string()))?;
    let mut metadata: UpdateBookRequest =
        serde_json::from_slice(metadata_bytes.as_deref().unwrap_or(b"{}"))
            .map_err(|error| TingError::InvalidRequest(format!("Invalid metadata: {error}")))?;
    // File placement is determined by the existing book, never multipart paths.
    if metadata.library_id.is_some() || metadata.path.is_some() || metadata.hash.is_some() {
        return Err(TingError::InvalidRequest(
            "Cover uploads cannot move a book or change its library".to_string(),
        ));
    }
    let jpeg = tokio::task::spawn_blocking(move || normalize_cover(bytes))
        .await
        .map_err(|error| {
            TingError::InvalidRequest(format!("Image processing failed: {error}"))
        })??;

    let target_dir = match library.library_type.as_str() {
        "local" => {
            let config = state.config.read().await;
            let root = resolve_existing_local_library_root(&library, &config)?;
            let path = std::path::Path::new(&book.path);
            let candidate = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            let canonical = tokio::fs::canonicalize(candidate).await?;
            ensure_path_inside_root(&root, &canonical)?;
            if !canonical.is_dir() {
                return Err(TingError::InvalidRequest(
                    "The book path must be a directory".to_string(),
                ));
            }
            canonical
        }
        "webdav" | "rss" => crate::core::books::metadata_writer::remote_metadata_dir(&book.path)?,
        _ => {
            return Err(TingError::InvalidRequest(
                "This library does not support cover uploads".to_string(),
            ));
        }
    };
    tokio::fs::create_dir_all(&target_dir).await?;
    let target = target_dir.join("cover.jpg");
    let staging = target_dir.join(format!(".cover-{}.tmp", uuid::Uuid::new_v4()));
    let write_result = async {
        tokio::fs::write(&staging, &jpeg).await?;
        tokio::fs::rename(&staging, &target).await
    }
    .await;
    if let Err(error) = write_result {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(TingError::IoError(error));
    }
    metadata.cover_url = Some(path_to_display_string(&target));
    // Recalculate even when replacing cover.jpg at the same path.
    metadata.theme_color = None;
    Ok(update_book(State(state), Path(id), Json(metadata))
        .await?
        .into_response())
}

fn normalize_cover(bytes: Vec<u8>) -> Result<Vec<u8>> {
    let format = image::guess_format(&bytes)
        .map_err(|_| TingError::InvalidRequest("Invalid cover image".to_string()))?;
    if !matches!(
        format,
        image::ImageFormat::Jpeg | image::ImageFormat::Png | image::ImageFormat::WebP
    ) {
        return Err(TingError::InvalidRequest(
            "Cover images must be JPEG, PNG or WebP".to_string(),
        ));
    }
    let (width, height) = image::ImageReader::with_format(Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|error| TingError::InvalidRequest(format!("Invalid cover image: {error}")))?;
    if u64::from(width) * u64::from(height) > 16_000_000 {
        return Err(TingError::InvalidRequest(
            "Cover images must not exceed 16 megapixels".to_string(),
        ));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| TingError::InvalidRequest(format!("Invalid cover image: {error}")))?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut decoded = image::DynamicImage::from_decoder(decoder)
        .map_err(|error| TingError::InvalidRequest(format!("Invalid cover image: {error}")))?;
    decoded.apply_orientation(orientation);
    let mut image = decoded.into_rgba8();
    // JPEG has no alpha channel; composite transparent input onto white.
    for pixel in image.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u32::from(*channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
        }
        pixel[3] = 255;
    }
    let rgb = image::DynamicImage::ImageRgba8(image).into_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&rgb)
        .map_err(|error| TingError::InvalidRequest(format!("Cover encoding failed: {error}")))?;
    Ok(jpeg)
}

#[cfg(test)]
#[path = "../../../../tests/unit/api/handlers/books/cover.rs"]
mod tests;
