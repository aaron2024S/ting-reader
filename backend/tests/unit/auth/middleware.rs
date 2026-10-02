use super::*;
use crate::auth::jwt::generate_token;
use axum::body::Body;

#[test]
fn collects_bearer_query_and_cookie_tokens() {
    let request = Request::builder()
        .uri("/api/settings?token=query-token")
        .header(header::AUTHORIZATION, "Bearer header-token")
        .header(
            header::COOKIE,
            "ost=fnos-session; ting_reader_token=cookie-token",
        )
        .body(Body::empty())
        .unwrap();

    assert_eq!(
        token_candidates(&request),
        vec!["header-token", "query-token", "cookie-token"]
    );
}

#[test]
fn accepts_valid_cookie_after_invalid_authorization_candidate() {
    let secret = "test-secret".to_string();
    let valid_token = generate_token("user-id", &secret).unwrap();
    let candidates = vec!["fnos-gateway-token".to_string(), valid_token];

    let claims = validate_token_candidates(&candidates, &[secret]).unwrap();

    assert_eq!(claims.user_id, "user-id");
}
