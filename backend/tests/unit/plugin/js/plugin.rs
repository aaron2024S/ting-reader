use super::*;
use crate::plugin::types::metadata::read_plugin_metadata;
use std::fs;
use tempfile::TempDir;

#[tokio::test]
async fn asynchronous_lifecycle_hooks_complete_before_returning() {
    let dir = create_test_plugin_dir("async-lifecycle", "javascript");
    let source = dir.path().join("async-lifecycle").join("plugin.js");
    fs::write(
        &source,
        r#"
        export async function initialize() {
            await Promise.resolve();
            globalThis.initialized = true;
        }
        export async function shutdown() {
            await Promise.resolve();
            globalThis.stopped = true;
        }
        export function list_plugins() { return []; }
    "#,
    )
    .unwrap();
    let loader = JavaScriptPluginLoader::new(dir.path().join("async-lifecycle")).unwrap();
    let mut executor = loader.create_executor().unwrap();
    executor.load_module().await.unwrap();
    executor
        .initialize(serde_json::json!({}), dir.path().into())
        .await
        .unwrap();
    executor
        .runtime
        .execute_script(
            "if (globalThis.initialized !== true) throw new Error('initialize not awaited');",
        )
        .unwrap();
    executor.shutdown().await.unwrap();
    executor
        .runtime
        .execute_script("if (globalThis.stopped !== true) throw new Error('shutdown not awaited');")
        .unwrap();
}

#[tokio::test]
async fn zero_permission_lifecycle_loops_are_terminated() {
    for initializing in [true, false] {
        let dir = create_test_plugin_dir("bounded-lifecycle", "javascript");
        let plugin_dir = dir.path().join("bounded-lifecycle");
        let source = plugin_dir.join("plugin.js");
        fs::write(&source, if initializing {
            "export function initialize() { while(true) {} } export function list_plugins() { return []; }"
        } else {
            "export function shutdown() { while(true) {} } export function list_plugins() { return []; }"
        }).unwrap();
        let metadata = read_plugin_metadata(&plugin_dir).unwrap();
        assert!(metadata.permissions.is_empty());
        let runtime = JsRuntimeWrapper::new_with_limits(
            source,
            metadata.clone(),
            None,
            None,
            crate::plugin::sandbox::ResourceLimits {
                max_memory_bytes: 32 * 1024 * 1024,
                max_cpu_time: std::time::Duration::from_millis(100),
                ..Default::default()
            },
        )
        .unwrap();
        let mut executor = JavaScriptPluginExecutor {
            runtime,
            metadata,
            plugin_dir,
            initialized: false,
        };
        executor.load_module().await.unwrap();
        let error = if initializing {
            executor
                .initialize(serde_json::json!({}), dir.path().into())
                .await
                .unwrap_err()
        } else {
            executor.initialized = true;
            executor.shutdown().await.unwrap_err()
        };
        assert!(
            matches!(error.downcast_ref(), Some(TingError::Timeout(_))),
            "{error:#}"
        );
        assert!(executor.is_unavailable());
    }
}

fn create_test_plugin_dir(name: &str, runtime: &str) -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join(name);
    fs::create_dir(&plugin_dir).unwrap();

    // Create plugin.yml
    let metadata = serde_json::json!({
        "id": name,
        "name": name,
        "version": "1.0.0",
        "min_core_version": "2.0.0",
        "author": "Test Author",
        "description": {"en": "Test JavaScript plugin"},
        "runtime": runtime,
        "entry_point": "plugin.js",
        "dependencies": [],
        "permissions": [],
        "capabilities": [
            {
                "id": "test.tools",
                "kind": "plugin_store",
                "operations": ["list_plugins"]
            }
        ]
    });

    fs::write(
        plugin_dir.join("plugin.yml"),
        serde_yaml::to_string(&metadata).unwrap(),
    )
    .unwrap();

    // Create a simple JavaScript file
    fs::write(
        plugin_dir.join("plugin.js"),
        r#"
        function initialize(context) {
            console.log("Plugin initialized");
        }

        function shutdown() {
            console.log("Plugin shut down");
        }

        function hello(args) {
            return { message: "Hello, " + args.name + "!" };
        }
        export function list_plugins() { return { plugins: [] }; }
        "#,
    )
    .unwrap();

    temp_dir
}

#[test]
fn test_read_metadata() {
    let temp_dir = create_test_plugin_dir("test-plugin", "javascript");
    let plugin_dir = temp_dir.path().join("test-plugin");

    let metadata = read_plugin_metadata(&plugin_dir).unwrap();

    assert_eq!(metadata.name, "test-plugin");
    assert_eq!(metadata.version, "1.0.0");
    assert_eq!(metadata.author, "Test Author");
    assert_eq!(metadata.entry_point, "plugin.js");
}

#[test]
fn test_verify_runtime_javascript() {
    let temp_dir = create_test_plugin_dir("test-plugin", "javascript");
    let plugin_dir = temp_dir.path().join("test-plugin");

    let metadata = read_plugin_metadata(&plugin_dir).unwrap();
    let result = JavaScriptPluginLoader::verify_runtime(&metadata, &plugin_dir);

    assert!(result.is_ok());
}

#[test]
fn test_verify_runtime_wrong_runtime() {
    let temp_dir = create_test_plugin_dir("test-plugin", "wasm");
    let plugin_dir = temp_dir.path().join("test-plugin");

    let error = read_plugin_metadata(&plugin_dir).unwrap_err();
    assert!(error.to_string().contains("runtime must match"));
}

#[test]
fn test_new_javascript_plugin_loader() {
    let temp_dir = create_test_plugin_dir("test-plugin", "javascript");
    let plugin_dir = temp_dir.path().join("test-plugin");

    let loader = JavaScriptPluginLoader::new(plugin_dir);

    assert!(loader.is_ok());
    let loader = loader.unwrap();
    assert_eq!(loader.metadata().name, "test-plugin");
}

#[test]
fn test_new_missing_entry_point() {
    let temp_dir = TempDir::new().unwrap();
    let plugin_dir = temp_dir.path().join("test-plugin");
    fs::create_dir(&plugin_dir).unwrap();

    // Create plugin.yml but no plugin.js
    let metadata = serde_json::json!({
        "id": "test-plugin",
        "name": "test-plugin",
        "version": "1.0.0",
        "min_core_version": "2.0.0",
        "author": "Test Author",
        "description": {"en": "Test plugin"},
        "runtime": "javascript",
        "entry_point": "plugin.js",
        "capabilities": [
            {
                "id": "test.tools",
                "kind": "plugin_store",
                "operations": ["list_plugins"]
            }
        ]
    });

    fs::write(
        plugin_dir.join("plugin.yml"),
        serde_yaml::to_string(&metadata).unwrap(),
    )
    .unwrap();

    let result = JavaScriptPluginLoader::new(plugin_dir);

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Entry point file not found")
    );
}

#[tokio::test]
async fn test_create_executor_and_load() {
    let temp_dir = create_test_plugin_dir("test-plugin", "javascript");
    let plugin_dir = temp_dir.path().join("test-plugin");

    let loader = JavaScriptPluginLoader::new(plugin_dir).unwrap();
    let mut executor = loader.create_executor().unwrap();

    let result = executor.load_module().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn rejects_missing_declared_export_at_module_load() {
    let temp_dir = create_test_plugin_dir("test-plugin", "javascript");
    let plugin_dir = temp_dir.path().join("test-plugin");
    fs::write(
        plugin_dir.join("plugin.js"),
        "export function unrelated() { return { plugins: [] }; }",
    )
    .unwrap();
    let loader = JavaScriptPluginLoader::new(plugin_dir).unwrap();
    let mut executor = loader.create_executor().unwrap();
    let error = executor.load_module().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Missing declared plugin exports"),
        "{error:#}"
    );
}

#[tokio::test]
async fn loads_package_relative_esm_and_invokes_declared_export() {
    let temp_dir = create_test_plugin_dir("test-plugin", "javascript");
    let plugin_dir = temp_dir.path().join("test-plugin");
    fs::write(
        plugin_dir.join("sdk.js"),
        "export function items() { return [{ id: 'ok' }]; }",
    )
    .unwrap();
    fs::write(
        plugin_dir.join("plugin.js"),
        "import { items } from './sdk.js'; export async function list_plugins() { return { plugins: items() }; }",
    )
    .unwrap();
    let loader = JavaScriptPluginLoader::new(plugin_dir).unwrap();
    let mut executor = loader.create_executor().unwrap();
    executor.load_module().await.unwrap();
    let result: Value = executor
        .call_function("list_plugins", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(result["plugins"][0]["id"], "ok");
}

#[tokio::test]
async fn esm_imports_cannot_escape_package_or_load_remote_code() {
    let temp_dir = create_test_plugin_dir("test-plugin", "javascript");
    let plugin_dir = temp_dir.path().join("test-plugin");
    for request in ["../other.js", "https://example.com/remote.js", "left-pad"] {
        fs::write(
            plugin_dir.join("plugin.js"),
            format!("import '{request}'; export function list_plugins() {{ return {{}}; }}"),
        )
        .unwrap();
        let loader = JavaScriptPluginLoader::new(plugin_dir.clone()).unwrap();
        let mut executor = loader.create_executor().unwrap();
        assert!(executor.load_module().await.is_err(), "{request}");
    }
}

#[tokio::test]
async fn initialization_supplies_config_and_data_directory_to_plugin() {
    let temp_dir = create_test_plugin_dir("test-plugin", "javascript");
    let plugin_dir = temp_dir.path().join("test-plugin");
    fs::write(
        plugin_dir.join("plugin.js"),
        "export function list_plugins() { return { plugins: [], token: Ting.config.token, data_dir: Ting.dataDir }; }",
    )
    .unwrap();
    let loader = JavaScriptPluginLoader::new(plugin_dir.clone()).unwrap();
    let mut executor = loader.create_executor().unwrap();
    executor.load_module().await.unwrap();
    executor
        .initialize(
            serde_json::json!({"token": "configured"}),
            plugin_dir.join("data"),
        )
        .await
        .unwrap();
    let result: Value = executor
        .call_function("list_plugins", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(result["token"], "configured");
    assert!(result["data_dir"].as_str().unwrap().ends_with("data"));
}
