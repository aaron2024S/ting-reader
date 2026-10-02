use super::*;

#[test]
fn typed_updates_reject_unknown_and_empty_patches() {
    assert!(
        validate_patch_keys(
            serde_json::json!({"cover_url": "cover.png"})
                .as_object()
                .unwrap(),
            &["cover_url"],
        )
        .is_ok()
    );
    assert!(
        validate_patch_keys(
            serde_json::json!({"admin": true}).as_object().unwrap(),
            &["cover_url"],
        )
        .is_err()
    );
    assert!(
        validate_patch_keys(serde_json::json!({}).as_object().unwrap(), &["cover_url"],).is_err()
    );
}

fn test_book(path: String, cover_url: Option<&str>) -> Book {
    Book {
        id: "book-1".to_string(),
        library_id: "library-1".to_string(),
        path,
        cover_url: cover_url.map(ToOwned::to_owned),
        ..Book::default()
    }
}

fn test_library(root_path: String) -> Library {
    Library {
        id: "library-1".to_string(),
        name: "Library".to_string(),
        library_type: "local".to_string(),
        url: String::new(),
        username: None,
        password: None,
        root_path,
        last_scanned_at: None,
        created_at: String::new(),
        scraper_config: None,
    }
}

#[test]
fn book_cover_theme_sources_try_book_relative_path_first() {
    let temp = tempfile::tempdir().unwrap();
    let book_path = temp.path().join("book");
    let library_root = temp.path().join("library");
    let book = test_book(
        pathbuf_to_theme_source(book_path.clone()),
        Some("cover.png"),
    );
    let library = test_library(pathbuf_to_theme_source(library_root.clone()));

    let sources = book_cover_theme_sources(&book, Some(&library));

    assert_eq!(
        sources.first().map(String::as_str),
        Some(pathbuf_to_theme_source(book_path.join("cover.png")).as_str())
    );
    assert!(sources.contains(&pathbuf_to_theme_source(library_root.join("cover.png"))));
}

#[test]
fn book_cover_theme_sources_keep_absolute_cover_path() {
    let temp = tempfile::tempdir().unwrap();
    let cover_path = temp.path().join("cover.png");
    let cover = pathbuf_to_theme_source(cover_path);
    let book = test_book(String::new(), Some(&cover));

    assert_eq!(book_cover_theme_sources(&book, None), vec![cover]);
}

#[test]
fn book_cover_theme_sources_keep_remote_cover_url() {
    let book = test_book(
        "book-dir".to_string(),
        Some("https://example.com/cover.png"),
    );

    assert_eq!(
        book_cover_theme_sources(&book, None),
        vec!["https://example.com/cover.png".to_string()]
    );
}
