use super::*;

#[tokio::test]
async fn plugin_cache_is_scoped_by_plugin_id() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cache = PluginCache::new(temp_dir.path().to_path_buf()).unwrap();

    cache
        .set("plugin-a", "shared-key", serde_json::json!({"value": 1}))
        .await
        .unwrap();
    cache
        .set("plugin-b", "shared-key", serde_json::json!({"value": 2}))
        .await
        .unwrap();

    assert_eq!(
        cache
            .get("plugin-a", "shared-key")
            .await
            .unwrap()
            .unwrap()
            .value,
        serde_json::json!({"value": 1})
    );
    assert_eq!(
        cache
            .get("plugin-b", "shared-key")
            .await
            .unwrap()
            .unwrap()
            .value,
        serde_json::json!({"value": 2})
    );
}

#[test]
fn plugin_cache_key_rejects_control_characters() {
    assert!(validate_plugin_cache_key("reader:last-open").is_ok());
    assert!(validate_plugin_cache_key("bad\nkey").is_err());
    assert!(validate_plugin_cache_key("").is_err());
}

#[tokio::test]
async fn plugin_cache_can_be_migrated_and_deleted_as_a_namespace() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cache = PluginCache::new(temp_dir.path().to_path_buf()).unwrap();

    cache
        .set(
            "plugin-a@1.0.0",
            "conversation-index",
            serde_json::json!({"items": ["conversation-1"]}),
        )
        .await
        .unwrap();

    assert!(
        cache
            .migrate_plugin("plugin-a@1.0.0", "plugin-a@1.1.0")
            .await
            .unwrap()
    );
    assert!(
        cache
            .get("plugin-a@1.0.0", "conversation-index")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        cache
            .get("plugin-a@1.1.0", "conversation-index")
            .await
            .unwrap()
            .unwrap()
            .value,
        serde_json::json!({"items": ["conversation-1"]})
    );

    assert!(cache.delete_plugin("plugin-a@1.1.0").await.unwrap());
    assert!(
        cache
            .get("plugin-a@1.1.0", "conversation-index")
            .await
            .unwrap()
            .is_none()
    );
}
