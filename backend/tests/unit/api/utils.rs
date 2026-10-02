use super::*;

fn test_user(role: &str) -> AuthUser {
    AuthUser {
        user_id: "user-1".to_string(),
        id: "user-1".to_string(),
        username: "alice".to_string(),
        role: role.to_string(),
    }
}

#[test]
fn require_admin_rejects_non_admin_users() {
    let error = require_admin(&test_user("user")).unwrap_err();
    assert!(matches!(error, TingError::PermissionDenied(_)));
    require_admin(&test_user("admin")).unwrap();
}

#[test]
fn normalizes_ipv4_mapped_ipv6_from_forwarded_header() {
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", "::ffff:192.168.1.17".parse().unwrap());

    assert_eq!(
        extract_real_ip(&headers, None).as_deref(),
        Some("192.168.1.17")
    );
}

#[test]
fn normalizes_ipv4_mapped_ipv6_peer_address() {
    let peer = "[::ffff:192.168.1.17]:3000".parse().unwrap();
    assert_eq!(
        extract_real_ip(&HeaderMap::new(), Some(peer)).as_deref(),
        Some("192.168.1.17")
    );
}
