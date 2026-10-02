use super::*;
use axum::{
    Router, body::Body, http::StatusCode, middleware, response::IntoResponse, routing::get,
};
use tower::util::ServiceExt;

// For oneshot method

async fn protected_handler() -> impl IntoResponse {
    (StatusCode::OK, "Protected resource")
}

#[tokio::test]
async fn test_auth_middleware_with_valid_token() {
    let api_key = ApiKey::new(true, "test-secret-key".to_string());

    let app = Router::new()
        .route("/protected", get(protected_handler))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let api_key = api_key.clone();
                async move {
                    req.extensions_mut().insert(api_key);
                    auth_middleware(req, next).await
                }
            },
        ));

    let request = Request::builder()
        .uri("/protected")
        .header(AUTHORIZATION_HEADER, "Bearer test-secret-key")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_auth_middleware_with_invalid_token() {
    let api_key = ApiKey::new(true, "test-secret-key".to_string());

    let app = Router::new()
        .route("/protected", get(protected_handler))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let api_key = api_key.clone();
                async move {
                    req.extensions_mut().insert(api_key);
                    auth_middleware(req, next).await
                }
            },
        ));

    let request = Request::builder()
        .uri("/protected")
        .header(AUTHORIZATION_HEADER, "Bearer wrong-token")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_auth_middleware_with_missing_token() {
    let api_key = ApiKey::new(true, "test-secret-key".to_string());

    let app = Router::new()
        .route("/protected", get(protected_handler))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let api_key = api_key.clone();
                async move {
                    req.extensions_mut().insert(api_key);
                    auth_middleware(req, next).await
                }
            },
        ));

    let request = Request::builder()
        .uri("/protected")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_auth_middleware_with_invalid_format() {
    let api_key = ApiKey::new(true, "test-secret-key".to_string());

    let app = Router::new()
        .route("/protected", get(protected_handler))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let api_key = api_key.clone();
                async move {
                    req.extensions_mut().insert(api_key);
                    auth_middleware(req, next).await
                }
            },
        ));

    let request = Request::builder()
        .uri("/protected")
        .header(AUTHORIZATION_HEADER, "Basic dGVzdDp0ZXN0")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_auth_middleware_disabled() {
    let api_key = ApiKey::new(false, "test-secret-key".to_string());

    let app = Router::new()
        .route("/protected", get(protected_handler))
        .layer(middleware::from_fn(
            move |mut req: Request<Body>, next: Next| {
                let api_key = api_key.clone();
                async move {
                    req.extensions_mut().insert(api_key);
                    auth_middleware(req, next).await
                }
            },
        ));

    // Request without token should succeed when auth is disabled
    let request = Request::builder()
        .uri("/protected")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
