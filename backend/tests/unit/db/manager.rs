use super::*;
use tempfile::TempDir;

fn create_test_db() -> (DatabaseManager, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let manager = DatabaseManager::new(&db_path, 5, Duration::from_secs(5)).unwrap();
    (manager, temp_dir)
}

#[test]
fn test_database_manager_creation() {
    let (manager, _temp_dir) = create_test_db();
    assert_eq!(manager.pool_size(), 5);
}

#[test]
fn test_get_connection() {
    let (manager, _temp_dir) = create_test_db();
    let conn = manager.get_connection();
    assert!(conn.is_ok());
}

#[tokio::test]
async fn test_execute_async() {
    let (manager, _temp_dir) = create_test_db();

    let result = manager
        .execute(|conn| {
            conn.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)", [])
                .map_err(TingError::DatabaseError)?;
            Ok(())
        })
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_transaction_commit() {
    let (manager, _temp_dir) = create_test_db();

    // Create table
    manager
        .execute(|conn| {
            conn.execute(
                "CREATE TABLE test (id INTEGER PRIMARY KEY, value INTEGER)",
                [],
            )
            .map_err(TingError::DatabaseError)?;
            Ok(())
        })
        .await
        .unwrap();

    // Insert in transaction
    let result = manager
        .transaction(|tx| {
            tx.execute("INSERT INTO test (value) VALUES (?)", [42])
                .map_err(TingError::DatabaseError)?;
            Ok(())
        })
        .await;

    assert!(result.is_ok());

    // Verify data was committed
    let count: i64 = manager
        .execute(|conn| {
            conn.query_row("SELECT COUNT(*) FROM test", [], |row| row.get(0))
                .map_err(TingError::DatabaseError)
        })
        .await
        .unwrap();

    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_transaction_rollback() {
    let (manager, _temp_dir) = create_test_db();

    // Create table
    manager
        .execute(|conn| {
            conn.execute(
                "CREATE TABLE test (id INTEGER PRIMARY KEY, value INTEGER)",
                [],
            )
            .map_err(TingError::DatabaseError)?;
            Ok(())
        })
        .await
        .unwrap();

    // Insert in transaction that fails
    let result: Result<()> = manager
        .transaction(|tx| {
            tx.execute("INSERT INTO test (value) VALUES (?)", [42])
                .map_err(TingError::DatabaseError)?;
            // Simulate error
            Err(TingError::InvalidRequest("test error".into()))
        })
        .await;

    assert!(result.is_err());

    // Verify data was rolled back
    let count: i64 = manager
        .execute(|conn| {
            conn.query_row("SELECT COUNT(*) FROM test", [], |row| row.get(0))
                .map_err(TingError::DatabaseError)
        })
        .await
        .unwrap();

    assert_eq!(count, 0);
}

#[test]
fn test_backup() {
    let (manager, temp_dir) = create_test_db();

    // Create and populate table
    let conn = manager.get_connection().unwrap();
    conn.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)", [])
        .unwrap();
    conn.execute("INSERT INTO test (name) VALUES ('test')", [])
        .unwrap();
    drop(conn);

    // Backup database
    let backup_path = temp_dir.path().join("backup.db");
    let result = manager.backup(&backup_path);
    assert!(result.is_ok());

    // Verify backup exists and contains data
    let backup_conn = Connection::open(&backup_path).unwrap();
    let count: i64 = backup_conn
        .query_row("SELECT COUNT(*) FROM test", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_backup_async() {
    let (manager, temp_dir) = create_test_db();

    // Create and populate table
    manager
        .execute(|conn| {
            conn.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)", [])
                .map_err(TingError::DatabaseError)?;
            conn.execute("INSERT INTO test (name) VALUES ('test')", [])
                .map_err(TingError::DatabaseError)?;
            Ok(())
        })
        .await
        .unwrap();

    // Backup database asynchronously
    let backup_path = temp_dir.path().join("backup_async.db");
    let result = manager.backup_async(backup_path.clone()).await;
    assert!(result.is_ok());

    // Verify backup exists and contains data
    let backup_conn = Connection::open(&backup_path).unwrap();
    let count: i64 = backup_conn
        .query_row("SELECT COUNT(*) FROM test", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn test_connection_pool_stats() {
    let (manager, _temp_dir) = create_test_db();

    assert_eq!(manager.pool_size(), 5);
    assert!(manager.idle_connections() > 0);

    // Get a connection
    let _conn = manager.get_connection().unwrap();

    // Active connections should increase
    assert!(manager.active_connections() > 0);
}

mod model_defaults {
    use crate::db::models::ScraperConfig;

    #[test]
    fn extract_extra_chapters_defaults_to_true_and_accepts_false() {
        let legacy: ScraperConfig = serde_json::from_str("{}").unwrap();
        let disabled: ScraperConfig =
            serde_json::from_str(r#"{"extract_extra_chapters":false}"#).unwrap();

        assert!(legacy.extract_extra_chapters);
        assert!(!disabled.extract_extra_chapters);
    }
}
