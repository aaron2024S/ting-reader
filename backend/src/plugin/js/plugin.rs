//! JavaScript Plugin Loader
//!
//! This module provides the JavaScript plugin loading and lifecycle management.
//!
//! **Important Note on Thread Safety:**
//! JavaScript plugins using Deno Core cannot implement the Plugin trait directly
//! because Deno's JsRuntime is not Send + Sync (V8 isolates are single-threaded).
//!
//! Instead, this module provides:
//! 1. JavaScriptPluginLoader - for loading and managing JS plugin metadata
//! 2. JavaScriptPluginExecutor - for executing JS plugins in a single-threaded context
//!
//! The plugin manager should handle JS plugins specially, executing them on a
//! dedicated single-threaded runtime.

use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tracing::info;

use super::super::types::PluginMetadata;
use super::super::types::metadata::read_plugin_metadata;
use super::runtime::JsRuntimeWrapper;
use crate::core::app::error::TingError;
use crate::plugin::PluginHostGatewayHandle;

/// JavaScript plugin loader
///
/// This struct handles loading JavaScript plugin metadata and creating executors.
/// It does NOT implement the Plugin trait due to thread safety constraints.
#[derive(Debug, Clone)]
pub struct JavaScriptPluginLoader {
    /// Plugin metadata
    metadata: PluginMetadata,

    /// Plugin directory path
    plugin_dir: PathBuf,
}

impl JavaScriptPluginLoader {
    /// Create a new JavaScript plugin loader from a plugin directory
    ///
    /// # Arguments
    /// * `plugin_dir` - Path to the plugin directory containing plugin.yml/plugin.yaml and .js files
    ///
    /// # Returns
    /// A new JavaScriptPluginLoader instance
    ///
    /// # Errors
    /// Returns an error if:
    /// - plugin.yml/plugin.yaml cannot be read or parsed
    /// - The runtime field is not "javascript"
    /// - The entry point file doesn't exist
    pub fn new(plugin_dir: PathBuf) -> Result<Self> {
        info!("Loading JavaScript plugin from: {}", plugin_dir.display());

        // Read and parse plugin.yml/plugin.yaml using shared metadata reader
        let metadata = read_plugin_metadata(&plugin_dir)?;

        // Verify this is a JavaScript plugin
        Self::verify_runtime(&metadata, &plugin_dir)?;

        // Get the entry point file path
        let entry_point = plugin_dir.join(&metadata.entry_point);
        if !entry_point.exists() {
            return Err(TingError::PluginLoadError(format!(
                "Entry point file not found: {}",
                entry_point.display()
            ))
            .into());
        }

        Ok(Self {
            metadata,
            plugin_dir,
        })
    }

    /// Get the plugin metadata
    pub fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    /// Get the plugin directory
    pub fn plugin_dir(&self) -> &Path {
        &self.plugin_dir
    }

    /// Get the plugin type
    /// Create an executor for this plugin
    ///
    /// The executor must be used in a single-threaded context (e.g., LocalSet)
    pub fn create_executor(&self) -> Result<JavaScriptPluginExecutor> {
        JavaScriptPluginExecutor::new(self.plugin_dir.clone(), self.metadata.clone())
    }

    pub fn create_executor_with_host_gateway(
        &self,
        host_gateway: Option<PluginHostGatewayHandle>,
    ) -> Result<JavaScriptPluginExecutor> {
        JavaScriptPluginExecutor::new_with_host_gateway(
            self.plugin_dir.clone(),
            self.metadata.clone(),
            host_gateway,
        )
    }

    pub(super) fn create_executor_with_limits(
        &self,
        host_gateway: Option<PluginHostGatewayHandle>,
        limits: crate::plugin::sandbox::ResourceLimits,
    ) -> Result<JavaScriptPluginExecutor> {
        JavaScriptPluginExecutor::new_with_limits(
            self.plugin_dir.clone(),
            self.metadata.clone(),
            host_gateway,
            limits,
        )
    }

    /// Verify that the plugin metadata specifies JavaScript runtime
    fn verify_runtime(metadata: &PluginMetadata, _plugin_dir: &Path) -> Result<()> {
        if let Some(runtime) = metadata.runtime.as_deref() {
            if runtime != "javascript" {
                return Err(TingError::PluginLoadError(format!(
                    "Plugin runtime is '{}', expected 'javascript'",
                    runtime
                ))
                .into());
            }
        } else if !metadata.entry_point.ends_with(".js") {
            return Err(TingError::PluginLoadError(format!(
                "Plugin entry_point '{}' is not a .js file and no 'runtime' field specified",
                metadata.entry_point
            ))
            .into());
        }

        Ok(())
    }
}
///
/// This struct wraps a JavaScript runtime and provides execution methods.
/// It must be used in a single-threaded context (e.g., tokio::task::LocalSet).
pub struct JavaScriptPluginExecutor {
    /// The JavaScript runtime wrapper
    runtime: JsRuntimeWrapper,

    /// Plugin metadata
    metadata: PluginMetadata,

    /// Plugin directory path
    plugin_dir: PathBuf,

    /// Whether the plugin has been initialized
    initialized: bool,
}

impl JavaScriptPluginExecutor {
    /// Create a new JavaScript plugin executor
    fn new(plugin_dir: PathBuf, metadata: PluginMetadata) -> Result<Self> {
        Self::new_with_host_gateway(plugin_dir, metadata, None)
    }

    fn new_with_host_gateway(
        plugin_dir: PathBuf,
        metadata: PluginMetadata,
        host_gateway: Option<PluginHostGatewayHandle>,
    ) -> Result<Self> {
        Self::new_with_limits(plugin_dir, metadata, host_gateway, Default::default())
    }

    fn new_with_limits(
        plugin_dir: PathBuf,
        metadata: PluginMetadata,
        host_gateway: Option<PluginHostGatewayHandle>,
        limits: crate::plugin::sandbox::ResourceLimits,
    ) -> Result<Self> {
        let entry_point = plugin_dir.join(&metadata.entry_point);
        let runtime = JsRuntimeWrapper::new_with_limits(
            entry_point,
            metadata.clone(),
            None,
            host_gateway,
            limits,
        )?;

        Ok(Self {
            runtime,
            metadata,
            plugin_dir,
            initialized: false,
        })
    }

    /// Load the JavaScript module
    pub async fn load_module(&mut self) -> Result<()> {
        self.runtime.load_module().await?;
        Ok(())
    }

    /// Initialize the plugin
    pub async fn initialize(&mut self, config: Value, data_dir: PathBuf) -> Result<()> {
        if self.initialized {
            return Ok(());
        }

        info!("Initializing JavaScript plugin: {}", self.metadata.name);

        // Await lifecycle promises through the same bounded invocation path.
        self.runtime.execute_script(
            r#"
            globalThis.__ting_initialize = async function(context) {
            globalThis.Ting = globalThis.Ting || {};
            globalThis.Ting.config = context.config || {};
            globalThis.Ting.dataDir = context.data_dir || null;
            if (typeof initialize === 'function') {
                await initialize(context);
            }
            return null;
            };
            "#,
        )?;
        self.runtime
            .call_function::<_, Value>(
                "__ting_initialize",
                serde_json::json!({
                    "config": config,
                    "data_dir": data_dir.to_string_lossy(),
                }),
                &Default::default(),
            )
            .await?;
        self.initialized = true;

        info!("JavaScript plugin initialized: {}", self.metadata.name);
        Ok(())
    }

    /// Shutdown the plugin
    pub async fn shutdown(&mut self) -> Result<()> {
        if !self.initialized {
            return Ok(());
        }

        info!("Shutting down JavaScript plugin: {}", self.metadata.name);

        self.runtime.execute_script(
            r#"
            globalThis.__ting_shutdown = async function() {
                if (typeof shutdown === 'function') {
                    await shutdown();
                }
                return null;
            };
        "#,
        )?;
        self.runtime
            .call_function::<_, Value>("__ting_shutdown", Value::Null, &Default::default())
            .await?;
        self.initialized = false;

        info!("JavaScript plugin shut down: {}", self.metadata.name);
        Ok(())
    }

    /// Garbage collect
    pub fn garbage_collect(&mut self) -> Result<()> {
        self.runtime.garbage_collect()
    }

    pub fn is_unavailable(&self) -> bool {
        self.runtime.is_unavailable()
    }

    /// Call a JavaScript function
    pub async fn call_function<T, R>(&mut self, function_name: &str, args: T) -> Result<R>
    where
        T: serde::Serialize,
        R: for<'de> serde::Deserialize<'de>,
    {
        self.runtime
            .call_function(
                function_name,
                args,
                &crate::plugin::types::PluginInvocationContext::default(),
            )
            .await
    }

    pub async fn call_function_with_context<T, R>(
        &mut self,
        function_name: &str,
        args: T,
        context: &crate::plugin::types::PluginInvocationContext,
    ) -> Result<R>
    where
        T: serde::Serialize,
        R: for<'de> serde::Deserialize<'de>,
    {
        self.runtime
            .call_function(function_name, args, context)
            .await
    }

    /// Get the plugin metadata
    pub fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    /// Get the plugin directory
    pub fn plugin_dir(&self) -> &Path {
        &self.plugin_dir
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/plugin/js/plugin.rs"]
mod tests;
