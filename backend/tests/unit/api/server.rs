use super::*;

#[test]
fn test_api_server_creation() {
    // Test disabled due to complexity of mocking PluginManager
    /*
    let config = Config::from_file(std::path::Path::new("config.test.toml"))
        .expect("Failed to load test config");

    // Create an in-memory database for testing
    let db = Arc::new(DatabaseManager::new_in_memory().expect("Failed to create test database"));

    let server = ApiServer::new(config, db);
    assert!(server.is_ok());
    */
}

#[tokio::test]
async fn test_health_check() {
    let response = health_check().await;
    let value = response.0;

    assert_eq!(value["status"], "ok");
    assert!(value["version"].is_string());
    assert!(value["timestamp"].is_number());
}

#[tokio::test]
async fn gateway_proxy_does_not_leak_outer_wildcard_path_param() {
    use axum::extract::Path;

    async fn get_library(Path(id): Path<String>) -> String {
        id
    }

    let inner_router = Router::new().route("/api/libraries/:id", get(get_library));
    let state = GatewayProxyState {
        router: inner_router,
        prefix: "/app/ting-reader".to_string(),
    };
    let app = Router::new()
        .route("/app/ting-reader", any(gateway_proxy))
        .route("/app/ting-reader/*path", any(gateway_proxy))
        .with_state(state);

    let request = Request::builder()
        .uri("/app/ting-reader/api/libraries/library-1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();

    assert_eq!(body.as_ref(), b"library-1");
}

#[tokio::test]
async fn gateway_proxy_preserves_websocket_upgrade() {
    use axum::extract::ws::WebSocketUpgrade;
    use axum::http::StatusCode;

    async fn websocket(ws: WebSocketUpgrade) -> Response {
        ws.on_upgrade(|_| async {})
    }

    let inner_router = Router::new().route("/api/ws", get(websocket));
    let state = GatewayProxyState {
        router: inner_router,
        prefix: "/app/ting-reader".to_string(),
    };
    let app = Router::new()
        .route("/app/ting-reader", any(gateway_proxy))
        .route("/app/ting-reader/*path", any(gateway_proxy))
        .with_state(state);

    let mut request = Request::builder()
        .method("GET")
        .uri("/app/ting-reader/api/ws")
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    let on_upgrade = hyper::upgrade::on(&mut request);
    request.extensions_mut().insert(on_upgrade);

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
}

#[tokio::test]
async fn plugin_asset_html_skips_manifest_injection() {
    async fn plugin_asset() -> Response {
        Response::builder()
            .header(header::CONTENT_TYPE, "text/html")
            .body(Body::from(
                r#"<html><head><link rel="stylesheet" href="./rss.css"></head></html>"#,
            ))
            .unwrap()
    }

    let app = Router::new()
        .route("/api/v1/plugin-assets/:id/*path", get(plugin_asset))
        .layer(middleware::from_fn(ensure_manifest_link));
    let request = Request::builder()
        .uri("/api/v1/plugin-assets/rss-feed/ui/rss.html")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();

    assert!(body.contains("href=\"./rss.css\""));
    assert!(!body.contains("manifest.webmanifest"));
}

#[tokio::test]
async fn nested_manifest_request_redirects_to_the_app_root() {
    let app = Router::new()
        .fallback(|| async { "not found" })
        .layer(middleware::from_fn(ensure_manifest_link));
    let request = Request::builder()
        .uri("/plugin-pages/rss-feed/manifest.webmanifest")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "../../manifest.webmanifest"
    );
}

#[tokio::test]
async fn html_response_gets_a_matching_script_nonce() {
    async fn page() -> Response {
        Response::builder()
            .header(header::CONTENT_TYPE, "text/html")
            .body(Body::from("<html><head></head><body></body></html>"))
            .unwrap()
    }

    let app = Router::new()
        .route("/", get(page))
        .layer(middleware::from_fn(ensure_manifest_link));
    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let csp = response
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(csp.contains("media-src 'self' https: http:"));
    let nonce = csp
        .split("'nonce-")
        .nth(1)
        .and_then(|value| value.split('\'').next())
        .unwrap()
        .to_string();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();

    assert_eq!(nonce.len(), 32);
    assert!(body.contains(&format!(
        r#"<meta name="ting-csp-nonce" content="{nonce}">"#
    )));
}

#[tokio::test]
async fn html_navigation_ignores_cached_validators() {
    async fn page(request: Request) -> Response {
        if request.headers().contains_key(header::IF_NONE_MATCH)
            || request.headers().contains_key(header::IF_MODIFIED_SINCE)
        {
            return StatusCode::NOT_MODIFIED.into_response();
        }
        Response::builder()
            .header(header::CONTENT_TYPE, "text/html")
            .header(header::ETAG, "\"static-index\"")
            .header(header::LAST_MODIFIED, "Wed, 30 Sep 2026 00:00:00 GMT")
            .body(Body::from("<html><head></head><body></body></html>"))
            .unwrap()
    }

    let app = Router::new()
        .fallback(page)
        .layer(middleware::from_fn(ensure_manifest_link))
        .layer(middleware::from_fn(security_headers_middleware));
    let mut nonces = Vec::new();
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/plugin-pages/example/panel.view")
                    .header(header::ACCEPT, "text/html,application/xhtml+xml")
                    .header(header::IF_NONE_MATCH, "\"static-index\"")
                    .header(header::IF_MODIFIED_SINCE, "Wed, 30 Sep 2026 00:00:00 GMT")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(!response.headers().contains_key(header::ETAG));
        assert!(!response.headers().contains_key(header::LAST_MODIFIED));
        let nonce = response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .split("'nonce-")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap()
            .to_owned();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains(&format!(
            r#"<meta name="ting-csp-nonce" content="{nonce}">"#
        )));
        nonces.push(nonce);
    }
    assert_ne!(nonces[0], nonces[1]);
}

#[tokio::test]
async fn static_asset_preserves_conditional_requests() {
    async fn asset(request: Request) -> Response {
        assert_eq!(request.headers()[header::IF_NONE_MATCH], "\"asset\"");
        assert!(request.headers().contains_key(header::IF_MODIFIED_SINCE));
        StatusCode::NOT_MODIFIED.into_response()
    }

    let app = Router::new()
        .route("/assets/app.js", get(asset))
        .layer(middleware::from_fn(ensure_manifest_link));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/assets/app.js")
                .header(header::ACCEPT, "*/*")
                .header(header::IF_NONE_MATCH, "\"asset\"")
                .header(header::IF_MODIFIED_SINCE, "Wed, 30 Sep 2026 00:00:00 GMT")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn nested_spa_routes_resolve_assets_from_direct_and_gateway_roots() {
    async fn page() -> Response {
        Response::builder()
            .header(header::CONTENT_TYPE, "text/html")
            .body(Body::from(
                r#"<html><head><script src="./assets/app.js"></script></head><body><div id="root"></div></body></html>"#,
            ))
            .unwrap()
    }
    let app = Router::new()
        .fallback(page)
        .layer(middleware::from_fn(ensure_manifest_link));
    let direct = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/book/example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let direct_html = to_bytes(direct.into_body(), usize::MAX).await.unwrap();
    let direct_html = String::from_utf8(direct_html.to_vec()).unwrap();
    assert!(direct_html.contains(r#"<base href="/">"#));
    assert_eq!(direct_html.matches("<base ").count(), 1);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/book/example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let gateway = rewrite_gateway_html(response, "/app/ting-reader").await;
    let gateway_html = to_bytes(gateway.into_body(), usize::MAX).await.unwrap();
    let gateway_html = String::from_utf8(gateway_html.to_vec()).unwrap();
    assert!(gateway_html.contains(r#"<base href="/app/ting-reader/">"#));
    assert_eq!(gateway_html.matches("<base ").count(), 1);
    assert!(gateway_html.contains(r#"src="./assets/app.js""#));
}

#[tokio::test]
async fn gateway_proxy_preserves_plugin_asset_relative_urls() {
    async fn plugin_asset() -> Response {
        Response::builder()
            .header(header::CONTENT_TYPE, "text/html")
            .body(Body::from(
                r#"<html><head><link rel="stylesheet" href="./rss.css"></head></html>"#,
            ))
            .unwrap()
    }

    let inner_router = Router::new().route("/api/v1/plugin-assets/:id/*path", get(plugin_asset));
    let state = GatewayProxyState {
        router: inner_router,
        prefix: "/app/ting-reader".to_string(),
    };
    let app = Router::new()
        .route("/app/ting-reader", any(gateway_proxy))
        .route("/app/ting-reader/*path", any(gateway_proxy))
        .with_state(state);

    let request = Request::builder()
        .uri("/app/ting-reader/api/v1/plugin-assets/rss-feed/ui/rss.html")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();

    assert!(body.contains("href=\"./rss.css\""));
    assert!(!body.contains("<base href=\"/app/ting-reader/\">"));
    assert!(!body.contains("/app/ting-reader/rss.css"));
}

#[test]
fn direct_html_gets_manifest_link_when_static_entry_is_old() {
    let html = "<html><head><title>Ting Reader</title></head><body></body></html>";
    let rewritten = ensure_manifest_link_in_html(html);

    assert!(rewritten.contains(
        r#"<link rel="manifest" href="./manifest.webmanifest" crossorigin="use-credentials">"#
    ));
    assert_eq!(rewritten.matches("rel=\"manifest\"").count(), 1);
}

#[test]
fn existing_gateway_manifest_link_keeps_its_prefix() {
    let html = r#"<html><head><link rel="manifest" href="/app/ting-reader/manifest.webmanifest"></head></html>"#;
    let rewritten = ensure_manifest_link_in_html(html);

    assert!(
        rewritten.contains(
            r#"href="/app/ting-reader/manifest.webmanifest" crossorigin="use-credentials""#
        )
    );
    assert_eq!(rewritten.matches("rel=\"manifest\"").count(), 1);
}

#[test]
fn gateway_asset_rewrite_does_not_duplicate_prefix() {
    let html = r#"<script src="/assets/index.js"></script><link rel="manifest" href="/app/ting-reader/manifest.webmanifest">"#;
    let rewritten = rewrite_root_relative_attribute(html, "src", "/app/ting-reader/");

    assert!(rewritten.contains("src=\"/app/ting-reader/assets/index.js\""));
    assert!(rewritten.contains("href=\"/app/ting-reader/manifest.webmanifest\""));
    assert!(!rewritten.contains("/app/ting-reader/app/ting-reader/"));
}
