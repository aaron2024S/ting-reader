//! Plugin system module
//!
//! This module provides the plugin system implementation including:
//! - Plugin manager for loading/unloading plugins
//! - Plugin registry for tracking installed plugins
//! - WASM runtime for executing WebAssembly plugins
//! - Native loader for loading native dynamic libraries
//! - Security sandbox for isolating plugin execution
//! - Plugin interfaces (Scraper, Format, Utility)

pub mod config;
pub mod fs_utils;
pub mod host_api;
pub mod installer;
pub mod js;
pub mod manager;
pub mod native;
pub mod registry;
pub mod store;
pub mod types;
pub mod wasm;

pub use config::{ConfigChangeEvent, PluginConfigManager};
pub use host_api::cache::{PluginCache, PluginCacheItem};
pub use host_api::{
    PluginHostGateway, PluginHostGatewayDependencies, PluginHostGatewayHandle,
    PluginHostPermission, PluginHostUser,
};
pub use installer::{PluginInstaller, PluginPackage};
pub use js::{
    JavaScriptPluginExecutor, JavaScriptPluginLoader, JavaScriptPluginWrapper, JsError,
    JsPluginLogger, JsRuntimeWrapper, create_js_runtime_with_bindings,
};
pub use manager::{PluginConfig, PluginInfo, PluginManager};
pub use native::{NativeLoader, NativePlugin};
pub use registry::{PluginEntry, PluginRegistry};
pub use store::{StoreDownload, StorePlugin};
pub use types::scraper::{BookDetail, BookItem, Chapter, SearchResult};
pub use types::{Plugin, PluginId, PluginMetadata, PluginState, PluginStats};
pub use wasm::{FileAccess, Permission, ResourceLimits, Sandbox, WasmPlugin, WasmRuntime};

pub(crate) fn require_successful_lifecycle_result(
    value: serde_json::Value,
    operation: &str,
) -> crate::core::app::error::Result<()> {
    use crate::core::app::error::TingError;
    use ting_plugin_contract::protocol::CallResult;
    match serde_json::from_value::<CallResult<serde_json::Value>>(value).map_err(|_| {
        TingError::PluginExecutionError(format!("Invalid {operation} result envelope"))
    })? {
        CallResult::Success(_) => Ok(()),
        CallResult::Failure(error) => {
            error.error.validate().map_err(|_| {
                TingError::PluginExecutionError(format!("Invalid {operation} error envelope"))
            })?;
            Err(TingError::PluginExecutionError(format!(
                "Plugin {operation} failed: {}",
                error.error.message
            )))
        }
    }
}
