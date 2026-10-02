use super::*;
use crate::plugin::manager::{PluginConfig, PluginManager};
use crate::plugin::types::PluginDependency;
use std::time::Duration;

#[tokio::test]
async fn discovery_ignores_workspace_and_manifestless_directories() {
    let temp_dir = tempfile::tempdir().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");
    let missing_manifest_dir = plugin_dir.join("missing-manifest@1.2.3");
    tokio::fs::create_dir_all(&missing_manifest_dir)
        .await
        .unwrap();
    for name in ["staging", "data", "configs", "temp", "notes"] {
        tokio::fs::create_dir_all(plugin_dir.join(name))
            .await
            .unwrap();
    }
    // Even stray manifests inside Host-owned staging are not plugin candidates.
    tokio::fs::write(plugin_dir.join("staging/plugin.yml"), "invalid: [")
        .await
        .unwrap();
    tokio::fs::create_dir_all(plugin_dir.join("not-a-plugin/plugin.yaml"))
        .await
        .unwrap();

    let manager = PluginManager::new(PluginConfig {
        plugin_dir: plugin_dir.clone(),
        enable_hot_reload: false,
        max_memory_per_plugin: 128 * 1024 * 1024,
        max_execution_time: Duration::from_secs(30),
    })
    .unwrap();

    manager.discover_plugins(&plugin_dir).await.unwrap();
    let plugins = manager.list_plugins().await;

    assert!(plugins.is_empty());
    assert!(manager.metadata_cache.read().await.is_empty());
}

#[test]
fn discovery_load_order_places_dependencies_first() {
    let base = PluginMetadata::new(
        "base-plugin".to_string(),
        "Base Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Base plugin".to_string(),
        "plugin.js".to_string(),
    );
    let mut dependent = PluginMetadata::new(
        "dependent-plugin".to_string(),
        "Dependent Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Dependent plugin".to_string(),
        "plugin.js".to_string(),
    );
    dependent.dependencies.push(PluginDependency::new(
        "base-plugin".to_string(),
        "^1.0.0".to_string(),
    ));

    let plugins = vec![
        (dependent, PathBuf::from("dependent")),
        (base, PathBuf::from("base")),
    ];

    let order = resolve_discovery_load_order(&plugins);

    assert_eq!(order, vec![1, 0]);
}
