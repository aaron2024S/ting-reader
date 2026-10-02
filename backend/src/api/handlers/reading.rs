use crate::api::state::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{Result, TingError};
use crate::db::repository::{Repository, reading::ReadingRepository};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;

#[derive(Default, Deserialize)]
pub struct ReadingPageQuery {
    pub page: Option<usize>,
    pub page_size: Option<usize>,
}
impl ReadingPageQuery {
    fn bounds(&self) -> (usize, usize) {
        (
            self.page.unwrap_or(1).clamp(1, 1_000_000),
            self.page_size.unwrap_or(40).clamp(1, 100),
        )
    }
}
fn repo(state: &AppState) -> ReadingRepository {
    ReadingRepository::new(state.book_repo.db().clone())
}
async fn ensure_access(state: &AppState, user: &AuthUser, book: &str) -> Result<()> {
    if state.book_repo.find_by_id(book).await?.is_none() {
        return Err(TingError::NotFound("Book not found".into()));
    }
    if !state
        .book_repo
        .check_access(book, &user.id, user.role == "admin")
        .await?
    {
        return Err(TingError::PermissionDenied("Book access denied".into()));
    }
    Ok(())
}
pub async fn history_books(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ReadingPageQuery>,
) -> Result<impl IntoResponse> {
    let (page, size) = query.bounds();
    Ok(Json(
        repo(&state)
            .activity_books(&user.id, user.role == "admin", false, page, size)
            .await?,
    ))
}

pub async fn history_summary(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse> {
    Ok(Json(
        repo(&state)
            .history_summary(&user.id, user.role == "admin")
            .await?,
    ))
}
pub async fn history_chapters(
    State(state): State<AppState>,
    user: AuthUser,
    Path(book): Path<String>,
    Query(query): Query<ReadingPageQuery>,
) -> Result<impl IntoResponse> {
    ensure_access(&state, &user, &book).await?;
    let (page, size) = query.bounds();
    Ok(Json(
        repo(&state)
            .history_chapters(&user.id, &book, page, size)
            .await?,
    ))
}
pub async fn bookmark_books(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ReadingPageQuery>,
) -> Result<impl IntoResponse> {
    let (page, size) = query.bounds();
    Ok(Json(
        repo(&state)
            .activity_books(&user.id, user.role == "admin", true, page, size)
            .await?,
    ))
}
pub async fn list_bookmarks(
    State(state): State<AppState>,
    user: AuthUser,
    Path(book): Path<String>,
    Query(query): Query<ReadingPageQuery>,
) -> Result<impl IntoResponse> {
    ensure_access(&state, &user, &book).await?;
    let (page, size) = query.bounds();
    Ok(Json(
        repo(&state).bookmarks(&user.id, &book, page, size).await?,
    ))
}
#[derive(Deserialize)]
pub struct MarkBooksRequest {
    pub book_ids: Vec<String>,
    pub read: bool,
}
pub async fn mark_books(
    State(state): State<AppState>,
    user: AuthUser,
    Json(request): Json<MarkBooksRequest>,
) -> Result<impl IntoResponse> {
    if request.book_ids.is_empty() || request.book_ids.len() > 200 {
        return Err(TingError::InvalidRequest(
            "Select between 1 and 200 books".into(),
        ));
    }
    for book in &request.book_ids {
        ensure_access(&state, &user, book).await?;
    }
    repo(&state)
        .mark_books(&user.id, request.book_ids, request.read)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
pub struct AddBookmarkRequest {
    pub book_id: String,
    pub chapter_id: String,
    pub position: f64,
    #[serde(default)]
    pub note: String,
}
fn validate_note(note: &str) -> Result<()> {
    if note.chars().count() > 2000 {
        return Err(TingError::InvalidRequest(
            "Bookmark note exceeds 2000 characters".into(),
        ));
    }
    Ok(())
}
pub async fn add_bookmark(
    State(state): State<AppState>,
    user: AuthUser,
    Json(request): Json<AddBookmarkRequest>,
) -> Result<impl IntoResponse> {
    ensure_access(&state, &user, &request.book_id).await?;
    validate_note(&request.note)?;
    if !request.position.is_finite() || request.position < 0.0 {
        return Err(TingError::InvalidRequest(
            "Invalid bookmark position".into(),
        ));
    }
    let chapter = state
        .chapter_repo
        .find_by_id(&request.chapter_id)
        .await?
        .ok_or_else(|| TingError::NotFound("Chapter not found".into()))?;
    if chapter.book_id != request.book_id {
        return Err(TingError::InvalidRequest(
            "Chapter does not belong to book".into(),
        ));
    }
    Ok((
        StatusCode::CREATED,
        Json(
            repo(&state)
                .add_bookmark(
                    &user.id,
                    request.book_id,
                    request.chapter_id,
                    request.position,
                    request.note,
                )
                .await?,
        ),
    ))
}
#[derive(Deserialize)]
pub struct EditBookmarkRequest {
    pub note: String,
}
async fn ensure_bookmark_access(state: &AppState, user: &AuthUser, id: &str) -> Result<()> {
    let book = repo(state)
        .bookmark_book(&user.id, id)
        .await?
        .ok_or_else(|| TingError::NotFound("Bookmark not found".into()))?;
    ensure_access(state, user, &book).await
}
pub async fn edit_bookmark(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(request): Json<EditBookmarkRequest>,
) -> Result<impl IntoResponse> {
    validate_note(&request.note)?;
    ensure_bookmark_access(&state, &user, &id).await?;
    repo(&state)
        .change_bookmark(&user.id, &id, Some(request.note))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn delete_bookmark(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    ensure_bookmark_access(&state, &user, &id).await?;
    repo(&state).change_bookmark(&user.id, &id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
