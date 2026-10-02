//! Plugin system module
//!
//! This module provides the plugin system implementation including:
//! - Plugin manager for loading/unloading plugins
//! - Plugin registry for tracking installed plugins
//! - JavaScript runtime for executing bounded JS plugins
//! - WASM runtime for executing WebAssembly plugins
//! - Native loader for loading native dynamic libraries
//! - Security sandbox for isolating plugin execution
//! - Plugin interfaces (Scraper, Format, Utility)

pub mod config;
pub mod fs_utils;
pub mod host_api;
pub mod installer;
pub mod js;
mod lifecycle_result;
pub mod manager;
pub mod native;
pub mod registry;
pub mod sandbox;
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
pub(crate) use lifecycle_result::require_successful_lifecycle_result;
pub use manager::{PluginConfig, PluginInfo, PluginManager};
pub use native::{NativeLoader, NativePlugin};
pub use registry::{PluginEntry, PluginRegistry};
pub use sandbox::{FileAccess, Permission, ResourceLimits, Sandbox};
pub use store::{StoreDownload, StorePlugin};
pub use types::scraper::{BookDetail, BookItem, Chapter, SearchResult};
pub use types::{Plugin, PluginId, PluginMetadata, PluginState, PluginStats};
pub use wasm::{WasmPlugin, WasmRuntime};
