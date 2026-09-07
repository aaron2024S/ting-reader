//! JavaScript Plugin Bindings
//!
//! This module provides the bridge between Rust and JavaScript for plugin functionality.
//! It implements:
//! - ScraperPlugin trait bindings for JavaScript plugins
//! - Rust function exports (logging, config, events) for JavaScript to call
//! - Data type conversion between Rust and JavaScript
//! - Async function support (Promise ↔ Future)

use anyhow::{Context, Result};
use futures::StreamExt;
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE,
    LOCATION, PROXY_AUTHORIZATION, TRANSFER_ENCODING,
};
use reqwest::{Method, StatusCode, Url};
use serde::Serialize;
use serde_json::Value;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tracing::{debug, error, info, warn};

use super::super::scraper::{Chapter, SearchResult};
use super::super::types::{
    PluginContext, PluginEventBus, PluginLogContext, PluginLogSource, PluginLogger, PluginMetadata,
    PluginType,
};
use super::npm::NpmDependency;
use super::plugin::JavaScriptPluginExecutor;
use crate::plugin::logger::{DefaultPluginLogger, PluginLogLevel};
use crate::plugin::{PluginHostGatewayHandle, PluginHostUser};

const MAX_JS_FETCH_REDIRECTS: usize = 5;
const MAX_JS_FETCH_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_JS_FETCH_TIMEOUT: Duration = Duration::from_secs(180);

/// JavaScript Scraper Plugin Adapter
///
/// This adapter wraps a JavaScriptPluginExecutor and implements the ScraperPlugin trait,
/// allowing JavaScript plugins to be used as scraper plugins.
///
/// Note: This struct is NOT Send + Sync because it contains a JavaScriptPluginExecutor
/// which wraps a Deno JsRuntime (V8 isolates are single-threaded).
pub struct JsScraperPlugin {
    executor: JavaScriptPluginExecutor,
    metadata: PluginMetadata,
}

impl JsScraperPlugin {
    /// Create a new JavaScript scraper plugin adapter
    pub fn new(executor: JavaScriptPluginExecutor) -> Self {
        let metadata = executor.metadata().clone();
        Self { executor, metadata }
    }
}

// Note: We cannot implement Plugin trait directly because it requires Send + Sync
// Instead, we provide similar methods that can be called in a single-threaded context

impl JsScraperPlugin {
    /// Get plugin metadata (similar to Plugin::metadata)
    pub fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    /// Initialize the plugin (similar to Plugin::initialize)
    pub async fn initialize(&mut self, context: &PluginContext) -> Result<()> {
        self.executor
            .initialize(context.config.clone(), context.data_dir.clone())
            .await
    }

    /// Shutdown the plugin (similar to Plugin::shutdown)
    pub fn shutdown(&mut self) -> Result<()> {
        self.executor.shutdown()
    }

    /// Get the plugin type (similar to Plugin::plugin_type)
    pub fn plugin_type(&self) -> PluginType {
        self.metadata.plugin_type
    }
}

// Implement ScraperPlugin methods (but not the trait itself due to Send + Sync requirement)
impl JsScraperPlugin {
    /// Search for books by keyword
    pub async fn search(&mut self, query: &str, page: u32) -> Result<SearchResult> {
        debug!("JavaScript plugin search: query={}, page={}", query, page);

        #[derive(Serialize)]
        struct SearchArgs {
            query: String,
            page: u32,
        }

        let args = SearchArgs {
            query: query.to_string(),
            page,
        };

        self.executor
            .call_function("search", args)
            .await
            .context("Failed to call JavaScript search function")
    }

    /// Get the list of chapters for a book
    pub async fn get_chapters(&mut self, book_id: &str) -> Result<Vec<Chapter>> {
        debug!("JavaScript plugin get_chapters: book_id={}", book_id);

        #[derive(Serialize)]
        struct ChaptersArgs {
            book_id: String,
        }

        let args = ChaptersArgs {
            book_id: book_id.to_string(),
        };

        self.executor
            .call_function("getChapters", args)
            .await
            .context("Failed to call JavaScript getChapters function")
    }

    /// Download a cover image
    pub async fn download_cover(&mut self, cover_url: &str) -> Result<Vec<u8>> {
        debug!("JavaScript plugin download_cover: url={}", cover_url);

        #[derive(Serialize)]
        struct CoverArgs {
            cover_url: String,
        }

        let args = CoverArgs {
            cover_url: cover_url.to_string(),
        };

        // JavaScript returns { data: "base64...", content_type: "..." }
        // We need to extract the data field
        let result_obj: serde_json::Value = self
            .executor
            .call_function("downloadCover", args)
            .await
            .context("Failed to call JavaScript downloadCover function")?;

        let base64_data = if let Some(data) = result_obj.get("data").and_then(|v| v.as_str()) {
            data.to_string()
        } else if let Some(s) = result_obj.as_str() {
            // Fallback for legacy plugins that return string directly
            s.to_string()
        } else {
            return Err(anyhow::anyhow!(
                "Invalid response format from downloadCover: missing 'data' field"
            ));
        };

        // Decode base64 to bytes
        use base64::{engine::general_purpose, Engine as _};
        general_purpose::STANDARD
            .decode(&base64_data)
            .context("Failed to decode base64 cover data")
    }

    /// Get the audio download URL for a chapter
    pub async fn get_audio_url(&mut self, chapter_id: &str) -> Result<String> {
        debug!("JavaScript plugin get_audio_url: chapter_id={}", chapter_id);

        #[derive(Serialize)]
        struct AudioUrlArgs {
            chapter_id: String,
        }

        let args = AudioUrlArgs {
            chapter_id: chapter_id.to_string(),
        };

        self.executor
            .call_function("getAudioUrl", args)
            .await
            .context("Failed to call JavaScript getAudioUrl function")
    }
}

// ============================================================================
// Rust Functions Exported to JavaScript (Helper Functions)
// ============================================================================

/// Plugin logger implementation for JavaScript plugins
#[derive(Clone)]
pub struct JsPluginLogger {
    plugin_name: String,
}

impl JsPluginLogger {
    pub fn new(plugin_name: String) -> Self {
        Self { plugin_name }
    }
}

impl PluginLogger for JsPluginLogger {
    fn debug(&self, message: &str) {
        debug!(plugin = %self.plugin_name, "{}", message);
    }

    fn info(&self, message: &str) {
        info!(plugin = %self.plugin_name, "{}", message);
    }

    fn warn(&self, message: &str) {
        warn!(plugin = %self.plugin_name, "{}", message);
    }

    fn error(&self, message: &str) {
        error!(plugin = %self.plugin_name, "{}", message);
    }
}

/// Plugin event bus implementation for JavaScript plugins
#[derive(Clone)]
pub struct JsPluginEventBus {
    plugin_name: String,
}

impl JsPluginEventBus {
    pub fn new(plugin_name: String) -> Self {
        Self { plugin_name }
    }
}

impl PluginEventBus for JsPluginEventBus {
    fn publish(&self, event_type: &str, _data: Value) -> crate::core::error::Result<()> {
        info!(
            plugin = %self.plugin_name,
            event_type = %event_type,
            "Publishing event"
        );
        // TODO: Implement actual event publishing when event bus is available
        Ok(())
    }

    fn subscribe(
        &self,
        event_type: &str,
        _handler: Box<dyn Fn(Value) + Send + Sync>,
    ) -> crate::core::error::Result<String> {
        info!(
            plugin = %self.plugin_name,
            event_type = %event_type,
            "Subscribing to event"
        );
        // TODO: Implement actual event subscription when event bus is available
        Ok(format!("sub_{}_{}", self.plugin_name, event_type))
    }

    fn unsubscribe(&self, subscription_id: &str) -> crate::core::error::Result<()> {
        info!(
            plugin = %self.plugin_name,
            subscription_id = %subscription_id,
            "Unsubscribing from event"
        );
        // TODO: Implement actual event unsubscription when event bus is available
        Ok(())
    }
}

#[derive(Clone)]
struct JsHostGatewayState {
    plugin_id: String,
    host_gateway: Option<PluginHostGatewayHandle>,
}

#[derive(Clone)]
struct JsPluginLogState {
    logger: DefaultPluginLogger,
}

#[derive(Clone, Default)]
pub struct JsHostInvocationContext {
    pub user: Option<PluginHostUser>,
}

#[derive(Clone)]
struct JsFetchOptions {
    method: Method,
    headers: HeaderMap,
    body: Option<String>,
    timeout: Duration,
}

impl Default for JsFetchOptions {
    fn default() -> Self {
        Self {
            method: Method::GET,
            headers: HeaderMap::new(),
            body: None,
            timeout: DEFAULT_JS_FETCH_TIMEOUT,
        }
    }
}

fn build_js_fetch_client(url: &Url, resolved: &[SocketAddr]) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(DEFAULT_JS_FETCH_TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy();
    if matches!(url.host(), Some(url::Host::Domain(_))) {
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("Plugin fetch URL is missing a host"))?;
        builder = builder.resolve_to_addrs(host, resolved);
    }
    builder
        .build()
        .context("Failed to build plugin fetch client")
}

fn parse_js_fetch_options(options: Option<&Value>) -> Result<JsFetchOptions> {
    let Some(options) = options else {
        return Ok(JsFetchOptions::default());
    };

    let mut parsed = JsFetchOptions::default();
    if let Some(method) = options.get("method").and_then(Value::as_str) {
        parsed.method = match method.to_ascii_uppercase().as_str() {
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "DELETE" => Method::DELETE,
            _ => Method::GET,
        };
    }

    if let Some(headers) = options.get("headers").and_then(Value::as_object) {
        for (name, value) in headers {
            let Some(value) = value.as_str() else {
                continue;
            };
            let name = HeaderName::from_bytes(name.as_bytes())
                .context("Plugin fetch contains an invalid header name")?;
            let value = HeaderValue::from_str(value)
                .context("Plugin fetch contains an invalid header value")?;
            parsed.headers.insert(name, value);
        }
    }

    parsed.body = options
        .get("body")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(timeout_ms) = options.get("timeout_ms").and_then(Value::as_u64) {
        parsed.timeout = Duration::from_millis(timeout_ms.clamp(1_000, 600_000));
    }

    Ok(parsed)
}

fn redacted_js_fetch_url(url: &Url) -> String {
    let mut redacted = url.clone();
    if redacted.set_username("").is_err() || redacted.set_password(None).is_err() {
        return "<invalid-url>".to_string();
    }
    redacted.set_query(None);
    redacted.set_fragment(None);
    redacted.to_string()
}

fn redacted_js_fetch_url_str(url: &str) -> String {
    Url::parse(url)
        .map(|parsed| redacted_js_fetch_url(&parsed))
        .unwrap_or_else(|_| "<invalid-url>".to_string())
}

async fn validate_js_fetch_target(
    allowed_domains: &[String],
    url: &Url,
) -> Result<Vec<SocketAddr>> {
    let redacted_url = redacted_js_fetch_url(url);
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("Network access denied: unsupported URL scheme for {redacted_url}");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("Network access denied: URL credentials are not allowed");
    }
    if !is_network_allowed(allowed_domains, url.as_str()) {
        anyhow::bail!("Network access denied for {redacted_url}");
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Network access denied: URL is missing a host"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("Network access denied: URL uses an unknown port"))?;
    if port == 0 {
        anyhow::bail!("Network access denied: port 0 is not allowed");
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }

    let resolved = tokio::net::lookup_host((host.as_str(), port))
        .await
        .with_context(|| format!("Failed to resolve plugin fetch host {host}"))?
        .collect::<Vec<_>>();
    if resolved.is_empty() {
        anyhow::bail!("Network access denied: host did not resolve to an address");
    }

    Ok(resolved)
}

fn is_followable_js_fetch_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

fn same_url_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

fn strip_cross_origin_fetch_headers(headers: &mut HeaderMap) {
    headers.remove(AUTHORIZATION);
    headers.remove(COOKIE);
    headers.remove(PROXY_AUTHORIZATION);
}

fn apply_js_fetch_redirect_semantics(
    status: StatusCode,
    method: &mut Method,
    headers: &mut HeaderMap,
    body: &mut Option<String>,
) {
    let switch_to_get = status == StatusCode::SEE_OTHER
        || (matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND)
            && *method == Method::POST);
    if switch_to_get {
        *method = Method::GET;
        *body = None;
        headers.remove(CONTENT_LENGTH);
        headers.remove(CONTENT_TYPE);
        headers.remove(TRANSFER_ENCODING);
    }
}

fn checked_js_fetch_body_len(current: usize, additional: usize) -> Result<usize> {
    let next = current
        .checked_add(additional)
        .ok_or_else(|| anyhow::anyhow!("Plugin fetch response is too large"))?;
    if next > MAX_JS_FETCH_RESPONSE_BYTES {
        anyhow::bail!(
            "Plugin fetch response exceeds the {} MiB limit",
            MAX_JS_FETCH_RESPONSE_BYTES / (1024 * 1024)
        );
    }
    Ok(next)
}

async fn execute_js_fetch(
    raw_url: &str,
    options: Option<&Value>,
    allowed_domains: &[String],
) -> Result<String> {
    let mut current_url = Url::parse(raw_url).map_err(|error| {
        anyhow::anyhow!(
            "Invalid plugin fetch URL {}: {error}",
            redacted_js_fetch_url_str(raw_url)
        )
    })?;
    let mut options = parse_js_fetch_options(options)?;
    let deadline = tokio::time::Instant::now() + options.timeout;
    let mut redirect_count = 0_usize;

    loop {
        let redacted_url = redacted_js_fetch_url(&current_url);
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .ok_or_else(|| anyhow::anyhow!("Plugin fetch timed out"))?;
        let resolved = match tokio::time::timeout(
            remaining,
            validate_js_fetch_target(allowed_domains, &current_url),
        )
        .await
        {
            Ok(Ok(resolved)) => resolved,
            Ok(Err(error)) => {
                warn!(
                    url = %redacted_url,
                    error = %error,
                    message_key = "plugin.fetch.target_rejected",
                    "Plugin fetch target rejected"
                );
                return Err(error);
            }
            Err(_) => return Err(anyhow::anyhow!("Plugin fetch timed out")),
        };

        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .ok_or_else(|| anyhow::anyhow!("Plugin fetch timed out"))?;
        info!(url = %redacted_url, "Plugin fetch request started");
        let client = build_js_fetch_client(&current_url, &resolved)?;
        let mut request = client
            .request(options.method.clone(), current_url.clone())
            .headers(options.headers.clone())
            .timeout(remaining);
        if let Some(body) = &options.body {
            request = request.body(body.clone());
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                let error = error.without_url().to_string();
                debug!(
                    url = %redacted_url,
                    error = %error,
                    message_key = "plugin.fetch.request_failed",
                    message_params = %serde_json::json!({ "error": &error }),
                    "Plugin fetch request failed"
                );
                return Err(anyhow::anyhow!("Plugin fetch request failed: {error}"));
            }
        };

        let remote = response.remote_addr().ok_or_else(|| {
            anyhow::anyhow!("Plugin fetch response did not expose its remote address")
        })?;
        if !resolved.iter().any(|address| address.ip() == remote.ip()) {
            warn!(
                url = %redacted_url,
                "Plugin fetch connected to an unexpected address"
            );
            anyhow::bail!("Network access denied: connected to an unexpected address");
        }

        let status = response.status();
        if is_followable_js_fetch_redirect(status) {
            if redirect_count >= MAX_JS_FETCH_REDIRECTS {
                anyhow::bail!(
                    "Plugin fetch exceeded the {} redirect limit",
                    MAX_JS_FETCH_REDIRECTS
                );
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| anyhow::anyhow!("Plugin fetch redirect omitted Location"))?;
            let next_url = current_url
                .join(location)
                .map_err(|_| anyhow::anyhow!("Plugin fetch returned an invalid redirect target"))?;
            if !same_url_origin(&current_url, &next_url) {
                strip_cross_origin_fetch_headers(&mut options.headers);
            }
            apply_js_fetch_redirect_semantics(
                status,
                &mut options.method,
                &mut options.headers,
                &mut options.body,
            );
            info!(
                from = %redacted_url,
                to = %redacted_js_fetch_url(&next_url),
                status = %status,
                "Plugin fetch following redirect"
            );
            current_url = next_url;
            redirect_count += 1;
            continue;
        }

        if response
            .content_length()
            .is_some_and(|length| length > MAX_JS_FETCH_RESPONSE_BYTES as u64)
        {
            anyhow::bail!(
                "Plugin fetch response exceeds the {} MiB limit",
                MAX_JS_FETCH_RESPONSE_BYTES / (1024 * 1024)
            );
        }

        let capacity = response
            .content_length()
            .unwrap_or_default()
            .min(MAX_JS_FETCH_RESPONSE_BYTES as u64) as usize;
        let mut body = Vec::with_capacity(capacity);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    let error = error.without_url().to_string();
                    debug!(
                        url = %redacted_url,
                        error = %error,
                        message_key = "plugin.fetch.body_read_failed",
                        message_params = %serde_json::json!({ "error": &error }),
                        "Plugin fetch body read failed"
                    );
                    return Err(anyhow::anyhow!(
                        "Plugin fetch response read failed: {error}"
                    ));
                }
            };
            checked_js_fetch_body_len(body.len(), chunk.len())?;
            body.extend_from_slice(&chunk);
        }

        let body = String::from_utf8_lossy(&body).into_owned();
        info!(
            url = %redacted_url,
            status = %status,
            body_length = body.len(),
            redirects = redirect_count,
            "Plugin fetch request completed"
        );
        return Ok(body);
    }
}

/// Helper to create a JavaScript runtime with plugin bindings
///
/// This function creates a Deno runtime and injects the Ting API into the global scope.
/// The Ting API provides logging, configuration access, and event bus functionality.
///
/// # Arguments
/// * `plugin_name` - Name of the plugin
/// * `config` - Plugin configuration
/// * `sandbox` - Optional sandbox for permission checking
pub fn create_js_runtime_with_bindings(
    plugin_name: String,
    plugin_id: String,
    config: Value,
    sandbox: Option<&crate::plugin::wasm::sandbox::Sandbox>,
    host_gateway: Option<PluginHostGatewayHandle>,
    plugin_dir: std::path::PathBuf,
    npm_dependencies: Vec<NpmDependency>,
) -> Result<deno_core::JsRuntime> {
    use deno_core::{op2, Extension, JsRuntime, Op, OpState, RuntimeOptions};
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::rc::Rc;

    #[derive(Clone)]
    struct JsFetchPermissions {
        allowed_domains: Vec<String>,
    }

    #[derive(Clone)]
    struct JsNpmModuleState {
        plugin_dir: PathBuf,
        allowed_packages: HashSet<String>,
    }

    let allowed_paths = sandbox
        .map(|s| {
            s.get_allowed_paths()
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let allowed_domains = sandbox
        .map(|s| s.get_allowed_domains().to_vec())
        .unwrap_or_default();

    #[op2]
    #[string]
    pub fn op_plugin_log(
        state: Rc<RefCell<OpState>>,
        #[string] level: String,
        #[string] message: String,
        #[serde] fields: Option<Value>,
    ) -> Result<String, anyhow::Error> {
        let level = PluginLogLevel::parse(&level)
            .ok_or_else(|| anyhow::anyhow!("Unsupported plugin log level"))?;
        let fields = match fields {
            None | Some(Value::Null) => None,
            Some(Value::Object(fields)) => Some(Value::Object(fields)),
            Some(_) => {
                return Err(anyhow::anyhow!("Plugin log fields must be a JSON object"));
            }
        };
        let logger = {
            let state = state.borrow();
            state
                .try_borrow::<JsPluginLogState>()
                .map(|log_state| log_state.logger.clone())
                .ok_or_else(|| anyhow::anyhow!("Plugin logger is not configured"))?
        };

        Ok(logger.log(level, &message, fields.as_ref()))
    }

    #[op2(async)]
    #[string]
    pub async fn op_fetch(
        state: Rc<RefCell<OpState>>,
        #[string] url: String,
        #[serde] options: Option<Value>,
    ) -> Result<String, anyhow::Error> {
        let allowed_domains = {
            let state = state.borrow();
            state
                .try_borrow::<JsFetchPermissions>()
                .map(|permissions| permissions.allowed_domains.clone())
                .unwrap_or_default()
        };

        execute_js_fetch(&url, options.as_ref(), &allowed_domains).await
    }

    #[op2(async)]
    #[serde]
    pub async fn op_host_invoke(
        state: Rc<RefCell<OpState>>,
        #[string] method: String,
        #[serde] params: serde_json::Value,
    ) -> Result<serde_json::Value, anyhow::Error> {
        let (plugin_id, host_gateway, user) = {
            let state = state.borrow();
            let host_state = state.try_borrow::<JsHostGatewayState>().cloned();
            let invocation_context = state
                .try_borrow::<JsHostInvocationContext>()
                .cloned()
                .unwrap_or_default();

            match host_state {
                Some(host_state) => (
                    host_state.plugin_id,
                    host_state.host_gateway.and_then(|handle| handle.get()),
                    invocation_context.user,
                ),
                None => (String::new(), None, invocation_context.user),
            }
        };

        let gateway = host_gateway.ok_or_else(|| {
            anyhow::anyhow!("Ting.host.invoke is not configured for this plugin runtime")
        })?;
        let user = user.ok_or_else(|| {
            anyhow::anyhow!("Ting.host.invoke requires an authenticated user context")
        })?;

        gateway
            .invoke_plugin(&plugin_id, &user, &method, params)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    #[op2]
    #[serde]
    pub fn op_require_module(
        state: Rc<RefCell<OpState>>,
        #[string] request: String,
        #[string] parent_path: String,
    ) -> Result<serde_json::Value, anyhow::Error> {
        let npm_state = {
            let state = state.borrow();
            state
                .try_borrow::<JsNpmModuleState>()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("npm module state is not configured"))?
        };

        let module_path = resolve_js_module(
            &npm_state.plugin_dir,
            &npm_state.allowed_packages,
            &request,
            &parent_path,
        )?;
        let canonical = std::fs::canonicalize(&module_path)?;
        ensure_path_inside(&npm_state.plugin_dir, &canonical)?;

        let code = if canonical
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            let json_text = std::fs::read_to_string(&canonical)?;
            let parsed: serde_json::Value = serde_json::from_str(&json_text)?;
            format!(
                "module.exports = {};",
                serde_json::to_string(&parsed).unwrap_or_else(|_| "null".to_string())
            )
        } else {
            std::fs::read_to_string(&canonical)?
        };

        let dirname = canonical
            .parent()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default();
        let filename = canonical.to_string_lossy().to_string();

        Ok(serde_json::json!({
            "id": filename,
            "filename": filename,
            "dirname": dirname,
            "code": code,
        }))
    }

    let ext = Extension {
        name: "ting_fetch",
        ops: std::borrow::Cow::Owned(vec![
            op_plugin_log::DECL,
            op_fetch::DECL,
            op_host_invoke::DECL,
            op_require_module::DECL,
        ]),
        ..Default::default()
    };

    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![ext],
        ..Default::default()
    });
    runtime.op_state().borrow_mut().put(JsFetchPermissions {
        allowed_domains: allowed_domains.clone(),
    });
    let (stable_plugin_id, plugin_version) = split_plugin_instance_id(&plugin_id);
    runtime.op_state().borrow_mut().put(JsPluginLogState {
        logger: DefaultPluginLogger::from_context(PluginLogContext {
            plugin_id: stable_plugin_id,
            plugin_instance_id: plugin_id.clone(),
            plugin_name: plugin_name.clone(),
            plugin_version,
            runtime: "javascript".to_string(),
            source: PluginLogSource::Code,
        }),
    });
    runtime.op_state().borrow_mut().put(JsHostGatewayState {
        plugin_id: plugin_id.clone(),
        host_gateway,
    });
    runtime
        .op_state()
        .borrow_mut()
        .put(JsHostInvocationContext::default());
    runtime.op_state().borrow_mut().put(JsNpmModuleState {
        plugin_dir: plugin_dir.clone(),
        allowed_packages: npm_dependencies
            .iter()
            .map(|dependency| dependency.name.clone())
            .collect(),
    });

    let init_code = super::init_code::generate_init_code(
        &plugin_name,
        &config,
        &allowed_paths,
        &allowed_domains,
    );

    runtime
        .execute_script("<init_bindings>", init_code.into())
        .context("Failed to initialize JavaScript bindings")?;

    Ok(runtime)
}

fn split_plugin_instance_id(instance_id: &str) -> (String, String) {
    match instance_id.rsplit_once('@') {
        Some((plugin_id, version)) if !plugin_id.is_empty() && !version.is_empty() => {
            (plugin_id.to_string(), version.to_string())
        }
        _ => (instance_id.to_string(), "unknown".to_string()),
    }
}

fn resolve_js_module(
    plugin_dir: &std::path::Path,
    allowed_packages: &std::collections::HashSet<String>,
    request: &str,
    parent_path: &str,
) -> Result<std::path::PathBuf, anyhow::Error> {
    let request = request.trim();
    if request.is_empty() {
        return Err(anyhow::anyhow!("require() needs a module name"));
    }

    if is_relative_module_request(request) {
        let base_dir = if parent_path.trim().is_empty() {
            plugin_dir.to_path_buf()
        } else {
            let parent = std::path::PathBuf::from(parent_path);
            parent
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| plugin_dir.to_path_buf())
        };
        return resolve_module_path(&base_dir.join(request));
    }

    if request.starts_with('/') || request.contains('\\') {
        return Err(anyhow::anyhow!(
            "require() only accepts declared package names or relative paths"
        ));
    }

    let package_name = npm_package_name(request)?;
    if !allowed_packages.contains(&package_name) {
        return Err(anyhow::anyhow!(
            "npm package '{}' is not declared in npm_dependencies",
            package_name
        ));
    }

    let package_root = plugin_dir.join("node_modules").join(&package_name);
    let remainder = request
        .strip_prefix(&package_name)
        .unwrap_or_default()
        .trim_start_matches('/');
    if remainder.is_empty() {
        resolve_module_path(&package_root)
    } else {
        resolve_module_path(&package_root.join(remainder))
    }
}

fn is_relative_module_request(request: &str) -> bool {
    request == "." || request == ".." || request.starts_with("./") || request.starts_with("../")
}

fn npm_package_name(request: &str) -> Result<String, anyhow::Error> {
    let mut parts = request.split('/');
    let first = parts.next().unwrap_or_default();
    if first.is_empty() || first == "." || first == ".." {
        return Err(anyhow::anyhow!("Invalid npm package name '{}'", request));
    }
    if first.starts_with('@') {
        let second = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("Invalid scoped npm package '{}'", request))?;
        if second.is_empty() || second == "." || second == ".." {
            return Err(anyhow::anyhow!("Invalid scoped npm package '{}'", request));
        }
        Ok(format!("{}/{}", first, second))
    } else {
        Ok(first.to_string())
    }
}

fn resolve_module_path(base: &std::path::Path) -> Result<std::path::PathBuf, anyhow::Error> {
    if base.is_file() {
        return Ok(base.to_path_buf());
    }

    for extension in ["js", "json"] {
        let candidate = base.with_extension(extension);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if base.is_dir() {
        let package_json = base.join("package.json");
        if package_json.is_file() {
            let package: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&package_json)?)?;
            if let Some(main) = package.get("main").and_then(serde_json::Value::as_str)
                && !main.trim().is_empty()
                    && let Ok(path) = resolve_module_path(&base.join(main)) {
                        return Ok(path);
                    }
        }

        for index in ["index.js", "index.json"] {
            let candidate = base.join(index);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(anyhow::anyhow!(
        "Cannot resolve JavaScript module '{}'",
        base.display()
    ))
}

fn ensure_path_inside(root: &std::path::Path, path: &std::path::Path) -> Result<(), anyhow::Error> {
    let canonical_root = std::fs::canonicalize(root)?;
    if path.starts_with(&canonical_root) {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Resolved JavaScript module escapes plugin directory"
        ))
    }
}

fn is_network_allowed(allowed_domains: &[String], url: &str) -> bool {
    if allowed_domains.is_empty() {
        return false;
    }

    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };

    allowed_domains
        .iter()
        .any(|domain| domain_matches(host, domain))
}

fn domain_matches(host: &str, pattern: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();

    if pattern == "*" {
        true
    } else if let Some(base) = pattern.strip_prefix("*.") {
        host == base || host.ends_with(&format!(".{}", base))
    } else {
        host == pattern
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_js_plugin_logger() {
        let logger = JsPluginLogger::new("test-plugin".to_string());
        logger.debug("Debug message");
        logger.info("Info message");
        logger.warn("Warning message");
        logger.error("Error message");
    }

    #[test]
    fn test_js_plugin_event_bus() {
        let event_bus = JsPluginEventBus::new("test-plugin".to_string());
        let result = event_bus.publish("test_event", serde_json::json!({"key": "value"}));
        assert!(result.is_ok());

        let handler = Box::new(|_data: Value| {});
        let result = event_bus.subscribe("test_event", handler);
        assert!(result.is_ok());

        let sub_id = result.unwrap();
        let result = event_bus.unsubscribe(&sub_id);
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_js_runtime_with_bindings() {
        let config = serde_json::json!({"api_key": "test_key", "cache_enabled": true});
        let temp_dir = tempfile::tempdir().unwrap();
        let result = create_js_runtime_with_bindings(
            "test-plugin".to_string(),
            "test-plugin@1.0.0".to_string(),
            config,
            None,
            None,
            temp_dir.path().to_path_buf(),
            Vec::new(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn js_plugin_logs_keep_legacy_signature_and_accept_fields() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut runtime = create_js_runtime_with_bindings(
            "test-plugin".to_string(),
            "stable-plugin@1.0.0".to_string(),
            serde_json::json!({}),
            None,
            None,
            temp_dir.path().to_path_buf(),
            Vec::new(),
        )
        .unwrap();

        let result = runtime.execute_script(
            "<plugin_log_compatibility>",
            r#"
            Ting.log.info("legacy message");
            Ting.log.warn("structured message", { code: 42 });
            console.error("console message", { retryable: false });
            "#
            .to_string()
            .into(),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn plugin_instance_id_is_split_into_stable_id_and_version() {
        assert_eq!(
            split_plugin_instance_id("demo-plugin@1.2.3"),
            ("demo-plugin".to_string(), "1.2.3".to_string())
        );
        assert_eq!(
            split_plugin_instance_id("demo-plugin"),
            ("demo-plugin".to_string(), "unknown".to_string())
        );
    }

    #[test]
    fn js_op_fetch_network_permission_denies_by_default() {
        assert!(!is_network_allowed(&[], "https://example.com"));
        assert!(is_network_allowed(
            &["example.com".to_string()],
            "https://example.com/path"
        ));
        assert!(is_network_allowed(
            &["*.example.com".to_string()],
            "https://api.example.com/path"
        ));
        assert!(is_network_allowed(
            &["*".to_string()],
            "https://plugins.example.net/path"
        ));
        assert!(!is_network_allowed(
            &["example.com".to_string()],
            "https://evil.example.net/path"
        ));
        assert!(!is_network_allowed(
            &["example.com".to_string()],
            "ftp://example.com/archive"
        ));
        assert!(!is_network_allowed(
            &["example.com".to_string()],
            "https://user:secret@example.com/private"
        ));
    }

    #[test]
    fn js_fetch_urls_are_redacted_before_logging() {
        let url =
            Url::parse("https://user:secret@[2606:4700:4700::1111]:8443/plugin?token=secret#part")
                .unwrap();
        assert_eq!(
            redacted_js_fetch_url(&url),
            "https://[2606:4700:4700::1111]:8443/plugin"
        );
        assert_eq!(
            redacted_js_fetch_url_str("not a URL?token=secret"),
            "<invalid-url>"
        );
    }

    #[tokio::test]
    async fn js_fetch_target_validation_allows_private_addresses_with_permission() {
        let allowed_domains = vec!["*".to_string()];
        for raw_url in [
            "http://127.0.0.1/private",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/private",
        ] {
            assert!(
                validate_js_fetch_target(&allowed_domains, &Url::parse(raw_url).unwrap())
                    .await
                    .is_ok()
            );
        }

        validate_js_fetch_target(
            &allowed_domains,
            &Url::parse("https://8.8.8.8/resource").unwrap(),
        )
        .await
        .unwrap();
    }

    #[test]
    fn js_fetch_redirects_drop_sensitive_headers_and_post_bodies() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        headers.insert(COOKIE, HeaderValue::from_static("session=secret"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("2"));
        strip_cross_origin_fetch_headers(&mut headers);
        assert!(!headers.contains_key(AUTHORIZATION));
        assert!(!headers.contains_key(COOKIE));

        let mut method = Method::POST;
        let mut body = Some("{}".to_string());
        apply_js_fetch_redirect_semantics(StatusCode::FOUND, &mut method, &mut headers, &mut body);
        assert_eq!(method, Method::GET);
        assert!(body.is_none());
        assert!(!headers.contains_key(CONTENT_TYPE));
        assert!(!headers.contains_key(CONTENT_LENGTH));
    }

    #[test]
    fn js_fetch_response_limit_is_enforced_without_overflow() {
        assert_eq!(
            checked_js_fetch_body_len(MAX_JS_FETCH_RESPONSE_BYTES - 1, 1).unwrap(),
            MAX_JS_FETCH_RESPONSE_BYTES
        );
        assert!(checked_js_fetch_body_len(MAX_JS_FETCH_RESPONSE_BYTES, 1).is_err());
        assert!(checked_js_fetch_body_len(usize::MAX, 1).is_err());
    }

    #[tokio::test]
    async fn js_fetch_client_pins_dns_and_does_not_follow_redirects_automatically() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://example.com/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });

        let url = Url::parse(&format!(
            "http://plugin-fetch.test:{}/start?token=secret",
            address.port()
        ))
        .unwrap();
        let client = build_js_fetch_client(&url, &[address]).unwrap();
        let response = client.get(url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.url().host_str(), Some("plugin-fetch.test"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn direct_js_op_fetch_is_denied_without_network_permission() {
        let config = serde_json::json!({});
        let temp_dir = tempfile::tempdir().unwrap();
        let mut runtime = create_js_runtime_with_bindings(
            "test-plugin".to_string(),
            "test-plugin@1.0.0".to_string(),
            config,
            None,
            None,
            temp_dir.path().to_path_buf(),
            Vec::new(),
        )
        .unwrap();

        let result = runtime.execute_script(
            "<direct_op_fetch>",
            r#"
            globalThis.__directFetchStatus = "pending";
            Deno.core.ops.op_fetch("https://example.com", {})
                .then(() => { globalThis.__directFetchStatus = "success"; })
                .catch((error) => { globalThis.__directFetchStatus = String(error); });
            "#
            .to_string()
            .into(),
        );
        assert!(result.is_ok());

        runtime.run_event_loop(Default::default()).await.unwrap();

        let scope = &mut runtime.handle_scope();
        let context = scope.get_current_context();
        let global = context.global(scope);
        let key = deno_core::v8::String::new(scope, "__directFetchStatus").unwrap();
        let value = global.get(scope, key.into()).unwrap();
        let status = value.to_string(scope).unwrap().to_rust_string_lossy(scope);

        assert!(status.contains("Network access denied"));
    }

    #[tokio::test]
    async fn ting_host_invoke_rejects_when_gateway_missing() {
        let config = serde_json::json!({});
        let temp_dir = tempfile::tempdir().unwrap();
        let mut runtime = create_js_runtime_with_bindings(
            "test-plugin".to_string(),
            "test-plugin@1.0.0".to_string(),
            config,
            None,
            None,
            temp_dir.path().to_path_buf(),
            Vec::new(),
        )
        .unwrap();

        let result = runtime.execute_script(
            "<host_invoke_without_gateway>",
            r#"
            globalThis.__hostInvokeStatus = "pending";
            Ting.host.invoke("books.list", {})
                .then(() => { globalThis.__hostInvokeStatus = "success"; })
                .catch((error) => { globalThis.__hostInvokeStatus = String(error); });
            "#
            .to_string()
            .into(),
        );
        assert!(result.is_ok());

        runtime.run_event_loop(Default::default()).await.unwrap();

        let scope = &mut runtime.handle_scope();
        let context = scope.get_current_context();
        let global = context.global(scope);
        let key = deno_core::v8::String::new(scope, "__hostInvokeStatus").unwrap();
        let value = global.get(scope, key.into()).unwrap();
        let status = value.to_string(scope).unwrap().to_rust_string_lossy(scope);

        assert!(status.contains("Ting.host.invoke is not configured"));
    }

    #[test]
    fn test_js_runtime_sandbox_file_paths() {
        use crate::plugin::wasm::sandbox::{Permission, ResourceLimits, Sandbox};
        use std::path::PathBuf;

        let config = serde_json::json!({});
        let permissions = vec![
            Permission::FileRead(PathBuf::from("./data/cache")),
            Permission::FileWrite(PathBuf::from("./data/output")),
        ];
        let sandbox = Sandbox::new(permissions, ResourceLimits::default());

        let mut runtime = create_js_runtime_with_bindings(
            "test-plugin".to_string(),
            "test-plugin@1.0.0".to_string(),
            config,
            Some(&sandbox),
            None,
            tempfile::tempdir().unwrap().path().to_path_buf(),
            Vec::new(),
        )
        .unwrap();

        let test_code = r#"
            const allowedPaths = Ting.sandbox.allowedPaths;
            JSON.stringify({ allowedPaths })
        "#;
        let result = runtime.execute_script("<test_sandbox>", test_code.to_string().into());
        assert!(result.is_ok());
    }

    #[test]
    fn js_require_loads_declared_commonjs_package() {
        let temp_dir = tempfile::tempdir().unwrap();
        let package_dir = temp_dir.path().join("node_modules").join("demo-package");
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::write(
            package_dir.join("package.json"),
            r#"{ "name": "demo-package", "main": "main.js" }"#,
        )
        .unwrap();
        std::fs::write(
            package_dir.join("main.js"),
            r#"const util = require("./util"); module.exports = { answer: util.answer + 1 };"#,
        )
        .unwrap();
        std::fs::write(package_dir.join("util.js"), r#"exports.answer = 41;"#).unwrap();

        let mut runtime = create_js_runtime_with_bindings(
            "test-plugin".to_string(),
            "test-plugin@1.0.0".to_string(),
            serde_json::json!({}),
            None,
            None,
            temp_dir.path().to_path_buf(),
            vec![NpmDependency::new(
                "demo-package".to_string(),
                "1.0.0".to_string(),
            )],
        )
        .unwrap();

        runtime
            .execute_script(
                "<declared_npm_require>",
                r#"
                globalThis.__npmRequireAnswer = String(require("demo-package").answer);
                "#
                .to_string()
                .into(),
            )
            .unwrap();

        let scope = &mut runtime.handle_scope();
        let context = scope.get_current_context();
        let global = context.global(scope);
        let key = deno_core::v8::String::new(scope, "__npmRequireAnswer").unwrap();
        let value = global.get(scope, key.into()).unwrap();
        let answer = value.to_string(scope).unwrap().to_rust_string_lossy(scope);

        assert_eq!(answer, "42");
    }

    #[test]
    fn js_require_rejects_undeclared_package() {
        let temp_dir = tempfile::tempdir().unwrap();
        let package_dir = temp_dir.path().join("node_modules").join("demo-package");
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::write(package_dir.join("index.js"), r#"module.exports = {};"#).unwrap();

        let mut runtime = create_js_runtime_with_bindings(
            "test-plugin".to_string(),
            "test-plugin@1.0.0".to_string(),
            serde_json::json!({}),
            None,
            None,
            temp_dir.path().to_path_buf(),
            Vec::new(),
        )
        .unwrap();

        runtime
            .execute_script(
                "<undeclared_npm_require>",
                r#"
                try {
                    require("demo-package");
                    globalThis.__npmRequireStatus = "loaded";
                } catch (error) {
                    globalThis.__npmRequireStatus = String(error);
                }
                "#
                .to_string()
                .into(),
            )
            .unwrap();

        let scope = &mut runtime.handle_scope();
        let context = scope.get_current_context();
        let global = context.global(scope);
        let key = deno_core::v8::String::new(scope, "__npmRequireStatus").unwrap();
        let value = global.get(scope, key.into()).unwrap();
        let status = value.to_string(scope).unwrap().to_rust_string_lossy(scope);

        assert!(status.contains("not declared in npm_dependencies"));
    }
}
