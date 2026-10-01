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
use std::time::Instant;
use tracing::{debug, info};

use super::super::types::PluginMetadata;
use super::super::wasm::sandbox::{ResourceLimits, Sandbox};
use super::bindings::{JsHostInvocationContext, create_js_runtime_with_bindings};
use crate::plugin::PluginHostGatewayHandle;

/// JavaScript Runtime wrapper for executing JavaScript plugins
pub struct JsRuntimeWrapper {
    /// The Deno Core runtime instance
    runtime: JsRuntime,
    /// Path to the plugin file
    plugin_path: PathBuf,
    /// Plugin metadata
    metadata: PluginMetadata,
    /// Security sandbox
    sandbox: Option<Sandbox>,
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
        debug!("Creating JavaScript runtime for plugin: {}", metadata.name);

        // Create sandbox from plugin permissions
        let sandbox = if !metadata.permissions.is_empty() {
            let resource_limits = ResourceLimits::default();
            Some(Sandbox::new(metadata.permissions.clone(), resource_limits))
        } else {
            None
        };

        // Create runtime with plugin bindings and sandbox
        let config = config.unwrap_or(Value::Object(serde_json::Map::new()));
        let runtime = create_js_runtime_with_bindings(
            metadata.name.clone(),
            metadata.instance_id(),
            config,
            sandbox.as_ref(),
            host_gateway,
            plugin_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
        )?;

        Ok(Self {
            runtime,
            plugin_path,
            metadata,
            sandbox,
            execution_start: None,
        })
    }

    /// Load and initialize the JavaScript module
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn load_module(&mut self) -> Result<()> {
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
        debug!("Calling JavaScript function: {}", function_name);

        // Start tracking execution time
        self.start_execution();

        // Identity travels separately from plugin-owned JSON.
        let args_value = serde_json::to_value(&args).context("Failed to serialize arguments")?;
        let host_context = JsHostInvocationContext {
            user: context.user.clone(),
            resources: context.resources.clone(),
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

        // Cleanup global variables to free memory
        // This is crucial to prevent memory leaks as _ting_result can hold large JSON strings
        let _ = self.runtime.execute_script(
            "<cleanup>",
            r#"
            globalThis._ting_result = undefined;
            globalThis._ting_error = undefined;
            globalThis._ting_status = undefined;
            "#
            .to_string()
            .into(),
        );

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

        self.runtime
            .execute_script("<execute_script>", code.to_string().into())
            .context("Failed to execute JavaScript code")?;

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

    /// Get the sandbox (if any)
    pub fn sandbox(&self) -> Option<&Sandbox> {
        self.sandbox.as_ref()
    }

    /// Start tracking execution time
    fn start_execution(&mut self) {
        self.execution_start = Some(Instant::now());
    }

    /// Check CPU time limit
    pub fn check_cpu_time_limit(&self) -> Result<()> {
        if let (Some(sandbox), Some(start_time)) = (&self.sandbox, self.execution_start) {
            let elapsed = start_time.elapsed();
            sandbox.check_cpu_time(elapsed)?;
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
        access: super::super::wasm::sandbox::FileAccess,
    ) -> Result<()> {
        if let Some(sandbox) = &self.sandbox {
            sandbox.check_file_access(path, access)?;
        }
        Ok(())
    }

    /// Check network access permission
    pub fn check_network_access(&self, url: &str) -> Result<()> {
        if let Some(sandbox) = &self.sandbox {
            sandbox.check_network_access(url)?;
        }
        Ok(())
    }

    /// Check memory limit
    pub fn check_memory_limit(&self, current_bytes: usize) -> Result<()> {
        if let Some(sandbox) = &self.sandbox {
            sandbox.check_memory_limit(current_bytes)?;
        }
        Ok(())
    }

    /// Request garbage collection
    pub fn garbage_collect(&mut self) -> Result<()> {
        debug!("Requesting garbage collection");
        self.runtime.v8_isolate().low_memory_notification();
        Ok(())
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
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

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
        use super::super::super::wasm::sandbox::Permission;

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
        let runtime =
            JsRuntimeWrapper::new(temp_file.path().to_path_buf(), metadata, None).unwrap();

        // Check allowed URL
        let result = runtime.check_network_access("https://api.example.com/data");
        assert!(result.is_ok());

        // Check disallowed URL
        let result = runtime.check_network_access("https://evil.com/data");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_sandbox_file_access_check() {
        use super::super::super::wasm::sandbox::{FileAccess, Permission};
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
        let runtime =
            JsRuntimeWrapper::new(temp_file.path().to_path_buf(), metadata, None).unwrap();

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
        use super::super::super::wasm::sandbox::Permission;

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
        let runtime =
            JsRuntimeWrapper::new(temp_file.path().to_path_buf(), metadata, None).unwrap();

        // Check within limit
        let result = runtime.check_memory_limit(100 * 1024 * 1024); // 100 MB
        assert!(result.is_ok());

        // Check exceeding limit
        let result = runtime.check_memory_limit(1024 * 1024 * 1024); // 1 GB (exceeds default 512 MB)
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_sandbox_cpu_time_tracking() {
        use super::super::super::wasm::sandbox::Permission;
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
}
