use crate::core::error::{Result, TingError};
use crate::core::time::{DEFAULT_TIME_ZONE, parse_time_zone};
use crate::db::manager::DatabaseManager;
use rusqlite::OptionalExtension;
use std::sync::Arc;

pub const APPLICATION_TIME_ZONE_KEY: &str = "application_time_zone";

pub struct SystemSettingsRepository {
    db: Arc<DatabaseManager>,
}

impl SystemSettingsRepository {
    pub fn new(db: Arc<DatabaseManager>) -> Self {
        Self { db }
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_string();
        self.db
            .execute(move |conn| {
                conn.query_row(
                    "SELECT value FROM system_settings WHERE key = ?",
                    [&key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(TingError::DatabaseError)
            })
            .await
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<()> {
        let key = key.to_string();
        let value = value.to_string();
        self.db
            .execute(move |conn| {
                conn.execute(
                    "INSERT INTO system_settings (key, value, updated_at) VALUES (?, ?, CURRENT_TIMESTAMP) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = CURRENT_TIMESTAMP",
                    rusqlite::params![key, value],
                )
                .map_err(TingError::DatabaseError)?;
                Ok(())
            })
            .await
    }

    pub async fn get_application_time_zone(&self) -> Result<Option<String>> {
        self.get(APPLICATION_TIME_ZONE_KEY).await
    }

    pub async fn set_application_time_zone(&self, time_zone: &str) -> Result<()> {
        let time_zone = parse_time_zone(time_zone).map_err(TingError::ValidationError)?;
        self.set(APPLICATION_TIME_ZONE_KEY, &time_zone).await
    }

    pub async fn application_time_zone_or_default(&self) -> Result<String> {
        Ok(self
            .get_application_time_zone()
            .await?
            .and_then(|time_zone| parse_time_zone(&time_zone).ok())
            .unwrap_or_else(|| DEFAULT_TIME_ZONE.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::SystemSettingsRepository;
    use crate::db::manager::DatabaseManager;
    use std::sync::Arc;

    #[tokio::test]
    async fn stores_the_application_time_zone() {
        let db = Arc::new(DatabaseManager::new_in_memory().unwrap());
        let repository = SystemSettingsRepository::new(db);

        repository
            .set_application_time_zone("Asia/Shanghai")
            .await
            .unwrap();

        assert_eq!(
            repository.application_time_zone_or_default().await.unwrap(),
            "Asia/Shanghai"
        );
    }

    #[tokio::test]
    async fn rejects_invalid_application_time_zones() {
        let db = Arc::new(DatabaseManager::new_in_memory().unwrap());
        let repository = SystemSettingsRepository::new(db);

        assert!(
            repository
                .set_application_time_zone("not/a-time-zone")
                .await
                .is_err()
        );
    }
}
