use super::*;
use crate::plugin::config::PluginConfigManager;
use crate::plugin::manager::PluginConfig;
use crate::plugin::types::PluginDependency;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

struct TestPlugin {
    metadata: PluginMetadata,
}

#[async_trait::async_trait]
impl Plugin for TestPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn initialize(&self, _context: &PluginContext) -> Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

fn test_metadata(id: &str, name: &str, version: &str) -> PluginMetadata {
    let mut metadata = PluginMetadata::new(
        id.to_string(),
        name.to_string(),
        version.to_string(),
        "Ting Reader".to_string(),
        "Test plugin".to_string(),
        "plugin.js".to_string(),
    );
    metadata.min_core_version = Some(env!("CARGO_PKG_VERSION").to_string());
    metadata
}

fn test_entry(metadata: PluginMetadata) -> PluginEntry {
    PluginEntry::new(
        metadata.clone(),
        Arc::new(TestPlugin { metadata }) as Arc<dyn Plugin>,
    )
}

#[test]
fn create_context_uses_persisted_config_when_available() {
    let temp_dir = tempfile::tempdir().unwrap();
    let plugin_dir = temp_dir.path().join("plugins");
    let manager = PluginManager::new(PluginConfig {
        plugin_dir: plugin_dir.clone(),
        enable_hot_reload: false,
        max_memory_per_plugin: 128 * 1024 * 1024,
        max_execution_time: Duration::from_secs(30),
    })
    .unwrap();

    let config_manager =
        Arc::new(PluginConfigManager::new(plugin_dir.join("configs"), [7u8; 32]).unwrap());
    manager.set_config_manager(config_manager.clone());

    let metadata = PluginMetadata::new(
        "config-plugin".to_string(),
        "Config Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Config plugin".to_string(),
        "plugin.js".to_string(),
    )
    .with_config_schema(json!({
        "type": "object",
        "properties": {
            "api_key": {
                "type": "string",
                "default": "schema-default"
            },
            "enabled": {
                "type": "boolean",
                "default": false
            }
        }
    }));

    let plugin_id = metadata.instance_id();
    config_manager
        .initialize_config(
            plugin_id,
            metadata.name.clone(),
            metadata.config_schema.clone(),
            json!({
                "api_key": "persisted-value",
                "enabled": true
            }),
        )
        .unwrap();

    let context = manager.create_plugin_context(&metadata).unwrap();

    assert_eq!(context.config["api_key"], "persisted-value");
    assert_eq!(context.config["enabled"], true);
}

#[test]
fn validate_core_compatibility_rejects_future_core_requirement() {
    let mut metadata = PluginMetadata::new(
        "future-plugin".to_string(),
        "Future Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Future plugin".to_string(),
        "plugin.js".to_string(),
    );
    metadata.min_core_version = Some("999.0.0".to_string());

    let error = PluginManager::validate_core_compatibility(&metadata).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("requires Ting Reader core >= 999.0.0")
    );
}

#[test]
fn validate_core_compatibility_accepts_exact_contract_floor() {
    let mut metadata = PluginMetadata::new(
        "format-plugin".into(),
        "Format Plugin".into(),
        "2.0.0".into(),
        "Ting Reader".into(),
        "Test plugin".into(),
        "plugin.js".into(),
    );
    metadata.min_core_version = Some(ting_plugin_contract::MIN_PLUGIN_CORE_VERSION.to_string());
    PluginManager::validate_core_compatibility_for_version(
        &metadata,
        ting_plugin_contract::MIN_PLUGIN_CORE_VERSION,
    )
    .unwrap();
    PluginManager::validate_core_compatibility(&metadata).unwrap();
}

#[test]
fn validate_core_compatibility_rejects_v_prefixed_versions() {
    let mut metadata = PluginMetadata::new(
        "current-plugin".to_string(),
        "Current Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Current plugin".to_string(),
        "plugin.js".to_string(),
    );
    metadata.min_core_version = Some("v2.0.0".to_string());

    let error = PluginManager::validate_core_compatibility(&metadata).unwrap_err();
    assert!(error.to_string().contains("Invalid min_core_version"));
}

#[test]
fn validate_core_compatibility_rejects_missing_core_requirement() {
    let metadata = PluginMetadata::new(
        "missing-core-plugin".to_string(),
        "Missing Core Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Missing core requirement".to_string(),
        "plugin.js".to_string(),
    );

    let error = PluginManager::validate_core_compatibility(&metadata).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("must declare min_core_version >= 2.0.0")
    );
}

#[test]
fn validate_core_compatibility_rejects_old_contract_even_when_current_satisfies_it() {
    let mut metadata = PluginMetadata::new(
        "old-core-plugin".to_string(),
        "Old Core Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Old core requirement".to_string(),
        "plugin.js".to_string(),
    );
    metadata.min_core_version = Some("1.4.7".to_string());

    let error =
        PluginManager::validate_core_compatibility_for_version(&metadata, "2.0.0").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("new plugin contract requires >= 2.0.0")
    );
}

#[test]
fn validate_core_compatibility_rejects_when_current_core_is_too_old() {
    let mut metadata = PluginMetadata::new(
        "new-core-plugin".to_string(),
        "New Core Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "New core requirement".to_string(),
        "plugin.js".to_string(),
    );
    metadata.min_core_version = Some("2.0.0".to_string());

    let error =
        PluginManager::validate_core_compatibility_for_version(&metadata, "1.6.1").unwrap_err();

    assert!(
        error
            .to_string()
            .contains("requires Ting Reader core >= 2.0.0")
    );
    assert!(error.to_string().contains("current core version is 1.6.1"));
}

#[tokio::test]
async fn validate_plugin_dependencies_requires_loaded_dependency() {
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(PluginConfig {
        plugin_dir: temp_dir.path().join("plugins"),
        enable_hot_reload: false,
        max_memory_per_plugin: 128 * 1024 * 1024,
        max_execution_time: Duration::from_secs(30),
    })
    .unwrap();

    let mut dependent = test_metadata("dependent-plugin", "Dependent Plugin", "1.0.0");
    dependent.dependencies.push(PluginDependency::new(
        "base-plugin".to_string(),
        "^1.0.0".to_string(),
    ));

    assert!(
        manager
            .validate_plugin_dependencies(&dependent, None)
            .await
            .is_err()
    );

    let base = test_metadata("base-plugin", "Base Plugin", "1.2.0");
    manager
        .registry
        .write()
        .await
        .insert(base.instance_id(), test_entry(base));

    manager
        .validate_plugin_dependencies(&dependent, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn uninstall_plugin_is_blocked_when_other_plugins_depend_on_it() {
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(PluginConfig {
        plugin_dir: temp_dir.path().join("plugins"),
        enable_hot_reload: false,
        max_memory_per_plugin: 128 * 1024 * 1024,
        max_execution_time: Duration::from_secs(30),
    })
    .unwrap();

    let base = test_metadata("base-plugin", "Base Plugin", "1.0.0");
    let base_id = base.instance_id();
    let mut dependent = test_metadata("dependent-plugin", "Dependent Plugin", "1.0.0");
    dependent.dependencies.push(PluginDependency::new(
        "base-plugin".to_string(),
        "^1.0.0".to_string(),
    ));

    let mut registry = manager.registry.write().await;
    registry.insert(base_id.clone(), test_entry(base));
    registry.insert(dependent.instance_id(), test_entry(dependent));
    drop(registry);

    let error = manager.uninstall_plugin(&base_id).await.unwrap_err();

    assert!(matches!(error, TingError::DependencyError(_)));
    assert!(error.to_string().contains("dependent-plugin@1.0.0"));
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn test_plugin_manager_send_sync() {
    assert_send_sync::<PluginManager>();
}
