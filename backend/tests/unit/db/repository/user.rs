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
