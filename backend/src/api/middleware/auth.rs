use axum::{
    Json,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

/// HTTP header name for authorization
pub const AUTHORIZATION_HEADER: &str = "Authorization";

/// Authentication middleware that validates Bearer tokens
///
/// This middleware:
/// - Checks for the Authorization header with Bearer token format
/// - Validates the token against the configured API key
/// - Returns 401 Unauthorized for invalid/missing tokens
/// - Allows requests to proceed if authentication is disabled in config
///
/// The middleware should be applied selectively to routes that require authentication.
/// Public endpoints (like /health) should not have this middleware applied.
pub async fn auth_middleware(request: Request, next: Next) -> Result<Response, AuthError> {
    // Extract the Authorization header
    let auth_header = request
        .headers()
        .get(AUTHORIZATION_HEADER)
        .and_then(|h| h.to_str().ok());

    // Get the API key from request extensions (injected by the router)
    let api_key = request
        .extensions()
        .get::<ApiKey>()
        .ok_or(AuthError::ConfigurationError)?;

    // If authentication is disabled, allow the request
    if !api_key.enabled {
        return Ok(next.run(request).await);
    }

    // Parse the Bearer token
    let token = auth_header
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(AuthError::MissingToken)?;

    // Validate the token
    if token != api_key.key {
        return Err(AuthError::InvalidToken);
    }

    // Token is valid, proceed with the request
    Ok(next.run(request).await)
}

/// Extension type for storing API key configuration in request extensions
#[derive(Clone, Debug)]
pub struct ApiKey {
    pub enabled: bool,
    pub key: String,
}

impl ApiKey {
    /// Create a new ApiKey configuration
    pub fn new(enabled: bool, key: String) -> Self {
        Self { enabled, key }
    }
}

/// Authentication errors
#[derive(Debug)]
pub enum AuthError {
    /// No Authorization header or invalid format
    MissingToken,
    /// Token does not match configured API key
    InvalidToken,
    /// Authentication configuration not found
    ConfigurationError,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            AuthError::MissingToken => (
                StatusCode::UNAUTHORIZED,
                "Missing or invalid Authorization header. Expected format: 'Authorization: Bearer <token>'",
            ),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid authentication token"),
            AuthError::ConfigurationError => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Authentication configuration error",
            ),
        };

        let body = Json(json!({
            "error": "AuthenticationError",
            "message": error_message,
        }));

        (status, body).into_response()
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/api/middleware/auth.rs"]
mod tests;
