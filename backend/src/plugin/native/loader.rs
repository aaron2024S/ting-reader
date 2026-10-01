//! Native dynamic library loader
//!
//! This module provides functionality for loading and managing native dynamic libraries
//! (.dll on Windows, .so on Linux, .dylib on macOS) as plugins.
//!
//! The NativeLoader handles:
//! - Cross-platform dynamic library loading
//! - FFI function symbol lookup and calling
//! - Safe library unloading and resource cleanup
//! - Thread-safe library management

use crate::core::error::{Result, TingError};
use crate::plugin::native::host_api;
use crate::plugin::types::{PluginId, PluginMetadata};
use crate::plugin::wasm::ResourceLimits;
use libloading::{Library, Symbol};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use ting_plugin_contract::native_abi::{
    MAX_NATIVE_CONTROL_BYTES, NATIVE_ABI_REVISION, NATIVE_BOOTSTRAP_SYMBOL, NativeAbiHeader,
    NativeCallOutput, NativeHostAllocator, NativePluginAbiV2, NativeTargetInfo,
};

/// Native plugin loader
///
/// Manages the loading, execution, and unloading of native dynamic libraries.
/// Provides thread-safe access to loaded libraries and their exported functions.
/// Includes safety wrappers for timeout control, error handling, and resource monitoring.
pub struct NativeLoader {
    /// Map of plugin ID to loaded library
    libraries: RwLock<HashMap<PluginId, Arc<LoadedLibrary>>>,

    /// Default resource limits for native plugins
    default_limits: ResourceLimits,
}

/// A loaded native library with its metadata
struct LoadedLibrary {
    /// The loaded library handle
    _library: Library,
    abi: NativePluginAbiV2,
    instance: usize,
    call_lock: Mutex<()>,
    /// A timed out in-process call cannot be killed; never dispatch to it again.
    unavailable: Arc<AtomicBool>,

    /// Path to the library file
    path: PathBuf,

    /// Plugin metadata
    metadata: PluginMetadata,

    /// Reference count for safe unloading
    ref_count: AtomicUsize,

    /// Resource limits for this plugin
    resource_limits: ResourceLimits,

    /// Resource usage statistics
    stats: Mutex<ResourceStats>,
}

#[derive(Default)]
struct HostAllocation {
    buffer: Option<Box<[u8]>>,
    limit: usize,
}

unsafe extern "C" fn allocate_native_output(context: *mut c_void, size: usize) -> *mut u8 {
    if context.is_null() || size == 0 || size > MAX_NATIVE_CONTROL_BYTES {
        return std::ptr::null_mut();
    }
    let allocation = unsafe { &mut *context.cast::<HostAllocation>() };
    if allocation.buffer.is_some() || size > allocation.limit {
        return std::ptr::null_mut();
    }
    let mut buffer = vec![0; size].into_boxed_slice();
    let pointer = buffer.as_mut_ptr();
    allocation.buffer = Some(buffer);
    pointer
}

impl Drop for LoadedLibrary {
    fn drop(&mut self) {
        // This runs while the library is still held by this struct.
        unsafe { (self.abi.destroy)(self.instance as *mut c_void) };
    }
}

/// Resource usage statistics for a native plugin
#[derive(Debug, Clone, Default)]
pub struct ResourceStats {
    /// Total number of calls
    pub total_calls: u64,

    /// Number of successful calls
    pub successful_calls: u64,

    /// Number of failed calls
    pub failed_calls: u64,

    /// Number of timeout errors
    pub timeout_errors: u64,

    /// Peak Host-allocated control output bytes. Private plugin heap is not included.
    pub peak_memory_bytes: usize,

    /// Total CPU time spent
    pub total_cpu_time: Duration,

    /// Last execution time
    pub last_execution_time: Option<Duration>,
}

impl NativeLoader {
    /// Create a new native loader with default resource limits
    pub fn new() -> Self {
        Self::with_limits(ResourceLimits::default())
    }

    /// Create a new native loader with custom resource limits
    pub fn with_limits(default_limits: ResourceLimits) -> Self {
        Self {
            libraries: RwLock::new(HashMap::new()),
            default_limits,
        }
    }

    /// Load a native library from the given path
    ///
    /// # Arguments
    /// * `plugin_id` - Unique identifier for this plugin instance
    /// * `path` - Path to the dynamic library file
    /// * `metadata` - Plugin metadata
    ///
    /// # Returns
    /// * `Ok(())` if the library was loaded successfully
    /// * `Err(TingError)` if loading failed
    ///
    /// # Safety
    /// Loading native libraries is inherently unsafe as it executes arbitrary code.
    /// The caller must ensure the library is from a trusted source.
    pub fn load_library(
        &self,
        plugin_id: PluginId,
        path: &Path,
        metadata: PluginMetadata,
    ) -> Result<()> {
        self.load_library_with_limits(plugin_id, path, metadata, self.default_limits.clone())
    }

    /// Load a native library with custom resource limits
    ///
    /// # Arguments
    /// * `plugin_id` - Unique identifier for this plugin instance
    /// * `path` - Path to the dynamic library file
    /// * `metadata` - Plugin metadata
    /// * `resource_limits` - Custom resource limits for this plugin
    ///
    /// # Returns
    /// * `Ok(())` if the library was loaded successfully
    /// * `Err(TingError)` if loading failed
    pub fn load_library_with_limits(
        &self,
        plugin_id: PluginId,
        path: &Path,
        metadata: PluginMetadata,
        resource_limits: ResourceLimits,
    ) -> Result<()> {
        // Validate the file exists
        if !path.exists() {
            return Err(TingError::PluginLoadError(format!(
                "Library file not found: {:?}",
                path
            )));
        }

        // Validate file extension matches platform
        if !self.is_valid_library_extension(path) {
            return Err(TingError::PluginLoadError(format!(
                "Invalid library file extension: {:?}",
                path
            )));
        }

        // Load the library
        let library = unsafe {
            Library::new(path).map_err(|e| {
                TingError::PluginLoadError(format!("Failed to load library {:?}: {}", path, e))
            })?
        };
        let abi = Self::load_abi(&library, &plugin_id)?;
        let instance = unsafe { (abi.create)() };
        if instance.is_null() {
            return Err(TingError::PluginLoadError(format!(
                "Native plugin {} failed to create instance",
                plugin_id
            )));
        }
        if let Err(error) = validate_native_exports(&abi, instance, &metadata) {
            unsafe { (abi.destroy)(instance) };
            return Err(error);
        }

        tracing::info!(
            plugin_id = %plugin_id,
            path = ?path,
            max_memory = resource_limits.max_memory_bytes,
            max_cpu_time = ?resource_limits.max_cpu_time,
            "Native library loaded successfully with resource limits applied"
        );

        // Store the loaded library
        let loaded = LoadedLibrary {
            _library: library,
            abi,
            instance: instance as usize,
            call_lock: Mutex::new(()),
            unavailable: Arc::new(AtomicBool::new(false)),
            path: path.to_path_buf(),
            metadata,
            ref_count: AtomicUsize::new(1),
            resource_limits,
            stats: Mutex::new(ResourceStats::default()),
        };

        let mut libraries = self.libraries.write().map_err(|e| {
            TingError::PluginLoadError(format!("Failed to acquire write lock: {}", e))
        })?;

        if libraries.contains_key(&plugin_id) {
            return Err(TingError::PluginLoadError(format!(
                "Native plugin {} is already loaded",
                plugin_id
            )));
        }
        libraries.insert(plugin_id, Arc::new(loaded));

        Ok(())
    }

    fn load_abi(library: &Library, plugin_id: &PluginId) -> Result<NativePluginAbiV2> {
        type Bootstrap = unsafe extern "C" fn() -> *const NativeAbiHeader;
        let bootstrap: Symbol<Bootstrap> = unsafe {
            library.get(NATIVE_BOOTSTRAP_SYMBOL).map_err(|error| {
                TingError::PluginLoadError(format!(
                    "Native plugin {} lacks ABI v2 bootstrap: {}",
                    plugin_id, error
                ))
            })?
        };
        let header = unsafe { bootstrap() };
        if header.is_null() {
            return Err(TingError::PluginLoadError(format!(
                "Native plugin {} returned a null ABI header",
                plugin_id
            )));
        }
        let header_value = unsafe { std::ptr::read(header) };
        if header_value.abi_revision != NATIVE_ABI_REVISION
            || header_value.struct_size as usize != std::mem::size_of::<NativePluginAbiV2>()
        {
            return Err(TingError::PluginLoadError(format!(
                "Native plugin {} has unsupported ABI revision {} or struct size {}",
                plugin_id, header_value.abi_revision, header_value.struct_size
            )));
        }
        let abi = unsafe { std::ptr::read(header.cast::<NativePluginAbiV2>()) };
        if abi.target != NativeTargetInfo::CURRENT {
            return Err(TingError::PluginLoadError(format!(
                "Native plugin {plugin_id} target layout does not match the Host"
            )));
        }
        Ok(abi)
    }

    /// Unload a native library
    ///
    /// # Arguments
    /// * `plugin_id` - ID of the plugin to unload
    ///
    /// # Returns
    /// * `Ok(())` if the library was unloaded successfully
    /// * `Err(TingError)` if unloading failed or the library is still in use
    pub fn unload_library(&self, plugin_id: &PluginId) -> Result<()> {
        let mut libraries = self.libraries.write().map_err(|e| {
            TingError::PluginLoadError(format!("Failed to acquire write lock: {}", e))
        })?;

        let loaded = libraries
            .get(plugin_id)
            .ok_or_else(|| TingError::PluginNotFound(format!("Plugin {} not found", plugin_id)))?;

        let ref_count = loaded.ref_count.load(Ordering::Acquire);
        if ref_count == 0 {
            return Err(TingError::PluginLoadError(format!(
                "Native plugin {plugin_id} was already unloaded"
            )));
        }
        loaded.ref_count.fetch_sub(1, Ordering::AcqRel);

        if ref_count == 1 {
            let path = loaded.path.clone();
            libraries.remove(plugin_id);

            tracing::info!(
                plugin_id = %plugin_id,
                path = ?path,
                "Native library unloaded"
            );
        } else {
            tracing::debug!(
                plugin_id = %plugin_id,
                ref_count = ref_count - 1,
                "Library still in use, not unloading"
            );
        }

        Ok(())
    }

    /// Call a function in a loaded library with safety wrappers
    ///
    /// A timed-out in-process call keeps the library mapped until its worker
    /// finishes. Subsequent calls to this instance are rejected.
    ///
    /// # Arguments
    /// * `plugin_id` - ID of the plugin
    /// * `function_name` - Name of the function to call
    /// * `args` - Arguments to pass to the function (as JSON)
    ///
    /// # Returns
    /// * `Ok(Value)` - The function's return value as JSON
    /// * `Err(TingError)` - If the function call failed
    ///
    /// # Safety
    /// The Native ABI v2 function table is validated during library loading.
    pub fn call_function(
        &self,
        plugin_id: &PluginId,
        function_name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.call_function_with_context(plugin_id, function_name, args, None)
    }

    pub(crate) fn call_function_with_context(
        &self,
        plugin_id: &PluginId,
        function_name: &str,
        args: serde_json::Value,
        mut host_context: Option<host_api::NativeHostInvocationContext>,
    ) -> Result<serde_json::Value> {
        let loaded = {
            let libraries = self.libraries.read().map_err(|e| {
                TingError::PluginLoadError(format!("Failed to acquire read lock: {}", e))
            })?;

            Arc::clone(libraries.get(plugin_id).ok_or_else(|| {
                TingError::PluginNotFound(format!("Plugin {} not found", plugin_id))
            })?)
        };
        let timeout = loaded.resource_limits.max_cpu_time;
        if !matches!(function_name, "initialize" | "shutdown")
            && !loaded
                .metadata
                .capabilities
                .iter()
                .any(|cap| cap.supports(function_name))
        {
            return Err(TingError::PluginExecutionError(format!(
                "Native plugin {plugin_id} did not declare operation {function_name}"
            )));
        }

        let args_str = serde_json::to_vec(&args).map_err(|e| {
            TingError::PluginExecutionError(format!("Failed to serialize arguments: {}", e))
        })?;
        if args_str.len() > MAX_NATIVE_CONTROL_BYTES {
            return Err(TingError::PluginExecutionError(
                "Native control input exceeds the 1 MiB limit".into(),
            ));
        }
        if loaded.unavailable.load(Ordering::Acquire) {
            return Err(TingError::PluginExecutionError(format!(
                "Native plugin {plugin_id} is unavailable after a timed-out call"
            )));
        }
        let plugin_id = plugin_id.clone();
        let worker_id = plugin_id.clone();
        let operation = function_name.to_owned();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = Arc::clone(&loaded);
        std::thread::Builder::new()
            .name("ting-native-call".into())
            .spawn(move || {
                let start = Instant::now();
                let host = host_api::table(host_context.as_mut());
                let result =
                    Self::execute_native_call(&worker, &worker_id, &operation, &args_str, &host);
                let elapsed = start.elapsed();
                if let Ok(mut stats) = worker.stats.lock() {
                    stats.total_calls += 1;
                    if let Ok((_, allocated_bytes)) = &result {
                        stats.successful_calls += 1;
                        stats.peak_memory_bytes = stats.peak_memory_bytes.max(*allocated_bytes);
                    } else {
                        stats.failed_calls += 1;
                    }
                    stats.total_cpu_time += elapsed;
                    stats.last_execution_time = Some(elapsed);
                }
                let _ = sender.send(result.map(|(output, _)| output));
            })
            .map_err(|error| {
                TingError::PluginExecutionError(format!("Failed to start Native worker: {error}"))
            })?;

        match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                loaded.unavailable.store(true, Ordering::Release);
                // The worker holds a strong reference to the library until
                // it returns. Unloading no longer blocks on the worker.
                if let Ok(mut stats) = loaded.stats.lock() {
                    stats.timeout_errors += 1;
                }
                Err(TingError::Timeout(format!(
                    "Native plugin {plugin_id} exceeded its {timeout:?} call deadline"
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                loaded.unavailable.store(true, Ordering::Release);
                Err(TingError::PluginExecutionError(format!(
                    "Native plugin {plugin_id} worker exited without returning"
                )))
            }
        }
    }

    /// Invoke an instance while retaining the library reference and serializing
    /// work on this instance. The Host alone owns the output allocation.
    fn execute_native_call(
        loaded: &LoadedLibrary,
        plugin_id: &PluginId,
        operation: &str,
        args: &[u8],
        host: &ting_plugin_contract::native_abi::NativeHostApiV2,
    ) -> Result<(serde_json::Value, usize)> {
        if !matches!(operation, "initialize" | "shutdown")
            && !loaded
                .metadata
                .capabilities
                .iter()
                .any(|cap| cap.supports(operation))
        {
            return Err(TingError::PluginExecutionError(format!(
                "Native plugin {plugin_id} did not declare operation {operation}"
            )));
        }
        if loaded.unavailable.load(Ordering::Acquire) {
            return Err(TingError::PluginExecutionError(format!(
                "Native plugin {plugin_id} is unavailable"
            )));
        }
        let _serial = loaded.call_lock.lock().map_err(|error| {
            TingError::PluginExecutionError(format!("Native instance lock failed: {error}"))
        })?;
        if loaded.unavailable.load(Ordering::Acquire) {
            return Err(TingError::PluginExecutionError(format!(
                "Native plugin {plugin_id} is unavailable"
            )));
        }
        if !unsafe {
            (loaded.abi.supports)(
                loaded.instance as *mut c_void,
                operation.as_ptr(),
                operation.len(),
            )
        } {
            return Err(TingError::PluginExecutionError(format!(
                "Native plugin {plugin_id} does not support {operation}"
            )));
        }
        let mut allocation = HostAllocation {
            buffer: None,
            limit: loaded
                .resource_limits
                .max_memory_bytes
                .min(MAX_NATIVE_CONTROL_BYTES),
        };
        let mut output = NativeCallOutput::default();
        let allocator = NativeHostAllocator {
            user_data: (&mut allocation as *mut HostAllocation).cast(),
            allocate: allocate_native_output,
        };
        let code = unsafe {
            (loaded.abi.invoke)(
                loaded.instance as *mut c_void,
                operation.as_ptr(),
                operation.len(),
                args.as_ptr(),
                args.len(),
                host,
                allocator,
                &mut output,
            )
        };
        if code != 0 {
            return Err(TingError::PluginExecutionError(format!(
                "Native plugin {plugin_id} returned error code {code} from {operation}"
            )));
        }
        let bytes = allocation.buffer.ok_or_else(|| {
            TingError::PluginExecutionError("Native plugin did not allocate an output".into())
        })?;
        if !std::ptr::eq(output.data, bytes.as_ptr()) || output.len > bytes.len() {
            return Err(TingError::PluginExecutionError(
                "Native plugin returned an invalid Host allocation".into(),
            ));
        }
        let value = serde_json::from_slice(&bytes[..output.len]).map_err(|error| {
            TingError::PluginExecutionError(format!("Invalid Native response JSON: {error}"))
        })?;
        Ok((value, bytes.len()))
    }

    /// Get resource usage statistics for a plugin
    pub fn get_stats(&self, plugin_id: &PluginId) -> Result<ResourceStats> {
        let libraries = self.libraries.read().map_err(|e| {
            TingError::PluginLoadError(format!("Failed to acquire read lock: {}", e))
        })?;

        let loaded = libraries
            .get(plugin_id)
            .ok_or_else(|| TingError::PluginNotFound(format!("Plugin {} not found", plugin_id)))?;

        loaded
            .stats
            .lock()
            .map(|stats| stats.clone())
            .map_err(|error| {
                TingError::PluginLoadError(format!("Native statistics lock failed: {error}"))
            })
    }

    /// Check if a library is currently loaded
    pub fn is_loaded(&self, plugin_id: &PluginId) -> bool {
        self.libraries
            .read()
            .map(|libs| libs.contains_key(plugin_id))
            .unwrap_or(false)
    }

    /// Get the path of a loaded library
    pub fn get_library_path(&self, plugin_id: &PluginId) -> Result<PathBuf> {
        let libraries = self.libraries.read().map_err(|e| {
            TingError::PluginLoadError(format!("Failed to acquire read lock: {}", e))
        })?;

        let loaded = libraries
            .get(plugin_id)
            .ok_or_else(|| TingError::PluginNotFound(format!("Plugin {} not found", plugin_id)))?;

        Ok(loaded.path.clone())
    }

    /// Get metadata for a loaded library
    pub fn get_metadata(&self, plugin_id: &PluginId) -> Result<PluginMetadata> {
        let libraries = self.libraries.read().map_err(|e| {
            TingError::PluginLoadError(format!("Failed to acquire read lock: {}", e))
        })?;

        let loaded = libraries
            .get(plugin_id)
            .ok_or_else(|| TingError::PluginNotFound(format!("Plugin {} not found", plugin_id)))?;

        Ok(loaded.metadata.clone())
    }

    /// Increment the reference count for a library
    ///
    /// This prevents the library from being unloaded while it's in use.
    pub fn increment_ref_count(&self, plugin_id: &PluginId) -> Result<()> {
        let libraries = self.libraries.read().map_err(|e| {
            TingError::PluginLoadError(format!("Failed to acquire read lock: {}", e))
        })?;

        let loaded = libraries
            .get(plugin_id)
            .ok_or_else(|| TingError::PluginNotFound(format!("Plugin {} not found", plugin_id)))?;

        loaded.ref_count.fetch_add(1, Ordering::AcqRel);

        Ok(())
    }

    /// Get the number of loaded libraries
    pub fn library_count(&self) -> usize {
        self.libraries.read().map(|libs| libs.len()).unwrap_or(0)
    }

    /// List all loaded library IDs
    pub fn list_loaded_libraries(&self) -> Vec<PluginId> {
        self.libraries
            .read()
            .map(|libs| libs.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Validate library file extension for the current platform
    fn is_valid_library_extension(&self, path: &Path) -> bool {
        let extension = path.extension().and_then(|e| e.to_str());

        match extension {
            Some(ext) => {
                #[cfg(target_os = "windows")]
                return ext == "dll";

                #[cfg(target_os = "linux")]
                return ext == "so";

                #[cfg(target_os = "macos")]
                return ext == "dylib";

                #[cfg(not(any(
                    target_os = "windows",
                    target_os = "linux",
                    target_os = "macos"
                )))]
                return false;
            }
            None => false,
        }
    }
}

/// The bootstrap table is checked before calling `supports` for each
/// manifest operation, so unknown operation names never reach the plugin.
fn validate_native_exports(
    abi: &NativePluginAbiV2,
    instance: *mut c_void,
    metadata: &PluginMetadata,
) -> Result<()> {
    let mut missing = missing_native_exports(metadata, |name| unsafe {
        (abi.supports)(instance, name.as_ptr(), name.len())
    });
    for lifecycle in ["initialize", "shutdown"] {
        if !unsafe { (abi.supports)(instance, lifecycle.as_ptr(), lifecycle.len()) } {
            missing.push(lifecycle);
        }
    }
    missing.sort_unstable();
    missing.dedup();
    if !missing.is_empty() {
        return Err(TingError::PluginLoadError(format!(
            "Native plugin {} is missing declared function exports: {}",
            metadata.instance_id(),
            missing.join(", ")
        )));
    }
    Ok(())
}

fn missing_native_exports(
    metadata: &PluginMetadata,
    has_export: impl Fn(&str) -> bool,
) -> Vec<&str> {
    let mut missing: Vec<_> = metadata
        .capabilities
        .iter()
        .flat_map(|capability| capability.required_exports())
        .filter(|name| !has_export(name))
        .collect();
    missing.sort_unstable();
    missing.dedup();
    missing
}

impl Default for NativeLoader {
    fn default() -> Self {
        Self::new()
    }
}

// Ensure NativeLoader is thread-safe
unsafe impl Send for NativeLoader {}
unsafe impl Sync for NativeLoader {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::Command;
    use std::time::Duration;

    fn fixture_library(bad_revision: bool) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let name = format!("native_fixture.{}", std::env::consts::DLL_EXTENSION);
        let library = temp.path().join(name);
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native_abi_v2.rs");
        let mut compiler = Command::new("rustc");
        compiler
            .args(["--edition=2024", "--crate-type=cdylib", "--cap-lints=allow"])
            .arg(source)
            .arg("-o")
            .arg(&library);
        if bad_revision {
            compiler.args(["--cfg", "bad_revision"]);
        }
        let result = compiler.output().unwrap();
        assert!(
            result.status.success(),
            "Native test fixture compilation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        (temp, library)
    }

    fn create_test_metadata() -> PluginMetadata {
        PluginMetadata::new(
            "test-plugin".to_string(),
            "test-plugin".to_string(),
            "1.0.0".to_string(),
            "Test Author".to_string(),
            "Test plugin".to_string(),
            "libtest.so".to_string(),
        )
    }

    #[test]
    fn test_native_loader_creation() {
        let loader = NativeLoader::new();
        assert_eq!(loader.library_count(), 0);
    }

    /// Set TING_TEST_XM_LIBRARY to a freshly built official XM DLL / SO. CI's
    /// source-plugin gate builds it before running this real ABI integration.
    #[tokio::test]
    async fn official_xm_native_abi_extracts_and_writes_without_file_paths() {
        let Ok(library) = std::env::var("TING_TEST_XM_LIBRARY") else {
            return;
        };
        let temporary = tempfile::tempdir().unwrap();
        let mut tag = id3::Tag::new();
        use id3::TagLike;
        tag.set_title("Chapter one");
        tag.set_album("Book | extra");
        tag.set_artist("Narrator");
        tag.add_frame(id3::Frame::text("TSIZ", "16"));
        tag.add_frame(id3::Frame::text("TENC", "00000000000000000000000000000000"));
        tag.add_frame(id3::Frame::text("TSSE", "prefix"));
        let mut bytes = Vec::new();
        tag.write_to(&mut bytes, id3::Version::Id3v23).unwrap();
        let header_size = bytes.len();
        bytes.extend([0u8; 16]);
        bytes.extend(b"plain-tail");
        let path = temporary.path().join("sample.xm");
        std::fs::write(&path, bytes).unwrap();

        let mut metadata = PluginMetadata::new(
            "xm-format".into(),
            "xm format".into(),
            "2.0.0".into(),
            "Ting Reader".into(),
            "XM format".into(),
            library.clone(),
        );
        metadata.capabilities = vec![
            serde_json::from_value(json!({
                "id":"format.handler","kind":"format_handler","extensions":["xm"],
                "operations":["probe","get_metadata_read_size","extract_metadata",
                    "write_metadata","open_decrypt","read_chunk","seek","close","cancel"]
            }))
            .unwrap(),
        ];
        let id = metadata.instance_id();
        let loader = NativeLoader::new();
        loader
            .load_library(id.clone(), Path::new(&library), metadata)
            .unwrap();
        let scope = Arc::new(crate::plugin::resources::ResourceScope::new(
            id.clone(),
            uuid::Uuid::new_v4(),
            None,
            temporary.path().join("staging"),
            crate::plugin::resources::ResourceLimits::default(),
        ));
        let input = scope.grant_file(&path, 8 * 1024 * 1024, None).unwrap();
        let host = || host_api::NativeHostInvocationContext {
            plugin_id: id.clone(),
            permissions: Vec::new(),
            user: None,
            host_gateway: None,
            resources: Some(Arc::clone(&scope)),
            runtime_handle: tokio::runtime::Handle::current(),
        };
        let initialized = loader.call_function(&id, "initialize", json!({})).unwrap();
        assert_eq!(initialized["ok"], true);
        let probed = loader
            .call_function_with_context(
                &id,
                "probe",
                json!({
                    "input":input, "extension_hint":"xm", "mime_hint":null,
                    "prefix_bytes":header_size,
                }),
                Some(host()),
            )
            .unwrap();
        assert_eq!(probed["data"]["kind"], "match");
        let extracted = loader
            .call_function_with_context(
                &id,
                "extract_metadata",
                json!({
                    "input":input, "extract_cover":false,
                }),
                Some(host()),
            )
            .unwrap();
        assert_eq!(extracted["data"]["title"], "Chapter one");
        assert_eq!(extracted["data"]["album"], "Book");
        assert_eq!(extracted["data"]["narrator"], "Narrator");
        let output = scope.create_output(None).unwrap();
        let revision = scope.stat(&input).unwrap().revision.unwrap();
        let written = loader
            .call_function_with_context(
                &id,
                "write_metadata",
                json!({
                    "input":input,"output":output,"source_revision":revision,
                    "patch":{"title":"Chapter two"}
                }),
                Some(host()),
            )
            .unwrap();
        assert_eq!(written["ok"], true, "{written}");
        scope
            .commit_local_output(&output, &input, &path, &revision)
            .unwrap();
        let tag = id3::Tag::read_from_path(&path).unwrap();
        assert_eq!(tag.title(), Some("Chapter two"));
        assert_eq!(
            tag.get("TSIZ").and_then(|frame| frame.content().text()),
            Some("16")
        );
        assert_eq!(
            tag.get("TSSE").and_then(|frame| frame.content().text()),
            Some("prefix")
        );
        loader.call_function(&id, "shutdown", json!({})).unwrap();
        loader.unload_library(&id).unwrap();
    }

    #[test]
    fn native_preflight_requires_all_declared_symbols_before_registration() {
        let mut metadata = create_test_metadata();
        metadata.capabilities = vec![
            serde_json::from_value(json!({
                "id": "format.xm", "kind": "format_handler",
                "extensions": ["xm"],
                "operations": ["probe", "get_metadata_read_size", "extract_metadata"]
            }))
            .unwrap(),
            serde_json::from_value(json!({
                "id": "format.special", "kind": "format_handler",
                "extensions": ["special"],
                "operations": ["probe", "extract_metadata"]
            }))
            .unwrap(),
        ];
        assert_eq!(
            missing_native_exports(&metadata, |name| name == "probe"),
            ["extract_metadata", "get_metadata_read_size"]
        );
        assert!(missing_native_exports(&metadata, |_| true).is_empty());
    }

    #[test]
    fn native_v2_fixture_invokes_and_enforces_host_output_limit() {
        let (_temp, library) = fixture_library(false);
        let mut metadata = create_test_metadata();
        metadata.capabilities = vec![
            serde_json::from_value(json!({
                "id": "format.test", "kind": "format_handler",
                "extensions": ["test"], "operations": ["probe", "extract_metadata"]
            }))
            .unwrap(),
        ];
        let id = metadata.instance_id();
        let loader = NativeLoader::new();
        loader.load_library(id.clone(), &library, metadata).unwrap();
        assert_eq!(
            loader
                .call_function(&id, "probe", json!({"input": "opaque"}))
                .unwrap(),
            json!({"kind":"no_match"})
        );
        assert!(
            loader
                .call_function(&id, "probe", json!({"case": "oversize"}))
                .is_err()
        );
        assert!(loader.call_function(&id, "oversize", json!({})).is_err());
        let stats = loader.get_stats(&id).unwrap();
        assert_eq!(
            (
                stats.total_calls,
                stats.successful_calls,
                stats.failed_calls
            ),
            (2, 1, 1)
        );
        assert_eq!(stats.peak_memory_bytes, br#"{"kind":"no_match"}"#.len());
        loader.unload_library(&id).unwrap();
    }

    #[test]
    fn native_v2_rejects_wrong_revision_before_reading_function_table() {
        let (_temp, library) = fixture_library(true);
        let loader = NativeLoader::new();
        let error = loader
            .load_library("fixture".into(), &library, create_test_metadata())
            .unwrap_err();
        assert!(error.to_string().contains("unsupported ABI revision"));
        assert_eq!(loader.library_count(), 0);
    }

    #[test]
    fn timed_out_native_instance_never_accepts_another_call() {
        let (_temp, library) = fixture_library(false);
        let mut metadata = create_test_metadata();
        metadata.capabilities = vec![
            serde_json::from_value(json!({
                "id": "format.test", "kind": "format_handler",
                "extensions": ["test"], "operations": ["probe", "extract_metadata"]
            }))
            .unwrap(),
        ];
        let id = metadata.instance_id();
        let limits = ResourceLimits::custom(
            1024 * 1024,
            Duration::from_millis(25),
            1024 * 1024,
            1024 * 1024,
        );
        let loader = NativeLoader::with_limits(limits);
        loader.load_library(id.clone(), &library, metadata).unwrap();
        let start = Instant::now();
        assert!(matches!(
            loader.call_function(&id, "probe", json!({"case": "slow"})),
            Err(TingError::Timeout(_))
        ));
        assert!(start.elapsed() < Duration::from_millis(150));
        assert!(loader.call_function(&id, "probe", json!({})).is_err());
        let stats = loader.get_stats(&id).unwrap();
        assert_eq!(stats.timeout_errors, 1);
        // Unloading removes the instance immediately; the running worker
        // retains its own library reference until it completes.
        let unload_start = Instant::now();
        loader.unload_library(&id).unwrap();
        assert!(unload_start.elapsed() < Duration::from_millis(120));
        std::thread::sleep(Duration::from_millis(180));
    }

    #[test]
    fn test_native_loader_with_custom_limits() {
        let limits = ResourceLimits::custom(
            256 * 1024 * 1024, // 256 MB
            Duration::from_secs(60),
            50 * 1024 * 1024, // 50 MB
            5 * 1024 * 1024,  // 5 MB/s
        );
        let loader = NativeLoader::with_limits(limits);
        assert_eq!(loader.library_count(), 0);
    }

    #[test]
    fn test_is_loaded_returns_false_for_nonexistent() {
        let loader = NativeLoader::new();
        assert!(!loader.is_loaded(&"nonexistent".to_string()));
    }

    #[test]
    fn test_list_loaded_libraries_empty() {
        let loader = NativeLoader::new();
        assert_eq!(loader.list_loaded_libraries().len(), 0);
    }

    #[test]
    fn test_load_nonexistent_library_fails() {
        let loader = NativeLoader::new();
        let metadata = create_test_metadata();
        let result = loader.load_library(
            "test".to_string(),
            Path::new("/nonexistent/library.so"),
            metadata,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_load_library_with_custom_limits() {
        let loader = NativeLoader::new();
        let metadata = create_test_metadata();
        let limits = ResourceLimits::restrictive();
        let result = loader.load_library_with_limits(
            "test".to_string(),
            Path::new("/nonexistent/library.so"),
            metadata,
            limits,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_unload_nonexistent_library_fails() {
        let loader = NativeLoader::new();
        let result = loader.unload_library(&"nonexistent".to_string());
        assert!(result.is_err());
    }

    #[test]
    fn test_get_metadata_nonexistent_fails() {
        let loader = NativeLoader::new();
        let result = loader.get_metadata(&"nonexistent".to_string());
        assert!(result.is_err());
    }

    #[test]
    fn test_get_stats_nonexistent_fails() {
        let loader = NativeLoader::new();
        let result = loader.get_stats(&"nonexistent".to_string());
        assert!(result.is_err());
    }

    #[test]
    fn test_resource_stats_default() {
        let stats = ResourceStats::default();
        assert_eq!(stats.total_calls, 0);
        assert_eq!(stats.successful_calls, 0);
        assert_eq!(stats.failed_calls, 0);
        assert_eq!(stats.timeout_errors, 0);
        assert_eq!(stats.peak_memory_bytes, 0);
        assert_eq!(stats.total_cpu_time, Duration::from_secs(0));
        assert!(stats.last_execution_time.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_valid_library_extension_linux() {
        let loader = NativeLoader::new();
        assert!(loader.is_valid_library_extension(Path::new("test.so")));
        assert!(!loader.is_valid_library_extension(Path::new("test.dll")));
        assert!(!loader.is_valid_library_extension(Path::new("test.dylib")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_valid_library_extension_windows() {
        let loader = NativeLoader::new();
        assert!(loader.is_valid_library_extension(Path::new("test.dll")));
        assert!(!loader.is_valid_library_extension(Path::new("test.so")));
        assert!(!loader.is_valid_library_extension(Path::new("test.dylib")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_valid_library_extension_macos() {
        let loader = NativeLoader::new();
        assert!(loader.is_valid_library_extension(Path::new("test.dylib")));
        assert!(!loader.is_valid_library_extension(Path::new("test.dll")));
        assert!(!loader.is_valid_library_extension(Path::new("test.so")));
    }
}
