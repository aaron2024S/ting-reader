use super::build_update_check_request;

#[test]
fn update_request_forces_fresh_no_cache_fetch() {
    let request = build_update_check_request(&reqwest::Client::new())
        .build()
        .expect("update request should build");

    assert_eq!(request.url().query(), Some("fresh=1"));
    assert_eq!(
        request
            .headers()
            .get("Cache-Control")
            .and_then(|value| value.to_str().ok()),
        Some("no-cache")
    );
    assert_eq!(
        request
            .headers()
            .get("Pragma")
            .and_then(|value| value.to_str().ok()),
        Some("no-cache")
    );
}
