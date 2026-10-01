//! REST API module
//!
//! This module provides the HTTP server and REST API endpoints including:
//! - API routing and request handling
//! - Authentication and authorization middleware
//! - Rate limiting
//! - Error handling and response formatting

pub mod handlers;
pub mod middleware;
pub mod models;
pub mod routes;
pub mod server;
pub mod utils;
pub mod ws;

pub use middleware::{TRACE_ID_HEADER, TraceId, trace_id_middleware};
pub use models::{ErrorResponse, SearchQuery, SearchResponse};
pub use server::ApiServer;
pub use utils::require_admin;
