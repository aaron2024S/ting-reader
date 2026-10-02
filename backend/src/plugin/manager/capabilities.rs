use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ting_plugin_contract::capability::{ToolDeclaration, route_parameter};

use super::{PluginManager, PluginRegistry};
use crate::core::app::error::{Result, TingError};
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
#[path = "../../../tests/unit/plugin/manager/capabilities.rs"]
mod tests;
