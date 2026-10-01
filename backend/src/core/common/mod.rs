//! Shared core data structures and utilities.

pub mod lru_cache;
pub mod utils;

pub use lru_cache::LruCache;
pub use utils::release_memory;
