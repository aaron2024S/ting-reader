use crate::core::app::error::{
    ConfigurationErrorKind, DeadlineErrorKind, LookupErrorKind, PluginErrorKind,
};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

use super::{FailedPlugin, PluginEntry, PluginManager};
use crate::core::app::error::{Result, TingError};
use crate::plugin::host_api::logger::{PluginLogLevel, emit_plugin_event};
use crate::plugin::js::{JavaScriptPluginLoader, JavaScriptPluginWrapper};
use crate::plugin::native::{NativeLoader, NativePlugin};
use crate::plugin::types::{
    Plugin, PluginContext, PluginId, PluginLogSource, PluginMetadata, PluginState,
};

impl PluginManager {
    /// Load a plugin from a directory
    pub async fn load_plugin(&self, plugin_path: &Path) -> Result<PluginId> {
        let _permit = self.load_semaphore.acquire().await.map_err(|e| {
            let message = format!("Failed to acquire load permit: {}", e);
            TingError::plugin_load(
                PluginErrorKind::Loading,
                "load_plugin::plugin_load",
                message,
            )
            .with_source(anyhow::anyhow!(e))
        })?;

        info!("Acquired plugin load permit for: {}", plugin_path.display());

        let metadata = self.read_plugin_metadata(plugin_path)?;
        let plugin_id = metadata.instance_id();

        info!(
            "Loading plugin: {} from {}",
            plugin_id,
            plugin_path.display()
        );

        {
            let registry = self.registry.read().await;
            if registry.contains_key(&plugin_id) {
                info!("Plugin {} already loaded, skipping", plugin_id);
                return Ok(plugin_id);
            }
        }

        let preflight_result = match Self::validate_core_compatibility(&metadata) {
            Ok(()) => {
                let registry = self.registry.read().await;
                let result = Self::validate_capability_registration(&registry, &metadata, None);
                drop(registry);
                match result {
                    Ok(()) => self.validate_plugin_dependencies(&metadata, None).await,
                    Err(error) => Err(error),
                }
            }
            Err(e) => Err(e),
        };

        let (instance, state, error) = match preflight_result {
            Err(e) => {
                error!("Plugin preflight failed {}: {}", plugin_id, e);
                let error = Arc::new(e);
                (
                    Arc::new(FailedPlugin::new(metadata.clone(), error.clone())) as Arc<dyn Plugin>,
                    PluginState::Failed,
                    Some(error),
                )
            }
            Ok(()) => match tokio::time::timeout(
                Duration::from_secs(30),
                self.load_plugin_instance(plugin_path, &metadata),
            )
            .await
            {
                Ok(Ok(inst)) => (inst, PluginState::Loaded, None),
                Ok(Err(e)) => {
                    error!("Failed to load plugin {}: {}", plugin_id, e);
                    let error = Arc::new(e);
                    (
                        Arc::new(FailedPlugin::new(metadata.clone(), error.clone()))
                            as Arc<dyn Plugin>,
                        PluginState::Failed,
                        Some(error),
                    )
                }
                Err(_) => {
                    let timeout_err = Arc::new(TingError::timeout(
                        DeadlineErrorKind::Elapsed,
                        "plugin.load",
                        "Plugin load timeout after 30s".into(),
                    ));
                    error!("{} for plugin {}", timeout_err, plugin_id);
                    (
                        Arc::new(FailedPlugin::new(metadata.clone(), timeout_err.clone()))
                            as Arc<dyn Plugin>,
                        PluginState::Failed,
                        Some(timeout_err),
                    )
                }
            },
        };

        // Register plugin
        {
            let mut registry = self.registry.write().await;
            let mut entry = PluginEntry::new(metadata.clone(), instance);
            entry.state = state;
            entry.load_error = error;
            if entry.state != PluginState::Failed
                && let Err(error) =
                    Self::validate_capability_registration(&registry, &metadata, None)
            {
                entry.state = PluginState::Failed;
                entry.load_error = Some(Arc::new(error));
            }
            let registered_state = entry.state;
            registry.insert(plugin_id.clone(), entry);
            drop(registry);

            if registered_state == PluginState::Failed && state != PluginState::Failed {
                return Err(TingError::plugin_load(
                    PluginErrorKind::AbiMismatch,
                    "load_plugin::plugin_load",
                    "Capability registration changed during plugin loading".into(),
                ));
            }
        }

        // Update cache
        {
            let mut cache = self.metadata_cache.write().await;
            cache.insert(plugin_id.clone(), plugin_path.to_path_buf());
        }

        if state != PluginState::Failed {
            if let Err(e) = self.initialize_plugin(&plugin_id).await {
                error!("Failed to initialize plugin {}: {}", plugin_id, e);
                let mut registry = self.registry.write().await;
                if let Some(entry) = registry.get_mut(&plugin_id) {
                    entry.state = PluginState::Failed;
                    entry.load_error = Some(Arc::new(e));
                }
            }
        } else {
            let fields = serde_json::json!({
                "op": "plugin.lifecycle.load",
                "state": "failed",
            });
            emit_plugin_event(
                &metadata,
                PluginLogSource::Lifecycle,
                PluginLogLevel::Error,
                "Plugin failed to load",
                Some(&fields),
            );
        }

        info!("Plugin load completed for: {} (permit released)", plugin_id);
        Ok(plugin_id)
    }

    pub(crate) async fn load_plugin_instance(
        &self,
        plugin_path: &Path,
        metadata: &PluginMetadata,
    ) -> Result<Arc<dyn Plugin>> {
        if metadata.entry_point.ends_with(".js") {
            let loader = JavaScriptPluginLoader::new(plugin_path.to_path_buf()).map_err(|e| {
                let message = format!("Failed to create JS loader: {}", e);
                TingError::plugin_load(
                    PluginErrorKind::Loading,
                    "load_plugin_instance::plugin_load",
                    message,
                )
                .with_source(anyhow::anyhow!(e))
            })?;
            let wrapper = JavaScriptPluginWrapper::new_with_limits(
                loader,
                Some(self.host_gateway_handle()),
                crate::plugin::sandbox::ResourceLimits {
                    max_memory_bytes: self.config.max_memory_per_plugin,
                    max_cpu_time: self.config.max_execution_time,
                    ..Default::default()
                },
            )?;
            Ok(Arc::new(wrapper))
        } else if metadata.entry_point.ends_with(".wasm") {
            let wasm_path = plugin_path.join(&metadata.entry_point);
            let module = self.wasm_runtime.load_module_from_file(&wasm_path).await?;
            let instance = self
                .wasm_runtime
                .instantiate_with_host_gateway(module, metadata, Some(self.host_gateway_handle()))
                .await?;
            Ok(Arc::new(instance))
        } else if metadata.entry_point.ends_with(".dll")
            || metadata.entry_point.ends_with(".so")
            || metadata.entry_point.ends_with(".dylib")
        {
            let lib_path = plugin_path.join(&metadata.entry_point);
            let loader = Arc::new(NativeLoader::new());
            loader.load_library(metadata.instance_id(), &lib_path, metadata.clone())?;
            let plugin = NativePlugin::new(
                metadata.instance_id(),
                metadata.clone(),
                loader,
                plugin_path.to_path_buf(),
                Some(self.host_gateway_handle()),
            );
            Ok(Arc::new(plugin))
        } else {
            Err(TingError::plugin_load(
                PluginErrorKind::InvalidManifest,
                "load_plugin_instance::plugin_load",
                format!("Unsupported entry point: {}", metadata.entry_point),
            ))
        }
    }

    /// Unload a plugin
    pub async fn unload_plugin(&self, plugin_id: &PluginId) -> Result<()> {
        info!("Unloading plugin: {}", plugin_id);
        if let Some(entry) = self.registry.read().await.get(plugin_id) {
            entry.cancel_resource_scopes();
        }
        self.shutdown_plugin(plugin_id).await?;

        let mut registry = self.registry.write().await;
        if registry.remove(plugin_id).is_some() {
            info!("Plugin unloaded: {}", plugin_id);
            Ok(())
        } else {
            Err(TingError::plugin_not_found(
                LookupErrorKind::Plugin,
                "unload_plugin::plugin_not_found",
                plugin_id.clone(),
            ))
        }
    }

    pub async fn reload_plugin(&self, id: &PluginId) -> Result<()> {
        tracing::info!(
            plugin_id = %id,
            message_key = "plugin.reload.started",
            message_params = %serde_json::json!({ "plugin_id": id }),
            "Reloading plugin"
        );

        let (plugin_path, _old_metadata) = {
            let cache = self.metadata_cache.read().await;
            let path = cache.get(id).cloned().ok_or_else(|| {
                TingError::plugin_not_found(
                    LookupErrorKind::Plugin,
                    "reload_plugin::plugin_not_found",
                    format!("Plugin {} not found in cache", id),
                )
            })?;

            let registry = self.registry.read().await;
            let metadata = registry
                .get(id)
                .map(|e| e.metadata.clone())
                .ok_or_else(|| {
                    TingError::plugin_not_found(
                        LookupErrorKind::Plugin,
                        "reload_plugin::plugin_not_found",
                        id.clone(),
                    )
                })?;

            (path, metadata)
        };

        let new_metadata = self.read_plugin_metadata(&plugin_path)?;
        Self::validate_core_compatibility(&new_metadata)?;
        {
            let registry = self.registry.read().await;
            Self::validate_capability_registration(&registry, &new_metadata, Some(id))?;
        }
        self.validate_plugin_dependencies(&new_metadata, Some(id))
            .await?;
        let new_id = new_metadata.instance_id();

        if new_id == *id {
            tracing::info!(
                plugin_id = %id,
                message_key = "plugin.reload.same_version",
                message_params = %serde_json::json!({ "plugin_id": id }),
                "Reloading same plugin version"
            );

            match self.load_plugin_instance(&plugin_path, &new_metadata).await {
                Ok(instance) => {
                    if let Err(e) = self.unload_plugin(id).await {
                        tracing::error!(
                            plugin_id = %id,
                            error = %e,
                            message_key = "plugin.unload_old_failed",
                            message_params = %serde_json::json!({
                                "plugin_id": id,
                                "error": e.to_string(),
                            }),
                            "Failed to unload old plugin version"
                        );
                        return Err(e);
                    }

                    {
                        let mut registry = self.registry.write().await;
                        Self::validate_capability_registration(&registry, &new_metadata, None)?;
                        registry.insert(
                            new_metadata.instance_id(),
                            PluginEntry::new(new_metadata.clone(), instance),
                        );
                    }

                    self.initialize_plugin(&new_id).await?;

                    {
                        let mut cache = self.metadata_cache.write().await;
                        cache.insert(new_id.clone(), plugin_path);
                    }

                    tracing::info!(
                        plugin_id = %new_id,
                        message_key = "plugin.reload.completed",
                        message_params = %serde_json::json!({ "plugin_id": new_id }),
                        "Plugin reloaded"
                    );
                    Ok(())
                }
                Err(e) => {
                    tracing::error!(
                        plugin_id = %id,
                        error = %e,
                        message_key = "plugin.reload_new_instance_failed",
                        message_params = %serde_json::json!({
                            "plugin_id": id,
                            "error": e.to_string(),
                        }),
                        "Failed to load new plugin instance; aborting reload"
                    );
                    Err(e)
                }
            }
        } else {
            tracing::info!(
                old_id = %id,
                new_id = %new_id,
                message_key = "plugin.reload.version_changed",
                message_params = %serde_json::json!({
                    "old_id": id,
                    "new_id": new_id,
                }),
                "Reloading plugin with version change"
            );

            // Preserve old config before loading new version.
            let preserved_old_config: Option<serde_json::Value> = {
                let cm_lock = self.config_manager.read().unwrap();
                cm_lock.as_ref().and_then(|cm| cm.get_config(id).ok())
            };

            // Apply the same schema-preserving migration for install and reload upgrades.
            if let Some(ref old_config) = preserved_old_config
                && let Some(ref schema) = new_metadata.config_schema
            {
                let manager = self.config_manager.read().map_err(|error| {
                    TingError::configuration(
                        ConfigurationErrorKind::Synchronization,
                        "reload_plugin",
                        format!("Failed to acquire config manager: {error}"),
                    )
                })?;
                if let Some(manager) = manager.as_ref() {
                    manager.migrate_config(
                        new_id.clone(),
                        new_metadata.name.clone(),
                        schema,
                        old_config,
                    )?;
                }
            }

            match self.load_plugin(&plugin_path).await {
                Ok(loaded_id) => {
                    if let Err(e) = self.unload_plugin(id).await {
                        tracing::warn!(
                            plugin_id = %id,
                            error = %e,
                            message_key = "plugin.unload_after_upgrade_failed",
                            message_params = %serde_json::json!({
                                "plugin_id": id,
                                "error": e.to_string(),
                            }),
                            "Failed to unload old plugin version after upgrade"
                        );
                    }
                    if let Some(gateway) = self.host_gateway_handle().get()
                        && let Err(e) = gateway.migrate_plugin_cache(id, &loaded_id).await
                    {
                        tracing::warn!(
                            old_plugin_id = %id,
                            new_plugin_id = %loaded_id,
                            error = %e,
                            message_key = "plugin.cache.migration_failed",
                            message_params = %serde_json::json!({
                                "old_plugin_id": id,
                                "new_plugin_id": &loaded_id,
                                "error": e.to_string(),
                            }),
                            "Failed to migrate plugin cache during reload upgrade"
                        );
                    }
                    tracing::info!(
                        old_id = %id,
                        new_id = %loaded_id,
                        message_key = "plugin.upgrade.completed",
                        message_params = %serde_json::json!({
                            "old_id": id,
                            "new_id": loaded_id,
                        }),
                        "Plugin upgraded"
                    );
                    Ok(())
                }
                Err(e) => {
                    tracing::error!(
                        plugin_id = %id,
                        error = %e,
                        message_key = "plugin.load_new_version_failed",
                        message_params = %serde_json::json!({
                            "plugin_id": id,
                            "error": e.to_string(),
                        }),
                        "Failed to load new plugin version"
                    );
                    Err(e)
                }
            }
        }
    }

    pub(crate) async fn initialize_plugin(&self, plugin_id: &PluginId) -> Result<()> {
        let (metadata, config_schema) = {
            let registry = self.registry.read().await;
            let entry = registry.get(plugin_id).ok_or_else(|| {
                TingError::plugin_not_found(
                    LookupErrorKind::Plugin,
                    "initialize_plugin::plugin_not_found",
                    plugin_id.clone(),
                )
            })?;
            (entry.metadata.clone(), entry.metadata.config_schema.clone())
        };

        // Initialize per-plugin config if plugin has a schema and no config exists yet
        if let Some(ref schema) = config_schema
            && let Some(cm) = self.config_manager.read().unwrap().as_ref()
        {
            let default_config = crate::plugin::config::schema_defaults(schema);
            let _ = cm.ensure_config(
                plugin_id.clone(),
                metadata.name.clone(),
                Some(schema.clone()),
                default_config,
            );
        }

        let mut context = self.create_plugin_context(&metadata)?;
        let initializing_fields = serde_json::json!({
            "op": "plugin.lifecycle.initialize",
            "state": "initializing",
        });
        emit_plugin_event(
            &metadata,
            PluginLogSource::Lifecycle,
            PluginLogLevel::Info,
            "Plugin initialization started",
            Some(&initializing_fields),
        );

        let (instance, scope) = {
            let mut registry = self.registry.write().await;
            let entry = registry.get_mut(plugin_id).ok_or_else(|| {
                TingError::plugin_not_found(
                    LookupErrorKind::Plugin,
                    "initialize_plugin::plugin_not_found",
                    plugin_id.clone(),
                )
            })?;
            entry.set_state(PluginState::Initializing);
            let scope = Arc::new(crate::plugin::host_api::resources::ResourceScope::new(
                plugin_id.clone(),
                entry.generation,
                None,
                self.config.plugin_dir.join("staging"),
                crate::plugin::host_api::resources::ResourceLimits::default(),
            ));
            entry.track_resource_scope(&scope)?;
            (entry.instance.clone(), scope)
        };
        context.resources = Some(scope);

        if let Err(error) = instance.initialize(&context).await {
            let failed_fields = serde_json::json!({
                "op": "plugin.lifecycle.initialize",
                "state": "failed",
            });
            emit_plugin_event(
                &metadata,
                PluginLogSource::Lifecycle,
                PluginLogLevel::Error,
                "Plugin initialization failed",
                Some(&failed_fields),
            );
            return Err(error.bind_plugin_error(&metadata.id, "lifecycle", "initialize"));
        }

        {
            let mut registry = self.registry.write().await;
            if let Some(entry) = registry.get_mut(plugin_id) {
                entry.set_state(PluginState::Active);
            }
        }
        let active_fields = serde_json::json!({
            "op": "plugin.lifecycle.initialize",
            "state": "active",
        });
        emit_plugin_event(
            &metadata,
            PluginLogSource::Lifecycle,
            PluginLogLevel::Info,
            "Plugin is active",
            Some(&active_fields),
        );

        Ok(())
    }

    pub(crate) async fn shutdown_plugin(&self, plugin_id: &PluginId) -> Result<()> {
        let (instance, metadata) = {
            let mut registry = self.registry.write().await;
            let entry = registry.get_mut(plugin_id).ok_or_else(|| {
                TingError::plugin_not_found(
                    LookupErrorKind::Plugin,
                    "shutdown_plugin::plugin_not_found",
                    plugin_id.clone(),
                )
            })?;
            entry.set_state(PluginState::Unloading);
            (entry.instance.clone(), entry.metadata.clone())
        };

        if let Err(error) = instance.shutdown().await {
            let fields = serde_json::json!({
                "op": "plugin.lifecycle.shutdown",
                "state": "failed",
            });
            emit_plugin_event(
                &metadata,
                PluginLogSource::Lifecycle,
                PluginLogLevel::Error,
                "Plugin shutdown failed",
                Some(&fields),
            );
            return Err(error.bind_plugin_error(&metadata.id, "lifecycle", "shutdown"));
        }
        let fields = serde_json::json!({
            "op": "plugin.lifecycle.shutdown",
            "state": "unloaded",
        });
        emit_plugin_event(
            &metadata,
            PluginLogSource::Lifecycle,
            PluginLogLevel::Info,
            "Plugin shutdown completed",
            Some(&fields),
        );
        Ok(())
    }

    pub(crate) fn create_plugin_context(&self, metadata: &PluginMetadata) -> Result<PluginContext> {
        let instance_id = metadata.instance_id();
        let config = if let Some(cm) = self.config_manager.read().unwrap().as_ref() {
            cm.get_config(&instance_id).unwrap_or_else(|_| {
                metadata
                    .config_schema
                    .as_ref()
                    .map(crate::plugin::config::schema_defaults)
                    .unwrap_or(serde_json::json!({}))
            })
        } else {
            metadata
                .config_schema
                .as_ref()
                .map(crate::plugin::config::schema_defaults)
                .unwrap_or(serde_json::json!({}))
        };

        Ok(PluginContext {
            config,
            data_dir: self.config.plugin_dir.join("data").join(&metadata.name),
            resources: None,
            logger: Arc::new(
                crate::plugin::host_api::logger::DefaultPluginLogger::from_metadata(metadata),
            ),
            event_bus: self.event_bus.clone(),
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/plugin/manager/lifecycle.rs"]
mod tests;
