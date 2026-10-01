use crate::core::StorageService;
use crate::core::error::{Result, TingError};
use crate::core::metadata_writer::{
    AudiobookshelfMetadata, remote_metadata_dir, write_metadata_json,
};
use crate::core::nfo_manager::{BookMetadata, NfoManager};
use crate::db::models::{Book, Library, ScraperConfig};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

pub fn is_cover_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://") || value.starts_with("//")
}

/// Download completely before replacing a cached file, so a failed scan retains it.
pub async fn cache_remote_file(
    storage: &StorageService,
    library: &Library,
    remote_url: &str,
    target: &Path,
    key: &[u8; 32],
    limit: u64,
) -> Result<()> {
    let (reader, length) = storage
        .get_webdav_reader(library, remote_url, None, key)
        .await?;
    if length > limit {
        return Err(TingError::InvalidRequest(
            "WebDAV metadata file is too large".into(),
        ));
    }
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > limit || (length > 0 && bytes.len() as u64 != length) {
        return Err(TingError::InvalidRequest(
            "Incomplete or oversized WebDAV metadata file".into(),
        ));
    }
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let staging = target.with_file_name(format!(".metadata-{}.tmp", uuid::Uuid::new_v4()));
    let result = async {
        tokio::fs::write(&staging, bytes).await?;
        tokio::fs::rename(&staging, target).await
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(error.into());
    }
    Ok(())
}

pub fn write_cached_sidecars(
    book: &Book,
    metadata: &AudiobookshelfMetadata,
    nfo_manager: &NfoManager,
) -> Result<PathBuf> {
    write_sidecars(book, metadata, nfo_manager, false)
}

pub fn ensure_cached_sidecars(
    book: &Book,
    metadata: &AudiobookshelfMetadata,
    nfo_manager: &NfoManager,
) -> Result<PathBuf> {
    write_sidecars(book, metadata, nfo_manager, true)
}

fn write_sidecars(
    book: &Book,
    metadata: &AudiobookshelfMetadata,
    nfo_manager: &NfoManager,
    only_missing: bool,
) -> Result<PathBuf> {
    let dir = remote_metadata_dir(&book.path)?;
    std::fs::create_dir_all(&dir)?;
    let cover = portable_cover(book, &dir)?;
    if !only_missing || !dir.join("metadata.json").is_file() {
        let mut metadata = metadata.clone();
        for field in ["cover", "coverUrl", "cover_url"] {
            if metadata.extra.contains_key(field) {
                metadata
                    .extra
                    .insert(field.to_string(), serde_json::json!(cover));
            }
        }
        write_metadata_json(&dir, &metadata)?;
    }
    if only_missing && dir.join("book.nfo").is_file() {
        return Ok(dir);
    }
    let mut nfo = nfo_manager
        .read_book_nfo(&dir.join("book.nfo"))
        .unwrap_or_else(|_| {
            BookMetadata::new(
                book.title.clone().unwrap_or_default(),
                "ting-reader".into(),
                book.id.clone(),
                0,
            )
        });
    nfo.title = book.title.clone().unwrap_or_default();
    nfo.author = book.author.clone();
    nfo.narrator = book.narrator.clone();
    nfo.intro = book.description.clone();
    nfo.subtitle = metadata.subtitle.clone();
    nfo.tags.items = metadata.tags.clone();
    nfo.genre.items = metadata.genres.clone();
    nfo.chapter_count = metadata.chapters.len() as u32;
    nfo.total_duration = metadata
        .chapters
        .last()
        .map(|chapter| chapter.end.max(0.0).round() as u64);
    nfo.cover_url = cover;
    nfo.touch();
    nfo_manager.write_book_nfo_to_dir(&dir, &nfo)?;
    Ok(dir)
}

/// File covers must belong to this book's cache. URL covers remain references.
fn portable_cover(book: &Book, dir: &Path) -> Result<Option<String>> {
    let Some(cover) = book.cover_url.as_deref().filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if is_cover_url(cover) {
        return Ok(Some(cover.to_string()));
    }
    let path = Path::new(cover);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    };
    if !path.is_file() {
        return Ok(None);
    }
    let path = path.canonicalize()?;
    if path.parent() != Some(dir.canonicalize()?.as_path()) {
        return Err(TingError::PermissionDenied(
            "Cover file is outside the book metadata cache".into(),
        ));
    }
    Ok(path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned))
}

pub async fn sync_to_remote(
    storage: &StorageService,
    library: &Library,
    book: &Book,
    key: &[u8; 32],
) -> Result<()> {
    let config: ScraperConfig = library
        .scraper_config
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    if library.library_type != "webdav" || !config.webdav_metadata_writing_enabled {
        return Ok(());
    }
    let dir = remote_metadata_dir(&book.path)?;
    // Upload a file cover first, then the sidecars that reference it. Switching
    // to a URL neither downloads that URL nor removes the old remote image.
    if let Some(cover) = portable_cover(book, &dir)?
        && !is_cover_url(&cover)
    {
        storage
            .put_webdav_sidecar(
                library,
                &book.path,
                &cover,
                tokio::fs::read(dir.join(&cover)).await?,
                key,
            )
            .await?;
    }
    for filename in ["metadata.json", "book.nfo"] {
        storage
            .put_webdav_sidecar(
                library,
                &book.path,
                filename,
                tokio::fs::read(dir.join(filename)).await?,
                key,
            )
            .await?;
    }
    Ok(())
}
