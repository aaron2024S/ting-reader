use super::normalize_rss_sync_config;
use serde_json::json;

#[test]
fn rss_config_keeps_only_scheduled_sync_settings() {
    let normalized = normalize_rss_sync_config(json!({
        "default_sources": ["scraper"],
        "metadata_writing_enabled": true,
        "scheduled_sync_enabled": true,
        "scheduled_sync_interval": "weekly",
    }))
    .expect("scheduled sync settings should be retained");

    assert_eq!(
        normalized,
        json!({
            "scheduled_sync_enabled": true,
            "scheduled_sync_interval": "weekly",
        })
    );
}

#[test]
fn rss_config_without_schedule_is_omitted() {
    assert!(
        normalize_rss_sync_config(json!({
            "default_sources": ["scraper"],
        }))
        .is_none()
    );
}
