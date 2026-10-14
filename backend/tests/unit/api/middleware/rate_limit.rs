use super::*;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
    routing::get,
};
use tower::util::ServiceExt;

// For oneshot method

#[tokio::test]
async fn test_rate_limiter_allows_requests_within_limit() {
    let limiter = RateLimiter::new(5, 60); // 5 requests per 60 seconds
    let ip = IpAddr::from([127, 0, 0, 1]);

    // First 5 requests should succeed
    for _ in 0..5 {
        assert!(limiter.check_rate_limit(ip).await.is_ok());
    }
}

#[tokio::test]
async fn test_rate_limiter_blocks_requests_exceeding_limit() {
    let limiter = RateLimiter::new(3, 60); // 3 requests per 60 seconds
    let ip = IpAddr::from([127, 0, 0, 1]);

    // First 3 requests should succeed
    for _ in 0..3 {
        assert!(limiter.check_rate_limit(ip).await.is_ok());
    }

    // 4th request should be blocked
    let result = limiter.check_rate_limit(ip).await;
    assert!(result.is_err());

    if let Err(RateLimitError::LimitExceeded {
        limit,
        window_seconds,
        retry_after,
    }) = result
    {
        assert_eq!(limit, 3);
        assert_eq!(window_seconds, 60);
        assert!(retry_after > 0);
    } else {
        panic!("Expected LimitExceeded error");
    }
}

#[tokio::test]
async fn test_rate_limiter_different_ips_independent() {
    let limiter = RateLimiter::new(2, 60); // 2 requests per 60 seconds
    let ip1 = IpAddr::from([127, 0, 0, 1]);
    let ip2 = IpAddr::from([127, 0, 0, 2]);

    // IP1 makes 2 requests
    assert!(limiter.check_rate_limit(ip1).await.is_ok());
    assert!(limiter.check_rate_limit(ip1).await.is_ok());

    // IP1 is now rate limited
    assert!(limiter.check_rate_limit(ip1).await.is_err());

    // IP2 should still be able to make requests
    assert!(limiter.check_rate_limit(ip2).await.is_ok());
    assert!(limiter.check_rate_limit(ip2).await.is_ok());

    // IP2 is now also rate limited
    assert!(limiter.check_rate_limit(ip2).await.is_err());
}

#[tokio::test]
async fn test_rate_limiter_sliding_window() {
    let limiter = RateLimiter::new(2, 1); // 2 requests per 1 second
    let ip = IpAddr::from([127, 0, 0, 1]);

    // Make 2 requests
    assert!(limiter.check_rate_limit(ip).await.is_ok());
    assert!(limiter.check_rate_limit(ip).await.is_ok());

    // 3rd request should be blocked
    assert!(limiter.check_rate_limit(ip).await.is_err());

    // Wait for window to expire
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // Should be able to make requests again
    assert!(limiter.check_rate_limit(ip).await.is_ok());
}

#[tokio::test]
async fn test_rate_limiter_cleanup_expired() {
    let limiter = RateLimiter::new(5, 1); // 5 requests per 1 second
    let ip = IpAddr::from([127, 0, 0, 1]);

    // Make some requests
    for _ in 0..3 {
        limiter.check_rate_limit(ip).await.unwrap();
    }

    // Verify state has entries
    {
        let state = limiter.state.read().await;
        assert_eq!(state.requests.len(), 1);
        assert_eq!(state.requests.get(&ClientKey::Ip(ip)).unwrap().len(), 3);
    }

    // Wait for window to expire
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // Cleanup expired entries
    limiter.cleanup_expired().await;

    // State should be empty now
    {
        let state = limiter.state.read().await;
        assert_eq!(state.requests.len(), 0);
    }
}

#[tokio::test]
async fn test_rate_limit_middleware_integration() {
    let limiter = RateLimiter::new(3, 60);

    let app = Router::new()
        .route("/test", get(|| async { "OK" }))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let limiter = limiter.clone();
                async move {
                    req.extensions_mut().insert(limiter);
                    rate_limit_middleware(req, next).await
                }
            },
        ))
        .layer(axum::Extension(ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            1234,
        )))));

    // First 3 requests should succeed
    for i in 0..3 {
        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "Request {} should succeed",
            i + 1
        );
    }

    // 4th request should be rate limited
    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    // Check for Retry-After header
    assert!(response.headers().contains_key("Retry-After"));
}

#[tokio::test]
async fn test_rate_limit_error_response_format() {
    let limiter = RateLimiter::new(1, 60);

    let app = Router::new()
        .route("/test", get(|| async { "OK" }))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let limiter = limiter.clone();
                async move {
                    req.extensions_mut().insert(limiter);
                    rate_limit_middleware(req, next).await
                }
            },
        ))
        .layer(axum::Extension(ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            1234,
        )))));

    // First request succeeds
    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();
    app.clone().oneshot(request).await.unwrap();

    // Second request is rate limited
    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    // Parse response body
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // Verify error response structure
    assert_eq!(body["error"], "RateLimitExceeded");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("Rate limit exceeded")
    );
    assert_eq!(body["details"]["limit"], 1);
    assert_eq!(body["details"]["window_seconds"], 60);
    assert!(body["details"]["retry_after"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn test_forwarding_chain_uses_first_untrusted_hop() {
    let peer = IpAddr::from([10, 0, 0, 2]);
    let request = Request::builder()
        .uri("/test")
        .extension(ConnectInfo(SocketAddr::new(peer, 1234)))
        .header("X-Forwarded-For", "1.2.3.4, 192.168.1.100, 10.0.0.1")
        .body(Body::empty())
        .unwrap();

    let key = extract_client_key(&request, &[peer, IpAddr::from([10, 0, 0, 1])]).unwrap();
    assert_eq!(key, ClientKey::Ip(IpAddr::from([192, 168, 1, 100])));
}

#[tokio::test]
async fn test_extract_client_ip_from_x_real_ip() {
    let peer = IpAddr::from([10, 0, 0, 1]);
    let request = Request::builder()
        .uri("/test")
        .extension(ConnectInfo(SocketAddr::new(peer, 1234)))
        .header("X-Real-IP", "192.168.1.200")
        .body(Body::empty())
        .unwrap();

    let key = extract_client_key(&request, &[peer]).unwrap();
    assert_eq!(key, ClientKey::Ip(IpAddr::from([192, 168, 1, 200])));
}

#[tokio::test]
async fn missing_connection_does_not_fall_back_to_localhost() {
    let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

    assert!(matches!(
        extract_client_key(&request, &[]),
        Err(RateLimitError::MissingClientIp)
    ));
}

#[test]
fn tcp_clients_cannot_spoof_proxy_or_gateway_identity() {
    let peer = SocketAddr::from(([192, 168, 1, 20], 1234));
    let request = Request::builder()
        .extension(ConnectInfo(peer))
        .header("X-Forwarded-For", "1.2.3.4")
        .header("X-Real-IP", "1.2.3.5")
        .header("X-Trim-Userid", "1000")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        extract_client_key(&request, &[]).unwrap(),
        ClientKey::Ip(peer.ip())
    );
}

#[tokio::test]
async fn gateway_users_have_separate_budgets() {
    let limiter = RateLimiter::new(1, 60);
    for user in [1000, 1001] {
        let request = Request::builder()
            .extension(GatewayTransport)
            .header("X-Trim-Userid", user.to_string())
            .body(Body::empty())
            .unwrap();
        let key = extract_client_key(&request, &[]).unwrap();
        assert!(limiter.check_client(key.clone()).await.is_ok());
        assert!(limiter.check_client(key).await.is_err());
    }
}

#[tokio::test]
async fn capacity_is_bounded_without_resetting_active_budgets() {
    let mut limiter = RateLimiter::new(1, 60);
    limiter.max_clients = 2;
    let ip = IpAddr::from([192, 168, 1, 1]);
    assert!(limiter.check_rate_limit(ip).await.is_ok());
    assert!(
        limiter
            .check_rate_limit(IpAddr::from([192, 168, 1, 2]))
            .await
            .is_ok()
    );
    for last in 3..100 {
        assert!(matches!(
            limiter
                .check_rate_limit(IpAddr::from([192, 168, 1, last]))
                .await,
            Err(RateLimitError::CapacityExceeded)
        ));
    }
    assert_eq!(limiter.state.read().await.requests.len(), 2);
    assert!(matches!(
        limiter.check_rate_limit(ip).await,
        Err(RateLimitError::LimitExceeded { .. })
    ));
}

#[test]
fn retry_after_rounds_up_to_the_actual_expiration() {
    let limiter = RateLimiter::new(1, 60);
    let start = Instant::now();
    assert_eq!(
        limiter.calculate_retry_after(&[start + Duration::from_millis(1500)], start),
        2
    );
}

#[tokio::test]
async fn test_rate_limiter_concurrent_requests() {
    use std::sync::Arc;

    let limiter = Arc::new(RateLimiter::new(10, 60));
    let ip = IpAddr::from([127, 0, 0, 1]);

    // Spawn 10 concurrent requests
    let mut handles = vec![];
    for _ in 0..10 {
        let limiter = limiter.clone();
        let handle = tokio::spawn(async move { limiter.check_rate_limit(ip).await });
        handles.push(handle);
    }

    // All 10 should succeed
    let mut success_count = 0;
    for handle in handles {
        if handle.await.unwrap().is_ok() {
            success_count += 1;
        }
    }

    assert_eq!(success_count, 10);

    // 11th request should fail
    assert!(limiter.check_rate_limit(ip).await.is_err());
}
