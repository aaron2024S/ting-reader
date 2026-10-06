use super::webdav_directory_fingerprint;

struct WebDavTestServer(tokio::task::JoinHandle<()>);

impl Drop for WebDavTestServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn cloud_library() -> crate::db::models::Library {
    crate::db::models::Library {
        id: "cloud".into(),
        name: "cloud".into(),
        library_type: "webdav".into(),
        url: "http://127.0.0.1:1/dav".into(),
        username: None,
        password: None,
        root_path: "/".into(),
        last_scanned_at: None,
        created_at: String::new(),
        scraper_config: None,
    }
}

#[test]
fn cloud_mode_disables_all_audio_probes_independently_of_cover_and_write_settings() {
    let mut library = cloud_library();
    assert!(super::metadata::audio_metadata_enabled(&library));
    for cloud_mode in [true, false] {
        let config = crate::db::models::ScraperConfig {
            cloud_mode,
            extract_audio_cover: true,
            webdav_metadata_writing_enabled: true,
            ..Default::default()
        };
        library.scraper_config = Some(serde_json::to_string(&config).unwrap());
        assert_eq!(
            super::metadata::audio_metadata_enabled(&library),
            !cloud_mode
        );
        assert!(library.can_write_metadata_files());
    }
}

#[tokio::test]
async fn cloud_mode_extraction_never_requests_remote_audio() {
    use crate::core::library_scanner::{LibraryScanner, LibraryScannerDependencies};
    use crate::db::repository::{
        BookRepository, ChapterRepository, LibraryRepository, SeriesRepository,
    };
    use crate::plugin::manager::{PluginConfig, PluginManager};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(
        crate::db::DatabaseManager::new(
            &directory.path().join("test.db"),
            1,
            Duration::from_secs(5),
        )
        .unwrap(),
    );
    let scanner = LibraryScanner::new(LibraryScannerDependencies {
        book_repo: Arc::new(BookRepository::new(Arc::clone(&database))),
        chapter_repo: Arc::new(ChapterRepository::new(Arc::clone(&database))),
        library_repo: Arc::new(LibraryRepository::new(Arc::clone(&database))),
        series_repo: Arc::new(SeriesRepository::new(database)),
        text_cleaner: Arc::new(crate::core::books::text_cleaner::TextCleaner::new(
            Default::default(),
        )),
        nfo_manager: Arc::new(crate::core::books::nfo_manager::NfoManager::new(
            directory.path().into(),
        )),
        audio_streamer: Arc::new(crate::core::audio::AudioStreamer::new(Default::default())),
        plugin_manager: Arc::new(
            PluginManager::new(PluginConfig {
                plugin_dir: directory.path().join("plugins"),
                enable_hot_reload: false,
                max_memory_per_plugin: 16 * 1024 * 1024,
                max_execution_time: Duration::from_secs(1),
            })
            .unwrap(),
        ),
    })
    .with_storage_service(Arc::new(crate::core::StorageService::new()));
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    let app = axum::Router::new().fallback(axum::routing::get(move || {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            axum::http::StatusCode::NOT_FOUND
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut library = cloud_library();
    library.url = format!("http://{}/dav", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let server = WebDavTestServer(server);
    for extract_cover in [false, true] {
        for allow_writes in [false, true] {
            library.scraper_config = Some(
                serde_json::to_string(&crate::db::models::ScraperConfig {
                    cloud_mode: true,
                    extract_audio_cover: extract_cover,
                    webdav_metadata_writing_enabled: allow_writes,
                    ..Default::default()
                })
                .unwrap(),
            );
            for extension in ["m4a", "wma", "xm", "strm"] {
                let result = tokio::time::timeout(
                    Duration::from_secs(1),
                    scanner.extract_webdav_metadata(
                        &library,
                        &format!("{}/001.{extension}", library.url),
                        Some(directory.path()),
                        extract_cover,
                    ),
                )
                .await
                .unwrap();
                assert_eq!(result, (String::new(), "001".into(), None, None, None, 0));
                assert_eq!(library.can_write_metadata_files(), allow_writes);
            }
        }
    }
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(!directory.path().join("cover.jpg").exists());
    drop(server);
}

#[test]
fn directory_fingerprint_is_order_independent() {
    let first = vec![
        (
            "https://example.test/book/001.mp3".to_string(),
            None,
            Some("etag-1".to_string()),
        ),
        (
            "https://example.test/book/002.mp3".to_string(),
            None,
            Some("etag-2".to_string()),
        ),
    ];
    let mut reversed = first.clone();
    reversed.reverse();

    assert_eq!(
        webdav_directory_fingerprint(&first),
        webdav_directory_fingerprint(&reversed)
    );
}

#[test]
fn directory_fingerprint_changes_with_validator() {
    let before = vec![(
        "https://example.test/book/001.mp3".to_string(),
        None,
        Some("etag-1".to_string()),
    )];
    let after = vec![(
        "https://example.test/book/001.mp3".to_string(),
        None,
        Some("etag-2".to_string()),
    )];

    assert_ne!(
        webdav_directory_fingerprint(&before),
        webdav_directory_fingerprint(&after)
    );
}
