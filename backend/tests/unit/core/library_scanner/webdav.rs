use super::webdav_directory_fingerprint;

struct WebDavTestServer(tokio::task::JoinHandle<()>);

struct WebDavTestCache(std::path::PathBuf);

impl Drop for WebDavTestCache {
    fn drop(&mut self) {
        let root = std::env::current_dir().unwrap().join("temp");
        if self.0.parent() == Some(root.as_path()) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[tokio::test]
async fn webdav_saves_clean_titles_and_merges_completed_range_directories() {
    use crate::core::library_scanner::test_support;
    use crate::db::repository::Repository;
    use std::collections::HashMap;

    let directory = tempfile::tempdir().unwrap();
    let scanner = test_support::scanner(directory.path());
    let mut library = test_support::library(&scanner, directory.path()).await;
    library.library_type = "webdav".into();
    library.url = format!("https://example.test/{}", uuid::Uuid::new_v4());
    let config = crate::db::models::ScraperConfig {
        metadata_priority: vec!["local_metadata".into()],
        extract_audio_cover: false,
        cloud_mode: true,
        ..Default::default()
    };
    library.scraper_config = Some(serde_json::to_string(&config).unwrap());
    scanner.library_repo.update(&library).await.unwrap();

    for (name, title_override, author, narrator) in [
        ("书名-作者／演播", None, Some("作者"), Some("演播")),
        ("书名-作者", None, Some("作者"), None),
        ("书名-示例演播", None, Some("示例演播"), None),
        (
            "书名 | 简介",
            Some("合并书名-作者／演播 | 全本"),
            Some("作者"),
            Some("演播"),
        ),
    ] {
        let mut url = url::Url::parse(&library.url).unwrap();
        url.path_segments_mut().unwrap().push(name);
        let parent = url.to_string();
        let cache = crate::core::books::metadata::remote_metadata_dir(&parent).unwrap();
        assert!(!cache.exists());
        let _cache = WebDavTestCache(cache);
        let groups = ["1-50", "51-100", "101-150集（已完结，感谢收听）"]
            .into_iter()
            .map(|segment| {
                let mut url = url::Url::parse(&parent).unwrap();
                url.path_segments_mut().unwrap().push(segment);
                let group_url = url.to_string();
                url.path_segments_mut().unwrap().push("chapter.mp3");
                (group_url, vec![(url.to_string(), None, None)])
            })
            .collect::<HashMap<_, Vec<super::WebDavFileEntry>>>();
        let (groups, coalesced) = scanner.coalesce_webdav_range_directory_groups(groups);
        assert_eq!(groups.len(), 1);
        assert!(groups.contains_key(&parent));
        let files: Vec<_> = groups[&parent]
            .iter()
            .map(|entry| entry.0.clone())
            .collect();
        assert_eq!(files.len(), 3);
        assert_eq!(coalesced[&parent].child_dirs.len(), 3);
        let (id, _) = scanner
            .process_webdav_book(super::processing::WebDavBookContext {
                library: &library,
                dir_url: &parent,
                file_urls: &files,
                metadata_files: &[],
                scraper_config: &config,
                full_scan: true,
                existing_info: None,
                fallback_title_override: title_override,
            })
            .await
            .unwrap();
        let book = scanner.book_repo.find_by_id(&id).await.unwrap().unwrap();
        assert_eq!(
            book.title.as_deref(),
            Some(if title_override.is_some() {
                "合并书名"
            } else {
                "书名"
            })
        );
        assert_eq!(book.author.as_deref(), author);
        assert_eq!(book.narrator.as_deref(), narrator);
        assert_eq!(
            scanner.chapter_repo.find_by_book(&id).await.unwrap().len(),
            3
        );
    }
}

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
    assert!(super::extraction::audio_metadata_enabled(&library));
    for cloud_mode in [true, false] {
        let config = crate::db::models::ScraperConfig {
            cloud_mode,
            extract_audio_cover: true,
            webdav_metadata_writing_enabled: true,
            ..Default::default()
        };
        library.scraper_config = Some(serde_json::to_string(&config).unwrap());
        assert_eq!(
            super::extraction::audio_metadata_enabled(&library),
            !cloud_mode
        );
        assert!(library.can_write_metadata_files());
    }
}

#[tokio::test]
async fn cloud_mode_extraction_never_requests_remote_audio() {
    use crate::core::library_scanner::test_support;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let directory = tempfile::tempdir().unwrap();
    let scanner = test_support::scanner(directory.path());
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
