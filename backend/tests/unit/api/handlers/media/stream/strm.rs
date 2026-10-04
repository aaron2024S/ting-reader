use super::*;
use axum::{
    Router,
    extract::Path,
    http::{HeaderMap, Method},
    routing::get,
};

struct StrmTestServer {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for StrmTestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_strm_source(app: Router) -> StrmTestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    StrmTestServer {
        url: format!("http://{address}"),
        task: tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    }
}

fn pending_audio() -> Body {
    Body::from_stream(futures::stream::pending::<std::io::Result<bytes::Bytes>>())
}

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
async fn strm_without_redirect_returns_original_url_without_reading_audio() {
    let server = serve_strm_source(Router::new().route(
        "/audio.mp3",
        get(|method: Method, headers: HeaderMap| async move {
            assert_eq!(method, Method::GET);
            assert_eq!(headers["range"], "bytes=0-0");
            // A source may ignore Range and send an unbounded body. Resolution
            // must finish from its headers rather than wait for audio bytes.
            (StatusCode::OK, pending_audio())
        }),
    ))
    .await;
    let url = format!("{}/audio.mp3?signature=a%2Fb&token=x%2By", server.url);
    let response = tokio::time::timeout(Duration::from_secs(2), redirect_strm_url(url.clone()))
        .await
        .unwrap()
        .unwrap();
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
async fn credentialed_strm_without_redirect_preserves_url_and_authenticates_probe() {
    let server = serve_strm_source(Router::new().route(
        "/audio.mp3",
        get(|headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Basic dXNlcjpwQHNz");
            (StatusCode::PARTIAL_CONTENT, pending_audio())
        }),
    ))
    .await;
    let url = format!(
        "{}/audio.mp3?signature=a%2Fb",
        server.url.replace("http://", "http://user:p%40ss@")
    );
    validate_strm_url(&url).unwrap();
    let response = redirect_strm_url(url.clone()).await.unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers()["location"], url);
}

#[tokio::test]
async fn strm_follows_playback_redirects_and_preserves_final_signed_query() {
    let server = serve_strm_source(
        Router::new()
            .route(
                "/hop/:step",
                get(|Path(step): Path<usize>, method: Method| async move {
                    assert_eq!(method, Method::GET);
                    let codes = [301, 302, 303, 307, 308];
                    let target = if step + 1 < codes.len() {
                        format!("/hop/{}", step + 1)
                    } else {
                        "/audio.mp3?signature=a%2Fb&token=x%2By".into()
                    };
                    (
                        StatusCode::from_u16(codes[step]).unwrap(),
                        [("Location", target)],
                    )
                }),
            )
            .route(
                "/audio.mp3",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["range"], "bytes=0-0");
                    (StatusCode::PARTIAL_CONTENT, pending_audio())
                }),
            ),
    )
    .await;
    let response = redirect_strm_url(format!("{}/hop/0", server.url))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers()["location"],
        format!("{}/audio.mp3?signature=a%2Fb&token=x%2By", server.url)
    );
}

#[tokio::test]
async fn strm_keeps_credentials_on_same_origin_and_drops_them_on_changed_port() {
    let final_server = serve_strm_source(Router::new().route(
        "/audio.mp3",
        get(|headers: HeaderMap| async move {
            assert!(!headers.contains_key("authorization"));
            assert!(!headers.contains_key("cookie"));
            assert!(!headers.contains_key("referer"));
            (StatusCode::PARTIAL_CONTENT, pending_audio())
        }),
    ))
    .await;
    let target = format!("{}/audio.mp3?signature=a%2Fb", final_server.url);
    let final_url = target.clone();
    let server = serve_strm_source(
        Router::new()
            .route(
                "/entry",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Basic dXNlcjpwQHNz");
                    (StatusCode::FOUND, [("Location", "/relative")])
                }),
            )
            .route(
                "/relative",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Basic dXNlcjpwQHNz");
                    (StatusCode::FOUND, [("Location", "/absolute")])
                }),
            )
            .route(
                "/absolute",
                get(move |headers: HeaderMap| {
                    let target = target.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Basic dXNlcjpwQHNz");
                        (StatusCode::FOUND, [("Location", target)])
                    }
                }),
            ),
    )
    .await;
    let response = redirect_strm_url(format!(
        "{}/entry",
        server.url.replace("http://", "http://user:p%40ss@")
    ))
    .await
    .unwrap();
    assert_eq!(response.headers()["location"], final_url);
}

#[tokio::test]
async fn strm_same_origin_absolute_redirect_preserves_url_credentials() {
    let server = serve_strm_source(
        Router::new()
            .route(
                "/entry",
                get(|headers: HeaderMap| async move {
                    let target = format!("http://{}/audio.mp3", headers["host"].to_str().unwrap());
                    (StatusCode::FOUND, [("Location", target)])
                }),
            )
            .route(
                "/audio.mp3",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Basic dXNlcjpwQHNz");
                    (StatusCode::PARTIAL_CONTENT, pending_audio())
                }),
            ),
    )
    .await;
    let credentialed_origin = server.url.replace("http://", "http://user:p%40ss@");
    let response = redirect_strm_url(format!("{credentialed_origin}/entry"))
        .await
        .unwrap();
    assert_eq!(
        response.headers()["location"],
        format!("{credentialed_origin}/audio.mp3")
    );
}

#[tokio::test]
async fn strm_retries_without_range_when_source_rejects_probe_range() {
    let server = serve_strm_source(Router::new().route(
        "/audio.mp3",
        get(|headers: HeaderMap| async move {
            if headers.contains_key("range") {
                StatusCode::RANGE_NOT_SATISFIABLE.into_response()
            } else {
                (StatusCode::OK, pending_audio()).into_response()
            }
        }),
    ))
    .await;
    let url = format!("{}/audio.mp3", server.url);
    let response = redirect_strm_url(url.clone()).await.unwrap();
    assert_eq!(response.headers()["location"], url);
}

#[tokio::test]
async fn strm_rejects_redirect_loops_missing_locations_and_non_http_targets() {
    let server = serve_strm_source(
        Router::new()
            .route(
                "/loop",
                get(|| async { (StatusCode::FOUND, [("Location", "/loop#fragment")]) }),
            )
            .route("/missing", get(|| async { StatusCode::FOUND }))
            .route(
                "/invalid",
                get(|| async { (StatusCode::FOUND, [("Location", "http://[")]) }),
            )
            .route(
                "/file",
                get(|| async { (StatusCode::FOUND, [("Location", "file:///private.mp3")]) }),
            )
            .route("/denied", get(|| async { StatusCode::FORBIDDEN })),
    )
    .await;
    for path in ["/loop", "/missing", "/invalid", "/file", "/denied"] {
        assert!(
            redirect_strm_url(format!("{}{path}", server.url))
                .await
                .is_err(),
            "{path}"
        );
    }
}

#[tokio::test]
async fn strm_allows_redirect_limit_but_rejects_longer_chains() {
    let server = serve_strm_source(Router::new().route(
        "/hop/:remaining",
        get(|Path(remaining): Path<usize>| async move {
            if remaining == 0 {
                StatusCode::OK.into_response()
            } else {
                (
                    StatusCode::FOUND,
                    [("Location", format!("/hop/{}", remaining - 1))],
                )
                    .into_response()
            }
        }),
    ))
    .await;
    let response = redirect_strm_url(format!("{}/hop/{STRM_MAX_REDIRECTS}", server.url))
        .await
        .unwrap();
    assert_eq!(
        response.headers()["location"],
        format!("{}/hop/0", server.url)
    );
    let error = redirect_strm_url(format!("{}/hop/{}", server.url, STRM_MAX_REDIRECTS + 1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("redirect limit exceeded"));
}

#[tokio::test]
async fn strm_resolution_times_out_and_redacts_failed_source_credentials() {
    let server =
        serve_strm_source(Router::new().route("/slow", get(futures::future::pending::<Response>)))
            .await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    assert!(matches!(
        resolve_strm_url(&client, &format!("{}/slow", server.url)).await,
        Err(TingError::Timeout(_))
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let error = redirect_strm_url(format!(
        "http://user:secret@{address}/audio.mp3?token=hidden"
    ))
    .await
    .unwrap_err()
    .to_string();
    assert!(!error.contains("secret"));
    assert!(!error.contains("hidden"));
}
