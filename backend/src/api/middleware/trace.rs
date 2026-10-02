use axum::{
    extract::Request,
    http::{HeaderValue, Uri},
    middleware::Next,
    response::Response,
};
use tracing::{Instrument, info_span};
use uuid::Uuid;

/// HTTP header name for trace ID
pub const TRACE_ID_HEADER: &str = "X-Trace-Id";

/// Middleware that generates a unique trace ID for each request and propagates it
/// through the request lifecycle.
///
/// The trace ID is:
/// - Generated as a UUID v4 for each request
/// - Added to the request extensions for access by handlers
/// - Included in all log entries via tracing spans
/// - Added to the response headers
/// - Included in error responses (handled by ErrorResponse)
pub async fn trace_id_middleware(request: Request, next: Next) -> Response {
    // Generate a unique trace ID for this request
    let trace_id = Uuid::new_v4().to_string();

    // Extract request information for logging
    let method = request.method().clone();
    let request_target = request_target_for_logs(request.uri());
    let version = request.version();

    // Create a tracing span with the trace ID
    // This ensures all logs within this request context include the trace_id field
    let span = info_span!(
        "http_request",
        trace_id = %trace_id,
        method = %method,
        uri = %request_target,
        version = ?version,
    );

    // Log the incoming request
    tracing::debug!(
        parent: &span,
        "Request started"
    );

    // Store trace_id in request extensions so handlers can access it
    let mut request = request;
    request.extensions_mut().insert(TraceId(trace_id.clone()));

    // Process the request within the span context
    let response = async move {
        let response = next.run(request).await;

        // Log the response
        tracing::debug!(
            status = %response.status(),
            "Request completed"
        );

        response
    }
    .instrument(span)
    .await;

    // Add trace ID to response headers
    let (mut parts, body) = response.into_parts();
    parts.headers.insert(
        TRACE_ID_HEADER,
        HeaderValue::from_str(&trace_id).unwrap_or_else(|_| HeaderValue::from_static("invalid")),
    );

    Response::from_parts(parts, body)
}

fn request_target_for_logs(uri: &Uri) -> String {
    let mut segments = uri
        .path()
        .split('/')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if let Some(index) = segments
        .iter()
        .position(|segment| segment == "plugin-assets")
        && let Some(grant) = segments.get_mut(index + 1)
    {
        *grant = "<redacted-grant>".to_string();
    }
    let path = segments.join("/");
    if uri.query().is_some() {
        format!("{path}?<redacted-query>")
    } else {
        path
    }
}

/// Extension type for storing trace ID in request extensions
#[derive(Clone, Debug)]
pub struct TraceId(pub String);

impl TraceId {
    /// Get the trace ID string
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TraceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/api/middleware/trace.rs"]
mod tests;
