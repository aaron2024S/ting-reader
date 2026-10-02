//! Native dynamic library plugin subsystem

pub mod host_api;
pub mod loader;
pub mod plugin;

pub use loader::NativeLoader;
pub use plugin::NativePlugin;

#[cfg(test)]
#[path = "../../../tests/unit/plugin/native/abi.rs"]
mod tests;
