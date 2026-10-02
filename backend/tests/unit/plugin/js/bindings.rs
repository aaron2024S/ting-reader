use super::*;
use deno_core::{OpState, op2};

#[op2]
#[string]
pub(super) fn op_test_principal(state: &mut OpState) -> String {
    state
        .borrow::<JsHostInvocationContext>()
        .user
        .as_ref()
        .map(|user| user.id.clone())
        .unwrap_or_else(|| "anonymous".into())
}

#[test]
fn test_js_plugin_logger() {
    let logger = JsPluginLogger::new("test-plugin".to_string());
    logger.debug("Debug message");
    logger.info("Info message");
    logger.warn("Warning message");
    logger.error("Error message");
}

#[test]
fn test_create_js_runtime_with_bindings() {
    let config = serde_json::json!({"api_key": "test_key", "cache_enabled": true});
    let temp_dir = tempfile::tempdir().unwrap();
    let result = create_js_runtime_with_bindings(
        "test-plugin".to_string(),
        "test-plugin@1.0.0".to_string(),
        config,
        None,
        None,
        temp_dir.path().to_path_buf(),
    );
    assert!(result.is_ok());
}

#[test]
fn js_plugin_logs_keep_legacy_signature_and_accept_fields() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut runtime = create_js_runtime_with_bindings(
        "test-plugin".to_string(),
        "stable-plugin@1.0.0".to_string(),
        serde_json::json!({}),
        None,
        None,
        temp_dir.path().to_path_buf(),
    )
    .unwrap();

    let result = runtime.execute_script(
        "<plugin_log_compatibility>",
        r#"
        Ting.log.info("legacy message");
        Ting.log.warn("structured message", { code: 42 });
        console.error("console message", { retryable: false });
        "#
        .to_string()
        .into(),
    );

    assert!(result.is_ok());
}

#[test]
fn plugin_instance_id_is_split_into_stable_id_and_version() {
    assert_eq!(
        split_plugin_instance_id("demo-plugin@1.2.3"),
        ("demo-plugin".to_string(), "1.2.3".to_string())
    );
    assert_eq!(
        split_plugin_instance_id("demo-plugin"),
        ("demo-plugin".to_string(), "unknown".to_string())
    );
}

#[tokio::test]
async fn ting_host_invoke_rejects_when_gateway_missing() {
    let config = serde_json::json!({});
    let temp_dir = tempfile::tempdir().unwrap();
    let mut runtime = create_js_runtime_with_bindings(
        "test-plugin".to_string(),
        "test-plugin@1.0.0".to_string(),
        config,
        None,
        None,
        temp_dir.path().to_path_buf(),
    )
    .unwrap();

    let result = runtime.execute_script(
        "<host_invoke_without_gateway>",
        r#"
        globalThis.__hostInvokeStatus = "pending";
        Ting.host.invoke("books.list", {})
            .then(() => { globalThis.__hostInvokeStatus = "success"; })
            .catch((error) => { globalThis.__hostInvokeStatus = String(error); });
        "#
        .to_string()
        .into(),
    );
    assert!(result.is_ok());

    runtime.run_event_loop(Default::default()).await.unwrap();

    let scope = &mut runtime.handle_scope();
    let context = scope.get_current_context();
    let global = context.global(scope);
    let key = deno_core::v8::String::new(scope, "__hostInvokeStatus").unwrap();
    let value = global.get(scope, key.into()).unwrap();
    let status = value.to_string(scope).unwrap().to_rust_string_lossy(scope);

    assert!(status.contains("Ting.host.invoke is not configured"));
}

#[test]
fn test_js_runtime_sandbox_file_paths() {
    use crate::plugin::sandbox::{Permission, ResourceLimits, Sandbox};

    let config = serde_json::json!({});
    let permissions = vec![
        Permission::FileRead {
            path: "./data/cache".into(),
        },
        Permission::FileWrite {
            path: "./data/output".into(),
        },
    ];
    let sandbox = Sandbox::new(permissions, ResourceLimits::default());

    let mut runtime = create_js_runtime_with_bindings(
        "test-plugin".to_string(),
        "test-plugin@1.0.0".to_string(),
        config,
        Some(&sandbox),
        None,
        tempfile::tempdir().unwrap().path().to_path_buf(),
    )
    .unwrap();

    let test_code = r#"
        const allowedPaths = Ting.sandbox.allowedPaths;
        JSON.stringify({ allowedPaths })
    "#;
    let result = runtime.execute_script("<test_sandbox>", test_code.to_string().into());
    assert!(result.is_ok());
}

mod initialization {
    use crate::plugin::js::init_code::generate_init_code;

    #[test]
    fn generated_ting_host_get_context_is_scoped_to_invocation_args() {
        let code = generate_init_code("test-plugin", &serde_json::json!({}), &[], &[]);

        assert!(code.contains("getContext: () => globalThis._ting_context || null"));
        assert!(code.contains("args._context || null"));
        assert!(code.contains("globalThis._ting_context = undefined"));
    }

    #[test]
    fn generated_ting_host_invoke_calls_host_op() {
        let code = generate_init_code("test-plugin", &serde_json::json!({}), &[], &[]);

        assert!(code.contains("op_host_invoke(method, params ?? {})"));
        assert!(code.contains("Ting.host.invoke requires a method string"));
    }

    #[test]
    fn generated_logging_uses_host_op_and_keeps_legacy_calls() {
        let code = generate_init_code("test-plugin", &serde_json::json!({}), &[], &[]);

        assert!(code.contains("op_plugin_log"));
        assert!(code.contains("info: (message, fields)"));
        assert!(code.contains("globalThis.console.log = (...args)"));
        assert!(!code.contains("[INFO] [test-plugin]"));
    }

    #[test]
    fn generated_events_use_host_gateway_without_fake_subscriptions() {
        let code = generate_init_code("test-plugin", &serde_json::json!({}), &[], &[]);

        assert!(code.contains("Ting.host.invoke(\"events.publish\""));
        assert!(code.contains("Dynamic event subscriptions are unavailable"));
        assert!(!code.contains("sub_test-plugin_"));
    }
}
