use super::*;
use crate::db::manager::DatabaseManager;

async fn create_repository() -> (Arc<DatabaseManager>, ProgressRepository) {
    let db = Arc::new(DatabaseManager::new_in_memory().expect("create test database"));
    db.execute(|conn| {
        conn.execute(
            "INSERT INTO users (id, username, password_hash, role) VALUES ('user-1', 'tester', 'hash', 'user')",
            [],
        )?;
        conn.execute(
            "INSERT INTO libraries (id, name, type, url) VALUES ('library-1', 'Test', 'local', '')",
            [],
        )?;
        conn.execute(
            "INSERT INTO books (id, library_id, title, path, hash) VALUES ('book-1', 'library-1', 'Book', '/book', 'book-hash')",
            [],
        )?;
        conn.execute(
            "INSERT INTO chapters (id, book_id, title, path) VALUES ('chapter-1', 'book-1', 'Chapter', '/chapter')",
            [],
        )?;
        Ok(())
    })
    .await
    .expect("seed test database");

    let repository = ProgressRepository::new(db.clone());
    (db, repository)
}

fn progress(id: &str, position: f64) -> Progress {
    Progress {
        id: id.to_string(),
        user_id: "user-1".to_string(),
        book_id: "book-1".to_string(),
        chapter_id: Some("chapter-1".to_string()),
        position,
        duration: Some(100.0),
        updated_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[tokio::test]
async fn repeated_heartbeats_keep_one_daily_activity_record() {
    let (db, repository) = create_repository().await;

    repository.upsert(&progress("event-1", 10.0)).await.unwrap();
    repository.upsert(&progress("event-2", 12.0)).await.unwrap();

    let (count, updates, listen_seconds): (i64, i64, f64) = db
        .execute(|conn| {
            conn.query_row(
                "SELECT COUNT(*), SUM(progress_updates), COALESCE(SUM(listen_seconds), 0) FROM listening_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(TingError::DatabaseError)
        })
        .await
        .unwrap();

    assert_eq!(count, 1);
    assert_eq!(updates, 1);
    assert_eq!(listen_seconds, 12.0);

    let (total_rows, total_updates, total_seconds): (i64, i64, f64) = db
        .execute(|conn| {
            conn.query_row(
                "SELECT COUNT(*), SUM(progress_updates), SUM(listen_seconds) FROM listening_totals",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(TingError::DatabaseError)
        })
        .await
        .unwrap();
    assert_eq!(total_rows, 1);
    assert_eq!(total_updates, 0);
    assert_eq!(total_seconds, 12.0);
}

#[tokio::test]
async fn creates_a_new_event_for_a_different_day() {
    let (db, repository) = create_repository().await;

    repository.upsert(&progress("event-1", 10.0)).await.unwrap();
    db.execute(|conn| {
        conn.execute(
            "UPDATE listening_events SET \
                aggregate_key = 'previous-day', \
                activity_date = DATE('now', '-1 day'), \
                created_at = DATETIME('now', '-1 day'), \
                last_active_at = DATETIME('now', '-1 day')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    repository.upsert(&progress("event-2", 12.0)).await.unwrap();

    let count: i64 = db
        .execute(|conn| {
            conn.query_row("SELECT COUNT(*) FROM listening_events", [], |row| {
                row.get(0)
            })
            .map_err(TingError::DatabaseError)
        })
        .await
        .unwrap();

    assert_eq!(count, 2);

    let (total_rows, total_updates): (i64, i64) = db
        .execute(|conn| {
            conn.query_row(
                "SELECT COUNT(*), SUM(progress_updates) FROM listening_totals",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(TingError::DatabaseError)
        })
        .await
        .unwrap();
    assert_eq!(total_rows, 1);
    assert_eq!(total_updates, 0);
}

#[tokio::test]
async fn cleans_up_expired_daily_events() {
    let (db, repository) = create_repository().await;

    repository.upsert(&progress("event-1", 10.0)).await.unwrap();
    db.execute(|conn| {
        conn.execute(
            "UPDATE listening_events SET \
                activity_date = DATE('now', '-91 days'), \
                created_at = DATETIME('now', '-91 days'), \
                last_active_at = DATETIME('now', '-91 days')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();

    let removed = repository.cleanup_expired_records().await.unwrap();
    assert_eq!(removed, 1);

    let count: i64 = db
        .execute(|conn| {
            conn.query_row("SELECT COUNT(*) FROM listening_events", [], |row| {
                row.get(0)
            })
            .map_err(TingError::DatabaseError)
        })
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn cleans_up_long_hidden_progress() {
    let (db, repository) = create_repository().await;

    repository.upsert(&progress("event-1", 10.0)).await.unwrap();
    db.execute(|conn| {
        conn.execute(
            "UPDATE progress SET history_hidden_at = DATETIME('now', '-181 days')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();

    let removed = repository.cleanup_expired_records().await.unwrap();
    assert_eq!(removed, 1);

    let count: i64 = db
        .execute(|conn| {
            conn.query_row("SELECT COUNT(*) FROM progress", [], |row| row.get(0))
                .map_err(TingError::DatabaseError)
        })
        .await
        .unwrap();
    assert_eq!(count, 0);
}
