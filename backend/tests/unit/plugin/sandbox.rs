use super::*;

#[test]
fn test_domain_matches_exact() {
    assert!(Sandbox::domain_matches("example.com", "example.com"));
    assert!(!Sandbox::domain_matches("example.com", "other.com"));
}

#[test]
fn test_domain_matches_wildcard() {
    assert!(Sandbox::domain_matches("sub.example.com", "*.example.com"));
    assert!(Sandbox::domain_matches("example.com", "*.example.com"));
    assert!(!Sandbox::domain_matches("example.org", "*.example.com"));
    assert!(Sandbox::domain_matches("plugins.example.org", "*"));
}

#[test]
fn test_extract_domain() {
    assert_eq!(
        Sandbox::extract_domain("https://example.com/path").unwrap(),
        "example.com"
    );
    assert_eq!(
        Sandbox::extract_domain("http://example.com:8080/path").unwrap(),
        "example.com"
    );
}
