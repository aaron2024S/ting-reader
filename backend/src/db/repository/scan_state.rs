use crate::core::app::error::{Result, TingError};
use crate::db::manager::DatabaseManager;
use rusqlite::OptionalExtension;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct LibraryScanState {
    pub library_id: String,
    pub entry_path: String,
    pub entry_kind: String,
    pub fingerprint: String,
    pub config_fingerprint: Option<String>,
    pub modified_at: Option<String>,
    pub etag: Option<String>,
    pub size: Option<i64>,
    pub parent_path: Option<String>,
}

impl LibraryScanState {
    pub fn new(
        library_id: impl Into<String>,
        entry_path: impl Into<String>,
        entry_kind: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            library_id: library_id.into(),
            entry_path: entry_path.into(),
            entry_kind: entry_kind.into(),
            fingerprint: fingerprint.into(),
            config_fingerprint: None,
            modified_at: None,
            etag: None,
            size: None,
            parent_path: None,
        }
    }
}

#[derive(Clone)]
pub struct LibraryScanStateRepository {
    db: Arc<DatabaseManager>,
}

impl LibraryScanStateRepository {
    pub fn new(db: Arc<DatabaseManager>) -> Self {
        Self { db }
    }

    pub async fn find(
        &self,
        library_id: &str,
        entry_path: &str,
        entry_kind: &str,
    ) -> Result<Option<LibraryScanState>> {
        let library_id = library_id.to_string();
        let entry_path = entry_path.to_string();
        let entry_kind = entry_kind.to_string();
        self.db
            .execute(move |conn| {
                conn.query_row(
                    "SELECT library_id, entry_path, entry_kind, fingerprint, config_fingerprint,
                            modified_at, etag, size, parent_path
                       FROM library_scan_state
                      WHERE library_id = ? AND entry_path = ? AND entry_kind = ?",
                    rusqlite::params![library_id, entry_path, entry_kind],
                    map_state,
                )
                .optional()
                .map_err(TingError::DatabaseError)
            })
            .await
    }

    pub async fn find_by_library_kind(
        &self,
        library_id: &str,
        entry_kind: &str,
    ) -> Result<HashMap<String, LibraryScanState>> {
        let library_id = library_id.to_string();
        let entry_kind = entry_kind.to_string();
        self.db
            .execute(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT library_id, entry_path, entry_kind, fingerprint, config_fingerprint,
                                modified_at, etag, size, parent_path
                           FROM library_scan_state
                          WHERE library_id = ? AND entry_kind = ?",
                    )
                    .map_err(TingError::DatabaseError)?;
                let states = stmt
                    .query_map(rusqlite::params![library_id, entry_kind], map_state)
                    .map_err(TingError::DatabaseError)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(TingError::DatabaseError)?;
                Ok(states
                    .into_iter()
                    .map(|state| (state.entry_path.clone(), state))
                    .collect())
            })
            .await
    }

    pub async fn find_files_under(
        &self,
        library_id: &str,
        directory_path: &str,
    ) -> Result<Vec<LibraryScanState>> {
        let library_id = library_id.to_string();
        let directory_path = directory_path.trim_end_matches('/').to_string();
        let pattern = format!("{}/%", escape_like(&directory_path));
        self.db
            .execute(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT library_id, entry_path, entry_kind, fingerprint, config_fingerprint,
                                modified_at, etag, size, parent_path
                           FROM library_scan_state
                          WHERE library_id = ?
                            AND entry_kind = 'file'
                            AND (parent_path = ? OR parent_path LIKE ? ESCAPE '\\')",
                    )
                    .map_err(TingError::DatabaseError)?;
                let rows = stmt
                    .query_map(
                        rusqlite::params![library_id, directory_path, pattern],
                        map_state,
                    )
                    .map_err(TingError::DatabaseError)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(TingError::DatabaseError)
            })
            .await
    }

    pub async fn upsert_many(&self, states: Vec<LibraryScanState>) -> Result<()> {
        if states.is_empty() {
            return Ok(());
        }
        self.db
            .transaction(move |tx| {
                let mut stmt = tx
                    .prepare(
                        "INSERT INTO library_scan_state
                            (library_id, entry_path, entry_kind, fingerprint, config_fingerprint,
                             modified_at, etag, size, parent_path, updated_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
                         ON CONFLICT(library_id, entry_path, entry_kind) DO UPDATE SET
                            fingerprint = excluded.fingerprint,
                            config_fingerprint = excluded.config_fingerprint,
                            modified_at = excluded.modified_at,
                            etag = excluded.etag,
                            size = excluded.size,
                            parent_path = excluded.parent_path,
                            updated_at = CURRENT_TIMESTAMP",
                    )
                    .map_err(TingError::DatabaseError)?;
                for state in states {
                    stmt.execute(rusqlite::params![
                        state.library_id,
                        state.entry_path,
                        state.entry_kind,
                        state.fingerprint,
                        state.config_fingerprint,
                        state.modified_at,
                        state.etag,
                        state.size,
                        state.parent_path,
                    ])
                    .map_err(TingError::DatabaseError)?;
                }
                Ok(())
            })
            .await
    }

    pub async fn delete_many(
        &self,
        library_id: &str,
        entry_kind: &str,
        entry_paths: Vec<String>,
    ) -> Result<()> {
        if entry_paths.is_empty() {
            return Ok(());
        }
        let library_id = library_id.to_string();
        let entry_kind = entry_kind.to_string();
        self.db
            .transaction(move |tx| {
                let mut stmt = tx
                    .prepare(
                        "DELETE FROM library_scan_state
                          WHERE library_id = ? AND entry_kind = ? AND entry_path = ?",
                    )
                    .map_err(TingError::DatabaseError)?;
                for entry_path in entry_paths {
                    stmt.execute(rusqlite::params![&library_id, &entry_kind, entry_path])
                        .map_err(TingError::DatabaseError)?;
                }
                Ok(())
            })
            .await
    }

    /// Delete queue-like states only when the persisted fingerprint still
    /// matches the version consumed by the caller. A newer watcher event for
    /// the same path therefore survives an older scan finishing later.
    pub async fn delete_matching(
        &self,
        library_id: &str,
        entry_kind: &str,
        entries: Vec<(String, String)>,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let library_id = library_id.to_string();
        let entry_kind = entry_kind.to_string();
        self.db
            .transaction(move |tx| {
                let mut stmt = tx
                    .prepare(
                        "DELETE FROM library_scan_state
                          WHERE library_id = ?
                            AND entry_kind = ?
                            AND entry_path = ?
                            AND fingerprint = ?",
                    )
                    .map_err(TingError::DatabaseError)?;
                for (entry_path, fingerprint) in entries {
                    stmt.execute(rusqlite::params![
                        &library_id,
                        &entry_kind,
                        entry_path,
                        fingerprint
                    ])
                    .map_err(TingError::DatabaseError)?;
                }
                Ok(())
            })
            .await
    }

    pub async fn replace_library_kinds(
        &self,
        library_id: &str,
        entry_kinds: &[&str],
        states: Vec<LibraryScanState>,
    ) -> Result<()> {
        let library_id = library_id.to_string();
        let entry_kinds: Vec<String> = entry_kinds.iter().map(|kind| kind.to_string()).collect();
        self.db
            .transaction(move |tx| {
                for entry_kind in entry_kinds {
                    tx.execute(
                        "DELETE FROM library_scan_state WHERE library_id = ? AND entry_kind = ?",
                        rusqlite::params![library_id, entry_kind],
                    )
                    .map_err(TingError::DatabaseError)?;
                }

                let mut stmt = tx
                    .prepare(
                        "INSERT OR REPLACE INTO library_scan_state
                            (library_id, entry_path, entry_kind, fingerprint, config_fingerprint,
                             modified_at, etag, size, parent_path, updated_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
                    )
                    .map_err(TingError::DatabaseError)?;
                for state in states {
                    stmt.execute(rusqlite::params![
                        state.library_id,
                        state.entry_path,
                        state.entry_kind,
                        state.fingerprint,
                        state.config_fingerprint,
                        state.modified_at,
                        state.etag,
                        state.size,
                        state.parent_path,
                    ])
                    .map_err(TingError::DatabaseError)?;
                }
                Ok(())
            })
            .await
    }
}

fn map_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryScanState> {
    Ok(LibraryScanState {
        library_id: row.get(0)?,
        entry_path: row.get(1)?,
        entry_kind: row.get(2)?,
        fingerprint: row.get(3)?,
        config_fingerprint: row.get(4)?,
        modified_at: row.get(5)?,
        etag: row.get(6)?,
        size: row.get(7)?,
        parent_path: row.get(8)?,
    })
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
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
}
