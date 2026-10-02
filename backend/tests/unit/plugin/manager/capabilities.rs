use super::*;
use crate::plugin::manager::{FailedPlugin, PluginConfig, PluginEntry};
use crate::plugin::types::{Plugin, PluginMetadata};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn test_manager() -> PluginManager {
    let temp_dir = tempfile::tempdir().unwrap();
    PluginManager::new(PluginConfig {
        plugin_dir: temp_dir.path().join("plugins"),
        enable_hot_reload: false,
        max_memory_per_plugin: 128 * 1024 * 1024,
        max_execution_time: Duration::from_secs(30),
    })
    .unwrap()
}

async fn insert_metadata(manager: &PluginManager, metadata: PluginMetadata) -> PluginId {
    let plugin_id = metadata.instance_id();
    let instance =
        Arc::new(FailedPlugin::new(metadata.clone(), "unused".to_string())) as Arc<dyn Plugin>;

    manager
        .registry
        .write()
        .await
        .insert(plugin_id.clone(), PluginEntry::new(metadata, instance));

    plugin_id
}

#[tokio::test]
async fn capability_registry_lists_declared_capabilities() {
    let manager = test_manager();
    let mut metadata = PluginMetadata::new(
        "metadata-plugin".to_string(),
        "Metadata Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Metadata provider".to_string(),
        "plugin.js".to_string(),
    );
    metadata.capabilities.push(serde_json::from_value(json!({"id": "metadata.search", "kind": "metadata_provider", "operations": ["search"], "search_fields": [{"key": "title", "label": {"en": "Title"}, "required": true, "type": "text"}], "result_fields": [{"key": "title", "label": {"en": "Title"}}]})).unwrap());
    let plugin_id = insert_metadata(&manager, metadata).await;

    let capabilities = manager.list_capabilities().await;

    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].plugin_id, plugin_id);
    assert_eq!(capabilities[0].capability.kind(), "metadata_provider");
}

#[tokio::test]
async fn capability_registry_lists_admin_only_flag() {
    let manager = test_manager();
    let mut metadata = PluginMetadata::new(
        "admin-panel".to_string(),
        "Admin Panel".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Admin panel".to_string(),
        "plugin.js".to_string(),
    );
    metadata.admin_only = true;
    metadata.capabilities.push(serde_json::from_value(json!({"id": "admin.panel", "kind": "ui_extension", "slots": ["global.panel"], "contexts": ["global"], "title": {"en": "Admin"}, "render": {"mode": "action", "bridge": {"capabilities": [], "host_methods": []}}})).unwrap());
    insert_metadata(&manager, metadata).await;

    let capabilities = manager.list_capabilities().await;

    assert_eq!(capabilities.len(), 1);
    assert!(capabilities[0].admin_only);
}

#[tokio::test]
async fn capability_registry_matches_http_route_with_params() {
    let manager = test_manager();
    let mut metadata = PluginMetadata::new(
        "rss-plugin".to_string(),
        "RSS Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "RSS generator".to_string(),
        "plugin.js".to_string(),
    );
    metadata.capabilities.push(serde_json::from_value(json!({"id": "rss.feed", "kind": "http_route", "route": {"method": "GET", "path": "/rss/{library_id}/feed.xml", "auth": "signed"}})).unwrap());
    let plugin_id = insert_metadata(&manager, metadata).await;

    let matched = manager
        .find_http_route("GET", "/rss/main/feed.xml")
        .await
        .expect("route should match");

    assert_eq!(matched.registration.plugin_id, plugin_id);
    assert!(matched.registration.capability.supports("handle"));
    assert_eq!(matched.params["library_id"], "main");
    assert!(
        manager
            .find_http_route("POST", "/rss/main/feed.xml")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn overlapping_routes_and_duplicate_plugin_id_are_rejected_before_registration() {
    let manager = test_manager();
    let mut first = PluginMetadata::new(
        "source-a".into(),
        "Source A".into(),
        "2.0.0".into(),
        "Ting Reader".into(),
        "Test".into(),
        "plugin.js".into(),
    );
    first.capabilities.push(serde_json::from_value(json!({
        "kind":"http_route","id":"feed","route":{"method":"GET","path":"/rss/{book_id}/feed.xml","auth":"signed"}
    })).unwrap());
    let id = insert_metadata(&manager, first).await;
    let mut conflicting = PluginMetadata::new(
        "source-b".into(),
        "Source B".into(),
        "2.0.0".into(),
        "Ting Reader".into(),
        "Test".into(),
        "plugin.js".into(),
    );
    conflicting.capabilities.push(serde_json::from_value(json!({
        "kind":"http_route","id":"feed","route":{"method":"GET","path":"/rss/latest/feed.xml","auth":"public"}
    })).unwrap());
    let registry = manager.registry.read().await;
    let error =
        PluginManager::validate_capability_registration(&registry, &conflicting, None).unwrap_err();
    assert!(error.to_string().contains("Route conflict"));
    conflicting.id = "source-a".into();
    conflicting.version = "2.0.1".into();
    let error =
        PluginManager::validate_capability_registration(&registry, &conflicting, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("already has a registered instance")
    );
    assert_eq!(id, "source-a@2.0.0");
}

#[tokio::test]
async fn capability_registry_finds_content_processor_by_extension_and_operation() {
    let manager = test_manager();
    let mut metadata = PluginMetadata::new(
        "txt-reader".to_string(),
        "TXT Reader".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "TXT reader".to_string(),
        "plugin.js".to_string(),
    );
    metadata.capabilities.push(serde_json::from_value(json!({"id": "document.reader", "kind": "content_processor", "extensions": ["txt", "md"], "operations": ["probe", "open", "close", "cancel", "extract_metadata", "read_text"]})).unwrap());
    let plugin_id = insert_metadata(&manager, metadata).await;

    let matched = manager
        .find_content_processors(".TXT", Some("read_text"))
        .await;

    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].registration.plugin_id, plugin_id);
    assert!(
        manager
            .find_content_processors("pdf", Some("read_text"))
            .await
            .is_empty()
    );
    assert!(
        manager
            .find_content_processors("txt", Some("render_page"))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn capability_registry_finds_tool_provider_by_declared_tool() {
    let manager = test_manager();
    let mut metadata = PluginMetadata::new(
        "assistant-tools".to_string(),
        "Assistant Tools".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Assistant tools".to_string(),
        "plugin.js".to_string(),
    );
    metadata.capabilities.push(serde_json::from_value(json!({"id": "assistant.tools", "kind": "tool_provider", "invoke": "invokeTool", "tools": [{"name": "book.search", "description": {"en": "Search books"}, "input_schema": {"type": "object"}, "output_schema": {"type": "object"}, "side_effects": false}, {"name": "library.stats", "description": {"en": "Search books"}, "input_schema": {"type": "object"}, "output_schema": {"type": "object"}, "side_effects": false}]})).unwrap());
    let plugin_id = insert_metadata(&manager, metadata).await;

    let matched = manager.find_tool_providers(Some("book.search")).await;

    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].registration.plugin_id, plugin_id);
    assert_eq!(matched[0].tool.as_ref().unwrap().name, "book.search");
    assert!(
        manager
            .find_tool_providers(Some("missing.tool"))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn capability_registry_finds_task_handler_by_task_type() {
    let manager = test_manager();
    let mut metadata = PluginMetadata::new(
        "batch-tools".to_string(),
        "Batch Tools".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Batch tools".to_string(),
        "plugin.js".to_string(),
    );
    metadata.capabilities.push(serde_json::from_value(json!({"id": "batch.summarize", "kind": "task_handler", "tasks": [{"task_type": "book.summarize", "input_schema": {"type": "object"}, "output_schema": {"type": "object"}, "idempotent": true}, {"task_type": "library.reindex", "input_schema": {"type": "object"}, "output_schema": {"type": "object"}, "idempotent": true}]})).unwrap());
    let plugin_id = insert_metadata(&manager, metadata).await;

    let matched = manager.find_task_handlers(Some("book.summarize")).await;

    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].registration.plugin_id, plugin_id);
    assert!(
        manager
            .find_task_handlers(Some("missing.task"))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn capability_registry_finds_event_handler_by_event_name() {
    let manager = test_manager();
    let mut metadata = PluginMetadata::new(
        "event-tools".to_string(),
        "Event Tools".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Event tools".to_string(),
        "plugin.js".to_string(),
    );
    metadata.capabilities.push(serde_json::from_value(json!({"id": "events.all", "kind": "event_handler", "events": [{"name": "book.added", "schema": {"type": "object"}}]})).unwrap());
    let plugin_id = insert_metadata(&manager, metadata).await;

    let matched = manager.find_event_handlers(Some("book.added")).await;

    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].registration.plugin_id, plugin_id);
}
