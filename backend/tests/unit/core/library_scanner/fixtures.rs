use super::{LibraryScanner, LibraryScannerDependencies};
use crate::db::repository::{
    BookRepository, ChapterRepository, LibraryRepository, SeriesRepository,
};
use crate::plugin::manager::{PluginConfig, PluginManager};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub(super) fn scanner(directory: &Path) -> LibraryScanner {
    let db = Arc::new(crate::db::DatabaseManager::new_in_memory().unwrap());
    LibraryScanner::new(LibraryScannerDependencies {
        book_repo: Arc::new(BookRepository::new(Arc::clone(&db))),
        chapter_repo: Arc::new(ChapterRepository::new(Arc::clone(&db))),
        library_repo: Arc::new(LibraryRepository::new(Arc::clone(&db))),
        series_repo: Arc::new(SeriesRepository::new(db)),
        text_cleaner: Arc::new(crate::core::books::TextCleaner::new(Default::default())),
        nfo_manager: Arc::new(crate::core::books::nfo_manager::NfoManager::new(
            directory.into(),
        )),
        audio_streamer: Arc::new(crate::core::audio::AudioStreamer::new(Default::default())),
        plugin_manager: Arc::new(
            PluginManager::new(PluginConfig {
                plugin_dir: directory.join("plugins"),
                enable_hot_reload: false,
                max_memory_per_plugin: 16 * 1024 * 1024,
                max_execution_time: Duration::from_secs(1),
            })
            .unwrap(),
        ),
    })
    .with_storage_service(Arc::new(crate::core::StorageService::new()))
}

pub(super) async fn library(scanner: &LibraryScanner, path: &Path) -> crate::db::models::Library {
    let library = crate::db::models::Library {
        id: "library".into(),
        name: "Library".into(),
        library_type: "local".into(),
        url: path.to_string_lossy().into_owned(),
        root_path: "/".into(),
        username: None,
        password: None,
        last_scanned_at: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        scraper_config: None,
    };
    scanner.library_repo.create(&library).await.unwrap();
    library
}
