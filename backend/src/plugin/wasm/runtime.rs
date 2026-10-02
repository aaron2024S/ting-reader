//! WASM runtime implementation
//!
//! This module provides the WebAssembly runtime for loading and executing WASM plugins.
//! It uses wasmtime as the WASM engine and provides sandboxed execution with resource limits.

use super::plugin::{PluginState, StoreLimits, WasmExports, WasmPlugin, WasmPluginInner};
use crate::core::app::error::{Result, TingError};
use crate::plugin::PluginHostGatewayHandle;
use crate::plugin::sandbox::{Permission, ResourceLimits, Sandbox};
use crate::plugin::types::{PluginCapability, PluginId, PluginMetadata};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use wasmtime::*;

/// WASM runtime for loading and executing WASM plugins
///
/// Manages the wasmtime engine and provides methods for loading WASM modules,
/// creating instances, and executing WASM functions with sandboxing.
pub struct WasmRuntime {
    /// Wasmtime engine instance
    engine: Arc<Engine>,

    limits: ResourceLimits,

    /// Active sandboxes for each plugin
    sandboxes: Arc<RwLock<HashMap<PluginId, Sandbox>>>,
}

impl WasmRuntime {
    /// Create a new WASM runtime with default configuration
    pub fn new() -> Result<Self> {
        let limits = ResourceLimits::default();
        Self::with_limits(limits.max_memory_bytes, limits.max_cpu_time)
    }

    pub fn with_limits(max_memory_bytes: usize, max_execution_time: Duration) -> Result<Self> {
        if max_memory_bytes == 0 || max_execution_time.is_zero() {
            return Err(TingError::PluginExecutionError(
                "WASM memory and execution limits must be greater than zero".into(),
            ));
        }
        let mut config = Config::new();

        // Enable WASI support for system interface
        config.wasm_backtrace_details(WasmBacktraceDetails::Enable);
        config.wasm_multi_memory(true);
        config.epoch_interruption(true);

        // Create engine with configuration
        let engine = Arc::new(Engine::new(&config).map_err(|e| {
            TingError::PluginExecutionError(format!("Failed to create WASM engine: {}", e))
        })?);

        // Periodic guest yielding lets Tokio enforce deadlines even for loops
        // that never call Host functions. The thread stops with the last user.
        let ticker = Arc::downgrade(&engine);
        std::thread::Builder::new()
            .name("wasm-epoch".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(10));
                    let Some(engine) = ticker.upgrade() else {
                        break;
                    };
                    engine.increment_epoch();
                }
            })
            .map_err(|error| {
                TingError::PluginExecutionError(format!("Failed to start WASM clock: {error}"))
            })?;

        Ok(Self {
            engine,
            limits: ResourceLimits {
                max_memory_bytes,
                max_cpu_time: max_execution_time,
                ..ResourceLimits::default()
            },
            sandboxes: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Load a WASM module from bytes
    ///
    /// # Arguments
    /// * `wasm_bytes` - The WASM binary data
    ///
    /// # Returns
    /// A compiled WASM module ready for instantiation
    pub async fn load_module(&self, wasm_bytes: &[u8]) -> Result<Module> {
        Module::from_binary(&self.engine, wasm_bytes)
            .map_err(|e| TingError::PluginLoadError(format!("Failed to load WASM module: {}", e)))
    }

    /// Load a WASM module from a file
    ///
    /// # Arguments
    /// * `path` - Path to the WASM file
    ///
    /// # Returns
    /// A compiled WASM module ready for instantiation
    pub async fn load_module_from_file(&self, path: &Path) -> Result<Module> {
        Module::from_file(&self.engine, path).map_err(|e| {
            TingError::PluginLoadError(format!("Failed to load WASM module from file: {}", e))
        })
    }

    /// Instantiate a WASM module with sandboxing
    ///
    /// # Arguments
    /// * `module` - The compiled WASM module
    /// * `metadata` - Plugin metadata containing permissions
    ///
    /// # Returns
    /// A WasmPlugin instance ready for execution
    pub async fn instantiate(
        &self,
        module: Module,
        metadata: &PluginMetadata,
    ) -> Result<WasmPlugin> {
        self.instantiate_with_host_gateway(module, metadata, None)
            .await
    }

    pub async fn instantiate_with_host_gateway(
        &self,
        module: Module,
        metadata: &PluginMetadata,
        host_gateway: Option<PluginHostGatewayHandle>,
    ) -> Result<WasmPlugin> {
        validate_declared_exports(&module, metadata)?;
        tokio::time::timeout(
            self.limits.max_cpu_time,
            self.instantiate_module(module, metadata, host_gateway),
        )
        .await
        .map_err(|_| TingError::Timeout("WASM initialization exceeded its deadline".into()))?
    }

    async fn instantiate_module(
        &self,
        module: Module,
        metadata: &PluginMetadata,
        host_gateway: Option<PluginHostGatewayHandle>,
    ) -> Result<WasmPlugin> {
        // Create sandbox with permissions from metadata
        let sandbox = Sandbox::new(metadata.permissions.clone(), self.limits.clone());

        // Create WASI context. Network access must go through Ting host functions
        // so manifest permissions can be enforced consistently.
        let mut wasi_builder = wasmtime_wasi::WasiCtxBuilder::new();
        wasi_builder.inherit_stdio();

        // Create plugin state
        let state = PluginState {
            wasi: wasi_builder.build_p1(),
            host_responses: HashMap::new(),
            plugin_id: metadata.instance_id(),
            permissions: metadata.permissions.clone(),
            host_gateway,
            current_user: None,
            resources: None,
            unavailable: false,
            limiter: StoreLimits::new(self.limits.max_memory_bytes),
        };

        // Create store with resource limits
        let mut store = Store::new(&self.engine, state);
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);

        // Set resource limits on the store
        store.limiter(|state| &mut state.limiter);

        // Create linker for imports
        let mut linker = Linker::new(&self.engine);

        // Add WASI support (Preview 1 adapter)
        wasmtime_wasi::p1::add_to_linker_async(&mut linker, |state: &mut PluginState| {
            &mut state.wasi
        })
        .map_err(|e| {
            TingError::PluginExecutionError(format!("Failed to add WASI to linker: {}", e))
        })?;

        // Register custom HTTP host functions
        super::host_functions::add_host_functions(&mut linker).map_err(|e| {
            TingError::PluginExecutionError(format!("Failed to register host functions: {}", e))
        })?;

        // Instantiate the module
        let instance = linker
            .instantiate_async(&mut store, &module)
            .await
            .map_err(|e| {
                TingError::PluginExecutionError(format!("Failed to instantiate WASM module: {}", e))
            })?;

        // Extract exported functions
        let exports = WasmExports::from_instance(&instance, &mut store)?;
        {
            let revision = instance
                .get_typed_func::<(), i32>(&mut store, "ting_abi_revision")
                .map_err(|error| TingError::PluginLoadError(format!("WASM SDK ABI: {error}")))?
                .call_async(&mut store, ())
                .await
                .map_err(|error| {
                    TingError::PluginLoadError(format!("WASM SDK ABI call: {error}"))
                })?;
            if revision != 2 {
                return Err(TingError::PluginLoadError(format!(
                    "Unsupported WASM ABI {revision}"
                )));
            }
            let supports = instance
                .get_typed_func::<(i32, i32), i32>(&mut store, "supports")
                .map_err(|error| {
                    TingError::PluginLoadError(format!("WASM SDK supports: {error}"))
                })?;
            let alloc = instance
                .get_typed_func::<i32, i32>(&mut store, "alloc")
                .map_err(|error| TingError::PluginLoadError(format!("WASM SDK alloc: {error}")))?;
            let dealloc = instance
                .get_typed_func::<(i32, i32), ()>(&mut store, "dealloc")
                .map_err(|error| {
                    TingError::PluginLoadError(format!("WASM SDK dealloc: {error}"))
                })?;
            let memory = instance
                .get_memory(&mut store, "memory")
                .ok_or_else(|| TingError::PluginLoadError("WASM SDK memory missing".into()))?;
            let mut operations: Vec<&str> = vec!["initialize", "shutdown"];
            for capability in &metadata.capabilities {
                match capability {
                    PluginCapability::MetadataProvider(cap) => {
                        operations.extend(cap.operations.iter().map(|op| op.as_str()))
                    }
                    PluginCapability::FormatHandler(cap) => {
                        operations.extend(cap.operations.iter().map(|op| op.as_str()))
                    }
                    PluginCapability::PluginStore(cap) => {
                        operations.extend(cap.operations.iter().map(|op| op.as_str()))
                    }
                    PluginCapability::ContentProcessor(cap) => {
                        operations.extend(cap.operations.iter().map(|op| op.as_str()))
                    }
                    PluginCapability::ToolProvider(_) => operations.push("invokeTool"),
                    PluginCapability::HttpRoute(_) | PluginCapability::EventHandler(_) => {
                        operations.push("handle")
                    }
                    PluginCapability::UiExtension(_) => operations.push("open"),
                    PluginCapability::TaskHandler(_) => operations.push("run"),
                }
            }
            operations.sort_unstable();
            operations.dedup();
            for operation in operations {
                let bytes = operation.as_bytes();
                let pointer = alloc
                    .call_async(&mut store, bytes.len() as i32)
                    .await
                    .map_err(|error| {
                        TingError::PluginLoadError(format!("WASM SDK alloc: {error}"))
                    })?;
                let offset = usize::try_from(pointer)
                    .map_err(|_| TingError::PluginLoadError("Invalid WASM SDK pointer".into()))?;
                memory.write(&mut store, offset, bytes).map_err(|error| {
                    TingError::PluginLoadError(format!("WASM SDK write: {error}"))
                })?;
                let supported = supports
                    .call_async(&mut store, (pointer, bytes.len() as i32))
                    .await
                    .map_err(|error| {
                        TingError::PluginLoadError(format!("WASM SDK supports: {error}"))
                    })?;
                dealloc
                    .call_async(&mut store, (pointer, bytes.len() as i32))
                    .await
                    .map_err(|error| {
                        TingError::PluginLoadError(format!("WASM SDK release: {error}"))
                    })?;
                if supported != 1 {
                    return Err(TingError::PluginLoadError(format!(
                        "WASM SDK lacks declared operation {operation}"
                    )));
                }
            }
        }

        // Store sandbox for this plugin
        self.sandboxes
            .write()
            .unwrap()
            .insert(metadata.instance_id(), sandbox);

        let inner = WasmPluginInner {
            instance,
            store,
            exports,
            _module: module,
            _engine: self.engine.clone(),
            execution_timeout: self.limits.max_cpu_time,
        };

        Ok(WasmPlugin {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            metadata: Some(metadata.clone()),
        })
    }

    /// Create a sandbox with specific permissions and limits
    ///
    /// # Arguments
    /// * `permissions` - List of permissions to grant
    /// * `limits` - Resource limits to enforce
    ///
    /// # Returns
    /// A configured Sandbox instance
    pub fn create_sandbox(
        &self,
        permissions: Vec<Permission>,
        limits: ResourceLimits,
    ) -> Result<Sandbox> {
        Ok(Sandbox::new(permissions, limits))
    }

    /// Get the sandbox for a specific plugin
    pub fn get_sandbox(&self, plugin_id: &PluginId) -> Option<Sandbox> {
        self.sandboxes.read().unwrap().get(plugin_id).cloned()
    }

    /// Remove the sandbox for a plugin (called during unload)
    pub fn remove_sandbox(&self, plugin_id: &PluginId) {
        self.sandboxes.write().unwrap().remove(plugin_id);
    }
}

/// WASM v2 plugins use a single `invoke(method, params)` trampoline.
/// Reject old ABI exports even when their method signatures happen to match.
fn validate_declared_exports(module: &Module, metadata: &PluginMetadata) -> Result<()> {
    let signature_matches = |function: &FuncType, params: &[ValType], results: &[ValType]| {
        let actual_params: Vec<_> = function.params().collect();
        let actual_results: Vec<_> = function.results().collect();
        actual_params.len() == params.len()
            && actual_results.len() == results.len()
            && actual_params
                .iter()
                .zip(params)
                .all(|(a, b)| ValType::eq(a, b))
            && actual_results
                .iter()
                .zip(results)
                .all(|(a, b)| ValType::eq(a, b))
    };
    let required = [
        ("initialize", vec![], vec![ValType::I32]),
        ("shutdown", vec![], vec![ValType::I32]),
        ("alloc", vec![ValType::I32], vec![ValType::I32]),
        ("dealloc", vec![ValType::I32, ValType::I32], vec![]),
        (
            "invoke",
            vec![ValType::I32, ValType::I32],
            vec![ValType::I32],
        ),
    ];
    for (name, params, result) in [
        ("ting_abi_revision", vec![], vec![ValType::I32]),
        (
            "supports",
            vec![ValType::I32, ValType::I32],
            vec![ValType::I32],
        ),
    ] {
        let found = module
            .exports()
            .find(|export| export.name() == name)
            .map(|export| export.ty());
        if !matches!(found, Some(ExternType::Func(function))
            if signature_matches(&function, &params, &result))
        {
            return Err(TingError::PluginLoadError(format!(
                "WASM SDK export {name} is missing or has incompatible signature"
            )));
        }
    }
    let invalid: Vec<_> = required
        .iter()
        .filter_map(|(name, params, results)| {
            let found = module.exports().find(|export| export.name() == *name);
            match found.map(|export| export.ty()) {
                Some(ExternType::Func(function))
                    if signature_matches(&function, params, results) =>
                {
                    None
                }
                _ => Some(*name),
            }
        })
        .collect();
    if !invalid.is_empty()
        || !matches!(
            module
                .exports()
                .find(|export| export.name() == "memory")
                .map(|export| export.ty()),
            Some(ExternType::Memory(_))
        )
    {
        return Err(TingError::PluginLoadError(format!(
            "WASM plugin {} has missing or incompatible ABI exports: {}{}",
            metadata.instance_id(),
            invalid.join(", "),
            if module
                .exports()
                .any(|export| export.name() == "memory"
                    && matches!(export.ty(), ExternType::Memory(_)))
            {
                ""
            } else {
                " memory"
            }
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/unit/plugin/wasm/runtime.rs"]
mod tests;
