use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ting_plugin_contract::capability::{ToolDeclaration, route_parameter};

use super::{PluginManager, PluginRegistry};
use crate::core::error::{Result, TingError};
use crate::plugin::types::PluginMetadata;
use crate::plugin::types::{PluginCapability, PluginId, PluginState};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegisteredCapability {
    pub plugin_id: PluginId,
    pub plugin_name: String,
    #[serde(default)]
    pub admin_only: bool,
    pub capability: PluginCapability,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchedHttpRoute {
    pub registration: RegisteredCapability,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchedContentProcessor {
    pub registration: RegisteredCapability,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchedToolProvider {
    pub registration: RegisteredCapability,
    pub tool: Option<ToolDeclaration>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchedTaskHandler {
    pub registration: RegisteredCapability,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchedEventHandler {
    pub registration: RegisteredCapability,
}

impl PluginManager {
    /// Called under the registry lock, so concurrent loads cannot both claim
    /// the same route or publish two versions of one stable plugin ID.
    pub(crate) fn validate_capability_registration(
        registry: &PluginRegistry,
        metadata: &PluginMetadata,
        replacing: Option<&str>,
    ) -> Result<()> {
        ting_plugin_contract::capability::validate_capabilities(&metadata.capabilities)
            .map_err(|error| TingError::PluginLoadError(error.to_string()))?;
        for (instance_id, entry) in registry {
            if entry.state == PluginState::Failed || replacing == Some(instance_id.as_str()) {
                continue;
            }
            if entry.metadata.id == metadata.id {
                return Err(TingError::PluginLoadError(format!(
                    "Plugin {} already has a registered instance: {}",
                    metadata.id, instance_id
                )));
            }
            for candidate in &metadata.capabilities {
                let PluginCapability::HttpRoute(candidate) = candidate else {
                    continue;
                };
                for existing in &entry.metadata.capabilities {
                    let PluginCapability::HttpRoute(existing) = existing else {
                        continue;
                    };
                    if ting_plugin_contract::capability::routes_overlap(
                        &candidate.route,
                        &existing.route,
                    ) {
                        return Err(TingError::PluginLoadError(format!(
                            "Route conflict: {} {} with plugin {} capability {}",
                            candidate.route.method.as_str(),
                            candidate.route.path,
                            entry.metadata.id,
                            existing.id
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn list_capabilities(&self) -> Vec<RegisteredCapability> {
        let registry = self.registry.read().await;
        let mut capabilities = Vec::new();
        for entry in registry.values() {
            if entry.state == PluginState::Failed {
                continue;
            }
            capabilities.extend(
                entry
                    .metadata
                    .capabilities
                    .iter()
                    .cloned()
                    .map(|capability| RegisteredCapability {
                        plugin_id: entry.metadata.instance_id(),
                        plugin_name: entry.metadata.name.clone(),
                        admin_only: entry.metadata.admin_only,
                        capability,
                    }),
            );
        }
        capabilities.sort_by(|a, b| {
            a.plugin_id
                .cmp(&b.plugin_id)
                .then_with(|| a.capability.id().cmp(b.capability.id()))
        });
        capabilities
    }

    pub async fn find_capabilities_by_kind(&self, kind: &str) -> Vec<RegisteredCapability> {
        self.list_capabilities()
            .await
            .into_iter()
            .filter(|r| r.capability.kind() == kind)
            .collect()
    }

    pub async fn find_http_route(&self, method: &str, path: &str) -> Option<MatchedHttpRoute> {
        let mut matched = None;
        for registration in self.find_capabilities_by_kind("http_route").await {
            let PluginCapability::HttpRoute(cap) = &registration.capability else {
                continue;
            };
            if cap.route.method.as_str() != method {
                continue;
            }
            let Some(params) = match_route_path(&cap.route.path, path) else {
                continue;
            };
            // Registration rejects conflicts. Fail closed even if an unvalidated
            // registry entry is introduced by an internal caller.
            if matched.is_some() {
                return None;
            }
            matched = Some(MatchedHttpRoute {
                registration,
                params,
            });
        }
        matched
    }

    pub async fn find_content_processors(
        &self,
        extension: &str,
        operation: Option<&str>,
    ) -> Vec<MatchedContentProcessor> {
        let extension = extension
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        self.find_capabilities_by_kind("content_processor")
            .await
            .into_iter()
            .filter(|r| {
                r.capability.extensions().contains(&extension)
                    && operation.is_none_or(|op| r.capability.supports(op))
            })
            .map(|registration| MatchedContentProcessor { registration })
            .collect()
    }

    pub async fn find_tool_providers(&self, tool_name: Option<&str>) -> Vec<MatchedToolProvider> {
        self.find_capabilities_by_kind("tool_provider")
            .await
            .into_iter()
            .filter_map(|registration| {
                let PluginCapability::ToolProvider(cap) = &registration.capability else {
                    return None;
                };
                let tool = tool_name
                    .and_then(|name| cap.tools.iter().find(|tool| tool.name == name))
                    .cloned();
                if tool_name.is_some() && tool.is_none() {
                    return None;
                }
                Some(MatchedToolProvider { registration, tool })
            })
            .collect()
    }

    pub async fn find_task_handlers(&self, task_type: Option<&str>) -> Vec<MatchedTaskHandler> {
        self.find_capabilities_by_kind("task_handler")
            .await
            .into_iter()
            .filter(|r| {
                let PluginCapability::TaskHandler(cap) = &r.capability else {
                    return false;
                };
                task_type.is_none_or(|name| cap.tasks.iter().any(|task| task.task_type == name))
            })
            .map(|registration| MatchedTaskHandler { registration })
            .collect()
    }

    pub async fn find_event_handlers(&self, event: Option<&str>) -> Vec<MatchedEventHandler> {
        self.find_capabilities_by_kind("event_handler")
            .await
            .into_iter()
            .filter(|r| {
                let PluginCapability::EventHandler(cap) = &r.capability else {
                    return false;
                };
                event.is_none_or(|name| cap.events.iter().any(|event| event.name == name))
            })
            .map(|registration| MatchedEventHandler { registration })
            .collect()
    }
}

fn match_route_path(pattern: &str, path: &str) -> Option<BTreeMap<String, String>> {
    let pattern: Vec<_> = pattern.split('/').collect();
    let path: Vec<_> = path.split('/').collect();
    if pattern.len() != path.len() {
        return None;
    }
    let mut params = BTreeMap::new();
    for (expected, actual) in pattern.into_iter().zip(path) {
        if let Some(name) = route_parameter(expected) {
            if actual.is_empty() {
                return None;
            }
            params.insert(name.to_string(), actual.to_string());
        } else if expected != actual {
            return None;
        }
    }
    Some(params)
}

#[cfg(test)]
mod tests {
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
        let error = PluginManager::validate_capability_registration(&registry, &conflicting, None)
            .unwrap_err();
        assert!(error.to_string().contains("Route conflict"));
        conflicting.id = "source-a".into();
        conflicting.version = "2.0.1".into();
        let error = PluginManager::validate_capability_registration(&registry, &conflicting, None)
            .unwrap_err();
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
}
