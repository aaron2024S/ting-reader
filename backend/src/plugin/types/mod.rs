//! Plugin type definitions
//!
//! Defines core plugin interfaces and data structures for the plugin system.

pub mod metadata;
pub(crate) mod schema;
pub mod scraper;
pub mod stats;

use crate::core::app::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub use stats::{
    AlertType, PerformanceAlert, PerformanceComparison, PerformanceThresholds, PluginStats,
};

/// Unique identifier for a plugin instance
pub type PluginId = String;

/// Host-owned identity for one call. Never deserialized from plugin input.
#[derive(Debug, Clone, Default)]
pub struct PluginInvocationContext {
    pub user: Option<crate::plugin::PluginHostUser>,
    pub resources: Option<Arc<crate::plugin::host_api::resources::ResourceScope>>,
}

/// Base plugin trait that all plugins must implement
#[async_trait::async_trait]
pub trait Plugin: Send + Sync {
    fn metadata(&self) -> &PluginMetadata;
    async fn initialize(&self, context: &PluginContext) -> Result<()>;
    async fn shutdown(&self) -> Result<()>;
    /// Runtime adapter entry point. Capability validation happens in Manager.
    async fn invoke(
        &self,
        operation: &str,
        _input: serde_json::Value,
        _context: &PluginInvocationContext,
    ) -> Result<serde_json::Value> {
        Err(crate::core::app::error::TingError::PluginExecutionError(
            format!("Plugin instance is unavailable for operation {operation}"),
        ))
    }
    async fn garbage_collect(&self) -> Result<()> {
        Ok(())
    }
}

/// Plugin metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMetadata {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    #[serde(default)]
    pub description_i18n: LocalizedText,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    pub entry_point: String,
    /// Runtime type: "wasm", "javascript", or "native" (auto-detected if absent)
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<PluginDependency>,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub min_core_version: Option<String>,
    #[serde(default)]
    pub min_flutter_version: Option<String>,
    /// Whether this plugin should only be visible and callable by admin users.
    #[serde(default)]
    pub admin_only: bool,
    #[serde(default)]
    pub supported_extensions: Option<Vec<String>>,
    #[serde(default)]
    pub scraper: Option<ScraperCapabilities>,
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
}

pub type LocalizedText = BTreeMap<String, String>;

pub use ting_plugin_contract::capability::Capability as PluginCapability;
pub use ting_plugin_contract::manifest::Permission;

/// Scraper-specific capability declaration from plugin.yml.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScraperCapabilities {
    #[serde(default)]
    pub auto_scrape: bool,
    #[serde(default)]
    pub search_fields: Vec<ScraperSearchField>,
    #[serde(default)]
    pub result_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub result_field_labels: BTreeMap<String, LocalizedText>,
}

/// Search field shown in the manual scraping UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScraperSearchField {
    pub key: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_i18n: Option<LocalizedText>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, rename = "type")]
    pub field_type: Option<String>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder_i18n: Option<LocalizedText>,
    #[serde(default)]
    pub default_from: Option<String>,
}

/// Plan for decrypting a file stream
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecryptionPlan {
    pub segments: Vec<DecryptionSegment>,
    pub total_size: Option<u64>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub extension: Option<String>,
}

/// A segment of the decryption plan
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DecryptionSegment {
    #[serde(rename = "plain")]
    Plain { offset: u64, length: i64 },
    #[serde(rename = "encrypted")]
    Encrypted {
        offset: u64,
        length: i64,
        params: serde_json::Value,
    },
}

impl PluginMetadata {
    pub fn new(
        id: String,
        name: String,
        version: String,
        author: String,
        description: String,
        entry_point: String,
    ) -> Self {
        Self {
            id,
            name,
            version,
            author,
            description,
            description_i18n: BTreeMap::new(),
            license: None,
            repo: None,
            entry_point,
            runtime: None,
            dependencies: Vec::new(),
            permissions: Vec::new(),
            config_schema: None,
            min_core_version: None,
            min_flutter_version: None,
            admin_only: false,
            supported_extensions: None,
            scraper: None,
            capabilities: Vec::new(),
        }
    }

    pub fn with_runtime(mut self, runtime: String) -> Self {
        self.runtime = Some(runtime);
        self
    }

    pub fn with_dependency(mut self, dependency: PluginDependency) -> Self {
        self.dependencies.push(dependency);
        self
    }

    pub fn with_permission(mut self, permission: Permission) -> Self {
        self.permissions.push(permission);
        self
    }

    pub fn with_config_schema(mut self, schema: serde_json::Value) -> Self {
        self.config_schema = Some(schema);
        self
    }

    pub fn with_supported_extensions(mut self, extensions: Vec<String>) -> Self {
        self.supported_extensions = Some(extensions);
        self
    }

    pub fn effective_capabilities(&self) -> Vec<PluginCapability> {
        self.capabilities.clone()
    }

    pub fn instance_id(&self) -> PluginId {
        format!("{}@{}", self.id, self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_capabilities_uses_declared_capabilities_only() {
        let metadata = PluginMetadata::new(
            "assistant".to_string(),
            "Assistant".to_string(),
            "1.0.0".to_string(),
            "Ting Reader".to_string(),
            "Assistant".to_string(),
            "plugin.js".to_string(),
        );

        assert!(metadata.effective_capabilities().is_empty());

        let mut metadata = metadata;
        metadata.capabilities.push(
            serde_json::from_value(serde_json::json!({
                "id":"assistant.ui", "kind":"ui_extension", "slots":["global.panel"],
                "contexts":["global"], "title":{"en":"Assistant"},
                "render":{"mode":"action","bridge":{"capabilities":[],"host_methods":[]}}
            }))
            .unwrap(),
        );

        let capabilities = metadata.effective_capabilities();

        assert_eq!(capabilities.len(), 1);
        assert_eq!(capabilities[0].id(), "assistant.ui");
        assert_eq!(capabilities[0].kind(), "ui_extension");
        assert!(capabilities[0].supports("open"));
    }
}

/// Plugin dependency specification
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDependency {
    #[serde(rename = "plugin_id")]
    pub plugin_name: String,
    pub version_requirement: String,
}

impl PluginDependency {
    pub fn new(plugin_name: String, version_requirement: String) -> Self {
        Self {
            plugin_name,
            version_requirement,
        }
    }
}

/// Plugin runtime context
#[derive(Clone)]
pub struct PluginContext {
    pub config: serde_json::Value,
    pub data_dir: PathBuf,
    pub resources: Option<Arc<crate::plugin::host_api::resources::ResourceScope>>,
    pub logger: Arc<dyn PluginLogger>,
    pub event_bus: Arc<dyn PluginEventBus>,
}

impl PluginContext {
    pub fn new(
        config: serde_json::Value,
        data_dir: PathBuf,
        logger: Arc<dyn PluginLogger>,
        event_bus: Arc<dyn PluginEventBus>,
    ) -> Self {
        Self {
            config,
            data_dir,
            resources: None,
            logger,
            event_bus,
        }
    }
}

/// Host-bound identity attached to plugin-originated log events.
///
/// Plugins receive a logger that already owns this context, so they cannot
/// choose another plugin identity, version, runtime, or source when writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLogContext {
    pub plugin_id: String,
    pub plugin_instance_id: String,
    pub plugin_name: String,
    pub plugin_version: String,
    pub runtime: String,
    pub source: PluginLogSource,
}

impl PluginLogContext {
    pub fn from_metadata(metadata: &PluginMetadata, source: PluginLogSource) -> Self {
        Self {
            plugin_id: metadata.id.clone(),
            plugin_instance_id: metadata.instance_id(),
            plugin_name: metadata.name.clone(),
            plugin_version: metadata.version.clone(),
            runtime: metadata
                .runtime
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            source,
        }
    }
}

/// Stable origin labels used by structured plugin log events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginLogSource {
    Code,
    Lifecycle,
    Runtime,
    Gateway,
    Security,
}

impl PluginLogSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Lifecycle => "lifecycle",
            Self::Runtime => "runtime",
            Self::Gateway => "gateway",
            Self::Security => "security",
        }
    }
}

/// Plugin logger trait
pub trait PluginLogger: Send + Sync {
    fn debug(&self, message: &str);
    fn info(&self, message: &str);
    fn warn(&self, message: &str);
    fn error(&self, message: &str);
}

/// Plugin event bus trait
pub trait PluginEventBus: Send + Sync {
    fn publish(&self, event_type: &str, data: serde_json::Value) -> Result<()>;
    fn subscribe(
        &self,
        event_type: &str,
        handler: Box<dyn Fn(serde_json::Value) + Send + Sync>,
    ) -> Result<String>;
    fn unsubscribe(&self, subscription_id: &str) -> Result<()>;
}

/// Plugin state enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginState {
    Discovered,
    Loading,
    Loaded,
    Initializing,
    Active,
    Executing,
    Unloading,
    Unloaded,
    Failed,
}

/// Event triggered when a plugin's state changes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStateEvent {
    pub plugin_id: PluginId,
    pub plugin_name: String,
    pub old_state: Option<PluginState>,
    pub new_state: PluginState,
    pub timestamp: i64,
}
