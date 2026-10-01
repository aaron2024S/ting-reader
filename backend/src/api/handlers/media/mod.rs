//! Media handlers: audio streaming, caching, and cover proxying

pub mod cache;
pub mod proxy;
pub mod stream;

pub use cache::{cache_chapter, clear_all_caches, delete_chapter_cache, get_cache_list};
pub use proxy::{ProxyCoverQuery, proxy_cover};
pub use stream::{StreamQuery, stream_chapter};
