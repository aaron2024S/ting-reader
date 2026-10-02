//! JavaScript Plugin Bindings
//!
//! This module provides the bridge between Rust and JavaScript for plugin functionality.
//! It implements:
//! - Rust function exports (logging, config, events) for JavaScript to call
//! - Data type conversion between Rust and JavaScript
//! - Async function support (Promise ↔ Future)

use anyhow::{Context, Result};
use deno_core::v8;
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
    pub cancellation: Option<tokio_util::sync::CancellationToken>,
}

// Throw with V8's native API. deno_core 0.256's Rust Result converter calls a
// JavaScript error constructor, which cannot execute during isolate termination.
fn native_op_error<'a>(
    scope: &mut v8::HandleScope<'a>,
    error: anyhow::Error,
) -> v8::Local<'a, v8::Value> {
    if scope.is_execution_terminating() {
        return v8::undefined(scope).into();
    }
    let Some(message) = v8::String::new(scope, &error.to_string()) else {
        return v8::undefined(scope).into();
    };
    let exception = v8::Exception::error(scope, message);
    scope.throw_exception(exception)
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
    sandbox: Option<&crate::plugin::sandbox::Sandbox>,
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
    pub fn op_plugin_log<'a>(
        v8_scope: &mut v8::HandleScope<'a>,
        state: Rc<RefCell<OpState>>,
        #[string] level: String,
        #[string] message: String,
        #[serde] fields: Option<Value>,
    ) -> v8::Local<'a, v8::Value> {
        let result = (|| -> Result<String> {
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
        })();
        match result {
            Ok(id) => v8::String::new(v8_scope, &id)
                .map(Into::into)
                .unwrap_or_else(|| v8::undefined(v8_scope).into()),
            Err(error) => native_op_error(v8_scope, error),
        }
    }

    #[op2(async)]
    #[serde]
    pub async fn op_host_invoke(
        state: Rc<RefCell<OpState>>,
        #[string] method: String,
        #[serde] params: serde_json::Value,
    ) -> Result<serde_json::Value, anyhow::Error> {
        let (plugin_id, host_gateway, user, resources, cancellation) = {
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
                    invocation_context.cancellation,
                ),
                None => (
                    String::new(),
                    None,
                    invocation_context.user,
                    invocation_context.resources,
                    invocation_context.cancellation,
                ),
            }
        };

        let invoke = async {
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
        };
        if let Some(cancellation) = cancellation {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => anyhow::bail!("JS invocation cancelled"),
                result = invoke => result,
            }
        } else {
            invoke.await
        }
    }

    #[op2]
    #[string]
    fn op_decode_utf8(#[buffer] bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[op2]
    fn op_chunk_create<'a>(
        v8_scope: &mut v8::HandleScope<'a>,
        state: &mut OpState,
        #[buffer] bytes: &[u8],
    ) -> v8::Local<'a, v8::Value> {
        let result = (|| -> Result<String> {
            let scope = state
                .borrow::<JsHostInvocationContext>()
                .resources
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("No resource scope for this call"))?;
            Ok(scope.create_chunk(bytes)?.0)
        })();
        match result {
            Ok(id) => v8::String::new(v8_scope, &id)
                .map(Into::into)
                .unwrap_or_else(|| v8::undefined(v8_scope).into()),
            Err(error) => native_op_error(v8_scope, error),
        }
    }

    #[op2]
    fn op_chunk_copy<'a>(
        v8_scope: &mut v8::HandleScope<'a>,
        state: &mut OpState,
        #[string] id: String,
    ) -> v8::Local<'a, v8::Value> {
        let result = (|| -> Result<v8::Local<'a, v8::Uint8Array>> {
            let scope = state
                .borrow::<JsHostInvocationContext>()
                .resources
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("No resource scope for this call"))?;
            let chunk = scope.chunk(&ting_plugin_contract::format_calls::ChunkRef(id))?;
            state
                .borrow::<super::limits::JsBudget>()
                .copy_chunk(v8_scope, &chunk)
        })();
        match result {
            Ok(view) => view.into(),
            Err(error) => native_op_error(v8_scope, error),
        }
    }

    #[op2]
    fn op_resource_write<'a>(
        v8_scope: &mut v8::HandleScope<'a>,
        state: &mut OpState,
        #[string] id: String,
        offset: f64,
        #[buffer] bytes: &[u8],
    ) -> v8::Local<'a, v8::Value> {
        let result = (|| -> Result<u32> {
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
        })();
        match result {
            Ok(bytes) => v8::Integer::new_from_unsigned(v8_scope, bytes).into(),
            Err(error) => native_op_error(v8_scope, error),
        }
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
            tests::op_test_principal::DECL,
        ]),
        ..Default::default()
    };

    let memory_limit = sandbox
        .map(|sandbox| sandbox.resource_limits.max_memory_bytes)
        .unwrap_or_else(|| crate::plugin::sandbox::ResourceLimits::default().max_memory_bytes);
    let budget = super::limits::JsBudget::new(memory_limit)?;
    let mut runtime = JsRuntime::new(RuntimeOptions {
        create_params: Some(budget.create_params(memory_limit)),
        extensions: vec![ext],
        module_loader: Some(Rc::new(super::module_loader::PackageModuleLoader::new(
            &plugin_dir,
        )?)),
        ..Default::default()
    });
    budget.attach(&mut runtime);
    runtime.op_state().borrow_mut().put(budget);
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

    // deno_core also installs built-in ops (including Vec-backed encoding and
    // serialization). They are runtime internals, not plugin Host APIs. Expose
    // only the bounded transport ops; core's event loop retains its own closure
    // references to the original internal object.
    #[allow(unused_mut)]
    let mut allowed_ops = vec![
        "op_plugin_log",
        "op_decode_utf8",
        "op_host_invoke",
        "op_chunk_create",
        "op_chunk_copy",
        "op_resource_write",
    ];
    #[cfg(test)]
    allowed_ops.push("op_test_principal");
    runtime.execute_script(
        "<restrict_plugin_globals>",
        format!(
            r#"
        (() => {{
            const ops = Object.create(null);
            for (const name of {allowed}) ops[name] = Deno.core.ops[name];
            Object.defineProperty(globalThis, "Deno", {{
                value: Object.freeze({{core: Object.freeze({{ops: Object.freeze(ops)}})}}),
                writable: false, configurable: false,
            }});
            // JS plugins cannot create an unbudgeted second WASM runtime.
            // WASM plugins use the separate, limited Wasmtime adapter.
            Object.defineProperty(globalThis, "WebAssembly", {{
                value: undefined, writable: false, configurable: false,
            }});
            delete globalThis.__bootstrap;
        }})();
    "#,
            allowed = serde_json::to_string(&allowed_ops)?
        )
        .into(),
    )?;

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
#[path = "../../../tests/unit/plugin/js/bindings.rs"]
mod tests;
