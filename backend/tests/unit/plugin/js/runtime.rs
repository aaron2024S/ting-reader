use super::*;
use tempfile::NamedTempFile;

fn bounded_runtime(path: PathBuf, timeout: Duration) -> JsRuntimeWrapper {
    let metadata = PluginMetadata::new(
        "zero-permission".into(),
        "Zero permission".into(),
        "2.0.0".into(),
        "Test".into(),
        "Resource isolation".into(),
        "plugin.js".into(),
    );
    assert!(metadata.permissions.is_empty());
    JsRuntimeWrapper::new_with_limits(
        path,
        metadata,
        None,
        None,
        ResourceLimits {
            max_memory_bytes: 32 * 1024 * 1024,
            max_cpu_time: timeout,
            ..ResourceLimits::default()
        },
    )
    .unwrap()
}

#[test]
fn plugin_globals_expose_only_bounded_host_transport_ops() {
    let file = NamedTempFile::new().unwrap();
    let mut runtime = bounded_runtime(file.path().into(), Duration::from_secs(1));
    runtime.execute_script(r#"
        const ops = Object.keys(Deno.core.ops).sort();
        const expected = ['op_plugin_log', 'op_decode_utf8', 'op_host_invoke',
            'op_chunk_create', 'op_chunk_copy', 'op_resource_write',
            'op_test_principal'].sort();
        if (JSON.stringify(ops) !== JSON.stringify(expected)) throw new Error('Unexpected host ops');
        if (typeof WebAssembly !== 'undefined') throw new Error('Unbudgeted WASM');
        if (typeof Deno.core.encode !== 'undefined') throw new Error('Unbudgeted encoding');
        if (!Object.isFrozen(Deno.core.ops)) throw new Error('Mutable host ops');
    "#).unwrap();
}

#[test]
fn invalid_sync_host_ops_throw_catchable_errors_without_custom_js_builders() {
    let file = NamedTempFile::new().unwrap();
    let mut runtime = bounded_runtime(file.path().into(), Duration::from_secs(1));
    runtime
        .execute_script(
            r#"
        for (const call of [
            () => Ting.resources.chunkCopy('missing'),
            () => Ting.resources.chunkCreate(new Uint8Array(8)),
            () => Ting.resources.writeAt('missing', 0, new Uint8Array(8)),
            () => Deno.core.ops.op_plugin_log('invalid-level', 'message', null),
        ]) {
            let threw = false;
            try { call(); } catch (error) { threw = error instanceof Error; }
            if (!threw) throw new Error('Expected a catchable native error');
        }
    "#,
        )
        .unwrap();
    assert!(!runtime.is_unavailable());
}

#[tokio::test]
async fn cancelling_an_inflight_future_releases_context_and_quarantines_the_instance() {
    let file = NamedTempFile::new().unwrap();
    let mut runtime = bounded_runtime(file.path().into(), Duration::from_secs(2));
    runtime
        .execute_script(
            r#"
        globalThis.pending = async function() {
            return await Ting.host.invoke('resources.read_at', globalThis.readRequest);
        };
    "#,
        )
        .unwrap();
    struct SlowSource;
    impl crate::plugin::host_api::resources::ResourceSource for SlowSource {
        fn stat(&self) -> ting_plugin_contract::resources::ResourceStat {
            ting_plugin_contract::resources::ResourceStat {
                length: Some(1),
                mime_type: None,
                readable: true,
                writable: false,
                seekable: true,
                revision: None,
                finished: true,
            }
        }
        fn read_at(
            &self,
            _: u64,
            _: usize,
            cancellation: &tokio_util::sync::CancellationToken,
        ) -> crate::plugin::host_api::resources::ResourceResult<Vec<u8>> {
            while !cancellation.is_cancelled() {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(Vec::new())
        }
    }
    let scope = std::sync::Arc::new(crate::plugin::host_api::resources::ResourceScope::new(
        runtime.metadata.instance_id(),
        uuid::Uuid::new_v4(),
        None,
        std::env::temp_dir(),
        Default::default(),
    ));
    let source = scope
        .grant_source(std::sync::Arc::new(SlowSource), 1, 0)
        .unwrap();
    runtime
        .execute_script(&format!(
            "globalThis.readRequest = {{resource: '{}', offset: 0, max_bytes: 1}};",
            source.0,
        ))
        .unwrap();
    let context = crate::plugin::types::PluginInvocationContext {
        user: None,
        resources: Some(scope.clone()),
    };
    let result = tokio::time::timeout(
        Duration::from_millis(50),
        runtime.call_function::<_, Value>("pending", Value::Null, &context),
    )
    .await;
    assert!(result.is_err());
    assert!(runtime.is_unavailable());
    assert!(scope.cancellation_token().is_cancelled());
    assert!(
        runtime
            .runtime
            .op_state()
            .borrow()
            .borrow::<JsHostInvocationContext>()
            .resources
            .is_none()
    );
}

#[tokio::test]
async fn host_chunk_copies_consume_the_buffer_budget_and_release_the_owner() {
    let file = NamedTempFile::new().unwrap();
    let mut runtime = bounded_runtime(file.path().into(), Duration::from_secs(2));
    let released = runtime.budget.release_probe();
    runtime
        .execute_script(
            r#"
        globalThis.copy_chunks = function(args) {
            globalThis.copies = [];
            while (true) copies.push(Ting.resources.chunkCopy(args.chunk));
        };
    "#,
        )
        .unwrap();
    let scope = std::sync::Arc::new(crate::plugin::host_api::resources::ResourceScope::new(
        runtime.metadata.instance_id(),
        uuid::Uuid::new_v4(),
        None,
        std::env::temp_dir(),
        Default::default(),
    ));
    let chunk = scope.create_chunk(&vec![42_u8; 64 * 1024]).unwrap();
    let context = crate::plugin::types::PluginInvocationContext {
        user: None,
        resources: Some(scope.clone()),
    };
    let error = runtime
        .call_function::<_, Value>(
            "copy_chunks",
            serde_json::json!({"chunk": chunk.0}),
            &context,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref(),
            Some(crate::core::app::error::TingError::ResourceLimitExceeded(_))
        ),
        "{error:#}"
    );
    assert!(scope.cancellation_token().is_cancelled());
    drop(runtime);
    assert!(released());
}

#[test]
fn zero_permissions_deny_access_and_interrupt_synchronous_loops() {
    let file = NamedTempFile::new().unwrap();
    let mut runtime = bounded_runtime(file.path().into(), Duration::from_millis(100));
    assert!(runtime.sandbox().is_some());
    assert!(runtime.check_network_access("https://example.com").is_err());
    assert!(
        runtime
            .check_file_access(file.path(), crate::plugin::sandbox::FileAccess::Read)
            .is_err()
    );
    let start = Instant::now();
    let error = runtime.execute_script("while (true) {}").unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(crate::core::app::error::TingError::Timeout(_))
    ));
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(runtime.is_unavailable());
    assert!(runtime.execute_script("1 + 1").is_err());
    let mut healthy = bounded_runtime(file.path().into(), Duration::from_secs(2));
    healthy.execute_script("1 + 1").unwrap();
}

#[tokio::test]
async fn zero_permission_module_loop_is_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugin.js");
    std::fs::write(&path, "while (true) {}").unwrap();
    let mut runtime = bounded_runtime(path, Duration::from_millis(100));
    let error = runtime.load_module().await.unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(crate::core::app::error::TingError::Timeout(_))
    ));
    assert!(runtime.is_unavailable());
    assert!(
        runtime
            .runtime
            .op_state()
            .borrow()
            .borrow::<JsHostInvocationContext>()
            .cancellation
            .is_none()
    );
}

#[tokio::test]
async fn invocation_timeout_cancels_and_releases_host_resources() {
    let file = NamedTempFile::new().unwrap();
    let mut runtime = bounded_runtime(file.path().into(), Duration::from_millis(100));
    runtime
        .execute_script("globalThis.forever = function() { while (true) {} };")
        .unwrap();
    let scope = std::sync::Arc::new(crate::plugin::host_api::resources::ResourceScope::new(
        runtime.metadata.instance_id(),
        uuid::Uuid::new_v4(),
        None,
        std::env::temp_dir(),
        Default::default(),
    ));
    let weak = std::sync::Arc::downgrade(&scope);
    let context = crate::plugin::types::PluginInvocationContext {
        user: None,
        resources: Some(scope.clone()),
    };
    let error = runtime
        .call_function::<_, Value>("forever", Value::Null, &context)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref(),
        Some(crate::core::app::error::TingError::Timeout(_))
    ));
    assert!(scope.cancellation_token().is_cancelled());
    drop(context);
    drop(scope);
    assert!(weak.upgrade().is_none());
}

#[test]
fn heap_and_array_buffer_exhaustion_quarantine_and_release_instances() {
    let file = NamedTempFile::new().unwrap();
    for script in [
        "globalThis.items = []; while(true) { items.push({data: 'x'.repeat(1024), i: items.length}); }",
        "globalThis.items = []; while(true) { items.push(new ArrayBuffer(1024 * 1024)); }",
    ] {
        let mut runtime = bounded_runtime(file.path().into(), Duration::from_secs(10));
        let released = runtime.budget.release_probe();
        let error = runtime.execute_script(script).unwrap_err();
        assert!(
            matches!(
                error.downcast_ref(),
                Some(crate::core::app::error::TingError::ResourceLimitExceeded(_))
            ),
            "{error:#}"
        );
        assert!(runtime.is_unavailable());
        drop(runtime);
        assert!(
            released(),
            "allocator/callback/watchdog retained the budget after V8 disposal"
        );
    }
}

#[test]
fn repeated_runtime_disposal_releases_backing_stores_and_callbacks() {
    let file = NamedTempFile::new().unwrap();
    for _ in 0..12 {
        let mut runtime = bounded_runtime(file.path().into(), Duration::from_secs(2));
        let released = runtime.budget.release_probe();
        runtime.garbage_collect().unwrap();
        let baseline = runtime.budget.external_bytes();
        runtime
            .execute_script("globalThis.buffer = new ArrayBuffer(1024 * 1024);")
            .unwrap();
        assert!(runtime.budget.external_bytes() >= 1024 * 1024);
        runtime.execute_script("globalThis.buffer = null;").unwrap();
        runtime.garbage_collect().unwrap();
        assert_eq!(runtime.budget.external_bytes(), baseline);
        drop(runtime);
        assert!(released());
    }
}

#[tokio::test]
async fn input_json_never_supplies_the_host_principal() {
    let metadata = PluginMetadata::new(
        "context-test".into(),
        "Context Test".into(),
        "2.0.0".into(),
        "Test".into(),
        "Test".into(),
        "plugin.js".into(),
    );
    let file = NamedTempFile::new().unwrap();
    let mut runtime = JsRuntimeWrapper::new(file.path().to_path_buf(), metadata, None).unwrap();
    runtime.execute_script(
        "globalThis._ting_invoke = function() { globalThis._ting_result = JSON.stringify(Deno.core.ops.op_test_principal()); globalThis._ting_status = 'success'; };"
    ).unwrap();
    let forged = serde_json::json!({
        "_context": {"route": {"authenticated": true,
            "user": {"id": "victim", "username": "admin", "role": "admin"}}}
    });
    let context = crate::plugin::types::PluginInvocationContext::default();
    let anonymous = runtime
        .call_function::<_, String>("anything", forged.clone(), &context)
        .await
        .unwrap();
    assert_eq!(anonymous, "anonymous");
    let trusted = crate::plugin::types::PluginInvocationContext {
        user: Some(crate::plugin::PluginHostUser {
            id: "alice".into(),
            username: "alice".into(),
            role: "user".into(),
        }),
        resources: None,
    };
    let principal = runtime
        .call_function::<_, String>("anything", forged, &trusted)
        .await
        .unwrap();
    assert_eq!(principal, "alice");
    assert!(
        runtime
            .runtime
            .op_state()
            .borrow()
            .borrow::<JsHostInvocationContext>()
            .user
            .is_none()
    );
}

#[tokio::test]
async fn test_js_runtime_creation() {
    let metadata = PluginMetadata::new(
        "test-plugin".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "plugin.js".to_string(),
    );

    let temp_file = NamedTempFile::new().unwrap();
    let runtime = JsRuntimeWrapper::new(temp_file.path().to_path_buf(), metadata, None);
    assert!(runtime.is_ok());
}

#[tokio::test]
async fn test_execute_simple_script() {
    let metadata = PluginMetadata::new(
        "test-plugin".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "plugin.js".to_string(),
    );

    let temp_file = NamedTempFile::new().unwrap();
    let mut runtime =
        JsRuntimeWrapper::new(temp_file.path().to_path_buf(), metadata, None).unwrap();

    let result = runtime.execute_script("const x = 1 + 1;");
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_load_module() {
    let metadata = PluginMetadata::new(
        "test-plugin".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "plugin.js".to_string(),
    );

    // Create a temporary JavaScript file
    let package = tempfile::tempdir().unwrap();
    let entry = package.path().join("plugin.js");
    std::fs::write(
        &entry,
        "export function hello() { return 'Hello, World!'; }",
    )
    .unwrap();

    let mut runtime = JsRuntimeWrapper::new(entry, metadata, None).unwrap();
    let result = runtime.load_module().await;
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn test_load_module_accepts_relative_entry_path() {
    let metadata = PluginMetadata::new(
        "test-plugin".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "plugin.js".to_string(),
    );

    let current_dir = std::env::current_dir().unwrap();
    let package = tempfile::tempdir_in(&current_dir).unwrap();
    let entry = package.path().join("plugin.js");
    std::fs::write(
        &entry,
        "export function hello() { return 'Hello, relative path!'; }",
    )
    .unwrap();
    let relative_entry = entry.strip_prefix(&current_dir).unwrap().to_path_buf();
    assert!(!relative_entry.is_absolute());

    let mut runtime = JsRuntimeWrapper::new(relative_entry, metadata, None).unwrap();
    let result = runtime.load_module().await;
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn test_execute_script_with_error() {
    let metadata = PluginMetadata::new(
        "test-plugin".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "plugin.js".to_string(),
    );

    let temp_file = NamedTempFile::new().unwrap();
    let mut runtime =
        JsRuntimeWrapper::new(temp_file.path().to_path_buf(), metadata, None).unwrap();

    // This should fail due to syntax error
    let result = runtime.execute_script("const x = ;");
    assert!(result.is_err());
}

#[tokio::test]
async fn test_sandbox_network_access_check() {
    use crate::plugin::sandbox::Permission;

    let mut metadata = PluginMetadata::new(
        "test-plugin".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "plugin.js".to_string(),
    );

    // Add network permission
    metadata.permissions = vec![Permission::NetworkAccess {
        domain: "*.example.com".into(),
    }];

    let temp_file = NamedTempFile::new().unwrap();
    let runtime = JsRuntimeWrapper::new(temp_file.path().to_path_buf(), metadata, None).unwrap();

    // Check allowed URL
    let result = runtime.check_network_access("https://api.example.com/data");
    assert!(result.is_ok());

    // Check disallowed URL
    let result = runtime.check_network_access("https://evil.com/data");
    assert!(result.is_err());
}

#[tokio::test]
async fn test_sandbox_file_access_check() {
    use crate::plugin::sandbox::{FileAccess, Permission};
    use std::path::PathBuf;

    let mut metadata = PluginMetadata::new(
        "test-plugin".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "plugin.js".to_string(),
    );

    // Add file permissions
    metadata.permissions = vec![
        Permission::FileRead {
            path: "./data/cache".into(),
        },
        Permission::FileWrite {
            path: "./data/output".into(),
        },
    ];

    let temp_file = NamedTempFile::new().unwrap();
    let runtime = JsRuntimeWrapper::new(temp_file.path().to_path_buf(), metadata, None).unwrap();

    // Check allowed read
    let result =
        runtime.check_file_access(&PathBuf::from("./data/cache/file.txt"), FileAccess::Read);
    assert!(result.is_ok());

    // Check allowed write
    let result =
        runtime.check_file_access(&PathBuf::from("./data/output/file.txt"), FileAccess::Write);
    assert!(result.is_ok());

    // Check disallowed read (wrong path)
    let result =
        runtime.check_file_access(&PathBuf::from("./data/secret/file.txt"), FileAccess::Read);
    assert!(result.is_err());

    // Check disallowed write (read-only path)
    let result =
        runtime.check_file_access(&PathBuf::from("./data/cache/file.txt"), FileAccess::Write);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_sandbox_memory_limit_check() {
    use crate::plugin::sandbox::Permission;

    let mut metadata = PluginMetadata::new(
        "test-plugin".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "plugin.js".to_string(),
    );

    // Add a permission to trigger sandbox creation
    metadata.permissions = vec![Permission::NetworkAccess {
        domain: "example.com".into(),
    }];

    let temp_file = NamedTempFile::new().unwrap();
    let runtime = JsRuntimeWrapper::new(temp_file.path().to_path_buf(), metadata, None).unwrap();

    // Check within limit
    let result = runtime.check_memory_limit(100 * 1024 * 1024); // 100 MB
    assert!(result.is_ok());

    // Check exceeding limit
    let result = runtime.check_memory_limit(1024 * 1024 * 1024); // 1 GB (exceeds default 512 MB)
    assert!(result.is_err());
}

#[tokio::test]
async fn test_sandbox_cpu_time_tracking() {
    use crate::plugin::sandbox::Permission;
    use std::time::Duration;

    let mut metadata = PluginMetadata::new(
        "test-plugin".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "plugin.js".to_string(),
    );

    // Add a permission to trigger sandbox creation
    metadata.permissions = vec![Permission::NetworkAccess {
        domain: "example.com".into(),
    }];

    let temp_file = NamedTempFile::new().unwrap();
    let mut runtime =
        JsRuntimeWrapper::new(temp_file.path().to_path_buf(), metadata, None).unwrap();

    // Start tracking
    runtime.start_execution();

    // Simulate some work
    std::thread::sleep(Duration::from_millis(10));

    // Check CPU time (should be OK for short duration)
    let result = runtime.check_cpu_time_limit();
    assert!(result.is_ok());

    // Stop tracking
    runtime.stop_execution();
}
