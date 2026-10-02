use super::*;
use crate::plugin::types::{Plugin, PluginContext, PluginInvocationContext};
use crate::plugin::wasm::host_functions::guarded_host_callback;
use crate::plugin::wasm::resource_bindings::guarded_resource_callback;
use serde_json::json;
use wasmtime::ResourceLimiter;

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
    assert!(fixture_metadata().permissions.is_empty());
    assert!(fixture_metadata().capabilities.is_empty());
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
        Arc::new(crate::plugin::host_api::logger::DefaultPluginLogger::from_metadata(&metadata)),
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
    let root = std::env::var("TING_TEST_WASM_SCRAPER_DIR").expect("set TING_TEST_WASM_SCRAPER_DIR");
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
        let metadata = crate::plugin::types::metadata::read_plugin_metadata(&directory).unwrap();
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
    let wrong_type = Module::new(&runtime.engine, r#"(module (func (export "invoke")))"#).unwrap();
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

#[tokio::test]
async fn test_wasm_runtime_creation() {
    let runtime = WasmRuntime::new();
    assert!(runtime.is_ok());
}

#[tokio::test]
async fn test_sandbox_creation() {
    let runtime = WasmRuntime::new().unwrap();
    let permissions = vec![Permission::FileRead {
        path: "/tmp".into(),
    }];
    let limits = ResourceLimits::default();

    let sandbox = runtime.create_sandbox(permissions, limits);
    assert!(sandbox.is_ok());
}

#[test]
fn test_store_limits() {
    let mut limits = StoreLimits::new(1024);

    assert!(limits.memory_growing(0, 512, None).unwrap());
    assert_eq!(limits.current_memory(), 512);

    assert!(limits.memory_growing(512, 1024, None).unwrap());
    assert_eq!(limits.current_memory(), 1024);

    assert!(!limits.memory_growing(1024, 2048, None).unwrap());
    assert_eq!(limits.current_memory(), 1024);
}

#[test]
fn host_callback_panics_become_internal_status() {
    let status = guarded_host_callback("test", || panic!("test panic"));
    assert_eq!(
        status,
        ting_plugin_contract::native_abi::NativeStatus::InternalError as i32
    );
}

#[test]
fn resource_callback_panics_become_internal_status() {
    let status = guarded_resource_callback("test", || panic!("test panic"));
    assert_eq!(
        status,
        ting_plugin_contract::native_abi::NativeStatus::InternalError as i32
    );
}
