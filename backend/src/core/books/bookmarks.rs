//! User-scoped book bookmarks shared by HTTP and plugin Host calls.

use crate::core::app::error::{Result, TingError};
use crate::db::repository::reading::{ActivityBook, Bookmark, Page, ReadingRepository};
use crate::db::repository::{BookRepository, ChapterRepository, Repository};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBookmark {
    pub book_id: String,
    pub chapter_id: String,
    pub position: f64,
    #[serde(default)]
    pub note: String,
}

pub struct BookmarkService<'a> {
    books: &'a BookRepository,
    chapters: &'a ChapterRepository,
    repository: ReadingRepository,
    user_id: &'a str,
    is_admin: bool,
}

impl<'a> BookmarkService<'a> {
    pub fn new(
        books: &'a BookRepository,
        chapters: &'a ChapterRepository,
        user_id: &'a str,
        is_admin: bool,
    ) -> Self {
        Self {
            books,
            chapters,
            repository: ReadingRepository::new(books.db().clone()),
            user_id,
            is_admin,
        }
    }

    pub async fn books(&self, page: usize, page_size: usize) -> Result<Page<ActivityBook>> {
        let (page, page_size) = page_bounds(page, page_size);
        self.repository
            .activity_books(self.user_id, self.is_admin, true, page, page_size)
            .await
    }

    pub async fn list(
        &self,
        book_id: &str,
        page: usize,
        page_size: usize,
    ) -> Result<Page<Bookmark>> {
        self.ensure_book_access(book_id).await?;
        let (page, page_size) = page_bounds(page, page_size);
        self.repository
            .bookmarks(self.user_id, book_id, page, page_size)
            .await
    }

    pub async fn get(&self, bookmark_id: &str) -> Result<Bookmark> {
        let bookmark = self
            .repository
            .bookmark(self.user_id, bookmark_id)
            .await?
            .ok_or_else(|| TingError::NotFound("Bookmark not found".into()))?;
        self.ensure_book_access(&bookmark.book_id).await?;
        Ok(bookmark)
    }

    pub async fn create(&self, request: CreateBookmark) -> Result<Bookmark> {
        self.ensure_book_access(&request.book_id).await?;
        validate_note(&request.note)?;
        if !request.position.is_finite() || request.position < 0.0 {
            return Err(TingError::InvalidRequest(
                "Invalid bookmark position".into(),
            ));
        }
        let chapter = self
            .chapters
            .find_by_id(&request.chapter_id)
            .await?
            .ok_or_else(|| TingError::NotFound("Chapter not found".into()))?;
        if chapter.book_id != request.book_id {
            return Err(TingError::InvalidRequest(
                "Chapter does not belong to book".into(),
            ));
        }
        self.repository
            .add_bookmark(
                self.user_id,
                request.book_id,
                request.chapter_id,
                request.position,
                request.note,
            )
            .await
    }

    pub async fn update(&self, bookmark_id: &str, note: String) -> Result<Bookmark> {
        validate_note(&note)?;
        self.get(bookmark_id).await?;
        let changed = self
            .repository
            .change_bookmark(self.user_id, bookmark_id, Some(note))
            .await?;
        if changed == 0 {
            return Err(TingError::NotFound("Bookmark not found".into()));
        }
        self.get(bookmark_id).await
    }

    pub async fn delete(&self, bookmark_id: &str) -> Result<()> {
        self.get(bookmark_id).await?;
        if self
            .repository
            .change_bookmark(self.user_id, bookmark_id, None)
            .await?
            == 0
        {
            return Err(TingError::NotFound("Bookmark not found".into()));
        }
        Ok(())
    }

    async fn ensure_book_access(&self, book_id: &str) -> Result<()> {
        if self.books.find_by_id(book_id).await?.is_none() {
            return Err(TingError::NotFound("Book not found".into()));
        }
        if !self
            .books
            .check_access(book_id, self.user_id, self.is_admin)
            .await?
        {
            return Err(TingError::PermissionDenied("Book access denied".into()));
        }
        Ok(())
    }
}

fn page_bounds(page: usize, page_size: usize) -> (usize, usize) {
    (page.clamp(1, 1_000_000), page_size.clamp(1, 100))
}

fn validate_note(note: &str) -> Result<()> {
    if note.chars().count() > 2000 {
        return Err(TingError::InvalidRequest(
            "Bookmark note exceeds 2000 characters".into(),
        ));
    }
    Ok(())
}
