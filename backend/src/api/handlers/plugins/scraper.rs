//! Scraper discovery and search HTTP endpoints.

use crate::api::models::{ScraperSearchRequest, ScraperSourcesResponse, SearchResponse};
use crate::api::state::AppState;
use crate::core::app::error::Result;
use axum::{Json, extract::State, response::IntoResponse};

/// Handler for GET /api/v1/scraper/sources - Get list of scraper sources
pub async fn get_scraper_sources(State(state): State<AppState>) -> Result<impl IntoResponse> {
    let sources = state.scraper_service.get_sources().await;

    Ok(Json(ScraperSourcesResponse { sources }))
}

/// Handler for POST /api/v1/scraper/search - Search for books using scraper
pub async fn scraper_search(
    State(state): State<AppState>,
    Json(request): Json<ScraperSearchRequest>,
) -> Result<impl IntoResponse> {
    let page = request.page.unwrap_or(1);
    let page_size = request.page_size.unwrap_or(20);
    let mut search_params = request.search_params.unwrap_or_default();
    if let Some(query) = request.query {
        search_params.entry("title".to_string()).or_insert(query);
    }
    if let Some(author) = request.author {
        search_params.entry("author".to_string()).or_insert(author);
    }
    if let Some(narrator) = request.narrator {
        search_params
            .entry("narrator".to_string())
            .or_insert(narrator);
    }

    let result = state
        .scraper_service
        .search_with_params(&search_params, request.source.as_deref(), page, page_size)
        .await?;

    Ok(Json(SearchResponse {
        items: result.items,
        total: result.total,
        has_more: result.has_more,
        page: result.page,
        page_size: result.page_size,
    }))
}
