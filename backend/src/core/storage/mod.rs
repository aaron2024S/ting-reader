//! Local storage paths, WebDAV clients and file transfers.

pub mod local_paths;
pub mod service;
pub mod webdav_client;

pub use service::{StorageService, WebDavFileRevision, WebDavWriteSource};
pub use webdav_client::WebDavClient;
