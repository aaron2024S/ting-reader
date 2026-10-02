//! JavaScript Runtime Module
//!
//! This module provides JavaScript plugin execution using Deno Core.
//! It allows loading and executing JavaScript plugins with proper error handling
//! and sandboxing.

use anyhow::{Context, Result};
use deno_core::{JsRuntime, ModuleSpecifier, v8};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{debug, info};

use super::super::types::PluginMetadata;
use super::bindings::{JsHostInvocationContext, create_js_runtime_with_bindings};
use super::limits::{ExecutionMonitor, JsBudget};
use crate::plugin::PluginHostGatewayHandle;
use crate::plugin::sandbox::{ResourceLimits, Sandbox};

/// JavaScript Runtime wrapper for executing JavaScript plugins
pub struct JsRuntimeWrapper {
    // Stop and join the watchdog before dropping V8 and its allocator.
    monitor: ExecutionMonitor,
    /// The Deno Core runtime instance
    runtime: JsRuntime,
    /// Path to the plugin file
    plugin_path: PathBuf,
    /// Plugin metadata
    metadata: PluginMetadata,
    /// Security sandbox
    sandbox: Sandbox,
    budget: JsBudget,
    /// Execution start time (for CPU time tracking)
    execution_start: Option<Instant>,
}

impl JsRuntimeWrapper {
    /// Create a new JavaScript runtime for a plugin
    ///
    /// # Arguments
    /// * `plugin_path` - Path to the JavaScript plugin file
    /// * `metadata` - Plugin metadata
    /// * `config` - Plugin configuration (optional)
    ///
    /// # Returns
    /// A new JsRuntimeWrapper instance
    pub fn new(
        plugin_path: PathBuf,
        metadata: PluginMetadata,
        config: Option<Value>,
    ) -> Result<Self> {
        Self::new_with_host_gateway(plugin_path, metadata, config, None)
    }

    pub fn new_with_host_gateway(
        plugin_path: PathBuf,
        metadata: PluginMetadata,
        config: Option<Value>,
        host_gateway: Option<PluginHostGatewayHandle>,
    ) -> Result<Self> {
        Self::new_with_limits(
            plugin_path,
            metadata,
            config,
            host_gateway,
            ResourceLimits::default(),
        )
    }

    pub(super) fn new_with_limits(
        plugin_path: PathBuf,
        metadata: PluginMetadata,
        config: Option<Value>,
        host_gateway: Option<PluginHostGatewayHandle>,
        resource_limits: ResourceLimits,
    ) -> Result<Self> {
        debug!("Creating JavaScript runtime for plugin: {}", metadata.name);

        if resource_limits.max_cpu_time.is_zero() {
            anyhow::bail!("JS execution budget must be greater than zero");
        }
        let sandbox = Sandbox::new(metadata.permissions.clone(), resource_limits);

        // Create runtime with plugin bindings and sandbox
        let config = config.unwrap_or(Value::Object(serde_json::Map::new()));
        let mut runtime = create_js_runtime_with_bindings(
            metadata.name.clone(),
            metadata.instance_id(),
            config,
            Some(&sandbox),
            host_gateway,
            plugin_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
        )?;
        let budget = runtime.op_state().borrow().borrow::<JsBudget>().clone();
        let monitor = ExecutionMonitor::new(budget.clone())?;

        Ok(Self {
            monitor,
            runtime,
            plugin_path,
            metadata,
            sandbox,
            budget,
            execution_start: None,
        })
    }

    /// Load and initialize the JavaScript module
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn load_module(&mut self) -> Result<()> {
        let timeout = self
            .sandbox
            .resource_limits
            .max_cpu_time
            .min(Duration::from_secs(30));
        let cleanup = InvocationCleanup {
            state: self.runtime.op_state(),
            budget: self.budget.clone(),
            resources: None,
        };
        let lease = self.monitor.begin(timeout)?;
        self.set_host_invocation_context(JsHostInvocationContext {
            cancellation: Some(lease.cancellation.clone()),
            ..Default::default()
        });
        let result = tokio::time::timeout(timeout, self.load_module_inner()).await;
        if result.is_err() {
            self.monitor.time_out();
        }
        lease.finish();
        drop(cleanup);
        self.budget.check()?;
        result.context("JS module load exceeded its deadline")?
    }

    async fn load_module_inner(&mut self) -> Result<()> {
        info!(
            "Loading JavaScript module from: {}",
            self.plugin_path.display()
        );

        let entry_path = std::fs::canonicalize(&self.plugin_path).with_context(|| {
            format!(
                "plugin entry file does not exist: {}",
                self.plugin_path.display()
            )
        })?;
        let specifier = ModuleSpecifier::from_file_path(&entry_path)
            .map_err(|_| anyhow::anyhow!("invalid plugin entry path"))?;
        let module_id = self
            .runtime
            .load_main_module(&specifier, None)
            .await
            .with_context(|| {
                format!("Failed to load plugin ESM: {}", self.plugin_path.display())
            })?;
        let evaluation = self.runtime.mod_evaluate(module_id);
        self.runtime
            .run_event_loop(Default::default())
            .await
            .context("Failed to evaluate plugin module")?;
        evaluation
            .await
            .context("Plugin module evaluation failed")?;

        // Publish only the declared entry points and optional lifecycle hooks
        // to the existing JS invocation bridge.
        let namespace = self.runtime.get_module_namespace(module_id)?;
        let mut required: Vec<_> = self
            .metadata
            .capabilities
            .iter()
            .flat_map(|capability| capability.required_exports())
            .collect();
        required.sort_unstable();
        required.dedup();
        let scope = &mut self.runtime.handle_scope();
        let exports = v8::Local::<v8::Object>::new(scope, namespace);
        let global = scope.get_current_context().global(scope);
        let mut missing = Vec::new();
        for name in required
            .iter()
            .copied()
            .chain(["initialize", "shutdown", "garbage_collect"])
        {
            let key = v8::String::new(scope, name)
                .ok_or_else(|| anyhow::anyhow!("Invalid plugin export name"))?;
            let function = exports.get(scope, key.into());
            if let Some(function) = function.filter(|value| value.is_function()) {
                global.set(scope, key.into(), function);
            } else if required.contains(&name) {
                missing.push(name);
            }
        }
        if !missing.is_empty() {
            anyhow::bail!("Missing declared plugin exports: {}", missing.join(", "));
        }

        info!("JavaScript module loaded successfully");
        Ok(())
    }

    /// Execute a JavaScript function with arguments
    ///
    /// # Arguments
    /// * `function_name` - Name of the function to call
    /// * `args` - JSON-serializable arguments
    ///
    /// # Returns
    /// Result containing the function's return value as JSON
    pub async fn call_function<T, R>(
        &mut self,
        function_name: &str,
        args: T,
        context: &crate::plugin::types::PluginInvocationContext,
    ) -> Result<R>
    where
        T: Serialize,
        R: for<'de> Deserialize<'de>,
    {
        let timeout = self.sandbox.resource_limits.max_cpu_time;
        let cleanup = InvocationCleanup {
            state: self.runtime.op_state(),
            budget: self.budget.clone(),
            resources: context.resources.clone(),
        };
        let lease = self.monitor.begin(timeout)?;
        let result = tokio::time::timeout(
            timeout,
            self.call_function_inner(function_name, args, context, lease.cancellation.clone()),
        )
        .await;
        if result.is_err() {
            self.monitor.time_out();
        }
        if !self.budget.unavailable() {
            let _ = self.runtime.execute_script(
                "<cleanup>",
                "globalThis._ting_result = undefined; globalThis._ting_error = undefined; globalThis._ting_status = undefined;".to_string().into(),
            );
        }
        lease.finish();
        drop(cleanup);
        self.stop_execution();
        self.budget.check()?;
        result.context("JS invocation exceeded its deadline")?
    }

    async fn call_function_inner<T, R>(
        &mut self,
        function_name: &str,
        args: T,
        context: &crate::plugin::types::PluginInvocationContext,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<R>
    where
        T: Serialize,
        R: for<'de> Deserialize<'de>,
    {
        debug!("Calling JavaScript function: {}", function_name);

        // Start tracking execution time
        self.start_execution();

        // Identity travels separately from plugin-owned JSON.
        let args_value = serde_json::to_value(&args).context("Failed to serialize arguments")?;
        let host_context = JsHostInvocationContext {
            user: context.user.clone(),
            resources: context.resources.clone(),
            cancellation: Some(cancellation),
        };
        let args_json =
            serde_json::to_string(&args_value).context("Failed to serialize function arguments")?;
        self.set_host_invocation_context(host_context);

        // Call _ting_invoke using V8 API to avoid compiling new scripts for arguments
        let call_result = (|| -> Result<()> {
            let scope = &mut self.runtime.handle_scope();
            let context = scope.get_current_context();
            let global = context.global(scope);

            // Get _ting_invoke function
            let invoke_name = v8::String::new(scope, "_ting_invoke").unwrap();
            let invoke_val = match global.get(scope, invoke_name.into()) {
                Some(value) => value,
                None => return Err(anyhow::anyhow!("_ting_invoke not found")),
            };
            let invoke_func = match v8::Local::<v8::Function>::try_from(invoke_val) {
                Ok(value) => value,
                Err(_) => return Err(anyhow::anyhow!("_ting_invoke is not a function")),
            };

            // Prepare arguments: [function_name, args_value]
            let func_name_v8 = v8::String::new(scope, function_name).unwrap();

            // Parse args JSON to V8 value
            let args_json_v8 = v8::String::new(scope, &args_json).unwrap();
            let args_val = match v8::json::parse(scope, args_json_v8) {
                Some(value) => value,
                None => return Err(anyhow::anyhow!("Failed to parse arguments JSON in V8")),
            };

            let recv = v8::undefined(scope).into();
            let args = [func_name_v8.into(), args_val];

            // Call _ting_invoke
            if invoke_func.call(scope, recv, &args).is_none() {
                Err(anyhow::anyhow!("Failed to call _ting_invoke"))
            } else {
                Ok(())
            }
        })();
        if let Err(error) = call_result {
            self.clear_host_invocation_context();
            self.stop_execution();
            return Err(error);
        }

        // Drive the event loop until completion
        if let Err(error) = self
            .runtime
            .run_event_loop(Default::default())
            .await
            .context("Failed to run event loop")
        {
            self.clear_host_invocation_context();
            self.stop_execution();
            return Err(error);
        }

        let invocation_result = (|| -> Result<(String, std::result::Result<String, String>)> {
            let scope = &mut self.runtime.handle_scope();
            let context = scope.get_current_context();
            let global = context.global(scope);

            // Helper to get string from global object
            let get_global_string =
                |scope: &mut deno_core::v8::HandleScope, key: &str| -> Option<String> {
                    let key_str = deno_core::v8::String::new(scope, key)?;
                    let val = global.get(scope, key_str.into())?;
                    if val.is_undefined() || val.is_null() {
                        return None;
                    }
                    Some(val.to_string(scope)?.to_rust_string_lossy(scope))
                };

            let status = get_global_string(scope, "_ting_status")
                .ok_or_else(|| anyhow::anyhow!("Failed to retrieve execution status"))?;

            let result = match status.as_str() {
                "success" => {
                    let res = get_global_string(scope, "_ting_result").ok_or_else(|| {
                        anyhow::anyhow!("Function finished successfully but returned no result")
                    })?;
                    Ok(res)
                }
                "error" => {
                    let err = get_global_string(scope, "_ting_error")
                        .unwrap_or_else(|| "Unknown error".to_string());
                    Err(err)
                }
                "pending" => Err("Event loop finished but function is still pending".to_string()),
                s => Err(format!("Invalid execution status: {}", s)),
            };
            Ok((status, result))
        })();

        // Stop tracking execution time
        self.clear_host_invocation_context();
        self.stop_execution();
        let (status, result_or_error) = invocation_result?;

        match result_or_error {
            Ok(result_str) => {
                // Deserialize the result
                let result: R = serde_json::from_str(&result_str)
                    .context("Failed to deserialize plugin function result")?;

                debug!("Function call completed successfully");
                Ok(result)
            }
            Err(err_msg) => {
                if status == "pending" {
                    Err(anyhow::anyhow!(err_msg))
                } else if status == "error" {
                    Err(JsError::FunctionCallError(err_msg).into())
                } else {
                    Err(anyhow::anyhow!(err_msg))
                }
            }
        }
    }

    /// Execute arbitrary JavaScript code
    ///
    /// # Arguments
    /// * `code` - JavaScript code to execute
    ///
    /// # Returns
    /// Result indicating success or failure
    pub fn execute_script(&mut self, code: &str) -> Result<()> {
        debug!("Executing JavaScript code");
        let lease = self
            .monitor
            .begin(self.sandbox.resource_limits.max_cpu_time)?;
        let result = self
            .runtime
            .execute_script("<execute_script>", code.to_string().into())
            .context("Failed to execute JavaScript code");
        lease.finish();
        self.budget.check()?;
        result?;

        debug!("JavaScript code executed successfully");
        Ok(())
    }

    fn set_host_invocation_context(&mut self, context: JsHostInvocationContext) {
        self.runtime.op_state().borrow_mut().put(context);
    }

    fn clear_host_invocation_context(&mut self) {
        self.runtime
            .op_state()
            .borrow_mut()
            .put(JsHostInvocationContext::default());
    }

    /// Get the plugin metadata
    pub fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    /// Get the plugin path
    pub fn plugin_path(&self) -> &Path {
        &self.plugin_path
    }

    /// Get the always-present sandbox policy.
    /// The optional return type is retained for existing callers.
    pub fn sandbox(&self) -> Option<&Sandbox> {
        Some(&self.sandbox)
    }

    pub fn is_unavailable(&self) -> bool {
        self.budget.unavailable()
    }

    /// Start tracking execution time
    fn start_execution(&mut self) {
        self.execution_start = Some(Instant::now());
    }

    /// Check CPU time limit
    pub fn check_cpu_time_limit(&self) -> Result<()> {
        if let Some(start_time) = self.execution_start {
            let elapsed = start_time.elapsed();
            self.sandbox.check_cpu_time(elapsed)?;
        }
        Ok(())
    }

    /// Stop tracking execution time
    fn stop_execution(&mut self) {
        self.execution_start = None;
    }

    /// Check file access permission
    pub fn check_file_access(
        &self,
        path: &Path,
        access: crate::plugin::sandbox::FileAccess,
    ) -> Result<()> {
        self.sandbox.check_file_access(path, access)?;
        Ok(())
    }

    /// Check network access permission
    pub fn check_network_access(&self, url: &str) -> Result<()> {
        self.sandbox.check_network_access(url)?;
        Ok(())
    }

    /// Check memory limit
    pub fn check_memory_limit(&self, current_bytes: usize) -> Result<()> {
        self.sandbox.check_memory_limit(current_bytes)?;
        Ok(())
    }

    /// Request garbage collection
    pub fn garbage_collect(&mut self) -> Result<()> {
        self.budget.check()?;
        debug!("Requesting garbage collection");
        self.runtime.v8_isolate().low_memory_notification();
        Ok(())
    }
}

// Async cancellation must also drop the Host principal/resources held in OpState.
struct InvocationCleanup {
    state: std::rc::Rc<std::cell::RefCell<deno_core::OpState>>,
    budget: JsBudget,
    resources: Option<std::sync::Arc<crate::plugin::host_api::resources::ResourceScope>>,
}

impl Drop for InvocationCleanup {
    fn drop(&mut self) {
        if self.budget.unavailable()
            && let Some(resources) = &self.resources
        {
            resources.cancel();
        }
        self.state
            .borrow_mut()
            .put(JsHostInvocationContext::default());
    }
}

/// JavaScript plugin error wrapper
#[derive(Debug, thiserror::Error)]
pub enum JsError {
    #[error("JavaScript execution error: {0}")]
    ExecutionError(String),

    #[error("JavaScript module load error: {0}")]
    ModuleLoadError(String),

    #[error("JavaScript function call error: {0}")]
    FunctionCallError(String),

    #[error("JavaScript serialization error: {0}")]
    SerializationError(String),

    #[error("JavaScript runtime error: {0}")]
    RuntimeError(String),
}

impl From<anyhow::Error> for JsError {
    fn from(err: anyhow::Error) -> Self {
        JsError::RuntimeError(err.to_string())
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/plugin/js/runtime.rs"]
mod tests;
