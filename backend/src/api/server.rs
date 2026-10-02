//! HTTP Server implementation
//!
//! This module provides the HTTP server using Axum framework with:
//! - Configurable host/port binding
//! - Graceful shutdown handling
//! - Connection limits and request timeouts
//! - Health check endpoint
//! - CORS support

use crate::api::middleware::{
    ApiKey, SecurityHeadersConfig, auth_middleware, security_headers_middleware,
    trace_id_middleware,
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

    /// Build the Axum router with all routes and middleware
    fn build_router(
        config: Config,
        db: Arc<DatabaseManager>,
        plugin_manager: Arc<crate::plugin::manager::PluginManager>,
        config_manager: Arc<crate::plugin::config::PluginConfigManager>,
        encryption_key: [u8; 32],
    ) -> anyhow::Result<Router> {
        // Create API key configuration for authentication
        let api_key = ApiKey::new(config.security.enable_auth, config.security.api_key.clone());

        // Create security headers configuration
        let security_headers_config =
            SecurityHeadersConfig::new(config.security.enable_hsts, config.security.hsts_max_age);

        // Create repositories
        let book_repo = Arc::new(BookRepository::new(db.clone()));
        let user_repo = Arc::new(crate::db::repository::UserRepository::new(db.clone()));
        let progress_repo = Arc::new(crate::db::repository::ProgressRepository::new(db.clone()));
        let favorite_repo = Arc::new(crate::db::repository::FavoriteRepository::new(db.clone()));

        // Keep rolling activity and long-hidden progress bounded even when playback is idle.
        let progress_cleanup_repo = progress_repo.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
            loop {
                interval.tick().await;
                match progress_cleanup_repo.cleanup_expired_records().await {
                    Ok(removed) if removed > 0 => {
                        tracing::info!(
                            removed,
                            message_key = "progress.activity_cleanup.completed",
                            "Expired playback records removed"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            message_key = "progress.activity_cleanup.failed",
                            message_params = %serde_json::json!({ "error": error.to_string() }),
                            "Expired playback record cleanup failed"
                        );
                    }
                }
            }
        });

        let settings_repo = Arc::new(crate::db::repository::UserSettingsRepository::new(
            db.clone(),
        ));
        let system_settings_repo = Arc::new(crate::db::repository::SystemSettingsRepository::new(
            db.clone(),
        ));
        let library_repo = Arc::new(crate::db::repository::LibraryRepository::new(db.clone()));
        let chapter_repo = Arc::new(crate::db::repository::ChapterRepository::new(db.clone()));
        let series_repo = Arc::new(crate::db::repository::SeriesRepository::new(db.clone()));
        let playlist_repo = Arc::new(crate::db::repository::PlaylistRepository::new(db.clone()));
        let notification_repo = Arc::new(
            crate::db::repository::NotificationWebhookRepository::new(db.clone()),
        );

        // Initialize JWT key manager (auto-generates and rotates keys)
        let jwt_key_manager = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                match crate::auth::JwtKeyManager::new(db.get_pool(), encryption_key).await {
                    Ok(manager) => {
                        tracing::info!(
                            message_key = "system.jwt_key.initialized",
                            "JWT key manager initialized"
                        );
                        let manager_arc = Arc::new(manager);
                        // 启动后台轮换任务
                        manager_arc.clone().start_rotation_task();
                        Some(manager_arc)
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            message_key = "system.jwt_key.init_failed",
                            message_params = %serde_json::json!({ "error": e.to_string() }),
                            "JWT key manager initialization failed; using configured secret"
                        );
                        None
                    }
                }
            })
        });

        // Get JWT secret from config (fallback)
        let jwt_secret = Arc::new(config.security.jwt_secret.clone());

        // Keep API state and plugin lifecycle on the same config manager instance.
        plugin_manager.set_config_manager(config_manager.clone());

        // Create services
        let book_service = Arc::new(crate::core::books::BookService::new(book_repo.clone()));
        let scraper_service = Arc::new(crate::core::books::ScraperService::new(
            plugin_manager.clone(),
        ));
        let merge_service = Arc::new(crate::core::books::merge_service::MergeService::new(
            book_repo.clone(),
            chapter_repo.clone(),
        ));

        // Create helpers
        let cleaner_config = crate::core::books::text_cleaner::CleanerConfig::default();
        let text_cleaner = Arc::new(crate::core::books::text_cleaner::TextCleaner::new(
            cleaner_config,
        ));

        let nfo_manager = Arc::new(crate::core::books::nfo_manager::NfoManager::new(
            config.storage.data_dir.clone(),
        ));

        // Create audio streamer with configuration
        let streamer_config = crate::core::audio::StreamerConfig {
            cache_enabled: config.audio.cache_enabled,
            cache_size: config.audio.cache_size,
            buffer_size: config.audio.buffer_size,
            supported_formats: vec![
                crate::core::audio::AudioFormat::Mp3,
                crate::core::audio::AudioFormat::M4a,
                crate::core::audio::AudioFormat::Aac,
                crate::core::audio::AudioFormat::Flac,
                crate::core::audio::AudioFormat::Wma,
            ],
        };
        let audio_streamer = Arc::new(crate::core::audio::AudioStreamer::new(streamer_config));

        // Create StorageService
        let storage_service = Arc::new(crate::core::StorageService::new());

        // Create Preload Cache
        let preload_cache = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let preload_cache_for_expiration = Arc::downgrade(&preload_cache);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                let Some(cache_lock) = preload_cache_for_expiration.upgrade() else {
                    break;
                };
                let mut cache = cache_lock.write().await;
                crate::api::handlers::media::stream::preload::evict_expired_cache(
                    &mut cache,
                    std::time::Instant::now(),
                );
            }
        });

        // Create task queue
        let task_queue = Arc::new(
            crate::core::task_queue::TaskQueue::new(
                config.task_queue.clone(),
                db.clone(),
                config.storage.temp_dir.clone(),
            )
            .with_repositories(book_repo.clone(), chapter_repo.clone(), series_repo.clone())
            .with_library_repo(library_repo.clone())
            .with_scraper_service(scraper_service.clone())
            .with_text_cleaner(text_cleaner.clone())
            .with_nfo_manager(nfo_manager.clone())
            .with_audio_streamer(audio_streamer.clone())
            .with_plugin_manager(plugin_manager.clone())
            .with_storage_service(storage_service.clone())
            .with_merge_service(merge_service.clone())
            .with_notification_repo(notification_repo.clone())
            .with_encryption_key(Arc::new(encryption_key)),
        );

        // Start task queue executor
        let task_queue_clone = task_queue.clone();
        tokio::spawn(async move {
            if let Err(e) = task_queue_clone.recover_tasks().await {
                tracing::error!(
                    error = %e,
                    message_key = "task.recovery.failed",
                    message_params = %serde_json::json!({ "error": e.to_string() }),
                    "Task recovery failed"
                );
            }
            task_queue_clone.start().await;
        });

        let sync_scheduler = Arc::new(
            crate::core::library_scanner::scheduler::LibrarySyncScheduler::new(
                library_repo.clone(),
                task_queue.clone(),
            ),
        );
        tokio::spawn(sync_scheduler.start());

        // Wrap config in Arc<RwLock> for shared mutable access
        let config_arc = Arc::new(tokio::sync::RwLock::new(config.clone()));

        // Create cache manager
        let cache_manager = Arc::new(
            crate::cache::CacheManager::new(config.storage.temp_dir.clone())
                .map_err(|e| anyhow::anyhow!("Failed to create cache manager: {}", e))?,
        );
        let plugin_cache = Arc::new(
            crate::plugin::PluginCache::new(config.storage.data_dir.join("plugin-cache"))
                .map_err(|e| anyhow::anyhow!("Failed to create plugin cache: {}", e))?,
        );
        let plugin_route_revocations = Arc::new(
            crate::core::security::signing::PluginRouteRevocations::new(
                config
                    .storage
                    .data_dir
                    .join("plugin-route-revocations.json"),
            )
            .map_err(|e| anyhow::anyhow!("Failed to create plugin route revocations: {}", e))?,
        );
        let plugin_host_gateway = Arc::new(crate::plugin::PluginHostGateway::new(
            crate::plugin::PluginHostGatewayDependencies {
                book_repo: book_repo.clone(),
                library_repo: library_repo.clone(),
                chapter_repo: chapter_repo.clone(),
                progress_repo: progress_repo.clone(),
                playlist_repo: playlist_repo.clone(),
                favorite_repo: favorite_repo.clone(),
                settings_repo: settings_repo.clone(),
                task_queue: task_queue.clone(),
                plugin_manager: plugin_manager.clone(),
                plugin_cache: plugin_cache.clone(),
                plugin_storage: Arc::new(
                    crate::plugin::PluginCache::new(config.storage.data_dir.join("plugin-storage"))
                        .map_err(|e| anyhow::anyhow!("Failed to create plugin storage: {}", e))?,
                ),
                config_manager: config_manager.clone(),
                plugin_route_revocations: plugin_route_revocations.clone(),
                encryption_key: Arc::new(encryption_key),
                config: config.clone(),
            },
        ));
        plugin_manager.set_host_gateway(&plugin_host_gateway);

        // Create library watcher
        let library_watcher = Arc::new(crate::core::library_scanner::watcher::LibraryWatcher::new(
            library_repo.clone(),
            Arc::new(crate::db::repository::LibraryScanStateRepository::new(
                db.clone(),
            )),
            task_queue.clone(),
            config.clone(),
        ));

        // Start watching all local libraries
        let watcher_clone = library_watcher.clone();
        tokio::spawn(async move {
            if let Err(e) = watcher_clone.start_all().await {
                tracing::warn!(
                    error = %e,
                    message_key = "library.watcher.start_failed",
                    message_params = %serde_json::json!({ "error": e.to_string() }),
                    "Library watcher failed to start"
                );
            }
        });

        // Create WebSocket session manager
        let ws_manager = crate::api::ws::manager::WsSessionManager::new();

        // Create HLS session manager
        let hls_temp_dir = config.storage.temp_dir.join("ting_hls_sessions");
        std::fs::create_dir_all(&hls_temp_dir)
            .map_err(|e| anyhow::anyhow!("Failed to create HLS temp directory: {}", e))?;
        let hls_session_manager = Arc::new(
            crate::api::handlers::media::stream::HlsSessionManager::new(hls_temp_dir),
        );

        // Start HLS session cleanup task (runs every 10 minutes)
        let hls_manager_clone = hls_session_manager.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(600));
            loop {
                interval.tick().await;
                hls_manager_clone.cleanup_expired().await;
            }
        });

        // Create application state
        let app_state = AppState {
            book_repo,
            user_repo,
            progress_repo,
            favorite_repo,
            settings_repo,
            system_settings_repo,
            library_repo,
            chapter_repo,
            series_repo,
            playlist_repo,
            notification_repo,
            book_service,
            scraper_service,
            plugin_manager,
            plugin_cache,
            plugin_host_gateway,
            plugin_route_revocations,
            config_manager,
            task_queue,
            config: config_arc,
            jwt_secret,
            jwt_key_manager, // 新增密钥管理器
            cache_manager,
            encryption_key: Arc::new(encryption_key),
            storage_service,
            preload_cache,
            audio_streamer,
            merge_service,
            nfo_manager,
            active_preload_tasks: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            library_watcher,
            ws_manager,
            hls_session_manager,
        };

        // Create public routes (no authentication required)
        let public_router = Router::new()
            .route("/health", get(health_check))
            .route(
                "/api/auth/login",
                axum::routing::post(crate::auth::handlers::login),
            )
            .route(
                "/api/auth/token-login",
                axum::routing::post(crate::auth::handlers::token_login),
            )
            .route(
                "/api/auth/session-restore",
                axum::routing::post(crate::auth::handlers::session_restore),
            )
            .route(
                "/api/v1/auth/token-login",
                axum::routing::post(crate::auth::handlers::token_login),
            )
            .route(
                "/api/v1/auth/session-restore",
                axum::routing::post(crate::auth::handlers::session_restore),
            )
            .route(
                "/api/auth/register",
                axum::routing::post(crate::auth::handlers::register),
            )
            // WebSocket endpoint — handles auth internally via query param token
            .route("/api/ws", get(crate::api::ws::handler::ws_handler))
            .route("/api/v1/ws", get(crate::api::ws::handler::ws_handler))
            .with_state(app_state.clone());

        // Create protected routes (authentication required)
        let protected_router = build_api_routes(app_state.clone()).layer(middleware::from_fn(
            move |mut req: Request, next: Next| {
                let api_key = api_key.clone();
                async move {
                    // Inject API key into request extensions
                    req.extensions_mut().insert(api_key);
                    // Call auth middleware
                    auth_middleware(req, next).await
                }
            },
        ));

        // Combine public and protected routes
        let api_router = Router::new().merge(public_router).merge(protected_router);

        // Static file serving for SPA
        let static_dir = std::env::var("STATIC_DIR").unwrap_or_else(|_| "static".to_string());
        let index_path = std::path::PathBuf::from(&static_dir).join("index.html");
        let manifest_path = std::path::PathBuf::from(&static_dir).join("manifest.webmanifest");
        let serve_manifest = ServeFile::new(manifest_path);
        let serve_dir = ServeDir::new(&static_dir).fallback(ServeFile::new(index_path));

        // Apply global middleware layers
        let router = api_router
            .route_service("/manifest.webmanifest", serve_manifest)
            .fallback_service(serve_dir);
        let router = router.layer(middleware::from_fn(ensure_manifest_link));
        let router = router.layer(
            ServiceBuilder::new()
                // Add security headers middleware
                .layer(middleware::from_fn(move |mut req: Request, next: Next| {
                    let config = security_headers_config.clone();
                    async move {
                        req.extensions_mut().insert(config);
                        security_headers_middleware(req, next).await
                    }
                }))
                // Add trace ID middleware for request tracking
                .layer(middleware::from_fn(trace_id_middleware))
                // Add tracing for all requests
                .layer(TraceLayer::new_for_http().on_failure(
                    |classification: ServerErrorsFailureClass,
                     latency: Duration,
                     span: &tracing::Span| {
                        let latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
                        match classification {
                            ServerErrorsFailureClass::StatusCode(status_code) => {
                                tracing::debug!(
                                    parent: span,
                                    status_code = status_code.as_u16(),
                                    latency_ms = latency_ms,
                                    message_key = "http.response.status_failed",
                                    message_params = %serde_json::json!({
                                        "status_code": status_code.as_u16(),
                                        "latency_ms": latency_ms,
                                    }),
                                    "HTTP response returned failure status"
                                );
                            }
                            ServerErrorsFailureClass::Error(error) => {
                                tracing::error!(
                                    parent: span,
                                    error = %error,
                                    latency_ms = latency_ms,
                                    message_key = "http.response.service_failed",
                                    message_params = %serde_json::json!({
                                        "latency_ms": latency_ms,
                                    }),
                                    "HTTP service failed"
                                );
                            }
                        }
                    },
                ))
                // Add CORS support
                .layer(Self::build_cors_layer(&config.security.allowed_origins)),
        );

        // The same route tree serves both the direct TCP endpoint and the
        // fnOS gateway endpoint. The gateway forwards the registered prefix
        // to this router, so the existing direct URLs remain unchanged.
        let router = if let Some(prefix) =
            normalize_gateway_prefix(config.server.gateway_prefix.as_deref())
        {
            let gateway_state = GatewayProxyState {
                router: router.clone(),
                prefix: prefix.clone(),
            };
            let gateway_path = prefix.clone();
            let gateway_wildcard_path = format!("{prefix}/*path");

            Router::new()
                .route(&gateway_path, any(gateway_proxy))
                .route(&gateway_wildcard_path, any(gateway_proxy))
                .with_state(gateway_state)
                .merge(router)
        } else {
            router
        };

        Ok(router)
    }

    /// Build CORS layer from allowed origins configuration
    fn build_cors_layer(allowed_origins: &[String]) -> CorsLayer {
        use tower_http::cors::Any;

        let cors = CorsLayer::new();

        // If allowed_origins contains "*", allow any origin
        if allowed_origins.contains(&"*".to_string()) {
            cors.allow_origin(Any).allow_methods(Any).allow_headers(Any)
        } else {
            // Parse allowed origins
            let origins: Vec<_> = allowed_origins
                .iter()
                .filter_map(|origin| origin.parse().ok())
                .collect();

            cors.allow_origin(origins)
                .allow_methods(Any)
                .allow_headers(Any)
        }
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
                async move { router.oneshot(request.map(Body::new)).await }
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

fn normalize_gateway_prefix(prefix: Option<&str>) -> Option<String> {
    let prefix = prefix?.trim();
    if prefix.is_empty() {
        return None;
    }

    let mut normalized = if prefix.starts_with('/') {
        prefix.to_string()
    } else {
        format!("/{prefix}")
    };
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    Some(normalized)
}

async fn gateway_proxy(State(state): State<GatewayProxyState>, mut request: Request) -> Response {
    let original_uri = request.uri().clone();
    let stripped_path = original_uri
        .path()
        .strip_prefix(&state.prefix)
        .filter(|path| !path.is_empty())
        .unwrap_or("/");
    let should_rewrite_html = !is_plugin_asset_path(stripped_path);
    let path_and_query = match original_uri.query() {
        Some(query) => format!("{stripped_path}?{query}"),
        None => stripped_path.to_string(),
    };

    let mut parts = original_uri.into_parts();
    parts.path_and_query = match PathAndQuery::try_from(path_and_query) {
        Ok(path_and_query) => Some(path_and_query),
        Err(_) => {
            return (axum::http::StatusCode::BAD_REQUEST, "Invalid gateway path").into_response();
        }
    };
    let rewritten_uri = match Uri::from_parts(parts) {
        Ok(uri) => uri,
        Err(_) => {
            return (axum::http::StatusCode::BAD_REQUEST, "Invalid gateway URI").into_response();
        }
    };
    *request.uri_mut() = rewritten_uri;

    // The outer `/*path` route has already inserted its wildcard into Axum's
    // private URL-parameter extension. Re-dispatching the same request through
    // the inner router would append the inner route parameter, so handlers such
    // as `Path<String>` would see two values for a one-parameter route.
    // Rebuild the routing extensions before the second dispatch while retaining
    // the connection address used by login auditing and Hyper's pending upgrade.
    let connect_info = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .copied();
    let on_upgrade = request
        .extensions_mut()
        .remove::<hyper::upgrade::OnUpgrade>();
    request.extensions_mut().clear();
    if let Some(connect_info) = connect_info {
        request.extensions_mut().insert(connect_info);
    }
    if let Some(on_upgrade) = on_upgrade {
        request.extensions_mut().insert(on_upgrade);
    }

    match state.router.oneshot(request).await {
        Ok(response) if should_rewrite_html => rewrite_gateway_html(response, &state.prefix).await,
        Ok(response) => response,
        Err(error) => match error {},
    }
}

fn is_plugin_asset_path(path: &str) -> bool {
    path.starts_with("/api/v1/plugin-assets/") || path.starts_with("/api/plugin-assets/")
}

fn rewrite_root_relative_attribute(html: &str, attribute: &str, prefix: &str) -> String {
    let root_attribute = format!("{attribute}=\"/");
    let prefixed_attribute = format!("{attribute}=\"{prefix}");
    let mut output = String::with_capacity(html.len());
    let mut remaining = html;

    while let Some(index) = remaining.find(&root_attribute) {
        output.push_str(&remaining[..index]);
        let candidate = &remaining[index..];

        if candidate.starts_with(&prefixed_attribute) {
            output.push_str(&prefixed_attribute);
            remaining = &candidate[prefixed_attribute.len()..];
        } else {
            output.push_str(&prefixed_attribute);
            remaining = &candidate[root_attribute.len()..];
        }
    }

    output.push_str(remaining);
    output
}

async fn ensure_manifest_link(mut request: Request, next: Next) -> Response {
    if is_plugin_asset_path(request.uri().path()) {
        return next.run(request).await;
    }

    // HTML receives a fresh CSP nonce and rewritten body for every navigation.
    // A 304 would reuse the cached body while the security layer supplies a
    // nonce-free CSP, preventing the sandboxed plugin scripts from executing.
    if request
        .headers()
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
    {
        request.headers_mut().remove(header::IF_NONE_MATCH);
        request.headers_mut().remove(header::IF_MODIFIED_SINCE);
    }

    let is_manifest_request = request.uri().path().ends_with("/manifest.webmanifest");
    if is_manifest_request && request.uri().path() != "/manifest.webmanifest" {
        let parent_depth = request
            .uri()
            .path()
            .trim_matches('/')
            .split('/')
            .count()
            .saturating_sub(1);
        let location = format!("{}manifest.webmanifest", "../".repeat(parent_depth));
        return Response::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header(header::LOCATION, location)
            .body(Body::empty())
            .expect("manifest redirect response is valid");
    }
    let response = next.run(request).await;
    if is_manifest_request {
        return response;
    }

    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.starts_with("text/html"))
        .unwrap_or(false);

    if !is_html {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, 8 * 1024 * 1024).await else {
        return Response::from_parts(parts, Body::empty());
    };
    let Ok(html) = String::from_utf8(bytes.to_vec()) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let nonce = create_document_script_nonce();
    let html = ensure_spa_asset_base(&html);
    let rewritten = inject_document_script_nonce(&ensure_manifest_link_in_html(&html), &nonce);
    let csp = document_content_security_policy(&nonce);
    parts.headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&csp).expect("document CSP is valid"),
    );
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    parts.headers.remove(header::ETAG);
    parts.headers.remove(header::LAST_MODIFIED);

    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(rewritten))
}

fn create_document_script_nonce() -> String {
    let mut bytes = [0_u8; 24];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn ensure_spa_asset_base(html: &str) -> String {
    // Relative Vite assets must resolve from the application root when a user
    // opens or reloads a nested route such as /book/:id.
    if html.contains("id=\"root\"") && !html.contains("<base ") {
        html.replacen("<head>", "<head><base href=\"/\">", 1)
    } else {
        html.to_string()
    }
}

fn document_content_security_policy(nonce: &str) -> String {
    format!(
        "default-src 'self'; script-src 'self' 'nonce-{nonce}'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self'; connect-src 'self'; media-src 'self' https: http:; object-src 'none'; frame-ancestors 'none';"
    )
}

fn inject_document_script_nonce(html: &str, nonce: &str) -> String {
    let meta = format!(r#"<meta name="ting-csp-nonce" content="{nonce}">"#);
    if let Some(index) = html.find("</head>") {
        let mut rewritten = String::with_capacity(html.len() + meta.len());
        rewritten.push_str(&html[..index]);
        rewritten.push_str(&meta);
        rewritten.push_str(&html[index..]);
        rewritten
    } else {
        format!("{meta}{html}")
    }
}

fn ensure_manifest_link_in_html(html: &str) -> String {
    let manifest_start = ["<link rel=\"manifest", "<link rel='manifest"]
        .into_iter()
        .filter_map(|marker| html.find(marker))
        .min();

    if let Some(start) = manifest_start {
        let Some(relative_end) = html[start..].find('>') else {
            return html.to_string();
        };
        let end = start + relative_end;
        let tag = &html[start..=end];
        let mut rewritten_tag = tag.to_string();
        if !rewritten_tag.contains("crossorigin=") {
            let insert_at = rewritten_tag
                .rfind("/>")
                .unwrap_or_else(|| rewritten_tag.len() - 1);
            rewritten_tag.insert_str(insert_at, " crossorigin=\"use-credentials\"");
        }
        if rewritten_tag == tag {
            return html.to_string();
        }

        let mut output = String::with_capacity(html.len());
        output.push_str(&html[..start]);
        output.push_str(&rewritten_tag);
        output.push_str(&html[end + 1..]);
        return output;
    }

    let manifest_link =
        r#"<link rel="manifest" href="./manifest.webmanifest" crossorigin="use-credentials">"#;
    if let Some(index) = html.find("</head>") {
        let mut rewritten = String::with_capacity(html.len() + manifest_link.len());
        rewritten.push_str(&html[..index]);
        rewritten.push_str(manifest_link);
        rewritten.push_str(&html[index..]);
        rewritten
    } else {
        format!("{manifest_link}{html}")
    }
}

async fn rewrite_gateway_html(response: Response, prefix: &str) -> Response {
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.starts_with("text/html"))
        .unwrap_or(false);

    if !is_html {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, 8 * 1024 * 1024).await else {
        return Response::from_parts(parts, Body::empty());
    };
    let Ok(mut html) = String::from_utf8(bytes.to_vec()) else {
        return Response::from_parts(parts, Body::from(bytes));
    };

    // The Docker image is also used for direct port access, so its Vite build
    // keeps root-relative assets. Rewrite only gateway HTML responses to the
    // registered prefix, keeping the direct TCP page fully compatible.
    let asset_prefix = format!("{prefix}/");
    html = ensure_manifest_link_in_html(&html);
    html = rewrite_root_relative_attribute(&html, "src", &asset_prefix);
    html = rewrite_root_relative_attribute(&html, "href", &asset_prefix);
    if !html.contains("<base ") {
        html = html.replacen(
            "<head>",
            &format!("<head><base href=\"{asset_prefix}\">"),
            1,
        );
    }

    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(html))
}

/// Health check endpoint handler
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
