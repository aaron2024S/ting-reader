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
