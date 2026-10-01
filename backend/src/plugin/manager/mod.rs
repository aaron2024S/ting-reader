//! Plugin manager - core orchestration for the plugin system

pub mod capabilities;
pub mod discovery;
pub mod dispatch;
pub mod event_bus;
pub mod formats;
pub mod lifecycle;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};
use tracing::info;

use crate::core::app::error::{Result, TingError};
use crate::plugin::config::PluginConfigManager;
use crate::plugin::host_api::{PluginHostGateway, PluginHostGatewayHandle};
use crate::plugin::types::{
    LocalizedText, Permission, Plugin, PluginCapability, PluginContext, PluginDependency, PluginId,
    PluginMetadata, PluginState, PluginStats, ScraperCapabilities,
};
use crate::plugin::wasm::WasmRuntime;

/// Configuration for the plugin manager
#[derive(Debug, Clone)]
pub struct PluginConfig {
    pub plugin_dir: PathBuf,
    pub enable_hot_reload: bool,
    pub max_memory_per_plugin: usize,
    pub max_execution_time: std::time::Duration,
}

/// Helper struct for plugin registry entries
pub(crate) struct PluginEntry {
    pub(crate) metadata: PluginMetadata,
    pub(crate) instance: Arc<dyn Plugin>,
    pub(crate) generation: uuid::Uuid,
    resource_scopes:
        std::sync::Mutex<Vec<std::sync::Weak<crate::plugin::host_api::resources::ResourceScope>>>,
    pub(crate) state: PluginState,
    pub(crate) load_error: Option<String>,
    pub(crate) stats: PluginStats,
    pub(crate) _active_tasks: Arc<std::sync::atomic::AtomicUsize>,
}

impl PluginEntry {
    pub(crate) fn new(metadata: PluginMetadata, instance: Arc<dyn Plugin>) -> Self {
        Self {
            metadata,
            instance,
            generation: uuid::Uuid::new_v4(),
            resource_scopes: std::sync::Mutex::new(Vec::new()),
            state: PluginState::Loaded,
            load_error: None,
            stats: PluginStats::new(),
            _active_tasks: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub(crate) fn set_state(&mut self, state: PluginState) {
        self.state = state;
    }

    pub(crate) fn track_resource_scope(
        &self,
        scope: &Arc<crate::plugin::host_api::resources::ResourceScope>,
    ) -> Result<()> {
        let mut scopes = self.resource_scopes.lock().map_err(|_| {
            TingError::PluginExecutionError("Resource scope registry unavailable".into())
        })?;
        scopes.retain(|scope| scope.strong_count() != 0);
        if scopes.len() >= 128 {
            return Err(TingError::PluginExecutionError(
                "Plugin scope limit reached".into(),
            ));
        }
        scopes.push(Arc::downgrade(scope));
        Ok(())
    }

    pub(crate) fn cancel_resource_scopes(&self) {
        if let Ok(mut scopes) = self.resource_scopes.lock() {
            for scope in scopes.drain(..).filter_map(|scope| scope.upgrade()) {
                scope.cancel();
            }
        }
    }
}

impl Drop for PluginEntry {
    fn drop(&mut self) {
        self.cancel_resource_scopes();
    }
}

pub(crate) type PluginRegistry = HashMap<PluginId, PluginEntry>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    #[serde(default)]
    pub description_i18n: LocalizedText,
    pub state: PluginState,
    pub total_calls: u64,
    pub successful_calls: u64,
    pub failed_calls: u64,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub supported_extensions: Option<Vec<String>>,
    #[serde(default)]
    pub dependencies: Vec<PluginDependency>,
    pub error: Option<String>,
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub min_core_version: Option<String>,
    #[serde(default)]
    pub min_flutter_version: Option<String>,
    #[serde(default)]
    pub admin_only: bool,
    #[serde(default)]
    pub scraper: Option<ScraperCapabilities>,
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
}

impl PluginInfo {
    pub fn from_metadata(metadata: &PluginMetadata, state: &PluginState) -> Self {
        Self {
            id: metadata.instance_id(),
            name: metadata.name.clone(),
            version: metadata.version.clone(),
            author: metadata.author.clone(),
            description: metadata.description.clone(),
            description_i18n: metadata.description_i18n.clone(),
            state: *state,
            total_calls: 0,
            successful_calls: 0,
            failed_calls: 0,
            runtime: metadata.runtime.clone(),
            supported_extensions: metadata.supported_extensions.clone(),
            dependencies: metadata.dependencies.clone(),
            error: None,
            config_schema: metadata.config_schema.clone(),
            permissions: metadata.permissions.clone(),
            license: metadata.license.clone(),
            repo: metadata.repo.clone(),
            min_core_version: metadata.min_core_version.clone(),
            min_flutter_version: metadata.min_flutter_version.clone(),
            admin_only: metadata.admin_only,
            scraper: metadata.scraper.clone(),
            capabilities: metadata.effective_capabilities(),
        }
    }

    pub fn from_metadata_with_error(
        metadata: &PluginMetadata,
        state: &PluginState,
        error: String,
    ) -> Self {
        let mut info = Self::from_metadata(metadata, state);
        info.error = Some(error);
        info
    }

    pub(crate) fn from_entry(entry: &PluginEntry) -> Self {
        let mut info = if entry.state == PluginState::Failed {
            Self::from_metadata_with_error(
                &entry.metadata,
                &entry.state,
                entry.load_error.clone().unwrap_or_default(),
            )
        } else {
            Self::from_metadata(&entry.metadata, &entry.state)
        };

        info.total_calls = entry.stats.total_calls;
        info.successful_calls = entry.stats.successful_calls;
        info.failed_calls = entry.stats.failed_calls;
        info
    }
}

/// A placeholder plugin implementation for failed plugins
pub(crate) struct FailedPlugin {
    metadata: PluginMetadata,
    error: String,
}

impl FailedPlugin {
    pub(crate) fn new(metadata: PluginMetadata, error: String) -> Self {
        Self { metadata, error }
    }
}

#[async_trait::async_trait]
impl Plugin for FailedPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }
    async fn initialize(&self, _context: &PluginContext) -> Result<()> {
        Err(TingError::PluginLoadError(self.error.clone()))
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

type PluginStateSubscriber = Box<dyn Fn(crate::plugin::types::PluginStateEvent) + Send + Sync>;
type PluginStateSubscribers = Arc<RwLock<Vec<PluginStateSubscriber>>>;

/// Manager for the plugin system
pub struct PluginManager {
    pub(crate) config: PluginConfig,
    pub(crate) registry: Arc<RwLock<PluginRegistry>>,
    pub(crate) metadata_cache: Arc<RwLock<HashMap<PluginId, PathBuf>>>,
    pub(crate) wasm_runtime: Arc<WasmRuntime>,
    pub(crate) http_client: reqwest::Client,
    pub(crate) _event_subscribers: PluginStateSubscribers,
    pub(crate) load_semaphore: Arc<Semaphore>,
    pub(crate) store_cache: Arc<crate::plugin::store::PluginCache>,
    pub(crate) config_manager: std::sync::RwLock<Option<Arc<PluginConfigManager>>>,
    pub(crate) host_gateway_handle: PluginHostGatewayHandle,
    pub(crate) event_bus: Arc<crate::plugin::manager::event_bus::DefaultPluginEventBus>,
}

impl PluginManager {
    /// Create a new plugin manager
    pub fn new(config: PluginConfig) -> Result<Self> {
        let wasm_runtime = Arc::new(WasmRuntime::with_limits(
            config.max_memory_per_plugin,
            config.max_execution_time,
        )?);
        let http_client = reqwest::Client::builder()
            .user_agent("TingReader/1.0")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .no_proxy()
            .build()
            .map_err(|e| TingError::NetworkError(e.to_string()))?;

        Ok(Self {
            config,
            registry: Arc::new(RwLock::new(HashMap::new())),
            metadata_cache: Arc::new(RwLock::new(HashMap::new())),
            wasm_runtime,
            http_client,
            _event_subscribers: Arc::new(RwLock::new(Vec::new())),
            load_semaphore: Arc::new(Semaphore::new(2)),
            store_cache: Arc::new(crate::plugin::store::PluginCache::new()),
            config_manager: std::sync::RwLock::new(None),
            host_gateway_handle: PluginHostGatewayHandle::default(),
            event_bus: Arc::new(crate::plugin::manager::event_bus::DefaultPluginEventBus::new()),
        })
    }

    /// Set the plugin config manager (called after construction since it's created later)
    pub fn set_config_manager(&self, cm: Arc<PluginConfigManager>) {
        let mut lock = self.config_manager.write().unwrap();
        *lock = Some(cm);
    }

    pub fn set_host_gateway(&self, gateway: &Arc<PluginHostGateway>) {
        self.host_gateway_handle.set(gateway);
    }

    pub(crate) fn host_gateway_handle(&self) -> PluginHostGatewayHandle {
        self.host_gateway_handle.clone()
    }

    /// Trigger garbage collection on all plugins
    pub async fn garbage_collect_all(&self) {
        info!("Triggering garbage collection for all plugins");

        let registry = self.registry.read().await;
        for entry in registry.values() {
            if let Err(e) = entry.instance.garbage_collect().await {
                tracing::warn!(
                    plugin = %entry.metadata.name,
                    error = %e,
                    message_key = "plugin.gc_failed",
                    message_params = %serde_json::json!({
                        "plugin": entry.metadata.name,
                        "error": e.to_string(),
                    }),
                    "Plugin garbage collection failed"
                );
            }
        }

        tokio::task::spawn_blocking(|| {
            crate::core::common::utils::release_memory();
        })
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                message_key = "system.memory.release_failed",
                message_params = %serde_json::json!({ "error": e.to_string() }),
                "Memory release failed"
            )
        });
    }

    /// List all installed plugins
    pub async fn list_plugins(&self) -> Vec<PluginInfo> {
        let registry = self.registry.read().await;
        registry.values().map(PluginInfo::from_entry).collect()
    }

    /// Get the list of plugins from the store
    pub async fn get_store_plugins(&self) -> Result<Vec<crate::plugin::store::StorePlugin>> {
        self.load_store_plugins(false).await
    }

    /// Refresh the plugin store list by bypassing the backend cache.
    pub async fn refresh_store_plugins(&self) -> Result<Vec<crate::plugin::store::StorePlugin>> {
        self.store_cache.clear().await;
        self.load_store_plugins(true).await
    }

    /// Clear the plugin store cache used by get_store_plugins.
    pub async fn clear_store_cache(&self) {
        self.store_cache.clear().await;
    }

    async fn load_store_plugins(
        &self,
        force_refresh: bool,
    ) -> Result<Vec<crate::plugin::store::StorePlugin>> {
        let mut providers = self.find_capabilities_by_kind("plugin_store").await;
        providers.sort_by(|left, right| {
            left.plugin_id
                .cmp(&right.plugin_id)
                .then_with(|| left.capability.id().cmp(right.capability.id()))
        });

        let Some(provider) = providers.into_iter().next() else {
            return Ok(Vec::new());
        };

        let cache_key = format!("{}:{}", provider.plugin_id, provider.capability.id());
        if !force_refresh && let Some(cached) = self.store_cache.get(&cache_key).await {
            return Ok(cached);
        }

        let response = self
            .invoke_capability(
                &provider.plugin_id,
                provider.capability.id(),
                "list_plugins",
                serde_json::json!({ "force_refresh": force_refresh }),
                &crate::plugin::types::PluginInvocationContext::default(),
            )
            .await?;
        let plugins = crate::plugin::store::parse_store_plugins_response(response)?;
        self.store_cache.set(cache_key, plugins.clone()).await;
        Ok(plugins)
    }

    pub fn get_plugin(&self, id: &PluginId) -> Result<PluginMetadata> {
        let registry = futures::executor::block_on(self.registry.read());
        let entry = registry
            .get(id)
            .ok_or_else(|| TingError::PluginNotFound(id.clone()))?;
        Ok(entry.metadata.clone())
    }

    pub async fn get_plugin_package_path(&self, id: &PluginId) -> Result<PathBuf> {
        let cache = self.metadata_cache.read().await;
        cache
            .get(id)
            .cloned()
            .ok_or_else(|| TingError::PluginNotFound(id.clone()))
    }

    pub async fn find_plugins_by_capability_kind(&self, kind: &str) -> Vec<PluginInfo> {
        let registry = self.registry.read().await;
        registry
            .values()
            .filter(|e| {
                e.metadata
                    .effective_capabilities()
                    .iter()
                    .any(|capability| capability.kind() == kind)
            })
            .map(|entry| {
                if entry.state == PluginState::Failed {
                    PluginInfo::from_metadata_with_error(
                        &entry.metadata,
                        &entry.state,
                        entry.load_error.clone().unwrap_or_default(),
                    )
                } else {
                    PluginInfo::from_metadata(&entry.metadata, &entry.state)
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn test_plugin_manager_send_sync() {
        assert_send_sync::<PluginManager>();
    }
}
