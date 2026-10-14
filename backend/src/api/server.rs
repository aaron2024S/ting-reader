//! HTTP Server implementation
//!
//! This module provides the HTTP server using Axum framework with:
//! - Configurable host/port binding
//! - Graceful shutdown handling
//! - Connection limits and request timeouts
//! - Health check endpoint
//! - CORS support

mod gateway;
mod router;
use gateway::*;

use crate::api::middleware::{
    ApiKey, GatewayTransport, RateLimiter, SecurityHeadersConfig, auth_middleware,
    rate_limit_middleware, security_headers_middleware, trace_id_middleware,
};
use crate::api::routes::build_api_routes;
use crate::api::state::AppState;
use crate::core::Config;
use crate::core::app::config::ServerConfig;
use crate::db::manager::DatabaseManager;
use crate::db::repository::BookRepository;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, StatusCode, Uri, header, uri::PathAndQuery},
    middleware,
    middleware::Next,
    response::{IntoResponse, Json, Response},
    routing::{any, get},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use serde_json::{Value, json};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tower::{ServiceBuilder, ServiceExt};
use tower_http::{
    classify::ServerErrorsFailureClass,
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::info;

#[cfg(unix)]
use hyper::body::Incoming;
#[cfg(unix)]
use hyper::server::conn::http1;
#[cfg(unix)]
use hyper_util::rt::TokioIo;

/// HTTP API Server
pub struct ApiServer {
    router: Router,
    config: ServerConfig,
}

#[derive(Clone)]
struct GatewayProxyState {
    router: Router,
    prefix: String,
}

impl ApiServer {
    /// Create a new API server with the given configuration and database manager
    pub fn new(
        config: Config,
        db: Arc<DatabaseManager>,
        plugin_manager: Arc<crate::plugin::manager::PluginManager>,
        config_manager: Arc<crate::plugin::config::PluginConfigManager>,
        encryption_key: [u8; 32],
    ) -> anyhow::Result<Self> {
        let server_config = config.server.clone();

        // Build the router with all routes and middleware
        let router =
            Self::build_router(config, db, plugin_manager, config_manager, encryption_key)?;

        Ok(Self {
            router,
            config: server_config,
        })
    }

    /// Start the HTTP server and listen for requests
    ///
    /// This method will block until the server is shut down gracefully.
    pub async fn serve(self) -> anyhow::Result<()> {
        let socket_addr = match self.config.host.parse::<IpAddr>() {
            Ok(ip) => SocketAddr::new(ip, self.config.port),
            Err(_) => format!("{}:{}", self.config.host, self.config.port).parse()?,
        };

        info!(
            host = %self.config.host,
            port = self.config.port,
            max_connections = self.config.max_connections,
            request_timeout = self.config.request_timeout,
            message_key = "system.http.starting",
            message_params = %serde_json::json!({
                "host": self.config.host,
                "port": self.config.port,
                "max_connections": self.config.max_connections,
                "request_timeout": self.config.request_timeout,
            }),
            "Starting HTTP server"
        );

        // Create TCP listener. This is intentionally kept enabled even when
        // the fnOS Unix Socket gateway is configured for backward-compatible
        // direct port access.
        let listener = tokio::net::TcpListener::bind(socket_addr).await?;

        info!(
            addr = %socket_addr,
            message_key = "system.http.listening",
            message_params = %serde_json::json!({ "addr": socket_addr.to_string() }),
            "HTTP server listening"
        );

        #[cfg(unix)]
        let gateway_socket = self.config.gateway_socket.clone();

        #[cfg(unix)]
        let gateway_listener = if let Some(path) = gateway_socket.as_ref() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if path.exists() {
                std::fs::remove_file(path)?;
            }

            let listener = tokio::net::UnixListener::bind(path)?;

            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;

            info!(
                socket = %path.display(),
                prefix = ?self.config.gateway_prefix,
                "fnOS unified gateway socket listening"
            );
            Some(listener)
        } else {
            None
        };

        #[cfg(unix)]
        if let Some(gateway_listener) = gateway_listener {
            let tcp_server = axum::serve(
                listener,
                self.router
                    .clone()
                    .into_make_service_with_connect_info::<SocketAddr>(),
            );
            let gateway_server = serve_unix_socket(gateway_listener, self.router.clone());

            tokio::select! {
                result = tcp_server => result?,
                result = gateway_server => result?,
                _ = shutdown_signal() => {},
            }

            if let Some(path) = gateway_socket.as_ref() {
                let _ = std::fs::remove_file(path);
            }
        } else {
            axum::serve(
                listener,
                self.router
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        }

        #[cfg(not(unix))]
        {
            if self.config.gateway_socket.is_some() {
                tracing::warn!(
                    "fnOS unified gateway socket is configured but Unix sockets are unavailable on this platform"
                );
            }

            axum::serve(
                listener,
                self.router
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        }

        info!("HTTP server shut down gracefully");

        Ok(())
    }

    /// Get a reference to the router
    pub fn router(&self) -> &Router {
        &self.router
    }
}

#[cfg(unix)]
async fn serve_unix_socket(
    listener: tokio::net::UnixListener,
    router: Router,
) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let router = router.clone();

        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = hyper::service::service_fn(move |request: hyper::Request<Incoming>| {
                let router = router.clone();
                async move {
                    let mut request = request.map(Body::new);
                    request.extensions_mut().insert(GatewayTransport);
                    router.oneshot(request).await
                }
            });

            if let Err(error) = http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                tracing::debug!(error = %error, "Unix socket HTTP connection closed with error");
            }
        });
    }
}

async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "timestamp": chrono::Utc::now().timestamp(),
    }))
}

/// Wait for shutdown signal (Ctrl+C or SIGTERM)
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C signal");
        },
        _ = terminate => {
            info!("Received SIGTERM signal");
        },
    }

    info!("Initiating graceful shutdown...");
}

#[cfg(test)]
#[path = "../../tests/unit/api/server.rs"]
mod tests;
