use super::{collapse_local_scan_roots, local_group_is_changed, mark_changed_local_directory};
use crate::core::library_scanner::shared::CoalescedRangeDirs;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[tokio::test]
async fn full_scan_saves_clean_directory_titles_and_all_completed_range_chapters() {
    use crate::core::library_scanner::{LocalLibraryScanContext, ScanMode, test_support};
    let directory = tempfile::tempdir().unwrap();
    let scanner = test_support::scanner(directory.path());
    let root = directory.path().join("books");
    for name in ["书名-作者／演播", "另一本 ｜ 简介", "单名示例-示例演播"] {
        for (range, chapter) in [
            ("1-50", "001"),
            ("51-100", "051"),
            ("101-150完结撒花", "101"),
        ] {
            let path = root.join(name).join(range);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join(format!("{chapter}.strm")),
                "https://example.test/audio.mp3",
            )
            .unwrap();
        }
    }
    let library = test_support::library(&scanner, &root).await;
    let config = crate::db::models::ScraperConfig {
        metadata_priority: vec!["local_metadata".into()],
        extract_audio_cover: false,
        cloud_mode: true,
        ..Default::default()
    };
    for mode in [ScanMode::Full, ScanMode::Full] {
        let result = scanner
            .scan_local_library(LocalLibraryScanContext {
                library_id: &library.id,
                path: &root,
                task_id: None,
                last_scanned: None,
                scraper_config: &config,
                mode,
                scan_paths: None,
            })
            .await
            .unwrap();
        assert_eq!(result.failed_count, 0, "{:?}", result.errors);
        let mut books = scanner
            .book_repo
            .find_by_library(&library.id)
            .await
            .unwrap();
        books.sort_by(|a, b| a.title.cmp(&b.title));
        assert_eq!(books.len(), 3);
        assert_eq!(
            books
                .iter()
                .map(|book| book.title.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["书名", "单名示例", "另一本"]
        );
        for book in books {
            if book.title.as_deref() == Some("书名") {
                assert_eq!(book.author.as_deref(), Some("作者"));
                assert_eq!(book.narrator.as_deref(), Some("演播"));
            } else if book.title.as_deref() == Some("单名示例") {
                assert_eq!(book.author.as_deref(), Some("示例演播"));
                assert!(book.narrator.is_none());
            } else {
                assert!(book.author.is_none());
                assert!(book.narrator.is_none());
            }
            let chapters = scanner.chapter_repo.find_by_book(&book.id).await.unwrap();
            assert_eq!(chapters.len(), 3);
            assert!(
                chapters
                    .iter()
                    .any(|chapter| chapter.path.contains("101-150完结撒花"))
            );
        }
    }
}

#[tokio::test]
async fn directory_title_cleanup_preserves_explicit_metadata_and_group_override() {
    use crate::core::library_scanner::test_support;
    let directory = tempfile::tempdir().unwrap();
    let scanner = test_support::scanner(directory.path());
    let book_dir = directory.path().join("目录书名-目录作者／目录演播");
    std::fs::create_dir_all(&book_dir).unwrap();
    std::fs::write(
        book_dir.join("metadata.json"),
        r#"{"title":"真实书名 - 副标题","authors":["真实作者"],"narrators":["真实演播"]}"#,
    )
    .unwrap();
    let mut config = crate::db::models::ScraperConfig {
        metadata_priority: vec!["local_metadata".into()],
        extract_audio_cover: false,
        use_filename_as_title: false,
        ..Default::default()
    };
    let (metadata, _) = scanner
        .extract_final_metadata(&book_dir, &[], &config, Some("合并书名-目录作者／目录演播"))
        .await;
    assert_eq!(metadata.title.as_deref(), Some("真实书名 - 副标题"));
    assert_eq!(metadata.author.as_deref(), Some("真实作者"));
    assert_eq!(metadata.narrator.as_deref(), Some("真实演播"));
    config.use_filename_as_title = true;
    let (metadata, _) = scanner
        .extract_final_metadata(&book_dir, &[], &config, Some("合并书名-目录作者／目录演播"))
        .await;
    assert_eq!(metadata.title.as_deref(), Some("合并书名"));
    assert_eq!(metadata.author.as_deref(), Some("真实作者"));
    assert_eq!(metadata.narrator.as_deref(), Some("真实演播"));
}

#[tokio::test]
async fn directory_contributors_are_available_without_scraping_or_sidecars() {
    use crate::core::library_scanner::test_support;
    let directory = tempfile::tempdir().unwrap();
    let scanner = test_support::scanner(directory.path());
    let config = crate::db::models::ScraperConfig {
        metadata_priority: vec!["local_metadata".into()],
        extract_audio_cover: false,
        ..Default::default()
    };
    for (name, author, narrator) in [
        ("示例书名-示例作者", Some("示例作者"), None),
        ("示例书名-示例演播", Some("示例演播"), None),
        (
            "示例书名-示例作者／示例演播",
            Some("示例作者"),
            Some("示例演播"),
        ),
        (
            "示例书名-示例作者-示例演播",
            Some("示例作者"),
            Some("示例演播"),
        ),
        ("示例书名-／示例演播", None, Some("示例演播")),
    ] {
        let path = directory.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let (metadata, _) = scanner
            .extract_final_metadata(&path, &[], &config, None)
            .await;
        assert_eq!(metadata.title.as_deref(), Some("示例书名"));
        assert_eq!(metadata.author.as_deref(), author);
        assert_eq!(metadata.narrator.as_deref(), narrator);
    }
}
#[test]
fn collapses_overlapping_scan_roots() {
    let roots = vec![
        PathBuf::from("library/book/chapters"),
        PathBuf::from("library/book"),
        PathBuf::from("library/other"),
        PathBuf::from("library/book"),
    ];

    assert_eq!(
        collapse_local_scan_roots(roots),
        vec![
            PathBuf::from("library/book"),
            PathBuf::from("library/other")
        ]
    );
}

#[test]
fn changed_directory_does_not_wake_siblings() {
    let mut changed_dirs = HashSet::new();
    let mut changed_ancestors = HashSet::new();
    mark_changed_local_directory(
        Some(Path::new("library/book-a")),
        &mut changed_dirs,
        &mut changed_ancestors,
    );

    assert!(local_group_is_changed(
        Path::new("library/book-a"),
        None,
        &changed_dirs,
        &changed_ancestors
    ));
    assert!(!local_group_is_changed(
        Path::new("library/book-b"),
        None,
        &changed_dirs,
        &changed_ancestors
    ));
}

#[test]
fn changed_range_child_wakes_only_its_coalesced_book() {
    let mut changed_dirs = HashSet::new();
    let mut changed_ancestors = HashSet::new();
    mark_changed_local_directory(
        Some(Path::new("library/book/001-100")),
        &mut changed_dirs,
        &mut changed_ancestors,
    );
    let range_dirs = CoalescedRangeDirs {
        child_dirs: vec![
            PathBuf::from("library/book/001-100"),
            PathBuf::from("library/book/101-200"),
        ],
        title_override: None,
    };

    assert!(local_group_is_changed(
        Path::new("library/book"),
        Some(&range_dirs),
        &changed_dirs,
        &changed_ancestors
    ));
    assert!(!local_group_is_changed(
        Path::new("library/other-book"),
        None,
        &changed_dirs,
        &changed_ancestors
    ));
}
