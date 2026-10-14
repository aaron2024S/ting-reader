//! Gateway prefix proxying and SPA document/resource rewriting.

use super::*;

pub(super) fn normalize_gateway_prefix(prefix: Option<&str>) -> Option<String> {
    let prefix = prefix?.trim();
    if prefix.is_empty() {
        return None;
    }

    let mut normalized = if prefix.starts_with('/') {
        prefix.to_string()
    } else {
        format!("/{prefix}")
    };
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    Some(normalized)
}

pub(super) async fn gateway_proxy(
    State(state): State<GatewayProxyState>,
    mut request: Request,
) -> Response {
    let original_uri = request.uri().clone();
    let stripped_path = original_uri
        .path()
        .strip_prefix(&state.prefix)
        .filter(|path| !path.is_empty())
        .unwrap_or("/");
    let should_rewrite_html = !is_plugin_asset_path(stripped_path);
    let path_and_query = match original_uri.query() {
        Some(query) => format!("{stripped_path}?{query}"),
        None => stripped_path.to_string(),
    };

    let mut parts = original_uri.into_parts();
    parts.path_and_query = match PathAndQuery::try_from(path_and_query) {
        Ok(path_and_query) => Some(path_and_query),
        Err(_) => {
            return (axum::http::StatusCode::BAD_REQUEST, "Invalid gateway path").into_response();
        }
    };
    let rewritten_uri = match Uri::from_parts(parts) {
        Ok(uri) => uri,
        Err(_) => {
            return (axum::http::StatusCode::BAD_REQUEST, "Invalid gateway URI").into_response();
        }
    };
    *request.uri_mut() = rewritten_uri;

    // The outer `/*path` route has already inserted its wildcard into Axum's
    // private URL-parameter extension. Re-dispatching the same request through
    // the inner router would append the inner route parameter, so handlers such
    // as `Path<String>` would see two values for a one-parameter route.
    // Rebuild the routing extensions before the second dispatch while retaining
    // the connection address used by login auditing and Hyper's pending upgrade.
    let connect_info = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .copied();
    let gateway_transport = request.extensions().get::<GatewayTransport>().copied();
    let on_upgrade = request
        .extensions_mut()
        .remove::<hyper::upgrade::OnUpgrade>();
    request.extensions_mut().clear();
    if let Some(connect_info) = connect_info {
        request.extensions_mut().insert(connect_info);
    }
    if let Some(gateway_transport) = gateway_transport {
        request.extensions_mut().insert(gateway_transport);
    }
    if let Some(on_upgrade) = on_upgrade {
        request.extensions_mut().insert(on_upgrade);
    }

    match state.router.oneshot(request).await {
        Ok(response) if should_rewrite_html => rewrite_gateway_html(response, &state.prefix).await,
        Ok(response) => response,
        Err(error) => match error {},
    }
}

pub(super) fn is_plugin_asset_path(path: &str) -> bool {
    path.starts_with("/api/v1/plugin-assets/") || path.starts_with("/api/plugin-assets/")
}

pub(super) fn rewrite_root_relative_attribute(html: &str, attribute: &str, prefix: &str) -> String {
    let root_attribute = format!("{attribute}=\"/");
    let prefixed_attribute = format!("{attribute}=\"{prefix}");
    let mut output = String::with_capacity(html.len());
    let mut remaining = html;

    while let Some(index) = remaining.find(&root_attribute) {
        output.push_str(&remaining[..index]);
        let candidate = &remaining[index..];

        if candidate.starts_with(&prefixed_attribute) {
            output.push_str(&prefixed_attribute);
            remaining = &candidate[prefixed_attribute.len()..];
        } else {
            output.push_str(&prefixed_attribute);
            remaining = &candidate[root_attribute.len()..];
        }
    }

    output.push_str(remaining);
    output
}

pub(super) async fn ensure_manifest_link(mut request: Request, next: Next) -> Response {
    if is_plugin_asset_path(request.uri().path()) {
        return next.run(request).await;
    }

    // HTML receives a fresh CSP nonce and rewritten body for every navigation.
    // A 304 would reuse the cached body while the security layer supplies a
    // nonce-free CSP, preventing the sandboxed plugin scripts from executing.
    if request
        .headers()
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
    {
        request.headers_mut().remove(header::IF_NONE_MATCH);
        request.headers_mut().remove(header::IF_MODIFIED_SINCE);
    }

    let is_manifest_request = request.uri().path().ends_with("/manifest.webmanifest");
    if is_manifest_request && request.uri().path() != "/manifest.webmanifest" {
        let parent_depth = request
            .uri()
            .path()
            .trim_matches('/')
            .split('/')
            .count()
            .saturating_sub(1);
        let location = format!("{}manifest.webmanifest", "../".repeat(parent_depth));
        return Response::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header(header::LOCATION, location)
            .body(Body::empty())
            .expect("manifest redirect response is valid");
    }
    let response = next.run(request).await;
    if is_manifest_request {
        return response;
    }

    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.starts_with("text/html"))
        .unwrap_or(false);

    if !is_html {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, 8 * 1024 * 1024).await else {
        return Response::from_parts(parts, Body::empty());
    };
    let Ok(html) = String::from_utf8(bytes.to_vec()) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let nonce = create_document_script_nonce();
    let html = ensure_spa_asset_base(&html);
    let rewritten = inject_document_script_nonce(&ensure_manifest_link_in_html(&html), &nonce);
    let csp = document_content_security_policy(&nonce);
    parts.headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&csp).expect("document CSP is valid"),
    );
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    parts.headers.remove(header::ETAG);
    parts.headers.remove(header::LAST_MODIFIED);

    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(rewritten))
}

pub(super) fn create_document_script_nonce() -> String {
    let mut bytes = [0_u8; 24];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(super) fn ensure_spa_asset_base(html: &str) -> String {
    // Relative Vite assets must resolve from the application root when a user
    // opens or reloads a nested route such as /book/:id.
    if html.contains("id=\"root\"") && !html.contains("<base ") {
        html.replacen("<head>", "<head><base href=\"/\">", 1)
    } else {
        html.to_string()
    }
}

pub(super) fn document_content_security_policy(nonce: &str) -> String {
    format!(
        "default-src 'self'; script-src 'self' 'nonce-{nonce}'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self'; connect-src 'self'; media-src 'self' https: http:; object-src 'none'; frame-ancestors 'none';"
    )
}

pub(super) fn inject_document_script_nonce(html: &str, nonce: &str) -> String {
    let meta = format!(r#"<meta name="ting-csp-nonce" content="{nonce}">"#);
    if let Some(index) = html.find("</head>") {
        let mut rewritten = String::with_capacity(html.len() + meta.len());
        rewritten.push_str(&html[..index]);
        rewritten.push_str(&meta);
        rewritten.push_str(&html[index..]);
        rewritten
    } else {
        format!("{meta}{html}")
    }
}

pub(super) fn ensure_manifest_link_in_html(html: &str) -> String {
    let manifest_start = ["<link rel=\"manifest", "<link rel='manifest"]
        .into_iter()
        .filter_map(|marker| html.find(marker))
        .min();

    if let Some(start) = manifest_start {
        let Some(relative_end) = html[start..].find('>') else {
            return html.to_string();
        };
        let end = start + relative_end;
        let tag = &html[start..=end];
        let mut rewritten_tag = tag.to_string();
        if !rewritten_tag.contains("crossorigin=") {
            let insert_at = rewritten_tag
                .rfind("/>")
                .unwrap_or_else(|| rewritten_tag.len() - 1);
            rewritten_tag.insert_str(insert_at, " crossorigin=\"use-credentials\"");
        }
        if rewritten_tag == tag {
            return html.to_string();
        }

        let mut output = String::with_capacity(html.len());
        output.push_str(&html[..start]);
        output.push_str(&rewritten_tag);
        output.push_str(&html[end + 1..]);
        return output;
    }

    let manifest_link =
        r#"<link rel="manifest" href="./manifest.webmanifest" crossorigin="use-credentials">"#;
    if let Some(index) = html.find("</head>") {
        let mut rewritten = String::with_capacity(html.len() + manifest_link.len());
        rewritten.push_str(&html[..index]);
        rewritten.push_str(manifest_link);
        rewritten.push_str(&html[index..]);
        rewritten
    } else {
        format!("{manifest_link}{html}")
    }
}

pub(super) async fn rewrite_gateway_html(response: Response, prefix: &str) -> Response {
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.starts_with("text/html"))
        .unwrap_or(false);

    if !is_html {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, 8 * 1024 * 1024).await else {
        return Response::from_parts(parts, Body::empty());
    };
    let Ok(mut html) = String::from_utf8(bytes.to_vec()) else {
        return Response::from_parts(parts, Body::from(bytes));
    };

    // The Docker image is also used for direct port access, so its Vite build
    // keeps root-relative assets. Rewrite only gateway HTML responses to the
    // registered prefix, keeping the direct TCP page fully compatible.
    let asset_prefix = format!("{prefix}/");
    html = ensure_manifest_link_in_html(&html);
    html = rewrite_root_relative_attribute(&html, "src", &asset_prefix);
    html = rewrite_root_relative_attribute(&html, "href", &asset_prefix);
    if !html.contains("<base ") {
        html = html.replacen(
            "<head>",
            &format!("<head><base href=\"{asset_prefix}\">"),
            1,
        );
    }

    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(html))
}
