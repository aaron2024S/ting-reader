use super::*;
use axum::{Router, middleware, routing::post};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

static CALLS: AtomicUsize = AtomicUsize::new(0);

async fn mutation() -> &'static str {
    CALLS.fetch_add(1, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(20)).await;
    "created"
}

#[tokio::test]
async fn concurrent_requests_with_same_key_execute_once() {
    CALLS.store(0, Ordering::SeqCst);
    entries().lock().await.clear();
    let app = Router::new()
        .route("/resource", post(mutation))
        .layer(middleware::from_fn(idempotency));

    let request = || {
        Request::builder()
            .method(Method::POST)
            .uri("/resource")
            .header("idempotency-key", "same-key")
            .body(Body::empty())
            .unwrap()
    };
    let (first, second) = tokio::join!(
        app.clone().oneshot(request()),
        app.clone().oneshot(request())
    );

    assert_eq!(first.unwrap().status(), StatusCode::OK);
    assert_eq!(second.unwrap().status(), StatusCode::OK);
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
}
