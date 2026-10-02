use super::*;

#[tokio::test]
async fn missing_duration_counts_exclude_strm_references() {
    let db = Arc::new(DatabaseManager::new_in_memory().unwrap());
    db.execute(|conn| {
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO books (id, library_id, path, hash)
             VALUES ('book-1', 'library-1', '/book', 'book-hash');",
        )
        .map_err(TingError::DatabaseError)?;
        for (id, path, duration) in [
            ("known", "/book/known.wma", Some(700)),
            ("zero", "/book/zero.wma", Some(0)),
            ("missing", "/book/missing.mp3", None),
            ("negative", "/book/negative.m4a", Some(-1)),
            ("strm-zero", "/book/remote.STRM", Some(0)),
            ("strm-missing", "/book/remote.strm", None),
        ] {
            conn.execute(
                "INSERT INTO chapters (id, book_id, path, duration)
                 VALUES (?1, 'book-1', ?2, ?3)",
                rusqlite::params![id, path, duration],
            )
            .map_err(TingError::DatabaseError)?;
        }
        Ok(())
    })
    .await
    .unwrap();

    let repository = ChapterRepository::new(db);
    let counts = repository.count_by_book("book-1").await.unwrap();
    assert_eq!(counts.total, 6);
    assert_eq!(counts.missing_duration, 3);
    let counts = repository.count_by_library("library-1").await.unwrap();
    assert_eq!(counts["book-1"].missing_duration, 3);
}

#[tokio::test]
async fn find_by_book_with_progress_groups_main_chapters_before_extras() {
    let db = Arc::new(DatabaseManager::new_in_memory().unwrap());
    db.execute(|conn| {
        conn.execute_batch("PRAGMA foreign_keys = OFF;")
            .map_err(TingError::DatabaseError)?;

        for (id, chapter_index, is_extra) in [
            ("main-1", 1, 0),
            ("extra-1", 1, 1),
            ("main-2", 2, 0),
            ("extra-2", 2, 1),
        ] {
            conn.execute(
                "INSERT INTO chapters (id, book_id, title, path, chapter_index, is_extra) \
                 VALUES (?1, 'book-1', ?1, ?1, ?2, ?3)",
                rusqlite::params![id, chapter_index, is_extra],
            )
            .map_err(TingError::DatabaseError)?;
        }

        Ok(())
    })
    .await
    .unwrap();

    let repository = ChapterRepository::new(db);
    let chapters = repository
        .find_by_book_with_progress("book-1", "user-1")
        .await
        .unwrap();
    let chapter_ids: Vec<_> = chapters
        .into_iter()
        .map(|(chapter, _, _)| chapter.id)
        .collect();

    assert_eq!(chapter_ids, vec!["main-1", "main-2", "extra-1", "extra-2"]);

    let paged_chapters = repository
        .find_by_book_with_progress_page("book-1", "user-1", None, 0, 10, false)
        .await
        .unwrap();
    let paged_chapter_ids: Vec<_> = paged_chapters
        .into_iter()
        .map(|(chapter, _, _)| chapter.id)
        .collect();

    assert_eq!(
        paged_chapter_ids,
        vec!["main-1", "main-2", "extra-1", "extra-2"]
    );
}
