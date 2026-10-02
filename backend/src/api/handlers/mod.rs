//! HTTP endpoint modules. Shared application state is defined in `api::state`.

pub mod books;
pub mod libraries;
pub mod media;
pub mod notifications;
pub mod playlists;
pub mod plugin_logs;
pub mod plugins;
pub mod reading;
pub mod series;
pub mod system;
pub mod tools;
pub mod users;

pub use books::*;
pub use libraries::*;
pub use media::*;
pub use notifications::*;
pub use playlists::*;
pub use plugin_logs::*;
pub use plugins::*;
pub use reading::*;
pub use series::*;
pub use system::*;
pub use tools::*;
pub use users::*;

/// Compatibility re-export; new consumers should use `api::state::AppState`.
pub use crate::api::state::AppState;
