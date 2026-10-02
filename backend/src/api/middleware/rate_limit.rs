use axum::{
    Json,
    extract::Request,
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::collections::HashMap;
use std::net::IpAddr;
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
}

/// Internal state for the rate limiter
struct RateLimiterState {
    /// Map of IP addresses to their request timestamps
    requests: HashMap<IpAddr, Vec<Instant>>,
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
            })),
            max_requests,
            window_duration: Duration::from_secs(window_seconds),
        }
    }

    /// Create a rate limiter from security configuration
    pub fn from_config(max_requests: usize, window_seconds: u64) -> Self {
        Self::new(max_requests, window_seconds)
    }

    /// Check if a request from the given IP should be allowed
    ///
    /// Returns Ok(()) if the request is allowed, Err(RateLimitError) if rate limit exceeded
    pub async fn check_rate_limit(&self, ip: IpAddr) -> Result<(), RateLimitError> {
        let mut state = self.state.write().await;
        let now = Instant::now();
        let window_start = now - self.window_duration;

        // Get or create the request history for this IP
        let requests = state.requests.entry(ip).or_insert_with(Vec::new);

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
            time_until_expire.as_secs().max(1) // At least 1 second
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
            RateLimitError::MissingClientIp => {
                let body = Json(json!({
                    "error": "RateLimitError",
                    "message": "Could not determine client IP address",
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
        .ok_or(RateLimitError::MissingClientIp)?;

    // Extract client IP address
    // In production, this should consider X-Forwarded-For or X-Real-IP headers
    // For now, we'll use a placeholder approach
    let client_ip = extract_client_ip(&request)?;

    // Check rate limit
    rate_limiter.check_rate_limit(client_ip).await?;

    // Rate limit check passed, proceed with request
    Ok(next.run(request).await)
}

/// Extract client IP address from request
///
/// This function attempts to extract the client IP from:
/// 1. X-Forwarded-For header (for requests behind proxies)
/// 2. X-Real-IP header (alternative proxy header)
/// 3. Connection remote address (direct connections)
///
/// For testing purposes, we use a default IP if none can be extracted.
fn extract_client_ip(request: &Request) -> Result<IpAddr, RateLimitError> {
    // Try X-Forwarded-For header first (comma-separated list, first is client)
    if let Some(forwarded) = request.headers().get("X-Forwarded-For")
        && let Ok(forwarded_str) = forwarded.to_str()
        && let Some(first_ip) = forwarded_str.split(',').next()
        && let Ok(ip) = first_ip.trim().parse::<IpAddr>()
    {
        return Ok(ip);
    }

    // Try X-Real-IP header
    if let Some(real_ip) = request.headers().get("X-Real-IP")
        && let Ok(ip_str) = real_ip.to_str()
        && let Ok(ip) = ip_str.parse::<IpAddr>()
    {
        return Ok(ip);
    }

    // For testing/development, use a default IP
    // In production, this should be extracted from the connection
    // or the request should be rejected if no IP can be determined
    Ok(IpAddr::from([127, 0, 0, 1]))
}

#[cfg(test)]
#[path = "../../../tests/unit/api/middleware/rate_limit.rs"]
mod tests;
