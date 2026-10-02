use super::*;
use axum::{Router, body::Body, middleware, routing::get};
use tower::util::ServiceExt;

// For oneshot method

#[tokio::test]
async fn test_security_headers_middleware_basic() {
    let app = Router::new()
        .route("/test", get(|| async { "OK" }))
        .layer(middleware::from_fn(security_headers_middleware));

    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Check that security headers are present
    assert_eq!(
        response.headers().get("X-Content-Type-Options").unwrap(),
        "nosniff"
    );
    assert_eq!(response.headers().get("X-Frame-Options").unwrap(), "DENY");
    assert_eq!(
        response.headers().get("X-XSS-Protection").unwrap(),
        "1; mode=block"
    );
    assert!(response.headers().contains_key("Content-Security-Policy"));
}

#[tokio::test]
async fn test_security_headers_middleware_with_hsts_disabled() {
    let config = SecurityHeadersConfig::development();

    let app = Router::new()
        .route("/test", get(|| async { "OK" }))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let config = config.clone();
                async move {
                    req.extensions_mut().insert(config);
                    security_headers_middleware(req, next).await
                }
            },
        ));

    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

    let response = app.oneshot(request).await.unwrap();

    // HSTS header should not be present when disabled
    assert!(!response.headers().contains_key("Strict-Transport-Security"));

    // Other security headers should still be present
    assert!(response.headers().contains_key("X-Content-Type-Options"));
    assert!(response.headers().contains_key("X-Frame-Options"));
}

#[tokio::test]
async fn test_security_headers_middleware_with_hsts_enabled() {
    let config = SecurityHeadersConfig::production();

    let app = Router::new()
        .route("/test", get(|| async { "OK" }))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let config = config.clone();
                async move {
                    req.extensions_mut().insert(config);
                    security_headers_middleware(req, next).await
                }
            },
        ));

    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

    let response = app.oneshot(request).await.unwrap();

    // HSTS header should be present when enabled
    let hsts_header = response.headers().get("Strict-Transport-Security").unwrap();
    let hsts_value = hsts_header.to_str().unwrap();

    assert!(hsts_value.contains("max-age=31536000"));
    assert!(hsts_value.contains("includeSubDomains"));
}

#[tokio::test]
async fn test_security_headers_middleware_custom_hsts_max_age() {
    let config = SecurityHeadersConfig::new(true, 86400); // 1 day

    let app = Router::new()
        .route("/test", get(|| async { "OK" }))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let config = config.clone();
                async move {
                    req.extensions_mut().insert(config);
                    security_headers_middleware(req, next).await
                }
            },
        ));

    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

    let response = app.oneshot(request).await.unwrap();

    let hsts_header = response.headers().get("Strict-Transport-Security").unwrap();
    let hsts_value = hsts_header.to_str().unwrap();

    assert!(hsts_value.contains("max-age=86400"));
}

#[tokio::test]
async fn test_security_headers_middleware_csp_header() {
    let app = Router::new()
        .route("/test", get(|| async { "OK" }))
        .layer(middleware::from_fn(security_headers_middleware));

    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

    let response = app.oneshot(request).await.unwrap();

    let csp_header = response.headers().get("Content-Security-Policy").unwrap();
    let csp_value = csp_header.to_str().unwrap();

    // Verify CSP contains expected directives
    assert!(csp_value.contains("default-src 'self'"));
    assert!(csp_value.contains("script-src 'self'"));
    assert!(csp_value.contains("media-src 'self' https: http:"));
    assert!(csp_value.contains("object-src 'none'"));
    assert!(csp_value.contains("frame-ancestors 'none'"));
}

#[tokio::test]
async fn test_page_preserves_handler_csp_nonce() {
    let app = Router::new()
        .route(
            "/test",
            get(|| async {
                Response::builder()
                    .header(
                        "Content-Security-Policy",
                        "default-src 'self'; script-src 'self' 'nonce-test-nonce'",
                    )
                    .body(Body::empty())
                    .unwrap()
            }),
        )
        .layer(middleware::from_fn(security_headers_middleware));

    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.headers().get("Content-Security-Policy").unwrap(),
        "default-src 'self'; script-src 'self' 'nonce-test-nonce'"
    );
}

#[tokio::test]
async fn test_plugin_asset_uses_non_executable_csp() {
    let app = Router::new()
        .route(
            "/api/v1/plugin-assets/example/ui/icon.svg",
            get(|| async { "asset" }),
        )
        .layer(middleware::from_fn(security_headers_middleware));

    let request = Request::builder()
        .uri("/api/v1/plugin-assets/example/ui/icon.svg")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.headers().get("Content-Security-Policy").unwrap(),
        PLUGIN_ASSET_CSP
    );
    assert_eq!(response.headers().get("X-Frame-Options").unwrap(), "DENY");
    assert_eq!(
        response.headers().get("Referrer-Policy").unwrap(),
        "no-referrer"
    );
}

#[tokio::test]
async fn test_plugin_asset_preserves_handler_csp() {
    let app = Router::new()
        .route(
            "/api/v1/plugin-assets/example/ui/index.html",
            get(|| async {
                Response::builder()
                    .header("Content-Security-Policy", "default-src 'none'; sandbox")
                    .body(Body::empty())
                    .unwrap()
            }),
        )
        .layer(middleware::from_fn(security_headers_middleware));

    let request = Request::builder()
        .uri("/api/v1/plugin-assets/example/ui/index.html")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.headers().get("Content-Security-Policy").unwrap(),
        "default-src 'none'; sandbox"
    );
}

#[tokio::test]
async fn test_security_headers_middleware_all_headers_present() {
    let config = SecurityHeadersConfig::production();

    let app = Router::new()
        .route("/test", get(|| async { "OK" }))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let config = config.clone();
                async move {
                    req.extensions_mut().insert(config);
                    security_headers_middleware(req, next).await
                }
            },
        ));

    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Verify all security headers are present
    let expected_headers = vec![
        "X-Content-Type-Options",
        "X-Frame-Options",
        "X-XSS-Protection",
        "Content-Security-Policy",
        "Strict-Transport-Security",
    ];

    for header in expected_headers {
        assert!(
            response.headers().contains_key(header),
            "Missing security header: {}",
            header
        );
    }
}
