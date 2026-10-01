//! JavaScript Plugin Bindings
//!
//! This module provides the bridge between Rust and JavaScript for plugin functionality.
//! It implements:
//! - Rust function exports (logging, config, events) for JavaScript to call
//! - Data type conversion between Rust and JavaScript
//! - Async function support (Promise ↔ Future)

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{debug, error, info, warn};

use super::super::types::{PluginLogContext, PluginLogSource, PluginLogger};
use crate::plugin::host_api::logger::{DefaultPluginLogger, PluginLogLevel};
use crate::plugin::{PluginHostGatewayHandle, PluginHostUser};

// ============================================================================
// Rust Functions Exported to JavaScript (Helper Functions)
// ============================================================================

/// Plugin logger implementation for JavaScript plugins
#[derive(Clone)]
pub struct JsPluginLogger {
    plugin_name: String,
}

impl JsPluginLogger {
    pub fn new(plugin_name: String) -> Self {
        Self { plugin_name }
    }
}

impl PluginLogger for JsPluginLogger {
    fn debug(&self, message: &str) {
        debug!(plugin = %self.plugin_name, "{}", message);
    }

    fn info(&self, message: &str) {
        info!(plugin = %self.plugin_name, "{}", message);
    }

    fn warn(&self, message: &str) {
        warn!(plugin = %self.plugin_name, "{}", message);
    }

    fn error(&self, message: &str) {
        error!(plugin = %self.plugin_name, "{}", message);
    }
}

#[derive(Clone)]
struct JsHostGatewayState {
    plugin_id: String,
    host_gateway: Option<PluginHostGatewayHandle>,
}

#[derive(Clone)]
struct JsPluginLogState {
    logger: DefaultPluginLogger,
}

#[derive(Clone, Default)]
pub struct JsHostInvocationContext {
    pub user: Option<PluginHostUser>,
    pub resources: Option<std::sync::Arc<crate::plugin::host_api::resources::ResourceScope>>,
}

/// Helper to create a JavaScript runtime with plugin bindings
///
/// This function creates a Deno runtime and injects the Ting API into the global scope.
/// The Ting API provides logging, configuration access, and event bus functionality.
///
/// # Arguments
/// * `plugin_name` - Name of the plugin
/// * `config` - Plugin configuration
/// * `sandbox` - Optional sandbox for permission checking
pub fn create_js_runtime_with_bindings(
    plugin_name: String,
    plugin_id: String,
    config: Value,
    sandbox: Option<&crate::plugin::wasm::sandbox::Sandbox>,
    host_gateway: Option<PluginHostGatewayHandle>,
    plugin_dir: std::path::PathBuf,
) -> Result<deno_core::JsRuntime> {
    use deno_core::{Extension, JsRuntime, Op, OpState, RuntimeOptions, op2};
    use std::cell::RefCell;
    use std::rc::Rc;

    let allowed_paths = sandbox
        .map(|s| {
            s.get_allowed_paths()
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let allowed_domains = sandbox
        .map(|s| s.get_allowed_domains().to_vec())
        .unwrap_or_default();

    #[op2]
    #[string]
    pub fn op_plugin_log(
        state: Rc<RefCell<OpState>>,
        #[string] level: String,
        #[string] message: String,
        #[serde] fields: Option<Value>,
    ) -> Result<String, anyhow::Error> {
        let level = PluginLogLevel::parse(&level)
            .ok_or_else(|| anyhow::anyhow!("Unsupported plugin log level"))?;
        let fields = match fields {
            None | Some(Value::Null) => None,
            Some(Value::Object(fields)) => Some(Value::Object(fields)),
            Some(_) => {
                return Err(anyhow::anyhow!("Plugin log fields must be a JSON object"));
            }
        };
        let logger = {
            let state = state.borrow();
            state
                .try_borrow::<JsPluginLogState>()
                .map(|log_state| log_state.logger.clone())
                .ok_or_else(|| anyhow::anyhow!("Plugin logger is not configured"))?
        };

        Ok(logger.log(level, &message, fields.as_ref()))
    }

    #[op2(async)]
    #[serde]
    pub async fn op_host_invoke(
        state: Rc<RefCell<OpState>>,
        #[string] method: String,
        #[serde] params: serde_json::Value,
    ) -> Result<serde_json::Value, anyhow::Error> {
        let (plugin_id, host_gateway, user, resources) = {
            let state = state.borrow();
            let host_state = state.try_borrow::<JsHostGatewayState>().cloned();
            let invocation_context = state
                .try_borrow::<JsHostInvocationContext>()
                .cloned()
                .unwrap_or_default();

            match host_state {
                Some(host_state) => (
                    host_state.plugin_id,
                    host_state.host_gateway.and_then(|handle| handle.get()),
                    invocation_context.user,
                    invocation_context.resources,
                ),
                None => (
                    String::new(),
                    None,
                    invocation_context.user,
                    invocation_context.resources,
                ),
            }
        };

        if method.starts_with("resources.") {
            let scope =
                resources.ok_or_else(|| anyhow::anyhow!("No resource scope for this call"))?;
            return tokio::task::spawn_blocking(move || scope.invoke(&method, params))
                .await?
                .map_err(anyhow::Error::from);
        }

        let gateway = host_gateway.ok_or_else(|| {
            anyhow::anyhow!("Ting.host.invoke is not configured for this plugin runtime")
        })?;
        crate::plugin::host_api::invoke(
            &plugin_id,
            None,
            user.as_ref(),
            resources.as_ref(),
            Some(&gateway),
            &method,
            params,
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    #[op2]
    #[string]
    fn op_decode_utf8(#[buffer] bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[op2]
    #[string]
    fn op_chunk_create(
        state: &mut OpState,
        #[buffer] bytes: &[u8],
    ) -> Result<String, anyhow::Error> {
        let scope = state
            .borrow::<JsHostInvocationContext>()
            .resources
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No resource scope for this call"))?;
        Ok(scope.create_chunk(bytes)?.0)
    }

    #[op2]
    #[buffer]
    fn op_chunk_copy(state: &mut OpState, #[string] id: String) -> Result<Vec<u8>, anyhow::Error> {
        let scope = state
            .borrow::<JsHostInvocationContext>()
            .resources
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No resource scope for this call"))?;
        Ok(scope
            .chunk(&ting_plugin_contract::format_calls::ChunkRef(id))?
            .to_vec())
    }

    #[op2(fast)]
    fn op_resource_write(
        state: &mut OpState,
        #[string] id: String,
        offset: f64,
        #[buffer] bytes: &[u8],
    ) -> Result<u32, anyhow::Error> {
        if !offset.is_finite()
            || offset < 0.0
            || offset.fract() != 0.0
            || offset > ting_plugin_contract::format_calls::MAX_SAFE_INTEGER as f64
        {
            anyhow::bail!("Invalid resource offset");
        }
        let scope = state
            .borrow::<JsHostInvocationContext>()
            .resources
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No resource scope for this call"))?;
        Ok(scope.write_at(
            &ting_plugin_contract::format_calls::ResourceId(id),
            offset as u64,
            bytes,
        )? as u32)
    }

    #[cfg(test)]
    #[op2]
    #[string]
    fn op_test_principal(state: &mut OpState) -> String {
        state
            .borrow::<JsHostInvocationContext>()
            .user
            .as_ref()
            .map(|user| user.id.clone())
            .unwrap_or_else(|| "anonymous".into())
    }

    let ext = Extension {
        name: "ting_fetch",
        ops: std::borrow::Cow::Owned(vec![
            op_plugin_log::DECL,
            op_decode_utf8::DECL,
            op_host_invoke::DECL,
            op_chunk_create::DECL,
            op_chunk_copy::DECL,
            op_resource_write::DECL,
            #[cfg(test)]
            op_test_principal::DECL,
        ]),
        ..Default::default()
    };

    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![ext],
        module_loader: Some(Rc::new(super::module_loader::PackageModuleLoader::new(
            &plugin_dir,
        )?)),
        ..Default::default()
    });
    let (stable_plugin_id, plugin_version) = split_plugin_instance_id(&plugin_id);
    runtime.op_state().borrow_mut().put(JsPluginLogState {
        logger: DefaultPluginLogger::from_context(PluginLogContext {
            plugin_id: stable_plugin_id,
            plugin_instance_id: plugin_id.clone(),
            plugin_name: plugin_name.clone(),
            plugin_version,
            runtime: "javascript".to_string(),
            source: PluginLogSource::Code,
        }),
    });
    runtime.op_state().borrow_mut().put(JsHostGatewayState {
        plugin_id: plugin_id.clone(),
        host_gateway,
    });
    runtime
        .op_state()
        .borrow_mut()
        .put(JsHostInvocationContext::default());

    let init_code = super::init_code::generate_init_code(
        &plugin_name,
        &config,
        &allowed_paths,
        &allowed_domains,
    );

    runtime
        .execute_script("<init_bindings>", init_code.into())
        .context("Failed to initialize JavaScript bindings")?;

    Ok(runtime)
}

fn split_plugin_instance_id(instance_id: &str) -> (String, String) {
    match instance_id.rsplit_once('@') {
        Some((plugin_id, version)) if !plugin_id.is_empty() && !version.is_empty() => {
            (plugin_id.to_string(), version.to_string())
        }
        _ => (instance_id.to_string(), "unknown".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use crate::plugin::wasm::sandbox::{Permission, ResourceLimits, Sandbox};

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
}
