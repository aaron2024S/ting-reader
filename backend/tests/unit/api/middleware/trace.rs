use super::*;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
    response::IntoResponse,
    routing::get,
};
use tower::util::ServiceExt;

// For oneshot method

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
