use super::*;
use serde_json::json;

#[test]
fn plugin_host_permission_requires_matching_manifest_permission() {
    assert_eq!(
        PluginHostGateway::required_permission("books.list"),
        Some(PluginHostPermission::BooksRead)
    );
    assert_eq!(
        PluginHostGateway::required_permission("chapters.get"),
        Some(PluginHostPermission::ChaptersRead)
    );
    assert_eq!(
        PluginHostGateway::required_permission("libraries.list"),
        Some(PluginHostPermission::LibrariesRead)
    );
    assert_eq!(
        PluginHostGateway::required_permission("progress.recent"),
        Some(PluginHostPermission::ProgressRead)
    );
    assert_eq!(
        PluginHostGateway::required_permission("media.get_url"),
        Some(PluginHostPermission::MediaReadUrl)
    );
    assert_eq!(
        PluginHostGateway::required_permission("media.get_signed_url"),
        Some(PluginHostPermission::MediaReadUrl)
    );
    assert_eq!(
        PluginHostGateway::required_permission("plugin_routes.sign"),
        Some(PluginHostPermission::PluginRouteSign)
    );
    assert_eq!(
        PluginHostGateway::required_permission("metadata.write"),
        Some(PluginHostPermission::MetadataWrite)
    );
    assert_eq!(
        PluginHostGateway::required_permission("tasks.create"),
        Some(PluginHostPermission::TaskCreate)
    );
    assert_eq!(
        PluginHostGateway::required_permission("cache.get"),
        Some(PluginHostPermission::CacheRead)
    );
    assert_eq!(
        PluginHostGateway::required_permission("cache.set"),
        Some(PluginHostPermission::CacheWrite)
    );
    assert_eq!(
        PluginHostGateway::required_permission("unknown.method"),
        None
    );

    assert!(PluginHostGateway::has_permission(
        &[Permission::BooksRead],
        PluginHostPermission::BooksRead
    ));
    assert!(PluginHostGateway::has_permission(
        &[Permission::LibrariesRead],
        PluginHostPermission::LibrariesRead
    ));
    assert!(!PluginHostGateway::has_permission(
        &[Permission::BooksRead],
        PluginHostPermission::LibrariesRead
    ));
    assert!(!PluginHostGateway::has_permission(
        &[Permission::BooksRead],
        PluginHostPermission::ChaptersRead
    ));
    assert!(PluginHostGateway::has_permission(
        &[Permission::ChaptersRead],
        PluginHostPermission::ChaptersRead
    ));
    assert!(PluginHostGateway::has_permission(
        &[Permission::ProgressRead],
        PluginHostPermission::ProgressRead
    ));
    assert!(!PluginHostGateway::has_permission(
        &[Permission::BooksRead],
        PluginHostPermission::ProgressRead
    ));
    assert!(PluginHostGateway::has_permission(
        &[Permission::MediaReadUrl],
        PluginHostPermission::MediaReadUrl
    ));
    assert!(PluginHostGateway::has_permission(
        &[Permission::PluginRouteSign],
        PluginHostPermission::PluginRouteSign
    ));
    assert!(!PluginHostGateway::has_permission(
        &[Permission::BooksRead],
        PluginHostPermission::MediaReadUrl
    ));
    assert!(PluginHostGateway::has_permission(
        &[Permission::MetadataWrite],
        PluginHostPermission::MetadataWrite
    ));
    assert!(!PluginHostGateway::has_permission(
        &[Permission::BooksWrite],
        PluginHostPermission::MetadataWrite
    ));
    assert!(PluginHostGateway::has_permission(
        &[Permission::TaskCreate],
        PluginHostPermission::TaskCreate
    ));
    assert!(!PluginHostGateway::has_permission(
        &[Permission::BooksWrite],
        PluginHostPermission::TaskCreate
    ));
    assert!(PluginHostGateway::has_permission(
        &[Permission::CacheRead],
        PluginHostPermission::CacheRead
    ));
    assert!(PluginHostGateway::has_permission(
        &[Permission::CacheWrite],
        PluginHostPermission::CacheRead
    ));
    assert!(PluginHostGateway::has_permission(
        &[Permission::CacheWrite],
        PluginHostPermission::CacheWrite
    ));
    assert!(!PluginHostGateway::has_permission(
        &[Permission::CacheRead],
        PluginHostPermission::CacheWrite
    ));
}

#[test]
fn plugin_host_authorize_rejects_unknown_or_missing_permissions() {
    assert!(PluginHostGateway::authorize("demo", &[Permission::BooksRead], "books.list").is_ok());
    assert!(
        PluginHostGateway::authorize("demo", &[Permission::ChaptersRead], "chapters.get").is_ok()
    );
    assert!(
        PluginHostGateway::authorize("demo", &[Permission::BooksWrite], "books.update").is_ok()
    );
    assert!(
        PluginHostGateway::authorize("demo", &[Permission::BooksRead], "books.update").is_err()
    );
    assert!(PluginHostGateway::required_permission("database.update").is_none());

    let unknown = PluginHostGateway::authorize("demo", &[Permission::BooksRead], "unknown.method")
        .unwrap_err();
    assert!(matches!(unknown, TingError::InvalidRequest(_)));

    let missing =
        PluginHostGateway::authorize("demo", &[Permission::BooksRead], "cache.set").unwrap_err();
    assert!(matches!(missing, TingError::PermissionDenied(_)));
}

#[test]
fn plugin_host_param_helpers_parse_optional_values() {
    let params = json!({
        "book_id": " book-1 ",
        "limit": 25,
        "download": true,
        "empty": " "
    });

    assert_eq!(string_param(&params, "book_id"), Some("book-1".to_string()));
    assert_eq!(string_param(&params, "empty"), None);
    assert_eq!(usize_param(&params, "limit"), Some(25));
    assert_eq!(bool_param(&params, "download"), Some(true));
    assert!(required_string_param(&params, "missing").is_err());
}

#[test]
fn plugin_task_priority_defaults_to_normal() {
    assert_eq!(plugin_task_priority(&json!({})), Priority::Normal);
    assert_eq!(
        plugin_task_priority(&json!({ "priority": "high" })),
        Priority::High
    );
    assert_eq!(
        plugin_task_priority(&json!({ "priority": "low" })),
        Priority::Low
    );
}
