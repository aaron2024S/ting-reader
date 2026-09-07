use axum::{
    extract::Request,
    http::{HeaderValue, Uri},
    middleware::Next,
    response::Response,
};
use tracing::{info_span, Instrument};
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
        && let Some(grant) = segments.get_mut(index + 1) {
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
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware,
        response::IntoResponse,
        routing::get,
        Router,
    };
    use tower::util::ServiceExt; // For oneshot method

    async fn test_handler(request: Request<Body>) -> impl IntoResponse {
        // Extract trace ID from extensions
        let trace_id = request
            .extensions()
            .get::<TraceId>()
            .map(|t| t.to_string())
            .unwrap_or_else(|| "no-trace-id".to_string());

        (StatusCode::OK, trace_id)
    }

    #[tokio::test]
    async fn test_trace_id_middleware_generates_id() {
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn(trace_id_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Check that trace ID is in response headers
        assert!(response.headers().contains_key(TRACE_ID_HEADER));

        let trace_id = response.headers().get(TRACE_ID_HEADER).unwrap();
        let trace_id_str = trace_id.to_str().unwrap();

        // Verify it's a valid UUID
        assert!(Uuid::parse_str(trace_id_str).is_ok());
    }

    #[test]
    fn request_target_redacts_plugin_grants_and_query_values() {
        let uri: Uri = "/api/v1/plugin-assets/signed-secret/demo/ui/index.html?token=secret"
            .parse()
            .unwrap();

        assert_eq!(
            request_target_for_logs(&uri),
            "/api/v1/plugin-assets/<redacted-grant>/demo/ui/index.html?<redacted-query>"
        );
    }

    #[test]
    fn request_target_keeps_safe_paths_without_query() {
        let uri: Uri = "/api/v1/books".parse().unwrap();
        assert_eq!(request_target_for_logs(&uri), "/api/v1/books");
    }

    #[tokio::test]
    async fn test_trace_id_available_in_handler() {
        let app = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn(trace_id_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Get trace ID from response header
        let header_trace_id = response
            .headers()
            .get(TRACE_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string(); // Clone the string before consuming response

        // Get trace ID from response body (returned by handler)
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_trace_id = String::from_utf8(body_bytes.to_vec()).unwrap();

        // They should match
        assert_eq!(header_trace_id, body_trace_id);
    }

    #[tokio::test]
    async fn test_trace_id_unique_per_request() {
        // Make first request
        let app1 = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn(trace_id_middleware));

        let request1 = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response1 = app1.oneshot(request1).await.unwrap();
        let trace_id1 = response1
            .headers()
            .get(TRACE_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // Make second request
        let app2 = Router::new()
            .route("/test", get(test_handler))
            .layer(middleware::from_fn(trace_id_middleware));

        let request2 = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response2 = app2.oneshot(request2).await.unwrap();
        let trace_id2 = response2
            .headers()
            .get(TRACE_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // Trace IDs should be different
        assert_ne!(trace_id1, trace_id2);
    }
}
