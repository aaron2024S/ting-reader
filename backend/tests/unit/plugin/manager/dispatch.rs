use super::*;
use crate::plugin::manager::{FailedPlugin, PluginConfig, PluginEntry};
use crate::plugin::types::{Plugin, PluginMetadata};
use std::sync::Arc;

#[tokio::test]
async fn zero_permission_js_uses_host_limits_and_marks_failed_after_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = serde_json::json!({
        "id": "bounded-js", "name": "Bounded JS", "version": "2.0.0",
        "min_core_version": "2.0.0", "author": "Test",
        "description": {"en": "Test"}, "runtime": "javascript",
        "entry_point": "plugin.js", "dependencies": [], "permissions": [],
        "capabilities": [
            {"kind": "plugin_store", "id": "test.store", "operations": ["list_plugins"]}
        ],
    });
    std::fs::write(
        dir.path().join("plugin.yml"),
        serde_yaml::to_string(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("plugin.js"),
        "export function list_plugins() { while (true) {} }",
    )
    .unwrap();
    let manager = PluginManager::new(PluginConfig {
        plugin_dir: dir.path().into(),
        enable_hot_reload: false,
        max_memory_per_plugin: 32 * 1024 * 1024,
        max_execution_time: Duration::from_millis(100),
    })
    .unwrap();
    let metadata = crate::plugin::types::metadata::read_plugin_metadata(dir.path()).unwrap();
    let id = metadata.instance_id();
    let instance = manager
        .load_plugin_instance(dir.path(), &metadata)
        .await
        .unwrap();
    let mut entry = PluginEntry::new(metadata, instance);
    entry.set_state(PluginState::Active);
    manager.registry.write().await.insert(id.clone(), entry);
    let scope = manager
        .resource_scope(
            &id,
            &Default::default(),
            dir.path().into(),
            Default::default(),
        )
        .await
        .unwrap();
    let started = Instant::now();
    let error = manager
        .invoke_plugin(
            &id,
            "list_plugins",
            serde_json::json!({}),
            &PluginInvocationContext {
                user: None,
                resources: Some(scope.clone()),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, TingError::Timeout(_)), "{error}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(scope.cancellation_token().is_cancelled());
    let registry = manager.registry.read().await;
    let entry = registry.get(&id).unwrap();
    assert_eq!(entry.state, PluginState::Failed);
    assert!(entry.load_error.as_ref().unwrap().contains("deadline"));
    assert!(!entry.instance.is_available());
}

#[test]
fn metadata_search_requires_the_formal_request_and_result() {
    let capability: PluginCapability = serde_json::from_value(serde_json::json!({
        "kind": "metadata_provider", "id": "metadata.search", "operations": ["search"],
        "auto_scrape": false, "search_fields": [{
            "key": "title", "label": {"en": "Title"}, "required": false, "type": "text"
        }], "result_fields": [{"key": "title", "label": {"en": "Title"}}]
    }))
    .unwrap();
    let request = serde_json::json!({
        "title": "Book", "author": null, "narrator": null,
        "page": 1, "page_size": 20, "filters": {},
        "chapter_candidates": [], "context": null
    });
    assert!(validate_capability_input(&capability, "search", &request).is_ok());
    assert!(
        validate_capability_input(
            &capability,
            "search",
            &serde_json::json!({
                "query": "Book", "page": 1, "page_size": 20, "filters": {}
            })
        )
        .is_err()
    );
    let mut output = serde_json::to_value(ting_plugin_contract::scraper::SearchPage {
        items: vec![ting_plugin_contract::scraper::ScraperResult {
            id: None,
            source_url: None,
            title: "Book".into(),
            author: None,
            narrator: None,
            cover_url: None,
            intro: None,
            subtitle: None,
            publisher: None,
            language: None,
            genre: None,
            published_year: None,
            published_date: None,
            isbn: None,
            asin: None,
            explicit: None,
            abridged: None,
            tags: Vec::new(),
            duration: None,
            score: None,
            chapter_title_template: None,
            chapter_titles: Vec::new(),
        }],
        page: 1,
        page_size: 20,
        total: None,
        has_more: None,
    })
    .unwrap();
    assert!(validate_capability_output(&capability, "search", &request, &output).is_ok());
    output["items"][0]["artist"] = serde_json::json!("old alias");
    assert!(validate_capability_output(&capability, "search", &request, &output).is_err());
}

#[test]
fn format_call_validates_declared_operation_request_and_response() {
    let capability: PluginCapability = serde_json::from_value(serde_json::json!({
        "kind": "format_handler",
        "id": "special.audio",
        "extensions": ["special"],
        "operations": ["probe", "get_metadata_read_size", "extract_metadata"]
    }))
    .unwrap();
    let input = serde_json::json!({
        "input": "host:opaque",
        "extension_hint": "special",
        "mime_hint": null,
        "prefix_bytes": 4096
    });
    assert!(validate_capability_input(&capability, "probe", &input).is_ok());
    let more = serde_json::json!({"kind": "need_more", "total_bytes": 8192});
    assert!(validate_capability_output(&capability, "probe", &input, &more).is_ok());
    let repeated = serde_json::json!({"kind": "need_more", "total_bytes": 4096});
    assert!(validate_capability_output(&capability, "probe", &input, &repeated).is_err());
    assert!(validate_capability_input(
        &capability,
        "probe",
        &serde_json::json!({"input": "host:opaque", "prefix_bytes": 4096, "file_path": "C:/secret"})
    )
    .is_err());
    assert!(validate_capability_input(&capability, "open_decrypt", &input).is_err());
}

#[tokio::test]
async fn record_plugin_call_updates_listed_stats() {
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(PluginConfig {
        plugin_dir: temp_dir.path().join("plugins"),
        enable_hot_reload: false,
        max_memory_per_plugin: 128 * 1024 * 1024,
        max_execution_time: Duration::from_secs(30),
    })
    .unwrap();

    let metadata = PluginMetadata::new(
        "stats-plugin".to_string(),
        "Stats Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Stats plugin".to_string(),
        "plugin.js".to_string(),
    );
    let plugin_id = metadata.instance_id();
    let instance =
        Arc::new(FailedPlugin::new(metadata.clone(), "unused".to_string())) as Arc<dyn Plugin>;

    manager
        .registry
        .write()
        .await
        .insert(plugin_id.clone(), PluginEntry::new(metadata, instance));

    manager
        .record_plugin_call(
            &plugin_id,
            "search",
            Duration::from_millis(25),
            None,
            PluginLogLevel::Info,
        )
        .await;
    manager
        .record_plugin_call(
            &plugin_id,
            "search",
            Duration::from_millis(5),
            Some(&TingError::PluginExecutionError("boom".to_string())),
            PluginLogLevel::Info,
        )
        .await;

    let plugins = manager.list_plugins().await;
    let stats_plugin = plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)
        .expect("stats plugin should be listed");

    assert_eq!(stats_plugin.total_calls, 2);
    assert_eq!(stats_plugin.successful_calls, 1);
    assert_eq!(stats_plugin.failed_calls, 1);
}

#[tokio::test]
async fn invoke_plugin_records_failure_for_unsupported_runtime() {
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(PluginConfig {
        plugin_dir: temp_dir.path().join("plugins"),
        enable_hot_reload: false,
        max_memory_per_plugin: 128 * 1024 * 1024,
        max_execution_time: Duration::from_secs(30),
    })
    .unwrap();

    let metadata = PluginMetadata::new(
        "unsupported-plugin".to_string(),
        "Unsupported Plugin".to_string(),
        "1.0.0".to_string(),
        "Ting Reader".to_string(),
        "Unsupported plugin".to_string(),
        "plugin.js".to_string(),
    );
    let plugin_id = metadata.instance_id();
    let instance =
        Arc::new(FailedPlugin::new(metadata.clone(), "unused".to_string())) as Arc<dyn Plugin>;

    manager
        .registry
        .write()
        .await
        .insert(plugin_id.clone(), PluginEntry::new(metadata, instance));

    let result = manager
        .invoke_plugin(
            &plugin_id,
            "anything",
            serde_json::json!({}),
            &PluginInvocationContext::default(),
        )
        .await;

    assert!(result.is_err());

    let plugins = manager.list_plugins().await;
    let plugin = plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)
        .expect("plugin should be listed");

    assert_eq!(plugin.total_calls, 1);
    assert_eq!(plugin.failed_calls, 1);
}
