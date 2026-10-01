//! Native plugin wrapper
//!
//! This module provides a wrapper that implements the Plugin trait for native dynamic libraries.
//! It bridges the NativeLoader functionality with the Plugin interface.

use super::super::types::{Plugin, PluginContext, PluginInvocationContext, PluginMetadata};
use super::host_api::NativeHostInvocationContext;
use super::loader::NativeLoader;
use crate::core::error::{Result, TingError};
use crate::plugin::PluginHostGatewayHandle;
use serde_json::Value;
use std::sync::{Arc, RwLock};

/// Native plugin wrapper that implements the Plugin trait
pub struct NativePlugin {
    /// Plugin metadata
    metadata: PluginMetadata,

    /// Plugin ID (name@version)
    plugin_id: String,

    /// Reference to the native loader
    native_loader: Arc<NativeLoader>,

    /// Initialization state
    initialized: RwLock<bool>,

    /// Plugin installation directory
    plugin_path: std::path::PathBuf,

    /// Optional HostGateway bridge configured by PluginManager
    host_gateway: Option<PluginHostGatewayHandle>,
}

impl NativePlugin {
    /// Create a new native plugin wrapper
    ///
    /// # Arguments
    /// * `plugin_id` - Unique plugin ID (name@version)
    /// * `metadata` - Plugin metadata
    /// * `native_loader` - Reference to the native loader that loaded this plugin
    /// * `plugin_path` - Path to the plugin installation directory
    pub fn new(
        plugin_id: String,
        metadata: PluginMetadata,
        native_loader: Arc<NativeLoader>,
        plugin_path: std::path::PathBuf,
        host_gateway: Option<PluginHostGatewayHandle>,
    ) -> Self {
        Self {
            metadata,
            plugin_id,
            native_loader,
            initialized: RwLock::new(false),
            plugin_path,
            host_gateway,
        }
    }

    /// Call a method on the native plugin
    ///
    /// # Arguments
    /// * `method` - Method name to invoke
    /// * `params` - JSON parameters for the method
    ///
    /// # Returns
    /// JSON result from the plugin
    pub async fn call_method(
        &self,
        method: &str,
        params: Value,
        context: &PluginInvocationContext,
    ) -> Result<Value> {
        let is_initialized = *self.initialized.read().map_err(|e| {
            TingError::PluginExecutionError(format!("Failed to check initialization state: {}", e))
        })?;

        if !is_initialized {
            return Err(TingError::PluginExecutionError(format!(
                "Plugin {} is not initialized",
                self.plugin_id
            )));
        }

        // Call the native function through the loader in a blocking task
        let loader = self.native_loader.clone();
        let plugin_id = self.plugin_id.clone();
        let method = method.to_string();
        let host_context = NativeHostInvocationContext {
            plugin_id: self.plugin_id.clone(),
            permissions: self.metadata.permissions.clone(),
            user: context.user.clone(),
            host_gateway: self.host_gateway.as_ref().and_then(|handle| handle.get()),
            resources: context.resources.clone(),
            runtime_handle: tokio::runtime::Handle::current(),
        };

        // Offload to blocking thread pool to avoid blocking the async runtime
        tokio::task::spawn_blocking(move || {
            loader.call_function_with_context(&plugin_id, &method, params, Some(host_context))
        })
        .await
        .map_err(|e| TingError::PluginExecutionError(format!("Task join error: {}", e)))?
    }
}

#[async_trait::async_trait]
impl Plugin for NativePlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn invoke(
        &self,
        operation: &str,
        input: Value,
        context: &PluginInvocationContext,
    ) -> Result<Value> {
        self.call_method(operation, input, context).await
    }

    async fn initialize(&self, context: &PluginContext) -> Result<()> {
        tracing::info!(
            plugin_id = %self.plugin_id,
            "Initializing native plugin"
        );

        // The new instance is created during library loading. Initialize its
        // effective settings before exposing it to callers.
        let init_params = serde_json::json!({
            "config": context.config,
            "data_dir": context.data_dir.to_string_lossy(),
            "plugin_path": self.plugin_path.to_string_lossy(),
        });

        let loader = self.native_loader.clone();
        let plugin_id = self.plugin_id.clone();
        let host_context = NativeHostInvocationContext {
            plugin_id: self.plugin_id.clone(),
            permissions: self.metadata.permissions.clone(),
            user: None,
            host_gateway: self.host_gateway.as_ref().and_then(|handle| handle.get()),
            resources: context.resources.clone(),
            runtime_handle: tokio::runtime::Handle::current(),
        };

        // Offload to blocking thread pool
        let result = tokio::task::spawn_blocking(move || {
            loader.call_function_with_context(
                &plugin_id,
                "initialize",
                init_params,
                Some(host_context),
            )
        })
        .await
        .map_err(|e| TingError::PluginExecutionError(format!("Task join error: {}", e)))?;

        crate::plugin::require_successful_lifecycle_result(result?, "initialize")?;

        // Mark as initialized
        let mut initialized = self.initialized.write().map_err(|e| {
            TingError::PluginExecutionError(format!("Failed to update initialization state: {}", e))
        })?;
        *initialized = true;

        tracing::info!(
            plugin_id = %self.plugin_id,
            "Native plugin initialized successfully"
        );

        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        tracing::info!(
            plugin_id = %self.plugin_id,
            "Shutting down native plugin"
        );

        let shutdown_params = serde_json::json!({});

        let loader = self.native_loader.clone();
        let plugin_id = self.plugin_id.clone();

        // Offload to blocking thread pool
        let result = tokio::task::spawn_blocking(move || {
            loader.call_function(&plugin_id, "shutdown", shutdown_params)
        })
        .await
        .map_err(|e| TingError::PluginExecutionError(format!("Task join error: {}", e)))?;

        crate::plugin::require_successful_lifecycle_result(result?, "shutdown")?;

        // Mark as not initialized
        let mut initialized = self.initialized.write().map_err(|e| {
            TingError::PluginExecutionError(format!("Failed to update initialization state: {}", e))
        })?;
        *initialized = false;

        tracing::info!(
            plugin_id = %self.plugin_id,
            "Native plugin shut down successfully"
        );

        Ok(())
    }

    async fn garbage_collect(&self) -> Result<()> {
        // ABI v2 does not declare a GC hook. Instance resources are reclaimed
        // at shutdown and the Host reclaims each control buffer after invoke.
        Ok(())
    }
}

// Ensure NativePlugin is thread-safe
unsafe impl Send for NativePlugin {}
unsafe impl Sync for NativePlugin {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_native_plugin_creation() {
        let metadata = PluginMetadata::new(
            "test-plugin@1.0.0".to_string(),
            "test-plugin".to_string(),
            "1.0.0".to_string(),
            "Test Author".to_string(),
            "Test plugin".to_string(),
            "plugin.dll".to_string(),
        );

        let loader = Arc::new(NativeLoader::new());
        let plugin = NativePlugin::new(
            "test-plugin@1.0.0".to_string(),
            metadata,
            loader,
            std::path::PathBuf::from("/tmp/test-plugin"),
            None,
        );

        assert_eq!(plugin.metadata().name, "test-plugin");
    }
}
