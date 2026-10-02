use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};

const PLUGIN_ASSET_CSP: &str = "default-src 'none'; script-src 'none'; style-src 'none'; img-src 'none'; media-src 'none'; font-src 'none'; connect-src 'none'; child-src 'none'; frame-src 'none'; worker-src 'none'; manifest-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; sandbox";

/// Security headers middleware
///
/// This middleware adds security-related HTTP headers to all responses:
/// - X-Content-Type-Options: nosniff (prevents MIME type sniffing)
/// - X-Frame-Options: DENY (prevents clickjacking)
/// - X-XSS-Protection: 1; mode=block (enables XSS filter in older browsers)
/// - Strict-Transport-Security: enforces HTTPS (only in production)
/// - Content-Security-Policy: restricts resource loading
///
/// These headers help protect against common web vulnerabilities.
pub async fn security_headers_middleware(request: Request, next: Next) -> Response {
    // Get security configuration from request extensions
    let security_config = request.extensions().get::<SecurityHeadersConfig>().cloned();

    let path = request.uri().path().to_string();
    // Check if request is for widget or a sandboxed plugin UI asset.
    let is_widget = path.starts_with("/widget");
    let is_plugin_asset =
        path.starts_with("/api/v1/plugin-assets/") || path.starts_with("/api/plugin-assets/");

    // Process the request
    let response = next.run(request).await;

    // Add security headers to response
    let (mut parts, body) = response.into_parts();

    // Always add these security headers
    parts.headers.insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );

    if is_widget {
        // Allow framing for widget pages
        parts.headers.insert(
            "Content-Security-Policy",
            HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self'; connect-src 'self'; media-src 'self' https: http:; object-src 'none'; frame-ancestors *;"),
        );
    } else if is_plugin_asset {
        parts
            .headers
            .entry("Content-Security-Policy")
            .or_insert(HeaderValue::from_static(PLUGIN_ASSET_CSP));
        parts
            .headers
            .insert("X-Frame-Options", HeaderValue::from_static("DENY"));
        parts
            .headers
            .insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
    } else {
        parts
            .headers
            .insert("X-Frame-Options", HeaderValue::from_static("DENY"));

        parts
            .headers
            .entry("Content-Security-Policy")
            .or_insert(HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self'; connect-src 'self'; media-src 'self' https: http:; object-src 'none'; frame-ancestors 'none';"));
    }

    parts.headers.insert(
        "X-XSS-Protection",
        HeaderValue::from_static("1; mode=block"),
    );

    // Add HSTS header if HTTPS is enabled
    if let Some(config) = security_config
        && config.enable_hsts
    {
        let hsts_value = format!("max-age={}; includeSubDomains", config.hsts_max_age);
        parts.headers.insert(
            "Strict-Transport-Security",
            HeaderValue::from_str(&hsts_value).unwrap_or_else(|_| {
                HeaderValue::from_static("max-age=31536000; includeSubDomains")
            }),
        );
    }

    Response::from_parts(parts, body)
}

/// Configuration for security headers
#[derive(Clone, Debug)]
pub struct SecurityHeadersConfig {
    /// Enable HSTS (HTTP Strict Transport Security) header
    pub enable_hsts: bool,
    /// HSTS max-age in seconds (default: 1 year = 31536000)
    pub hsts_max_age: u64,
}

impl SecurityHeadersConfig {
    /// Create a new security headers configuration
    pub fn new(enable_hsts: bool, hsts_max_age: u64) -> Self {
        Self {
            enable_hsts,
            hsts_max_age,
        }
    }

    /// Create default configuration for development (HSTS disabled)
    pub fn development() -> Self {
        Self {
            enable_hsts: false,
            hsts_max_age: 0,
        }
    }

    /// Create default configuration for production (HSTS enabled)
    pub fn production() -> Self {
        Self {
            enable_hsts: true,
            hsts_max_age: 31536000, // 1 year
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/api/middleware/security.rs"]
mod tests;
