use crate::core::error::{Result, TingError};
use crate::db::manager::DatabaseManager;
use crate::db::models::{User, UserSettings};
use crate::db::repository::base::Repository;
use async_trait::async_trait;
use rusqlite::OptionalExtension;
use std::sync::Arc;

/// Repository for User entities
pub struct UserRepository {
    db: Arc<DatabaseManager>,
}

impl UserRepository {
    /// Create a new UserRepository
    pub fn new(db: Arc<DatabaseManager>) -> Self {
        Self { db }
    }

    /// Find a user by username
    pub async fn find_by_username(&self, username: &str) -> Result<Option<User>> {
        let username = username.to_string();
        self.db.execute(move |conn| {
            conn.query_row(
                "SELECT id, username, password_hash, role, created_at FROM users WHERE username = ?",
                [&username],
                |row| {
                    Ok(User {
                        id: row.get(0)?,
                        username: row.get(1)?,
                        password_hash: row.get(2)?,
                        role: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                }
            ).optional()
            .map_err(TingError::DatabaseError)
        }).await
    }

    /// Count total users
    pub async fn count(&self) -> Result<i64> {
        self.db
            .execute(|conn| {
                conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
                    .map_err(TingError::DatabaseError)
            })
            .await
    }

    /// Create a user together with its initial settings in one transaction.
    pub async fn create_with_settings(&self, user: &User, settings: &UserSettings) -> Result<()> {
        if user.id != settings.user_id {
            return Err(TingError::ValidationError(
                "Initial user settings must belong to the user being created".to_string(),
            ));
        }

        let user = user.clone();
        let settings = settings.clone();

        self.db
            .transaction(move |tx| {
                tx.execute(
                    "INSERT INTO users (id, username, password_hash, role) VALUES (?, ?, ?, ?)",
                    rusqlite::params![&user.id, &user.username, &user.password_hash, &user.role],
                )
                .map_err(TingError::DatabaseError)?;

                tx.execute(
                    "INSERT INTO user_settings (user_id, playback_speed, theme, auto_play, skip_intro, skip_outro, settings_json, updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                    rusqlite::params![
                        &settings.user_id,
                        settings.playback_speed,
                        &settings.theme,
                        settings.auto_play,
                        settings.skip_intro,
                        settings.skip_outro,
                        &settings.settings_json,
                    ],
                )
                .map_err(TingError::DatabaseError)?;

                Ok(())
            })
            .await
    }

    /// Update user password
    pub async fn update_password(&self, user_id: &str, password_hash: &str) -> Result<()> {
        let user_id = user_id.to_string();
        let password_hash = password_hash.to_string();
        self.db
            .execute(move |conn| {
                conn.execute(
                    "UPDATE users SET password_hash = ? WHERE id = ?",
                    rusqlite::params![&password_hash, &user_id],
                )
                .map_err(TingError::DatabaseError)?;
                Ok(())
            })
            .await
    }

    /// Update user permissions (accessible libraries and books)
    pub async fn update_permissions(
        &self,
        user_id: &str,
        library_ids: Option<Vec<String>>,
        book_ids: Option<Vec<String>>,
    ) -> Result<()> {
        let user_id = user_id.to_string();
        let library_ids = library_ids.unwrap_or_default();
        let book_ids = book_ids.unwrap_or_default();

        self.db
            .transaction(move |tx| {
                // Update library access
                tx.execute(
                    "DELETE FROM user_library_access WHERE user_id = ?",
                    [&user_id],
                )
                .map_err(TingError::DatabaseError)?;

                for lib_id in library_ids {
                    tx.execute(
                        "INSERT INTO user_library_access (user_id, library_id) VALUES (?, ?)",
                        [&user_id, &lib_id],
                    )
                    .map_err(TingError::DatabaseError)?;
                }

                // Update book access
                tx.execute("DELETE FROM user_book_access WHERE user_id = ?", [&user_id])
                    .map_err(TingError::DatabaseError)?;

                for book_id in book_ids {
                    tx.execute(
                        "INSERT INTO user_book_access (user_id, book_id) VALUES (?, ?)",
                        [&user_id, &book_id],
                    )
                    .map_err(TingError::DatabaseError)?;
                }

                Ok(())
            })
            .await
    }

    /// Get accessible library IDs for a user
    pub async fn get_accessible_libraries(&self, user_id: &str) -> Result<Vec<String>> {
        let user_id = user_id.to_string();
        self.db
            .execute(move |conn| {
                let mut stmt = conn
                    .prepare("SELECT library_id FROM user_library_access WHERE user_id = ?")
                    .map_err(TingError::DatabaseError)?;

                let ids = stmt
                    .query_map([&user_id], |row| row.get(0))
                    .map_err(TingError::DatabaseError)?
                    .collect::<std::result::Result<Vec<String>, _>>()
                    .map_err(TingError::DatabaseError)?;

                Ok(ids)
            })
            .await
    }

    /// Get accessible book IDs for a user
    pub async fn get_accessible_books(&self, user_id: &str) -> Result<Vec<String>> {
        let user_id = user_id.to_string();
        self.db
            .execute(move |conn| {
                let mut stmt = conn
                    .prepare("SELECT book_id FROM user_book_access WHERE user_id = ?")
                    .map_err(TingError::DatabaseError)?;

                let ids = stmt
                    .query_map([&user_id], |row| row.get(0))
                    .map_err(TingError::DatabaseError)?
                    .collect::<std::result::Result<Vec<String>, _>>()
                    .map_err(TingError::DatabaseError)?;

                Ok(ids)
            })
            .await
    }
}

#[async_trait]
impl Repository<User> for UserRepository {
    async fn find_by_id(&self, id: &str) -> Result<Option<User>> {
        let id = id.to_string();
        self.db
            .execute(move |conn| {
                conn.query_row(
                    "SELECT id, username, password_hash, role, created_at FROM users WHERE id = ?",
                    [&id],
                    |row| {
                        Ok(User {
                            id: row.get(0)?,
                            username: row.get(1)?,
                            password_hash: row.get(2)?,
                            role: row.get(3)?,
                            created_at: row.get(4)?,
                        })
                    },
                )
                .optional()
                .map_err(TingError::DatabaseError)
            })
            .await
    }

    async fn find_all(&self) -> Result<Vec<User>> {
        self.db.execute(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, username, password_hash, role, created_at FROM users ORDER BY created_at DESC"
            ).map_err(TingError::DatabaseError)?;

            let users = stmt.query_map([], |row| {
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    password_hash: row.get(2)?,
                    role: row.get(3)?,
                    created_at: row.get(4)?,
                })
            }).map_err(TingError::DatabaseError)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(TingError::DatabaseError)?;

            Ok(users)
        }).await
    }

    async fn create(&self, user: &User) -> Result<()> {
        let user = user.clone();
        self.db
            .execute(move |conn| {
                conn.execute(
                    "INSERT INTO users (id, username, password_hash, role) VALUES (?, ?, ?, ?)",
                    rusqlite::params![&user.id, &user.username, &user.password_hash, &user.role,],
                )
                .map_err(TingError::DatabaseError)?;
                Ok(())
            })
            .await
    }

    async fn update(&self, user: &User) -> Result<()> {
        let user = user.clone();
        self.db
            .execute(move |conn| {
                conn.execute(
                    "UPDATE users SET username = ?, password_hash = ?, role = ? WHERE id = ?",
                    rusqlite::params![&user.username, &user.password_hash, &user.role, &user.id,],
                )
                .map_err(TingError::DatabaseError)?;
                Ok(())
            })
            .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.db
            .execute(move |conn| {
                conn.execute("DELETE FROM users WHERE id = ?", [&id])
                    .map_err(TingError::DatabaseError)?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::UserRepository;
    use crate::db::manager::DatabaseManager;
    use crate::db::models::{User, UserSettings};
    use crate::db::repository::user_settings::UserSettingsRepository;
    use std::sync::Arc;

    #[tokio::test]
    async fn creates_user_and_initial_settings_in_one_operation() {
        let db = Arc::new(DatabaseManager::new_in_memory().unwrap());
        let user_repo = UserRepository::new(db.clone());
        let settings_repo = UserSettingsRepository::new(db);
        let user = User {
            id: "initial-admin".to_string(),
            username: "admin".to_string(),
            password_hash: "hash".to_string(),
            role: "admin".to_string(),
            created_at: "2026-08-12T00:00:00Z".to_string(),
        };
        let settings = UserSettings {
            user_id: user.id.clone(),
            playback_speed: 1.0,
            theme: "auto".to_string(),
            auto_play: 1,
            skip_intro: 0,
            skip_outro: 0,
            settings_json: Some(r#"{"language":"en-US"}"#.to_string()),
            updated_at: "2026-08-12T00:00:00Z".to_string(),
        };

        user_repo
            .create_with_settings(&user, &settings)
            .await
            .unwrap();

        assert_eq!(user_repo.count().await.unwrap(), 1);
        assert_eq!(
            settings_repo
                .get_by_user(&user.id)
                .await
                .unwrap()
                .and_then(|settings| settings.settings_json),
            Some(r#"{"language":"en-US"}"#.to_string())
        );
    }

    #[tokio::test]
    async fn rejects_initial_settings_for_a_different_user() {
        let db = Arc::new(DatabaseManager::new_in_memory().unwrap());
        let user_repo = UserRepository::new(db);
        let user = User {
            id: "initial-admin".to_string(),
            username: "admin".to_string(),
            password_hash: "hash".to_string(),
            role: "admin".to_string(),
            created_at: "2026-08-12T00:00:00Z".to_string(),
        };
        let settings = UserSettings {
            user_id: "another-user".to_string(),
            playback_speed: 1.0,
            theme: "auto".to_string(),
            auto_play: 1,
            skip_intro: 0,
            skip_outro: 0,
            settings_json: Some(r#"{"language":"en-US"}"#.to_string()),
            updated_at: "2026-08-12T00:00:00Z".to_string(),
        };

        assert!(
            user_repo
                .create_with_settings(&user, &settings)
                .await
                .is_err()
        );
        assert_eq!(user_repo.count().await.unwrap(), 0);
    }
}
