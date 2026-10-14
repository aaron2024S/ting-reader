use axum::{
    Json,
    extract::{ConnectInfo, Request},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Rate limiter using sliding window algorithm
///
/// This implementation tracks request counts per IP address within a time window.
/// When a request comes in:
/// 1. Remove expired entries (older than the window)
/// 2. Count requests from this IP in the current window
/// 3. If count exceeds limit, reject with 429 Too Many Requests
/// 4. Otherwise, record the request and allow it through
#[derive(Clone)]
pub struct RateLimiter {
    /// Shared state containing request history per IP
    state: Arc<RwLock<RateLimiterState>>,
    /// Maximum number of requests allowed per window
    max_requests: usize,
    /// Time window duration in seconds
    window_duration: Duration,
    max_clients: usize,
    trusted_proxies: Vec<IpAddr>,
}

/// Only inserted by the Unix socket server, never inferred from HTTP headers.
#[derive(Clone, Copy)]
pub(crate) struct GatewayTransport;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ClientKey {
    Ip(IpAddr),
    GatewayUser(u64),
    GatewayAnonymous,
}

/// Internal state for the rate limiter
struct RateLimiterState {
    /// Map of IP addresses to their request timestamps
    requests: HashMap<ClientKey, Vec<Instant>>,
    last_cleanup: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter
    ///
    /// # Arguments
    /// * `max_requests` - Maximum number of requests allowed per window
    /// * `window_seconds` - Time window duration in seconds
    pub fn new(max_requests: usize, window_seconds: u64) -> Self {
        Self {
            state: Arc::new(RwLock::new(RateLimiterState {
                requests: HashMap::new(),
                last_cleanup: Instant::now(),
            })),
            max_requests,
            window_duration: Duration::from_secs(window_seconds),
            max_clients: 10_000,
            trusted_proxies: Vec::new(),
        }
    }

    /// Create a rate limiter from security configuration
    pub fn from_config(config: &crate::core::app::config::SecurityConfig) -> Self {
        Self {
            max_clients: config.rate_limit_max_clients,
            trusted_proxies: config.trusted_proxies.clone(),
            ..Self::new(config.rate_limit_requests, config.rate_limit_window)
        }
    }

    /// Check if a request from the given IP should be allowed
    ///
    /// Returns Ok(()) if the request is allowed, Err(RateLimitError) if rate limit exceeded
    pub async fn check_rate_limit(&self, ip: IpAddr) -> Result<(), RateLimitError> {
        self.check_client(ClientKey::Ip(ip)).await
    }

    async fn check_client(&self, key: ClientKey) -> Result<(), RateLimitError> {
        let mut state = self.state.write().await;
        let now = Instant::now();
        let window_start = now - self.window_duration;

        // Request-driven cleanup needs no background task to outlive the router.
        if now.duration_since(state.last_cleanup)
            >= self.window_duration.min(Duration::from_secs(60))
            || (!state.requests.contains_key(&key) && state.requests.len() >= self.max_clients)
        {
            state.requests.retain(|_, requests| {
                requests.retain(|&timestamp| timestamp > window_start);
                !requests.is_empty()
            });
            state.last_cleanup = now;
        }
        if !state.requests.contains_key(&key) && state.requests.len() >= self.max_clients {
            // Never evict active clients: that would reset their budgets.
            return Err(RateLimitError::CapacityExceeded);
        }

        // Get or create the request history for this IP
        let requests = state.requests.entry(key).or_default();

        // Remove expired requests (outside the current window)
        requests.retain(|&timestamp| timestamp > window_start);

        // Check if the limit is exceeded
        if requests.len() >= self.max_requests {
            return Err(RateLimitError::LimitExceeded {
                limit: self.max_requests,
                window_seconds: self.window_duration.as_secs(),
                retry_after: self.calculate_retry_after(requests, window_start),
            });
        }

        // Record this request
        requests.push(now);

        Ok(())
    }

    /// Calculate how many seconds until the oldest request expires
    fn calculate_retry_after(&self, requests: &[Instant], window_start: Instant) -> u64 {
        if let Some(&oldest) = requests.first() {
            let time_until_expire = oldest.duration_since(window_start);
            (time_until_expire.as_secs() + u64::from(time_until_expire.subsec_nanos() > 0)).max(1)
        } else {
            1
        }
    }

    /// Clean up expired entries to prevent memory growth
    ///
    /// This should be called periodically (e.g., every minute) to remove
    /// IP addresses that haven't made requests recently
    pub async fn cleanup_expired(&self) {
        let mut state = self.state.write().await;
        let now = Instant::now();
        let window_start = now - self.window_duration;

        // Remove IPs with no recent requests
        state.requests.retain(|_, requests| {
            requests.retain(|&timestamp| timestamp > window_start);
            !requests.is_empty()
        });
        state.last_cleanup = now;
    }
}

/// Rate limiting errors
#[derive(Debug)]
pub enum RateLimitError {
    /// Rate limit exceeded
    LimitExceeded {
        limit: usize,
        window_seconds: u64,
        retry_after: u64,
    },
    /// Could not extract client IP address
    MissingClientIp,
    MissingConfiguration,
    CapacityExceeded,
}

impl IntoResponse for RateLimitError {
    fn into_response(self) -> Response {
        match self {
            RateLimitError::LimitExceeded {
                limit,
                window_seconds,
                retry_after,
            } => {
                let body = Json(json!({
                    "error": "RateLimitExceeded",
                    "message": format!(
                        "Rate limit exceeded. Maximum {} requests per {} seconds allowed.",
                        limit, window_seconds
                    ),
                    "details": {
                        "limit": limit,
                        "window_seconds": window_seconds,
                        "retry_after": retry_after,
                    }
                }));

                // Build response with Retry-After header
                let mut response = (StatusCode::TOO_MANY_REQUESTS, body).into_response();
                response.headers_mut().insert(
                    "Retry-After",
                    HeaderValue::from_str(&retry_after.to_string())
                        .unwrap_or_else(|_| HeaderValue::from_static("60")),
                );
                response
            }
            RateLimitError::CapacityExceeded => {
                let mut response = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({
                        "error": "RateLimitExceeded",
                        "message": "Authentication rate limiter capacity reached. Retry later.",
                    })),
                )
                    .into_response();
                response
                    .headers_mut()
                    .insert("Retry-After", HeaderValue::from_static("1"));
                response
            }
            RateLimitError::MissingClientIp | RateLimitError::MissingConfiguration => {
                let body = Json(json!({
                    "error": "RateLimitError",
                    "message": "Authentication rate limiter is not configured for this transport",
                }));

                (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
            }
        }
    }
}

/// Rate limiting middleware
///
/// This middleware enforces rate limits based on client IP address using a sliding window algorithm.
/// The rate limiter configuration is injected via request extensions.
///
/// # Behavior
/// - Tracks requests per IP address within a time window
/// - Returns 429 Too Many Requests when limit is exceeded
/// - Includes Retry-After header indicating when to retry
/// - Cleans up expired entries to prevent memory growth
pub async fn rate_limit_middleware(
    request: Request,
    next: Next,
) -> Result<Response, RateLimitError> {
    // Extract the rate limiter from request extensions
    let rate_limiter = request
        .extensions()
        .get::<RateLimiter>()
        .cloned()
        .ok_or(RateLimitError::MissingConfiguration)?;

    // Check rate limit
    let client = extract_client_key(&request, &rate_limiter.trusted_proxies)?;
    rate_limiter.check_client(client).await?;

    // Rate limit check passed, proceed with request
    Ok(next.run(request).await)
}

/// Unix gateway identity is trusted only on the local socket. TCP forwarding
/// headers are trusted only when the connection peer is explicitly configured.
fn extract_client_key(
    request: &Request,
    trusted_proxies: &[IpAddr],
) -> Result<ClientKey, RateLimitError> {
    if request.extensions().get::<GatewayTransport>().is_some() {
        let user = request
            .headers()
            .get("X-Trim-Userid")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        return Ok(user
            .map(ClientKey::GatewayUser)
            .unwrap_or(ClientKey::GatewayAnonymous));
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .ok_or(RateLimitError::MissingClientIp)?
        .0
        .ip();
    if !trusted_proxies.contains(&peer) {
        return Ok(ClientKey::Ip(peer));
    }

    // Walk from the trusted peer backwards, stopping at the first untrusted
    // hop. An attacker-supplied address prepended to the chain is ignored.
    if let Some(forwarded) = request.headers().get("X-Forwarded-For")
        && let Ok(forwarded_str) = forwarded.to_str()
    {
        let chain = forwarded_str
            .split(',')
            .map(|value| value.trim().parse::<IpAddr>())
            .collect::<Result<Vec<_>, _>>();
        if let Ok(chain) = chain
            && !chain.is_empty()
            && chain.len() <= 32
        {
            let mut client = peer;
            for hop in chain.into_iter().rev() {
                if !trusted_proxies.contains(&client) {
                    break;
                }
                client = hop;
            }
            return Ok(ClientKey::Ip(client));
        }
        // Malformed forwarding chains cannot grant a fresh client budget.
        return Ok(ClientKey::Ip(peer));
    }

    // Try X-Real-IP header
    if let Some(real_ip) = request.headers().get("X-Real-IP")
        && let Ok(ip_str) = real_ip.to_str()
        && let Ok(ip) = ip_str.parse::<IpAddr>()
    {
        return Ok(ClientKey::Ip(ip));
    }

    Ok(ClientKey::Ip(peer))
}

#[cfg(test)]
#[path = "../../../tests/unit/api/middleware/rate_limit.rs"]
mod tests;
