use crate::core::app::error::LookupErrorKind;
mod chapters;
mod source;
pub use chapters::{batch_update_chapters, get_book_chapters, move_chapters, update_chapter};
pub use source::{DeleteBookQuery, delete_book, write_book_metadata_to_files};

mod cover;
pub mod scrape;

pub(crate) use cover::COVER_BODY_LIMIT;
pub use cover::upload_book_cover;
pub use scrape::{apply_scrape_result, scrape_book_diff};

use crate::api::models::{
    BookResponse, CreateBookRequest, MergeBooksRequest, SearchQuery, SearchResponse, StatsResponse,
    UpdateBookCorrectionRequest, UpdateBookRequest,
};
use crate::api::state::AppState;
use crate::core::app::error::{Result, TingError};
use crate::db::models::Book;
use crate::db::repository::Repository;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use uuid::Uuid;

/// Handler for GET /api/v1/books - List all books
pub async fn list_books(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    let search = params.get("search").cloned();
    let tag = params.get("tag").cloned();
    let library_id = params.get("library_id").cloned();
    let is_admin = user.role == "admin";

    let books = state
        .book_repo
        .find_with_filters(&user.id, is_admin, search, tag, library_id)
        .await?;

    let libraries = state.library_repo.find_all().await?;
    let library_info: std::collections::HashMap<String, (String, bool)> = libraries
        .into_iter()
        .map(|library| {
            let can_write = library.can_write_metadata_files();
            (library.id, (library.library_type, can_write))
        })
        .collect();

    let mut book_responses: Vec<BookResponse> = books.into_iter().map(BookResponse::from).collect();
    let percentages =
        crate::db::repository::reading::ReadingRepository::new(state.book_repo.db().clone())
            .percentages(&user.id)
            .await?;
    for r in &mut book_responses {
        if let Some((kind, can_write)) = library_info.get(&r.library_id) {
            r.library_type = Some(kind.clone());
            r.can_write_metadata_files = Some(*can_write);
        }
        r.progress_percent = percentages.get(&r.id).copied().unwrap_or(0.0);
    }

    Ok(Json(book_responses))
}

/// Handler for GET /api/v1/books/:id - Get book by ID
pub async fn get_book(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    let book = state.book_repo.find_by_id(&id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::Book,
            "get_book::not_found",
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
                "get_book::not_found",
                format!("Library with id {} not found", book.library_id),
            )
        })?;

    let is_fav = state.favorite_repo.is_favorited(&user.id, &id).await?;

    let mut response = BookResponse::from(book.clone());
    response.can_write_metadata_files = Some(library.can_write_metadata_files());
    response.library_type = Some(library.library_type);
    response.is_favorite = is_fav;
    response.progress_percent =
        crate::db::repository::reading::ReadingRepository::new(state.book_repo.db().clone())
            .percentages(&user.id)
            .await?
            .get(&id)
            .copied()
            .unwrap_or(0.0);

    Ok((StatusCode::OK, Json(response)))
}

/// Handler for POST /api/v1/books - Create a new book
pub async fn create_book(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Json(req): Json<CreateBookRequest>,
) -> Result<impl IntoResponse> {
    let book_id = Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();

    let mut theme_color = req.theme_color.clone();
    if theme_color.is_none()
        && let Some(ref url) = req.cover_url
    {
        let cover_path = if url.starts_with("http://") || url.starts_with("https://") {
            url.clone()
        } else {
            let path = std::path::Path::new(url);
            if path.is_absolute() {
                url.clone()
            } else {
                std::path::Path::new(&req.path)
                    .join(url)
                    .to_string_lossy()
                    .to_string()
            }
        };

        if let Ok(Some(color)) = crate::core::books::color::calculate_theme_color(&cover_path).await
        {
            theme_color = Some(color);
        }
    }

    let book = Book {
        id: book_id.clone(),
        library_id: req.library_id,
        title: req.title,
        author: req.author,
        narrator: req.narrator,
        cover_url: req.cover_url,
        theme_color,
        description: req.description,
        skip_intro: req.skip_intro,
        skip_outro: req.skip_outro,
        path: req.path,
        hash: req.hash,
        tags: req.tags,
        genre: None,
        year: None,
        created_at,
        manual_corrected: 0,
        match_pattern: None,
        chapter_regex: None,
    };

    state.book_repo.create(&book).await?;

    let library = state
        .library_repo
        .find_by_id(&book.library_id)
        .await
        .ok()
        .flatten();
    let book_title = book.title.clone().unwrap_or_else(|| "Unknown".to_string());
    let library_name = library.as_ref().map(|item| item.name.clone());

    tracing::info!(
        target: "audit::library",
        message_key = "book.created",
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
        "Book created"
    );

    crate::core::notifications::dispatch_application_event(
        state.notification_repo.clone(),
        state.plugin_manager.clone(),
        crate::core::notifications::NotificationEventPayload::new(
            "book.created",
            "作品入库",
            format!("作品 {} 已入库", book_title),
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

    Ok((StatusCode::CREATED, Json(BookResponse::from(book))))
}

/// Handler for PUT /api/v1/books/:id - Update a book
pub async fn update_book(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateBookRequest>,
) -> Result<impl IntoResponse> {
    let existing_book = state.book_repo.find_by_id(&id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::Book,
            "update_book::not_found",
            format!("Book with id {} not found", id),
        )
    })?;

    let mut theme_color = req.theme_color.clone();
    let book_path_str = req.path.clone().unwrap_or(existing_book.path.clone());
    let book_path = std::path::Path::new(&book_path_str);

    let cover_changed = if let Some(ref new_url) = req.cover_url {
        existing_book.cover_url.as_ref() != Some(new_url)
    } else {
        false
    };

    if theme_color.is_none() || cover_changed {
        if let Some(ref url) = req.cover_url {
            // If cover URL is provided, always recalculate theme color if not provided
            let cover_path = if url.starts_with("http://") || url.starts_with("https://") {
                url.clone()
            } else {
                let path = std::path::Path::new(url);
                if path.is_absolute() {
                    url.clone()
                } else {
                    book_path.join(url).to_string_lossy().to_string()
                }
            };

            match crate::core::books::color::calculate_theme_color(&cover_path).await {
                Ok(Some(color)) => {
                    theme_color = Some(color);
                }
                Ok(None) => {
                    // Try WebDAV if local/http failed and it's a webdav library
                    if let Ok(Some(library)) = state
                        .library_repo
                        .find_by_id(&existing_book.library_id)
                        .await
                        && library.library_type == "webdav"
                        && let Ok((mut reader, _)) = state
                            .storage_service
                            .get_webdav_reader(&library, url, None, state.encryption_key.as_ref())
                            .await
                    {
                        let mut buffer = Vec::new();
                        if tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut buffer)
                            .await
                            .is_ok()
                            && let Ok(Some(color)) =
                                crate::core::books::color::calculate_theme_color_from_bytes(&buffer)
                                    .await
                        {
                            theme_color = Some(color);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        message_key = "book.theme_color.calculate_failed",
                        message_params = %serde_json::json!({ "error": e.to_string() }),
                        "Book theme color calculation failed"
                    );
                }
            }
        } else {
            // If cover URL is NOT provided in request, keep existing theme color
            // UNLESS existing cover exists and theme color is missing
            theme_color = existing_book.theme_color.clone();
            if theme_color.is_none()
                && let Some(ref url) = existing_book.cover_url
            {
                let cover_path = if url.starts_with("http://") || url.starts_with("https://") {
                    url.clone()
                } else {
                    let path = std::path::Path::new(url);
                    if path.is_absolute() {
                        url.clone()
                    } else {
                        book_path.join(url).to_string_lossy().to_string()
                    }
                };

                match crate::core::books::color::calculate_theme_color(&cover_path).await {
                    Ok(Some(color)) => {
                        theme_color = Some(color);
                    }
                    Ok(None) => {
                        // Try WebDAV fallback for existing cover
                        if let Ok(Some(library)) = state
                            .library_repo
                            .find_by_id(&existing_book.library_id)
                            .await
                            && library.library_type == "webdav"
                            && let Ok((mut reader, _)) = state
                                .storage_service
                                .get_webdav_reader(
                                    &library,
                                    url,
                                    None,
                                    state.encryption_key.as_ref(),
                                )
                                .await
                        {
                            let mut buffer = Vec::new();
                            if tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut buffer)
                                .await
                                .is_ok()
                                && let Ok(Some(color)) =
                                    crate::core::books::color::calculate_theme_color_from_bytes(
                                        &buffer,
                                    )
                                    .await
                            {
                                theme_color = Some(color);
                            }
                        }
                    }
                    Err(_) => {}
                }
            }
        }
    }

    let mut updated_book = Book {
        id: existing_book.id,
        library_id: req.library_id.unwrap_or(existing_book.library_id),
        title: req.title.or(existing_book.title),
        author: req.author.or(existing_book.author),
        narrator: req.narrator.or(existing_book.narrator),
        cover_url: req.cover_url.or(existing_book.cover_url),
        theme_color,
        description: req.description.or(existing_book.description),
        skip_intro: req.skip_intro.unwrap_or(existing_book.skip_intro),
        skip_outro: req.skip_outro.unwrap_or(existing_book.skip_outro),
        path: req.path.unwrap_or(existing_book.path),
        hash: req.hash.unwrap_or(existing_book.hash),
        tags: req.tags.or(existing_book.tags),
        genre: req.genre.or(existing_book.genre),
        year: req.year.or(existing_book.year),
        created_at: existing_book.created_at,
        manual_corrected: existing_book.manual_corrected,
        match_pattern: existing_book.match_pattern,
        chapter_regex: req.chapter_regex.or(existing_book.chapter_regex),
    };

    // A manual metadata save locks the book by default. The detail editor may
    // explicitly turn the lock off; that choice must survive this same save.
    updated_book.manual_corrected = req
        .manual_corrected
        .map(|locked| if locked { 1 } else { 0 })
        .unwrap_or(1);
    updated_book.match_pattern = if updated_book.manual_corrected != 0 {
        updated_book
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(regex::escape)
    } else {
        None
    };

    state.book_repo.update(&updated_book).await?;

    state
        .metadata_service()
        .sync(
            &updated_book,
            crate::core::books::metadata::MetadataChange::Book,
            &Default::default(),
        )
        .await?;

    Ok(Json(BookResponse::from(updated_book)))
}

/// Handler for GET /api/v1/search - Search for books
pub async fn search_books(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<impl IntoResponse> {
    let result = state
        .scraper_service
        .search(
            &query.q,
            None,
            None,
            query.source.as_deref(),
            query.page,
            query.page_size,
        )
        .await?;

    Ok(Json(SearchResponse {
        items: result.items,
        total: result.total,
        has_more: result.has_more,
        page: result.page,
        page_size: result.page_size,
    }))
}

/// Handler for GET /api/v1/tags - Get all unique tags
pub async fn get_tags(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    let is_admin = user.role == "admin";

    // Use find_with_filters to respect user permissions
    let books = state
        .book_repo
        .find_with_filters(
            &user.id, is_admin, None, // search
            None, // tag
            None, // library_id
        )
        .await?;

    let mut tags_set = std::collections::HashSet::new();
    for book in books {
        if let Some(tags_str) = book.tags {
            for tag in tags_str.split(',') {
                let trimmed = tag.trim();
                if !trimmed.is_empty() {
                    tags_set.insert(trimmed.to_string());
                }
            }
        }
    }

    let mut tags: Vec<String> = tags_set.into_iter().collect();
    tags.sort();

    Ok(Json(tags))
}

/// Handler for GET /api/v1/stats - Get system statistics
pub async fn get_stats(State(state): State<AppState>) -> Result<impl IntoResponse> {
    let (total_books, total_chapters, total_duration) = state.book_repo.get_stats().await?;

    let libraries = state.library_repo.find_all().await?;
    let last_scan_time = libraries
        .iter()
        .filter_map(|l| l.last_scanned_at.as_ref())
        .max()
        .cloned();

    let stats = StatsResponse {
        total_books,
        total_chapters,
        total_duration,
        last_scan_time,
    };

    Ok(Json(stats))
}

// ============================================================================
// Merge System Handlers
// ============================================================================

/// Handler for POST /api/v1/books/merge - Merge two books
pub async fn merge_books(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Json(req): Json<MergeBooksRequest>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    state
        .merge_service
        .merge_books(&req.source_book_id, &req.target_book_id)
        .await?;

    Ok(Json(serde_json::json!({
        "message": "Books merged successfully",
        "result": ()
    })))
}

/// Handler for PUT /api/v1/books/:id/correction - Update book correction status
pub async fn update_book_correction(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: crate::auth::middleware::AuthUser,
    Json(req): Json<UpdateBookCorrectionRequest>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    state
        .merge_service
        .update_manual_correction(&id, req.manual_corrected, req.match_pattern)
        .await?;

    Ok(Json(serde_json::json!({
        "message": "Book correction status updated successfully"
    })))
}
