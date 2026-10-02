use crate::api::state::AppState;
use crate::core::app::error::Result;
use crate::core::books::metadata_writer::{
    build_audiobookshelf_chapters, read_metadata_json, remote_metadata_dir,
};
use crate::db::models::Book;
use crate::db::repository::ChapterRepository;

pub(crate) async fn save_webdav_metadata(state: &AppState, book: &Book) -> Result<()> {
    let Some(library) = state.library_repo.find_by_id(&book.library_id).await? else {
        return Ok(());
    };
    if library.library_type != "webdav" {
        return Ok(());
    }
    let dir = remote_metadata_dir(&book.path)?;
    let mut metadata = read_metadata_json(&dir)?.unwrap_or_default();
    metadata.update_book_fields(book);
    let chapters = ChapterRepository::new(state.book_repo.db().clone())
        .find_by_book(&book.id)
        .await?;
    metadata.chapters = build_audiobookshelf_chapters(chapters);
    metadata.series.clear();
    for series in state.series_repo.find_series_by_book(&book.id).await? {
        let books = state.series_repo.find_books_by_series(&series.id).await?;
        let title = books
            .iter()
            .find(|(candidate, _)| candidate.id == book.id)
            .map(|(_, order)| format!("{} #{}", series.title, order))
            .unwrap_or(series.title);
        metadata.series.push(title);
    }
    crate::core::books::webdav_metadata::write_cached_sidecars(
        book,
        &metadata,
        &state.nfo_manager,
    )?;
    crate::core::books::webdav_metadata::sync_to_remote(
        &state.storage_service,
        &library,
        book,
        state.encryption_key.as_ref(),
    )
    .await
}
