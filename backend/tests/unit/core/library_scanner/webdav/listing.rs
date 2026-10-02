use super::*;

#[test]
fn parses_sync_token_from_namespaced_propfind() {
    let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:">
  <D:response><D:propstat><D:prop>
    <D:sync-token>http://example.test/token/42</D:sync-token>
  </D:prop></D:propstat></D:response>
</D:multistatus>"#;

    assert_eq!(
        parse_webdav_sync_token(xml).as_deref(),
        Some("http://example.test/token/42")
    );
}

#[test]
fn parses_http_status_code() {
    assert_eq!(webdav_status_code("HTTP/1.1 404 Not Found"), Some(404));
    assert_eq!(webdav_status_code("HTTP/2 200"), Some(200));
}

#[test]
fn classifies_sync_response_statuses() {
    assert_eq!(
        webdav_sync_response_disposition(Some(404)),
        WebDavSyncResponseDisposition::Apply
    );
    assert_eq!(
        webdav_sync_response_disposition(Some(507)),
        WebDavSyncResponseDisposition::Truncated
    );
    assert_eq!(
        webdav_sync_response_disposition(Some(403)),
        WebDavSyncResponseDisposition::Fallback
    );
    assert_eq!(
        webdav_sync_response_disposition(Some(500)),
        WebDavSyncResponseDisposition::Fallback
    );
}

#[test]
fn builds_parent_url_without_borrowing_path_segments() {
    assert_eq!(
        webdav_parent_url("https://example.test/dav/books/one/001.mp3").as_deref(),
        Some("https://example.test/dav/books/one")
    );
}
