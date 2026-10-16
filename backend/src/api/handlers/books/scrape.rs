use crate::core::app::error::{InputErrorKind, LookupErrorKind};
use crate::core::books::metadata::{MetadataChange, MetadataPatch};

use crate::api::models::{BookResponse, ScrapeApplyRequest, ScrapeDiffRequest, ScrapeDiffResponse};
use crate::api::state::AppState;
use crate::core::app::error::{Result, TingError};
use crate::db::models::ScraperConfig;
use crate::db::repository::Repository;
use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};

/// Handler for POST /api/v1/books/:id/scrape-diff - Get scrape diff
pub async fn scrape_book_diff(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: crate::auth::middleware::AuthUser,
    Json(req): Json<ScrapeDiffRequest>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    let existing_book = state.book_repo.find_by_id(&id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::Book,
            "scrape_book_diff::not_found",
            format!("Book with id {} not found", id),
        )
    })?;

    // 1. Get Library Config
    let library = state
        .library_repo
        .find_by_id(&existing_book.library_id)
        .await?
        .ok_or_else(|| {
            TingError::not_found(
                LookupErrorKind::Book,
                "scrape_book_diff::not_found",
                format!("Library with id {} not found", existing_book.library_id),
            )
        })?;

    let config: ScraperConfig = if let Some(json) = &library.scraper_config {
        serde_json::from_str(json).unwrap_or_default()
    } else {
        ScraperConfig::default()
    };

    // 2. Determine Primary Source (from Default List or First Enabled)
    let sources = state.scraper_service.get_sources().await;

    // Find first enabled source from config defaults, or just first enabled system-wide
    let mut primary_source_id = None;
    for source_id in &config.default_sources {
        if let Some(s) = sources.iter().find(|s| s.id == *source_id && s.enabled) {
            primary_source_id = Some(s.id.clone());
            break;
        }
    }

    if primary_source_id.is_none() {
        primary_source_id = sources
            .iter()
            .find(|s| s.enabled && !s.aggregate_auto_scrape)
            .map(|s| s.id.clone());
    }

    let primary_source_id = match primary_source_id {
        Some(s) => s,
        None => {
            return Err(TingError::plugin_not_found(
                LookupErrorKind::Plugin,
                "scrape_book_diff::plugin_not_found",
                "No active scraper plugins found".to_string(),
            ));
        }
    };

    // 3. Search and Get Detail from Primary Source
    // Use scraper service search to find match by title
    let search_results = state
        .scraper_service
        .search(
            &req.query,
            req.author.as_deref(),
            req.narrator.as_deref(),
            Some(&primary_source_id),
            1,
            1,
        )
        .await?;

    if search_results.items.is_empty() {
        return Err(TingError::not_found(
            LookupErrorKind::Book,
            "scrape_book_diff::not_found",
            "No matching book found in scrapers".to_string(),
        ));
    }

    let best_match = &search_results.items[0];
    // We use search result directly as it now contains full metadata
    let mut detail = crate::plugin::types::scraper::BookDetail::from(best_match);

    // 4. Handle Specific Field Sources (Merge Strategy)
    // Helper to fetch and merge specific field
    // We assume the search query is the same (Book Title) for other sources too.

    // Check Author Source
    if let Some(srcs) = &config.author_sources
        && !srcs.is_empty()
        && srcs[0] != primary_source_id
        && let Some(s_id) = sources
            .iter()
            .find(|s| s.id == srcs[0] && s.enabled)
            .map(|s| s.id.clone())
        && let Ok(res) = state
            .scraper_service
            .search(
                &req.query,
                req.author.as_deref(),
                req.narrator.as_deref(),
                Some(&s_id),
                1,
                1,
            )
            .await
        && !res.items.is_empty()
    {
        let item = &res.items[0];
        if !item.author.is_empty() {
            detail.author = item.author.clone();
        }
    }

    // Check Narrator Source
    if let Some(srcs) = &config.narrator_sources
        && !srcs.is_empty()
        && srcs[0] != primary_source_id
        && let Some(s_id) = sources
            .iter()
            .find(|s| s.id == srcs[0] && s.enabled)
            .map(|s| s.id.clone())
        && let Ok(res) = state
            .scraper_service
            .search(
                &req.query,
                req.author.as_deref(),
                req.narrator.as_deref(),
                Some(&s_id),
                1,
                1,
            )
            .await
        && !res.items.is_empty()
    {
        let item = &res.items[0];
        if item.narrator.is_some() {
            detail.narrator = item.narrator.clone();
        }
    }

    // Check Cover Source
    if let Some(srcs) = &config.cover_sources
        && !srcs.is_empty()
        && srcs[0] != primary_source_id
        && let Some(s_id) = sources
            .iter()
            .find(|s| s.id == srcs[0] && s.enabled)
            .map(|s| s.id.clone())
        && let Ok(res) = state
            .scraper_service
            .search(
                &req.query,
                req.author.as_deref(),
                req.narrator.as_deref(),
                Some(&s_id),
                1,
                1,
            )
            .await
        && !res.items.is_empty()
    {
        let item = &res.items[0];
        if item.cover_url.is_some() {
            detail.cover_url = item.cover_url.clone();
        }
    }

    // Check Intro Source
    if let Some(srcs) = &config.intro_sources
        && !srcs.is_empty()
        && srcs[0] != primary_source_id
        && let Some(s_id) = sources
            .iter()
            .find(|s| s.id == srcs[0] && s.enabled)
            .map(|s| s.id.clone())
        && let Ok(res) = state
            .scraper_service
            .search(
                &req.query,
                req.author.as_deref(),
                req.narrator.as_deref(),
                Some(&s_id),
                1,
                1,
            )
            .await
        && !res.items.is_empty()
    {
        let item = &res.items[0];
        if let Some(intro) = &item.intro
            && !intro.is_empty()
        {
            detail.intro = intro.clone();
        }
    }

    // Check Tags Source
    if let Some(srcs) = &config.tags_sources
        && !srcs.is_empty()
        && srcs[0] != primary_source_id
        && let Some(s_id) = sources
            .iter()
            .find(|s| s.id == srcs[0] && s.enabled)
            .map(|s| s.id.clone())
        && let Ok(res) = state
            .scraper_service
            .search(
                &req.query,
                req.author.as_deref(),
                req.narrator.as_deref(),
                Some(&s_id),
                1,
                1,
            )
            .await
        && !res.items.is_empty()
    {
        let item = &res.items[0];
        if !item.tags.is_empty() {
            detail.tags = item.tags.clone();
        }
    }

    // Construct ScrapeMetadata for current book
    let current_meta = crate::api::models::books::ScrapeMetadata {
        title: existing_book.title.clone().unwrap_or_default(),
        author: existing_book.author.clone().unwrap_or_default(),
        narrator: existing_book.narrator.clone().unwrap_or_default(),
        description: existing_book.description.clone().unwrap_or_default(),
        cover_url: existing_book.cover_url.clone(),
        tags: existing_book.tags.map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        }),
        genre: existing_book.genre.clone(),
    };

    // Construct ScrapeMetadata for scraped detail
    let clean_cover_url = detail.cover_url.clone();

    let scraped_meta = crate::api::models::books::ScrapeMetadata {
        title: detail.title.clone(),
        author: detail.author.clone(),
        narrator: detail.narrator.clone().unwrap_or_default(),
        description: detail.intro.clone(),
        cover_url: clean_cover_url,
        tags: if detail.tags.is_empty() {
            None
        } else {
            Some(detail.tags.clone())
        },
        genre: detail.genre.clone(),
    };

    // Chapter changes (Not implemented yet, returning empty list)
    // To implement this, we need ScraperService to return chapter list
    let chapter_changes = Vec::new();

    Ok(Json(ScrapeDiffResponse {
        current: current_meta,
        scraped: scraped_meta,
        chapter_changes,
    }))
}

/// Handler for POST /api/v1/books/:id/scrape-apply - Apply scrape result
pub async fn apply_scrape_result(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: crate::auth::middleware::AuthUser,
    Json(req): Json<ScrapeApplyRequest>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    let mut book = state.book_repo.find_by_id(&id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::Book,
            "apply_scrape_result::not_found",
            format!("Book with id {} not found", id),
        )
    })?;

    if let Some(fields) = &req.fields {
        let mut extended = MetadataPatch::default();

        for (field, selection) in fields {
            match field.as_str() {
                "title" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        book.title = Some(value);
                    }
                }
                "author" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        book.author = Some(value);
                    }
                }
                "narrator" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        book.narrator = Some(value);
                    }
                }
                "description" | "intro" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        book.description = Some(value);
                    }
                }
                "cover_url" | "coverUrl" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        book.cover_url = Some(value.clone());
                        recalculate_cover_theme_color(&state, &mut book, &value).await;
                    }
                }
                "tags" => {
                    if let Some(value) = scrape_value_to_tags(&selection.value) {
                        book.tags = Some(value);
                    }
                }
                "genre" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        book.genre = Some(value);
                    }
                }
                "year" | "published_year" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        if let Ok(year) = value.parse::<i32>() {
                            book.year = Some(year);
                        }
                        extended.published_year = Some(value);
                    } else if let Some(year) = selection.value.as_i64() {
                        book.year = Some(year as i32);
                        extended.published_year = Some(year.to_string());
                    }
                }
                "subtitle" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        extended.subtitle = Some(value);
                    }
                }
                "published_date" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        extended.published_date = Some(value);
                    }
                }
                "publisher" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        extended.publisher = Some(value);
                    }
                }
                "isbn" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        extended.isbn = Some(value);
                    }
                }
                "asin" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        extended.asin = Some(value);
                    }
                }
                "language" => {
                    if let Some(value) = scrape_value_to_string(&selection.value) {
                        extended.language = Some(value);
                    }
                }
                "explicit" => {
                    if let Some(value) = scrape_value_to_bool(&selection.value) {
                        extended.explicit = Some(value);
                    }
                }
                "abridged" => {
                    if let Some(value) = scrape_value_to_bool(&selection.value) {
                        extended.abridged = Some(value);
                    }
                }
                "duration" => {
                    if let Some(value) = scrape_value_to_u64(&selection.value) {
                        extended.duration = Some(value);
                    }
                }
                _ => {}
            }
        }

        state.book_repo.update(&book).await?;
        sync_manual_scrape_lock(&state, &mut book).await?;
        state
            .metadata_service()
            .sync(&book, MetadataChange::Book, &extended)
            .await?;

        return Ok(Json(BookResponse::from(book)));
    }

    if req.apply_metadata {
        let detail = req.metadata.as_ref().ok_or_else(|| {
            TingError::validation(
                InputErrorKind::MissingField,
                "apply_scrape_result::validation",
                "metadata is required when apply_metadata is true".to_string(),
            )
        })?;

        if !detail.title.is_empty() {
            book.title = Some(detail.title.clone());
        }
        if !detail.author.is_empty() {
            book.author = Some(detail.author.clone());
        }
        if let Some(n) = &detail.narrator {
            book.narrator = Some(n.clone());
        }
        if !detail.intro.is_empty() {
            book.description = Some(detail.intro.clone());
        }
        if !detail.tags.is_empty() {
            book.tags = Some(detail.tags.join(","));
        }
        if let Some(g) = &detail.genre {
            book.genre = Some(g.clone());
        }
        if let Some(url) = &detail.cover_url {
            book.cover_url = Some(url.clone());

            // Handle referer for internal processing if present
            let mut internal_url = url.clone();
            if let Some(idx) = internal_url.find("#referer=") {
                internal_url = internal_url[..idx].to_string();
            }

            // Recalculate theme color for new cover
            match crate::core::books::color::calculate_theme_color(url).await {
                Ok(Some(color)) => {
                    tracing::info!("Updated theme color for book {}: {}", book.id, color);
                    book.theme_color = Some(color);
                }
                Ok(None) => {
                    // Try WebDAV if local/http failed and it's a webdav library
                    if let Ok(Some(library)) = state.library_repo.find_by_id(&book.library_id).await
                        && library.library_type == "webdav"
                        && let Ok((mut reader, _)) = state
                            .storage_service
                            .get_webdav_reader(
                                &library,
                                &internal_url,
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
                                crate::core::books::color::calculate_theme_color_from_bytes(&buffer)
                                    .await
                        {
                            book.theme_color = Some(color);
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
        }

        state.book_repo.update(&book).await?;
        sync_manual_scrape_lock(&state, &mut book).await?;
    }

    // Even a manual save without selected metadata fields establishes ownership
    // of the current book metadata and protects it from later automatic scans.
    sync_manual_scrape_lock(&state, &mut book).await?;

    let patch = req
        .metadata
        .as_ref()
        .filter(|_| req.apply_metadata)
        .map(|detail| MetadataPatch {
            subtitle: detail.subtitle.clone(),
            published_year: detail.published_year.clone(),
            published_date: detail.published_date.clone(),
            publisher: detail.publisher.clone(),
            isbn: detail.isbn.clone(),
            asin: detail.asin.clone(),
            language: detail.language.clone(),
            explicit: Some(detail.explicit),
            abridged: Some(detail.abridged),
            duration: None,
        })
        .unwrap_or_default();
    state
        .metadata_service()
        .sync(&book, MetadataChange::Book, &patch)
        .await?;

    // Handle chapter updates if any (req.apply_chapters)
    // Since we don't have scraped chapters yet, we skip this for now.

    Ok(Json(BookResponse::from(book)))
}

fn scrape_value_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn scrape_value_to_tags(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Array(items) => {
            let tags: Vec<String> = items
                .iter()
                .filter_map(scrape_value_to_string)
                .filter(|s| !s.trim().is_empty())
                .collect();
            if tags.is_empty() {
                None
            } else {
                Some(tags.join(","))
            }
        }
        _ => scrape_value_to_string(value),
    }
}

fn scrape_value_to_bool(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(value) => Some(*value),
        serde_json::Value::String(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "true" | "1" | "yes" | "y" | "是" => Some(true),
                "false" | "0" | "no" | "n" | "否" => Some(false),
                _ => None,
            }
        }
        serde_json::Value::Number(value) => value.as_i64().map(|value| value != 0),
        _ => None,
    }
}

fn scrape_value_to_u64(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(value) => value.as_u64(),
        serde_json::Value::String(value) => value.trim().parse::<u64>().ok(),
        _ => None,
    }
}

async fn recalculate_cover_theme_color(
    state: &AppState,
    book: &mut crate::db::models::Book,
    url: &str,
) {
    let mut internal_url = url.to_string();
    if let Some(idx) = internal_url.find("#referer=") {
        internal_url = internal_url[..idx].to_string();
    }

    match crate::core::books::color::calculate_theme_color(url).await {
        Ok(Some(color)) => {
            book.theme_color = Some(color);
        }
        Ok(None) => {
            if let Ok(Some(library)) = state.library_repo.find_by_id(&book.library_id).await
                && library.library_type == "webdav"
                && let Ok((mut reader, _)) = state
                    .storage_service
                    .get_webdav_reader(&library, &internal_url, None, state.encryption_key.as_ref())
                    .await
            {
                let mut buffer = Vec::new();
                if tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut buffer)
                    .await
                    .is_ok()
                    && let Ok(Some(color)) =
                        crate::core::books::color::calculate_theme_color_from_bytes(&buffer).await
                {
                    book.theme_color = Some(color);
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
}

async fn sync_manual_scrape_lock(
    state: &AppState,
    book: &mut crate::db::models::Book,
) -> Result<()> {
    let match_pattern = book
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(regex::escape);

    state
        .merge_service
        .update_manual_correction(&book.id, true, match_pattern)
        .await?;

    if let Some(updated_book) = state.book_repo.find_by_id(&book.id).await? {
        *book = updated_book;
    }

    Ok(())
}
