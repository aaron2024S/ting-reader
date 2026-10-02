use super::*;

#[test]
fn canonicalizes_legacy_relative_path_to_absolute_path() {
    let current_dir = std::env::current_dir().unwrap();
    let temp_dir = current_dir
        .join("target")
        .join(format!("chapter-path-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let absolute_path = temp_dir.join("chapter.mp3");
    std::fs::write(&absolute_path, b"audio").unwrap();
    let relative_path = absolute_path.strip_prefix(&current_dir).unwrap();

    assert_eq!(
        canonical_existing_path(relative_path),
        canonical_existing_path(&absolute_path)
    );

    std::fs::remove_dir_all(temp_dir).unwrap();
}
