//! WebAssembly plugin subsystem

pub mod host_functions;
mod invocation;
pub mod plugin;
mod resource_bindings;
pub mod runtime;
pub mod sandbox;
#[cfg(test)]
mod tests;

pub use plugin::WasmPlugin;
pub use runtime::WasmRuntime;
pub use sandbox::{FileAccess, Permission, ResourceLimits, Sandbox};
