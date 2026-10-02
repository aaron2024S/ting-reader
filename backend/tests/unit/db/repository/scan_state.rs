use super::*;

async fn repository() -> LibraryScanStateRepository {
    let db = Arc::new(DatabaseManager::new_in_memory().expect("create test database"));
    db.execute(|conn| {
        conn.execute(
            "INSERT INTO libraries (id, name, type, url)
             VALUES ('library-1', 'Test', 'local', '/media')",
            [],
        )
        .map_err(TingError::DatabaseError)?;
        Ok(())
    })
    .await
    .expect("insert test library");
    LibraryScanStateRepository::new(db)
}

#[tokio::test]
async fn delete_matching_preserves_newer_dirty_generation() {
    let repository = repository().await;
    repository
        .upsert_many(vec![LibraryScanState::new(
            "library-1",
            "/media/book",
            "local_dirty",
            "generation-1",
        )])
        .await
        .unwrap();
    repository
        .upsert_many(vec![LibraryScanState::new(
            "library-1",
            "/media/book",
            "local_dirty",
            "generation-2",
        )])
        .await
        .unwrap();

    repository
        .delete_matching(
            "library-1",
            "local_dirty",
            vec![("/media/book".to_string(), "generation-1".to_string())],
        )
        .await
        .unwrap();

    let state = repository
        .find("library-1", "/media/book", "local_dirty")
        .await
        .unwrap()
        .expect("newer state survives");
    assert_eq!(state.fingerprint, "generation-2");
}

#[tokio::test]
async fn delete_matching_removes_consumed_dirty_generation() {
    let repository = repository().await;
    repository
        .upsert_many(vec![LibraryScanState::new(
            "library-1",
            "/media/book",
            "local_dirty",
            "generation-1",
        )])
        .await
        .unwrap();

    repository
        .delete_matching(
            "library-1",
            "local_dirty",
            vec![("/media/book".to_string(), "generation-1".to_string())],
        )
        .await
        .unwrap();

    assert!(
        repository
            .find("library-1", "/media/book", "local_dirty")
            .await
            .unwrap()
            .is_none()
    );
}
