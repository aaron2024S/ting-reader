use super::*;

#[test]
fn http_network_permission_denies_by_default() {
    assert!(!is_network_allowed(&[], "https://example.com"));
    assert!(is_network_allowed(
        &["example.com".to_string()],
        "https://example.com/path"
    ));
    assert!(is_network_allowed(
        &["*.example.com".to_string()],
        "https://api.example.com/path"
    ));
    assert!(is_network_allowed(
        &["*".to_string()],
        "https://plugins.example.net/path"
    ));
    assert!(!is_network_allowed(
        &["example.com".to_string()],
        "https://evil.example.net/path"
    ));
    assert!(!is_network_allowed(
        &["example.com".to_string()],
        "ftp://example.com/archive"
    ));
    assert!(!is_network_allowed(
        &["example.com".to_string()],
        "https://user:secret@example.com/private"
    ));
}

#[test]
fn http_urls_are_redacted_before_logging() {
    let url =
        Url::parse("https://user:secret@[2606:4700:4700::1111]:8443/plugin?token=secret#part")
            .unwrap();
    assert_eq!(
        redacted_http_url(&url),
        "https://[2606:4700:4700::1111]:8443/plugin"
    );
    assert_eq!(
        redacted_http_url_str("not a URL?token=secret"),
        "<invalid-url>"
    );
}

#[tokio::test]
async fn http_target_validation_allows_private_addresses_with_permission() {
    let allowed_domains = vec!["*".to_string()];
    for raw_url in [
        "http://127.0.0.1/private",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/private",
    ] {
        assert!(
            validate_http_target(&allowed_domains, &Url::parse(raw_url).unwrap())
                .await
                .is_ok()
        );
    }

    validate_http_target(
        &allowed_domains,
        &Url::parse("https://8.8.8.8/resource").unwrap(),
    )
    .await
    .unwrap();
}

#[test]
fn http_redirects_drop_sensitive_headers_and_post_bodies() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
    headers.insert(COOKIE, HeaderValue::from_static("session=secret"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from_static("2"));
    strip_cross_origin_fetch_headers(&mut headers);
    assert!(!headers.contains_key(AUTHORIZATION));
    assert!(!headers.contains_key(COOKIE));

    let mut method = Method::POST;
    let mut body = Some("{}".to_string());
    apply_http_redirect_semantics(StatusCode::FOUND, &mut method, &mut headers, &mut body);
    assert_eq!(method, Method::GET);
    assert!(body.is_none());
    assert!(!headers.contains_key(CONTENT_TYPE));
    assert!(!headers.contains_key(CONTENT_LENGTH));
}

#[test]
fn http_response_limit_is_enforced_without_overflow() {
    assert_eq!(
        checked_http_body_len(MAX_HTTP_RESPONSE_BYTES - 1, 1).unwrap(),
        MAX_HTTP_RESPONSE_BYTES
    );
    assert!(checked_http_body_len(MAX_HTTP_RESPONSE_BYTES, 1).is_err());
    assert!(checked_http_body_len(usize::MAX, 1).is_err());
}

#[tokio::test]
async fn http_client_pins_dns_and_does_not_follow_redirects_automatically() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 302 Found\r\nLocation: http://example.com/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });

    let url = Url::parse(&format!(
        "http://plugin-fetch.test:{}/start?token=secret",
        address.port()
    ))
    .unwrap();
    let client = build_http_client(&url, &[address]).unwrap();
    let response = client.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.url().host_str(), Some("plugin-fetch.test"));
    server.await.unwrap();
}

#[tokio::test]
async fn shared_http_preserves_status_and_rejects_undeclared_redirect_target() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        for response in [
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope".as_slice(),
            b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.2:1234/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice()
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 512];
            let _ = socket.read(&mut request).await.unwrap();
            socket.write_all(response).await.unwrap();
        }
    });
    let allowed = vec!["127.0.0.1".to_string()];
    let url = format!("http://127.0.0.1:{port}/data");
    let result = request(&url, None, &allowed).await.unwrap();
    assert_eq!(result.status, 404);
    assert_eq!(result.body, b"nope");
    let error = request(&url, None, &allowed).await.unwrap_err();
    assert!(
        error.to_string().contains("Network access denied"),
        "{error}"
    );
    server.await.unwrap();
}
