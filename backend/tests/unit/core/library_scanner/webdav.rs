use super::webdav_directory_fingerprint;

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
