use super::*;

#[test]
fn strm_url_validation_accepts_credentials_and_signed_urls() {
    for url in [
        "https://audio.example.test/chapter.mp3",
        "https://user:pass@audio.example.test/chapter.mp3",
        "https://user@audio.example.test/chapter.mp3",
        "https://audio.example.test/chapter.mp3?token=abc&signature=xyz",
    ] {
        validate_strm_url(url).unwrap();
    }
    for url in ["javascript:alert(1)", "file:///chapter.mp3", "not a URL"] {
        assert!(validate_strm_url(url).is_err());
    }
}

#[tokio::test]
async fn strm_direct_redirect_does_not_fetch_origin() {
    // An unreachable source still yields a redirect, including URL credentials.
    for url in [
        "http://127.0.0.1:9/never-requested.mp3",
        "http://user:pass@127.0.0.1:9/never-requested.mp3",
    ] {
        let response = redirect_strm_url(url.into());
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers()["location"], url);
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        assert_eq!(
            response.headers()["access-control-allow-methods"],
            "GET, HEAD, OPTIONS"
        );
        assert!(
            axum::body::to_bytes(response.into_body(), 1)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn strm_reader_rejects_oversize_even_with_whitespace() {
    let oversized = format!("https://audio.example.test/a.mp3{}", " ".repeat(65_536));
    assert!(read_strm_url(oversized.as_bytes()).await.is_err());
    assert_eq!(
        read_strm_url(b" https://audio.example.test/a.mp3\n".as_slice())
            .await
            .unwrap(),
        "https://audio.example.test/a.mp3",
    );
}

#[tokio::test]
async fn credentialed_strm_redirect_preserves_url_without_requesting_media() {
    use axum::{Router, routing::get};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let recorded = hits.clone();
    let app = Router::new().route(
        "/audio.mp3",
        get(move || {
            recorded.fetch_add(1, Ordering::SeqCst);
            async { "audio" }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://user:p%40ss@{address}/audio.mp3?signature=a%2Fb");
    validate_strm_url(&url).unwrap();
    let response = redirect_strm_url(url.clone());
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers()["location"], url);
    assert!(
        axum::body::to_bytes(response.into_body(), 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    server.abort();
}
