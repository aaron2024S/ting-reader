//! WebAssembly plugin subsystem

pub mod host_functions;
mod invocation;
pub mod plugin;
mod resource_bindings;
pub mod runtime;

/// Compatibility path for the sandbox shared by all plugin runtimes.
pub use crate::plugin::sandbox;
pub use crate::plugin::sandbox::{FileAccess, Permission, ResourceLimits, Sandbox};
pub use plugin::WasmPlugin;
pub use runtime::WasmRuntime;
