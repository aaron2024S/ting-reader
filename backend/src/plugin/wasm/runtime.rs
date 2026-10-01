//! WASM runtime implementation
//!
//! This module provides the WebAssembly runtime for loading and executing WASM plugins.
//! It uses wasmtime as the WASM engine and provides sandboxed execution with resource limits.

use super::plugin::{PluginState, StoreLimits, WasmExports, WasmPlugin, WasmPluginInner};
use super::sandbox::{Permission, ResourceLimits, Sandbox};
use crate::core::app::error::{Result, TingError};
use crate::plugin::PluginHostGatewayHandle;
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
mod declared_export_tests {
    use super::*;
    use crate::plugin::types::{Plugin, PluginContext, PluginInvocationContext};
    use serde_json::json;

    fn failure_fixture(
        runtime: &WasmRuntime,
        initialize: &str,
        shutdown: &str,
        invoke: &str,
        start: &str,
    ) -> Module {
        Module::new(
            &runtime.engine,
            format!(
                r#"(module
                (memory (export "memory") 1)
                (data (i32.const 512) "{{\22ok\22:true}}\00")
                (global $next (mut i32) (i32.const 2048))
                (global $freed (export "freed") (mut i32) (i32.const 0))
                (func (export "ting_abi_revision") (result i32) i32.const 2)
                (func (export "supports") (param i32 i32) (result i32) i32.const 1)
                (func (export "initialize") (result i32) {initialize})
                (func (export "shutdown") (result i32) {shutdown})
                (func (export "alloc") (param i32) (result i32)
                    global.get $next
                    global.get $next local.get 0 i32.add global.set $next)
                (func (export "dealloc") (param i32 i32)
                    global.get $freed i32.const 1 i32.add global.set $freed)
                (func (export "invoke") (param i32 i32) (result i32) {invoke})
                {start})"#
            ),
        )
        .unwrap()
    }

    fn fixture_metadata() -> PluginMetadata {
        PluginMetadata::new(
            "wasm-failure-fixture".into(),
            "WASM failure fixture".into(),
            "2.0.0".into(),
            "Test".into(),
            "Failure isolation".into(),
            "fixture.wasm".into(),
        )
    }

    async fn assert_healthy_plugin(runtime: &WasmRuntime) {
        let module = failure_fixture(runtime, "i32.const 0", "i32.const 0", "i32.const 512", "");
        let plugin = runtime
            .instantiate(module, &fixture_metadata())
            .await
            .unwrap();
        assert_eq!(
            Plugin::invoke(
                &plugin,
                "probe",
                json!({}),
                &PluginInvocationContext::default(),
            )
            .await
            .unwrap(),
            json!({"ok": true})
        );
    }

    #[tokio::test]
    async fn guest_traps_quarantine_only_the_failed_instance() {
        let runtime = WasmRuntime::new().unwrap();
        for body in ["unreachable", "i32.const 65536 i32.load", "i32.const -1"] {
            let module = failure_fixture(&runtime, "i32.const 0", "i32.const 0", body, "");
            let plugin = runtime
                .instantiate(module, &fixture_metadata())
                .await
                .unwrap();
            let scope = Arc::new(crate::plugin::host_api::resources::ResourceScope::new(
                fixture_metadata().instance_id(),
                uuid::Uuid::new_v4(),
                None,
                std::env::temp_dir(),
                crate::plugin::host_api::resources::ResourceLimits::default(),
            ));
            let context = PluginInvocationContext {
                user: None,
                resources: Some(scope.clone()),
            };
            let error = Plugin::invoke(&plugin, "probe", json!({}), &context)
                .await
                .unwrap_err();
            assert!(
                matches!(error, TingError::PluginExecutionError(_)),
                "{error}"
            );
            assert!(scope.cancellation_token().is_cancelled());
            {
                let mut inner = plugin.inner.lock().await;
                assert!(inner.store.data().unavailable);
                assert!(inner.store.data().current_user.is_none());
                assert!(inner.store.data().resources.is_none());
                assert!(inner.store.data().host_responses.is_empty());
                let instance = inner.instance;
                let freed = instance.get_global(&mut inner.store, "freed").unwrap();
                // Do not re-enter guest dealloc after a failed invocation.
                assert_eq!(freed.get(&mut inner.store).i32(), Some(2));
            }
            let error = Plugin::invoke(&plugin, "probe", json!({}), &context)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("unavailable"), "{error}");
            assert_healthy_plugin(&runtime).await;
        }
    }

    #[tokio::test]
    async fn infinite_guest_loop_times_out_without_stopping_other_plugins() {
        let runtime = WasmRuntime::with_limits(65536, Duration::from_millis(50)).unwrap();
        let module = failure_fixture(
            &runtime,
            "i32.const 0",
            "i32.const 0",
            "(loop $again br $again) i32.const 512",
            "",
        );
        let plugin = runtime
            .instantiate(module, &fixture_metadata())
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let error = Plugin::invoke(
            &plugin,
            "probe",
            json!({}),
            &PluginInvocationContext::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TingError::Timeout(_)), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(plugin.inner.lock().await.store.data().unavailable);
        assert_healthy_plugin(&runtime).await;
    }

    #[tokio::test]
    async fn loading_and_lifecycle_loops_are_bounded() {
        let runtime = WasmRuntime::with_limits(65536, Duration::from_millis(50)).unwrap();
        let loop_body = "(loop $again br $again) i32.const 0";
        let metadata = fixture_metadata();
        let start = "(func $start (loop $again br $again)) (start $start)";
        let module = failure_fixture(
            &runtime,
            "i32.const 0",
            "i32.const 0",
            "i32.const 512",
            start,
        );
        let result = runtime.instantiate(module, &metadata).await;
        assert!(matches!(result, Err(TingError::Timeout(_))));
        for initializing in [true, false] {
            let module = failure_fixture(
                &runtime,
                if initializing {
                    loop_body
                } else {
                    "i32.const 0"
                },
                if initializing {
                    "i32.const 0"
                } else {
                    loop_body
                },
                "i32.const 512",
                "",
            );
            let plugin = runtime.instantiate(module, &metadata).await.unwrap();
            let result = if initializing {
                plugin.initialize_wasm().await
            } else {
                plugin.shutdown_wasm().await
            };
            assert!(matches!(result, Err(TingError::Timeout(_))));
            assert!(plugin.inner.lock().await.store.data().unavailable);
        }
        assert_healthy_plugin(&runtime).await;
    }

    #[tokio::test]
    async fn configured_memory_limit_rejects_growth() {
        let runtime = WasmRuntime::with_limits(65536, Duration::from_secs(1)).unwrap();
        let module = failure_fixture(
            &runtime,
            "i32.const 0",
            "i32.const 0",
            "i32.const 1 memory.grow drop i32.const 512",
            "",
        );
        let plugin = runtime
            .instantiate(module, &fixture_metadata())
            .await
            .unwrap();
        Plugin::invoke(
            &plugin,
            "probe",
            json!({}),
            &PluginInvocationContext::default(),
        )
        .await
        .unwrap();
        assert_eq!(plugin.memory_usage().await, 65536);
        let runtime = WasmRuntime::with_limits(32768, Duration::from_secs(1)).unwrap();
        let module = failure_fixture(&runtime, "i32.const 0", "i32.const 0", "i32.const 512", "");
        assert!(
            runtime
                .instantiate(module, &fixture_metadata())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn runtime_returns_forced_out_of_fuel_instead_of_aborting() {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config).unwrap();
        let module = Module::new(
            &engine,
            r#"(module (func (export "loop") (loop $again br $again))
                (func (export "healthy") (result i32) i32.const 42))"#,
        )
        .unwrap();
        let mut store = Store::new(&engine, ());
        store.set_fuel(100).unwrap();
        let instance = Linker::new(&engine)
            .instantiate_async(&mut store, &module)
            .await
            .unwrap();
        let function = instance
            .get_typed_func::<(), ()>(&mut store, "loop")
            .unwrap();
        let error = function.call_async(&mut store, ()).await.unwrap_err();
        assert_eq!(error.downcast_ref::<Trap>(), Some(&Trap::OutOfFuel));
        store.set_fuel(100).unwrap();
        assert_eq!(
            instance
                .get_typed_func::<(), i32>(&mut store, "healthy")
                .unwrap()
                .call_async(&mut store, ())
                .await
                .unwrap(),
            42
        );
    }

    /// Set TING_TEST_WASM_SDK to a compiled trpack WASM tool template to
    /// exercise the public SDK against the actual Wasmtime Host adapter.
    #[tokio::test]
    #[ignore = "requires a generated tool artifact in TING_TEST_WASM_SDK"]
    async fn generated_wasm_sdk_invokes_declared_tool() {
        let path = std::env::var("TING_TEST_WASM_SDK").expect("set TING_TEST_WASM_SDK");
        let runtime = WasmRuntime::new().unwrap();
        let module = runtime
            .load_module_from_file(Path::new(&path))
            .await
            .unwrap();
        let mut metadata = PluginMetadata::new(
            "wasm-tool".into(),
            "WASM tool".into(),
            "2.0.0".into(),
            "Ting Reader".into(),
            "Generated SDK tool".into(),
            "ting_plugin.wasm".into(),
        );
        metadata.capabilities.push(
            serde_json::from_value(json!({
                "id": "tools.example",
                "kind": "tool_provider",
                "invoke": "invokeTool",
                "tools": [{
                    "name": "example.echo",
                    "description": {"en": "Echo"},
                    "input_schema": {"type": "object"},
                    "output_schema": {"type": "object"},
                    "side_effects": false
                }]
            }))
            .unwrap(),
        );
        let plugin = runtime.instantiate(module, &metadata).await.unwrap();
        let result = Plugin::invoke(
            &plugin,
            "invokeTool",
            json!({"tool_name": "example.echo", "params": {"message": "hello"}}),
            &PluginInvocationContext::default(),
        )
        .await
        .unwrap();
        assert_eq!(result, json!({"ok": true, "data": {}}));
    }

    /// Point TING_TEST_WASM_SCRAPER at the compiled official Douban WASM.
    /// A search without a gateway must fail inside the SDK envelope, rather
    /// than fail to load, trap, or return a legacy bare JSON error.
    #[tokio::test]
    #[ignore = "requires an official scraper artifact in TING_TEST_WASM_SCRAPER"]
    async fn official_wasm_scraper_loads_with_sdk_and_returns_envelope() {
        let path = std::env::var("TING_TEST_WASM_SCRAPER").expect("set TING_TEST_WASM_SCRAPER");
        let runtime = WasmRuntime::new().unwrap();
        let module = runtime
            .load_module_from_file(Path::new(&path))
            .await
            .unwrap();
        let mut metadata = PluginMetadata::new(
            "douban-scraper-wasm".into(),
            "Douban".into(),
            "2.0.0".into(),
            "Ting Reader".into(),
            "Official scraper".into(),
            "douban_scraper_wasm.wasm".into(),
        );
        metadata.capabilities.push(
            serde_json::from_value(json!({
                "id": "metadata.search", "kind": "metadata_provider",
                "operations": ["search"], "search_fields": [], "result_fields": []
            }))
            .unwrap(),
        );
        let plugin = runtime.instantiate(module, &metadata).await.unwrap();
        let context = PluginContext::new(
            json!({"source": "test"}),
            std::env::temp_dir(),
            Arc::new(
                crate::plugin::host_api::logger::DefaultPluginLogger::from_metadata(&metadata),
            ),
            Arc::new(crate::plugin::manager::event_bus::DefaultPluginEventBus::new()),
        );
        Plugin::initialize(&plugin, &context).await.unwrap();
        let result = Plugin::invoke(
            &plugin,
            "search",
            json!({"title": "fixture"}),
            &PluginInvocationContext::default(),
        )
        .await
        .unwrap();
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["error"]["plugin_id"], "douban-scraper-wasm");
        assert_eq!(result["error"]["operation"], "search");
        Plugin::shutdown(&plugin).await.unwrap();
    }

    /// Build the fifteen official scraper artifacts, then point this at the
    /// plugin source root to verify every declared export in real Wasmtime.
    #[tokio::test]
    #[ignore = "requires compiled scraper artifacts in TING_TEST_WASM_SCRAPER_DIR"]
    async fn all_official_wasm_scrapers_load_with_v2_sdk() {
        let root =
            std::env::var("TING_TEST_WASM_SCRAPER_DIR").expect("set TING_TEST_WASM_SCRAPER_DIR");
        let root = Path::new(&root);
        let runtime = WasmRuntime::new().unwrap();
        let mut count = 0;
        for directory in std::fs::read_dir(root).unwrap() {
            let directory = directory.unwrap().path();
            let Some(name) = directory.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.ends_with("-scraper-wasm") {
                continue;
            }
            let metadata =
                crate::plugin::types::metadata::read_plugin_metadata(&directory).unwrap();
            let artifact = directory
                .join("target/wasm32-wasip1/debug")
                .join(&metadata.entry_point);
            assert!(artifact.is_file(), "Missing {} WASM artifact", metadata.id);
            let module = runtime.load_module_from_file(&artifact).await.unwrap();
            let plugin = runtime
                .instantiate(module, &metadata)
                .await
                .unwrap_or_else(|error| panic!("{} failed to load: {error}", metadata.id));
            let result = Plugin::invoke(
                &plugin,
                "search",
                json!({"title": "offline SDK integration"}),
                &PluginInvocationContext::default(),
            )
            .await
            .unwrap();
            assert!(
                result
                    .get("ok")
                    .and_then(serde_json::Value::as_bool)
                    .is_some(),
                "{} returned a result outside the SDK envelope: {result}",
                metadata.id
            );
            count += 1;
        }
        assert_eq!(count, 15, "Expected every official WASM scraper to load");
    }

    #[test]
    fn rejects_missing_or_incompatible_wasm_trampoline() {
        let runtime = WasmRuntime::new().unwrap();
        let mut metadata = PluginMetadata::new(
            "example".into(),
            "Example".into(),
            "2.0.0".into(),
            "Example".into(),
            "Example".into(),
            "plugin.wasm".into(),
        );
        metadata.capabilities = vec![
            serde_json::from_value(json!({
                "id": "format.example", "kind": "format_handler",
                "extensions": ["example"], "operations": ["probe", "extract_metadata"]
            }))
            .unwrap(),
        ];
        let wrong_type =
            Module::new(&runtime.engine, r#"(module (func (export "invoke")))"#).unwrap();
        let error = validate_declared_exports(&wrong_type, &metadata).unwrap_err();
        assert!(error.to_string().contains("ting_abi_revision"), "{error}");
        let complete = Module::new(
            &runtime.engine,
            r#"(module
                (memory (export "memory") 1)
                (func (export "initialize") (result i32) i32.const 0)
                (func (export "shutdown") (result i32) i32.const 0)
                (func (export "alloc") (param i32) (result i32) i32.const 0)
                (func (export "dealloc") (param i32 i32))
                (func (export "invoke") (param i32 i32) (result i32) i32.const 0))"#,
        )
        .unwrap();
        assert!(validate_declared_exports(&complete, &metadata).is_err());
        let wrong_signature = Module::new(
            &runtime.engine,
            r#"(module
                (memory (export "memory") 1)
                (func (export "initialize") (result i32) i32.const 0)
                (func (export "shutdown") (result i32) i32.const 0)
                (func (export "alloc") (param i32) (result i32) i32.const 0)
                (func (export "dealloc") (param i32 i32))
                (func (export "invoke") (param i32) (result i32) i32.const 0))"#,
        )
        .unwrap();
        assert!(validate_declared_exports(&wrong_signature, &metadata).is_err());
    }

    #[tokio::test]
    async fn invocation_releases_both_inputs_and_plugin_output() {
        let runtime = WasmRuntime::new().unwrap();
        let metadata = PluginMetadata::new(
            "wasm-owned-buffers".into(),
            "Wasm Owned Buffers".into(),
            "2.0.0".into(),
            "Test".into(),
            "Test buffer ownership".into(),
            "fixture.wasm".into(),
        );
        let module = Module::new(
            &runtime.engine,
            r#"(module
                (memory (export "memory") 1)
                (data (i32.const 512) "{\22ok\22:true}\00")
                (global $next (mut i32) (i32.const 2048))
                (global $freed (export "freed") (mut i32) (i32.const 0))
                (func (export "ting_abi_revision") (result i32) i32.const 2)
                (func (export "supports") (param i32 i32) (result i32) i32.const 1)
                (func (export "initialize") (result i32) i32.const 0)
                (func (export "shutdown") (result i32) i32.const 0)
                (func (export "alloc") (param i32) (result i32)
                    global.get $next
                    global.get $next
                    local.get 0
                    i32.add
                    global.set $next)
                (func (export "dealloc") (param i32 i32)
                    global.get $freed
                    i32.const 1
                    i32.add
                    global.set $freed)
                (func (export "invoke") (param i32 i32) (result i32) i32.const 512))"#,
        )
        .unwrap();
        let plugin = runtime.instantiate(module, &metadata).await.unwrap();
        let response = Plugin::invoke(
            &plugin,
            "probe",
            serde_json::json!({}),
            &PluginInvocationContext::default(),
        )
        .await
        .unwrap();
        assert_eq!(response, serde_json::json!({"ok": true}));
        let mut inner = plugin.inner.lock().await;
        let instance = inner.instance;
        let freed = instance.get_global(&mut inner.store, "freed").unwrap();
        // supports() receives two temporary buffers for lifecycle operations.
        assert_eq!(freed.get(&mut inner.store).i32(), Some(5));
    }
}
