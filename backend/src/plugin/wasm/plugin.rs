use super::runtime::WasmRuntime;
use super::sandbox::Permission;
use crate::core::app::error::{Result, TingError};
use crate::plugin::types::{
    Plugin, PluginContext, PluginId, PluginInvocationContext, PluginMetadata,
};
use crate::plugin::{PluginHostGatewayHandle, PluginHostUser};
use std::collections::HashMap;
use std::sync::Arc;
use wasmtime::*;
use wasmtime_wasi::p1::WasiP1Ctx;

impl Default for WasmRuntime {
    fn default() -> Self {
        Self::new().expect("Failed to create default WASM runtime")
    }
}

/// WASM plugin instance
///
/// Represents a loaded and instantiated WASM plugin with its execution context.
pub struct WasmPlugin {
    /// Inner state protected by mutex for concurrent access
    pub(crate) inner: Arc<tokio::sync::Mutex<WasmPluginInner>>,

    /// Plugin metadata
    pub(crate) metadata: Option<PluginMetadata>,
}

/// Inner state of WASM plugin
pub(crate) struct WasmPluginInner {
    /// WASM instance
    pub(crate) instance: Instance,

    /// WASM store (execution context)
    pub(crate) store: Store<PluginState>,

    /// Exported functions from the WASM module
    pub(crate) exports: WasmExports,

    /// Original module (for re-instantiation if needed)
    pub(crate) _module: Module,
    pub(crate) _engine: Arc<Engine>,
    pub(crate) execution_timeout: std::time::Duration,
}

impl WasmPlugin {
    /// Call the initialize function
    pub async fn initialize_wasm(&self) -> Result<i32> {
        self.call_lifecycle(true).await
    }

    /// Call the shutdown function
    pub async fn shutdown_wasm(&self) -> Result<i32> {
        self.call_lifecycle(false).await
    }

    /// Get the current memory usage in bytes
    pub async fn memory_usage(&self) -> usize {
        let mut inner = self.inner.lock().await;
        let instance = inner.instance;
        if let Some(memory) = instance.get_memory(&mut inner.store, "memory") {
            memory.data_size(&inner.store)
        } else {
            0
        }
    }
}

#[async_trait::async_trait]
impl Plugin for WasmPlugin {
    async fn invoke(
        &self,
        operation: &str,
        input: serde_json::Value,
        context: &PluginInvocationContext,
    ) -> Result<serde_json::Value> {
        let json = self
            .invoke_raw_json_with_context(operation, input, context)
            .await?;
        serde_json::from_str(&json).map_err(|error| {
            TingError::PluginExecutionError(format!("Invalid WASM response JSON: {error}"))
        })
    }

    fn metadata(&self) -> &PluginMetadata {
        self.metadata
            .as_ref()
            .expect("Metadata should be set for instantiated plugin")
    }

    async fn initialize(&self, context: &PluginContext) -> Result<()> {
        let res = self.initialize_wasm().await?;
        if res != 0 {
            return Err(TingError::PluginExecutionError(format!(
                "Initialize returned error code: {}",
                res
            )));
        }
        let result = Plugin::invoke(
            self,
            "initialize",
            serde_json::json!({
                "config": context.config,
                "data_dir": context.data_dir.to_string_lossy(),
            }),
            &PluginInvocationContext {
                user: None,
                resources: context.resources.clone(),
            },
        )
        .await?;
        crate::plugin::require_successful_lifecycle_result(result, "initialize")?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        let result = Plugin::invoke(
            self,
            "shutdown",
            serde_json::json!({}),
            &PluginInvocationContext::default(),
        )
        .await?;
        crate::plugin::require_successful_lifecycle_result(result, "shutdown")?;
        let res = self.shutdown_wasm().await?;
        if res != 0 {
            return Err(TingError::PluginExecutionError(format!(
                "Shutdown returned error code: {}",
                res
            )));
        }
        Ok(())
    }

    async fn garbage_collect(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        inner.store.gc_async(None).await;
        Ok(())
    }
}

/// Exported functions from a WASM module
///
/// Contains typed references to the standard plugin interface functions.
pub(crate) struct WasmExports {
    /// Initialize function: () -> i32
    pub initialize: TypedFunc<(), i32>,

    /// Shutdown function: () -> i32
    pub shutdown: TypedFunc<(), i32>,

    /// Invoke function: (method_ptr: i32, params_ptr: i32) -> i32
    pub invoke: TypedFunc<(i32, i32), i32>,
}

impl WasmExports {
    /// Extract exported functions from a WASM instance
    ///
    /// # Arguments
    /// * `instance` - The WASM instance
    /// * `store` - The WASM store
    ///
    /// # Returns
    /// WasmExports with typed function references
    pub fn from_instance(instance: &Instance, store: &mut Store<PluginState>) -> Result<Self> {
        let initialize = instance
            .get_typed_func::<(), i32>(&mut *store, "initialize")
            .map_err(|e| {
                TingError::PluginLoadError(format!("Failed to get 'initialize' function: {}", e))
            })?;

        let shutdown = instance
            .get_typed_func::<(), i32>(&mut *store, "shutdown")
            .map_err(|e| {
                TingError::PluginLoadError(format!("Failed to get 'shutdown' function: {}", e))
            })?;

        let invoke = instance
            .get_typed_func::<(i32, i32), i32>(&mut *store, "invoke")
            .map_err(|e| {
                TingError::PluginLoadError(format!("Failed to get 'invoke' function: {}", e))
            })?;

        Ok(Self {
            initialize,
            shutdown,
            invoke,
        })
    }
}

/// Plugin state stored in the WASM store
///
/// Contains the execution context and resource limiter for the plugin.
pub struct PluginState {
    /// WASI context for system interface
    pub(crate) wasi: WasiP1Ctx,

    /// HostGateway responses returned through ting_env.host_invoke
    pub(crate) host_responses: HashMap<u32, Vec<u8>>,

    /// Plugin instance id used for HostGateway permission and cache scoping
    pub(crate) plugin_id: PluginId,

    /// Manifest permissions used by HostGateway authorization
    pub(crate) permissions: Vec<Permission>,

    /// Optional HostGateway bridge configured by PluginManager
    pub(crate) host_gateway: Option<PluginHostGatewayHandle>,

    /// User context scoped to the current plugin invocation
    pub(crate) current_user: Option<PluginHostUser>,
    pub(crate) resources: Option<Arc<crate::plugin::host_api::resources::ResourceScope>>,
    pub(crate) unavailable: bool,

    /// Resource limiter for memory and compute
    pub(crate) limiter: StoreLimits,
}

impl PluginState {
    /// Create a new plugin state with default limits
    pub fn new() -> Self {
        let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
        builder.inherit_stdio();

        Self {
            wasi: builder.build_p1(),
            host_responses: HashMap::new(),
            plugin_id: String::new(),
            permissions: Vec::new(),
            host_gateway: None,
            current_user: None,
            resources: None,
            unavailable: false,
            limiter: StoreLimits::default(),
        }
    }

    /// Create a plugin state with custom limits
    pub fn with_limits(memory_limit: usize) -> Self {
        let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
        builder.inherit_stdio();

        Self {
            wasi: builder.build_p1(),
            host_responses: HashMap::new(),
            plugin_id: String::new(),
            permissions: Vec::new(),
            host_gateway: None,
            current_user: None,
            resources: None,
            unavailable: false,
            limiter: StoreLimits::new(memory_limit),
        }
    }
}

impl Default for PluginState {
    fn default() -> Self {
        Self::new()
    }
}

/// Store resource limits
///
/// Implements ResourceLimiter to enforce memory and compute limits on WASM execution.
pub struct StoreLimits {
    /// Maximum memory in bytes
    max_memory_bytes: usize,

    /// Current memory usage
    current_memory_bytes: usize,
}

impl StoreLimits {
    /// Create new store limits with specified maximum memory
    pub fn new(max_memory_bytes: usize) -> Self {
        Self {
            max_memory_bytes,
            current_memory_bytes: 0,
        }
    }

    /// Get current memory usage
    pub fn current_memory(&self) -> usize {
        self.current_memory_bytes
    }
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self::new(512 * 1024 * 1024) // 512 MB default
    }
}

impl ResourceLimiter for StoreLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let delta = desired.saturating_sub(current);
        let new_total = self.current_memory_bytes.saturating_add(delta);

        if new_total <= self.max_memory_bytes {
            self.current_memory_bytes = new_total;
            Ok(true)
        } else {
            tracing::warn!(
                current = current,
                desired = desired,
                limit = self.max_memory_bytes,
                "Memory limit exceeded"
            );
            Ok(false)
        }
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // Allow table growth (could add limits here if needed)
        Ok(true)
    }
}
