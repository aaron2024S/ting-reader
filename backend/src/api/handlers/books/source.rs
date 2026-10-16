//! Source file deletion and explicit metadata-write tasks.

use crate::api::state::AppState;
use crate::core::app::error::{InputErrorKind, LookupErrorKind};
use crate::core::app::error::{Result, TingError};
use crate::core::storage::local_paths::{
    ensure_path_inside_root, resolve_existing_local_library_root,
};
use crate::core::task_queue::{Priority, Task, TaskPayload};
use crate::db::models::Book;
use crate::db::repository::Repository;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Deserialize, Default)]
pub struct DeleteBookQuery {
    #[serde(default)]
    pub delete_files: bool,
}

/// Handler for DELETE /api/v1/books/:id - Delete a book
pub async fn delete_book(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteBookQuery>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    let book = state.book_repo.find_by_id(&id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::Book,
            "delete_book::not_found",
            format!("Book with id {} not found", id),
        )
    })?;

    let library = state
        .library_repo
        .find_by_id(&book.library_id)
        .await
        .ok()
        .flatten();

    if query.delete_files {
        delete_local_book_source_files(&state, &book, library.as_ref()).await?;
    }

    // Cleanup cover if cached (WebDAV temp/cache covers)
    if let Some(cover_url) = &book.cover_url {
        let path_str = cover_url.replace('\\', "/");
        if path_str.contains("/temp/covers/") || path_str.contains("/storage/cache/covers/") {
            let path = std::path::Path::new(&path_str);
            if path.exists() {
                if let Err(e) = std::fs::remove_file(path) {
                    tracing::warn!(
                        path = %cover_url,
                        error = %e,
                        message_key = "library.cover_cache.delete_failed",
                        message_params = %serde_json::json!({
                            "path": cover_url,
                            "error": e.to_string(),
                        }),
                        "Failed to delete cover cache"
                    );
                } else {
                    tracing::info!("Deleted orphan cover cache: {}", cover_url);
                }
            }
        }
    }

    state.book_repo.delete(&id).await?;

    let book_title = book.title.clone().unwrap_or_else(|| "Unknown".to_string());
    let library_name = library.as_ref().map(|item| item.name.clone());

    tracing::info!(
        target: "audit::library",
        message_key = "book.deleted",
        message_params = %serde_json::json!({
            "actor": user.username.as_str(),
            "book_id": book.id.as_str(),
            "book_title": book_title.as_str(),
            "library_id": book.library_id.as_str(),
            "library_name": library_name.as_deref().unwrap_or(""),
        }),
        actor_id = %user.id,
        actor = %user.username,
        book_id = %book.id,
        book_title = %book_title,
        library_id = %book.library_id,
        library_name = %library_name.as_deref().unwrap_or(""),
        "Book deleted"
    );

    crate::core::notifications::dispatch_application_event(
        state.notification_repo.clone(),
        state.plugin_manager.clone(),
        crate::core::notifications::NotificationEventPayload::new(
            "book.deleted",
            "删除作品",
            format!("作品 {} 已删除", book_title),
            serde_json::json!({
                "actor_id": user.id,
                "actor": user.username,
                "book_id": book.id,
                "book_title": book_title,
                "author": book.author,
                "narrator": book.narrator,
                "library_id": book.library_id,
                "library_name": library_name,
            }),
        ),
    );

    Ok(StatusCode::NO_CONTENT)
}

async fn delete_local_book_source_files(
    state: &AppState,
    book: &Book,
    library: Option<&crate::db::models::Library>,
) -> Result<()> {
    let Some(library) = library else {
        return Err(TingError::not_found(
            LookupErrorKind::Book,
            "delete_local_book_source_files::not_found",
            format!("Library with id {} not found", book.library_id),
        ));
    };
    if library.library_type != "local" {
        return Ok(());
    }

    let config = state.config.read().await;
    let library_root = resolve_existing_local_library_root(library, &config)?;
    drop(config);

    let book_path = std::path::PathBuf::from(&book.path);
    if !book_path.exists() {
        tracing::warn!(
            book_id = %book.id,
            path = %book.path,
            "Requested source file deletion, but book path no longer exists"
        );
        return Ok(());
    }

    let book_dir = std::fs::canonicalize(&book_path)?;
    ensure_path_inside_root(&library_root, &book_dir)?;

    if book_dir == library_root {
        delete_book_files_inside_library_root(state, book, &book_dir, &library_root).await?;
        return Ok(());
    }

    if book_dir.is_dir() {
        std::fs::remove_dir_all(&book_dir)?;
    } else {
        std::fs::remove_file(&book_dir)?;
    }
    tracing::info!(
        book_id = %book.id,
        path = %book_dir.display(),
        "Deleted local source directory for book"
    );
    Ok(())
}

async fn delete_book_files_inside_library_root(
    state: &AppState,
    book: &Book,
    book_dir: &std::path::Path,
    library_root: &std::path::Path,
) -> Result<()> {
    let chapters = state.chapter_repo.find_by_book(&book.id).await?;
    let mut deleted_paths = HashSet::new();

    for chapter in chapters {
        let chapter_path = std::path::PathBuf::from(&chapter.path);
        delete_file_if_inside_root(&chapter_path, library_root, &mut deleted_paths)?;
    }

    for sidecar in [
        "book.nfo",
        "metadata.json",
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "folder.jpg",
        "folder.jpeg",
        "folder.png",
        "cover.webp",
        "folder.webp",
    ] {
        let path = book_dir.join(sidecar);
        delete_file_if_inside_root(&path, library_root, &mut deleted_paths)?;
    }

    if let Some(cover_url) = &book.cover_url
        && !cover_url.starts_with("http://")
        && !cover_url.starts_with("https://")
        && !cover_url.starts_with("//")
    {
        let cover_path = std::path::PathBuf::from(cover_url);
        let candidate = if cover_path.is_absolute() {
            cover_path
        } else {
            book_dir.join(cover_path)
        };
        delete_file_if_inside_root(&candidate, library_root, &mut deleted_paths)?;
    }

    tracing::info!(
        book_id = %book.id,
        count = deleted_paths.len(),
        "Deleted local source files for book in library root"
    );
    Ok(())
}

fn delete_file_if_inside_root(
    path: &std::path::Path,
    library_root: &std::path::Path,
    deleted_paths: &mut HashSet<std::path::PathBuf>,
) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let canonical = std::fs::canonicalize(path)?;
    ensure_path_inside_root(library_root, &canonical)?;
    if canonical.is_file() && deleted_paths.insert(canonical.clone()) {
        std::fs::remove_file(&canonical)?;
    }
    Ok(())
}

/// Handler for POST /api/v1/books/:id/write-metadata - Write metadata to audio files
pub async fn write_book_metadata_to_files(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    let book = state.book_repo.find_by_id(&id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::Book,
            "write_book_metadata_to_files::not_found",
            format!("Book with id {} not found", id),
        )
    })?;

    let library = state
        .library_repo
        .find_by_id(&book.library_id)
        .await?
        .ok_or_else(|| {
            TingError::not_found(
                LookupErrorKind::Book,
                "write_book_metadata_to_files::not_found",
                format!("Library with id {} not found", book.library_id),
            )
        })?;
    if !library.can_write_metadata_files() {
        return Err(TingError::validation(
            InputErrorKind::MissingField,
            "write_book_metadata_to_files::validation",
            "Metadata file writing requires a local library or an enabled WebDAV library"
                .to_string(),
        ));
    }

    // Create task
    let mut task = Task::new(
        format!("写入元数据: {}", book.title.unwrap_or_default()),
        Priority::Normal,
        TaskPayload::Custom {
            task_type: "write_metadata".to_string(),
            data: serde_json::json!({
                "book_id": id
            }),
        },
    );
    if library.library_type == "webdav" {
        // A retry must be explicitly requested after reviewing which remote
        // chapters completed, rather than rewriting successful chapters again.
        // The queue counts the initial failed attempt toward this limit.
        task.retry_policy.max_retries = 1;
        task.timeout = std::time::Duration::from_secs(86400);
    }

    let task_id = state.task_queue.submit(task).await?;

    Ok(Json(serde_json::json!({
        "message": "Metadata write task submitted",
        "task_id": task_id
    })))
}
