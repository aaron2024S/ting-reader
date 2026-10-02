mod authorization {
    use super::super::authorization::*;
    use crate::auth::middleware::AuthUser;
    use crate::core::app::error::TingError;
    use chrono::Utc;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde_json::json;

    fn test_user(role: &str) -> AuthUser {
        AuthUser {
            user_id: "user-1".to_string(),
            id: "user-1".to_string(),
            username: "alice".to_string(),
            role: role.to_string(),
        }
    }

    #[test]
    fn plugin_system_writes_require_admin_role() {
        let error = require_plugin_system_write(&test_user("user")).unwrap_err();
        assert!(matches!(error, TingError::PermissionDenied(_)));

        require_plugin_system_write(&test_user("admin")).unwrap();
    }

    #[test]
    fn plugin_client_grants_require_a_valid_signature_type_and_expiry() {
        let secret = "test-plugin-grant-secret".to_string();
        let claims = PluginClientGrantClaims {
            sub: "user-1".to_string(),
            plugin_id: "demo@1.0.0".to_string(),
            capability_id: "demo.panel".to_string(),
            exp: (Utc::now() + chrono::Duration::minutes(5)).timestamp() as usize,
            grant_type: "plugin_client".to_string(),
        };
        let grant = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let decoded =
            decode_plugin_client_grant_with_secrets(&grant, &["old-secret".to_string(), secret])
                .unwrap();
        assert_eq!(decoded.sub, "user-1");
        assert_eq!(decoded.capability_id, "demo.panel");

        assert!(
            decode_plugin_client_grant_with_secrets(&grant, &["wrong-secret".to_string()]).is_err()
        );

        let wrong_type = PluginClientGrantClaims {
            grant_type: "access".to_string(),
            ..claims
        };
        let wrong_type_grant = encode(
            &Header::new(Algorithm::HS256),
            &wrong_type,
            &EncodingKey::from_secret("test-plugin-grant-secret".as_bytes()),
        )
        .unwrap();
        assert!(
            decode_plugin_client_grant_with_secrets(
                &wrong_type_grant,
                &["test-plugin-grant-secret".to_string()]
            )
            .is_err()
        );
    }

    #[test]
    fn plugin_route_context_includes_authenticated_user_only_for_private_routes() {
        let user = AuthUser {
            user_id: "user-1".to_string(),
            id: "user-1".to_string(),
            username: "alice".to_string(),
            role: "admin".to_string(),
        };

        let authenticated =
            plugin_route_context_json(PluginRouteAccess::Authenticated, Some(&user));
        assert_eq!(authenticated["access"], "authenticated");
        assert_eq!(authenticated["authenticated"], true);
        assert_eq!(authenticated["user"]["id"], "user-1");
        assert_eq!(authenticated["user"]["username"], "alice");
        assert_eq!(authenticated["user"]["role"], "admin");

        let public = plugin_route_context_json(PluginRouteAccess::Public, Some(&user));
        assert_eq!(public["access"], "public");
        assert_eq!(public["authenticated"], false);
        assert!(public["user"].is_null());

        let signed = plugin_route_context_json(PluginRouteAccess::SignedUser, Some(&user));
        assert_eq!(signed["access"], "signed");
        assert_eq!(signed["authenticated"], true);
        assert_eq!(signed["user"]["id"], "user-1");
    }

    #[test]
    fn plugin_invocation_context_is_attached_to_capability_params() {
        let user = AuthUser {
            user_id: "user-1".to_string(),
            id: "user-1".to_string(),
            username: "alice".to_string(),
            role: "user".to_string(),
        };

        let params = attach_plugin_invocation_context(
            json!({"prompt": "hello"}),
            "assistant@1.0.0",
            "assistant.ui",
            PluginRouteAccess::Authenticated,
            Some(&user),
        );

        assert_eq!(params["prompt"], "hello");
        assert_eq!(params["_context"]["plugin_id"], "assistant@1.0.0");
        assert_eq!(params["_context"]["capability_id"], "assistant.ui");
        assert_eq!(params["_context"]["route"]["user"]["username"], "alice");

        let wrapped = attach_plugin_invocation_context(
            json!("raw"),
            "assistant@1.0.0",
            "assistant.ui",
            PluginRouteAccess::Authenticated,
            Some(&user),
        );
        assert_eq!(wrapped["input"], "raw");
        assert!(wrapped["_context"].is_object());
    }
}

mod assets {
    use super::super::assets::*;
    use crate::plugin::types::PluginState;
    use std::path::PathBuf;

    #[test]
    fn plugin_asset_path_allows_only_ui_and_assets_directories() {
        assert_eq!(
            normalize_plugin_asset_path("ui/assistant.html").unwrap(),
            PathBuf::from("ui").join("assistant.html")
        );
        assert_eq!(
            normalize_plugin_asset_path("/assets/icon.png").unwrap(),
            PathBuf::from("assets").join("icon.png")
        );
        assert!(normalize_plugin_asset_path("data/secret.json").is_err());
        assert!(normalize_plugin_asset_path("ui/../plugin.yml").is_err());
    }

    #[test]
    fn plugin_document_assets_are_forced_to_download() {
        assert!(plugin_document_disposition("text/html; charset=utf-8").is_some());
        assert!(plugin_document_disposition("application/xhtml+xml").is_some());
        assert!(plugin_document_disposition("image/svg+xml").is_none());
        assert!(plugin_document_disposition("text/css").is_none());
    }

    #[test]
    fn plugin_assets_are_only_served_for_active_runtime_states() {
        assert!(plugin_assets_available(PluginState::Active));
        assert!(plugin_assets_available(PluginState::Executing));
        assert!(!plugin_assets_available(PluginState::Discovered));
        assert!(!plugin_assets_available(PluginState::Unloaded));
        assert!(!plugin_assets_available(PluginState::Failed));
    }
}

mod capabilities {
    use super::super::capabilities::*;
    use crate::core::app::error::TingError;
    use crate::plugin::types::metadata::parse_plugin_metadata_content;
    use crate::plugin::types::{PluginCapability, PluginMetadata};
    use axum::http::StatusCode;
    use serde_json::json;

    fn bridge_test_metadata() -> PluginMetadata {
        parse_plugin_metadata_content(
            r#"
    id: bridge-test
    name: Bridge Test
    version: 1.0.0
    author: Ting Reader
    description: {en: Bridge policy test}
    min_core_version: 2.0.0
    entry_point: index.js
    runtime: javascript
    capabilities:
      - id: assistant.panel
        kind: ui_extension
        slots: [global.panel]
        contexts: [global]
        title: {en: Panel}
        render:
          mode: web_container
          entry: ui/index.html
          bridge:
            capabilities: [assistant.tools]
            host_methods: [books.list, user_settings.get]
      - id: disabled.panel
        kind: ui_extension
        slots: [global.panel]
        contexts: [global]
        title: {en: Panel}
        render:
          mode: web_container
          entry: ui/disabled.html
          bridge:
            capabilities: []
            host_methods: []
      - id: assistant.tools
        kind: tool_provider
        invoke: invokeTool
        tools:
          - name: test.search
            description: {en: Search}
            input_schema: {type: object}
            output_schema: {type: object}
            side_effects: false
      - id: background.task
        kind: task_handler
        tasks:
          - task_type: books.summarize
            input_schema: {type: object}
            output_schema: {type: object}
            idempotent: true
    "#,
            "bridge-test.yml",
        )
        .unwrap()
    }

    #[test]
    fn ui_bridge_allows_current_and_declared_capabilities_only() {
        let metadata = bridge_test_metadata();

        require_ui_bridge_capability(&metadata, "assistant.panel", "assistant.panel").unwrap();
        require_ui_bridge_capability(&metadata, "assistant.panel", "assistant.tools").unwrap();

        let error = require_ui_bridge_capability(&metadata, "assistant.panel", "background.task")
            .unwrap_err();
        assert!(matches!(error, TingError::PermissionDenied(_)));
    }

    #[test]
    fn ui_bridge_with_empty_allowlist_rejects_other_capabilities() {
        let metadata = bridge_test_metadata();

        let error = require_ui_bridge_capability(&metadata, "disabled.panel", "assistant.tools")
            .unwrap_err();
        assert!(matches!(error, TingError::PermissionDenied(_)));
    }

    #[test]
    fn ui_bridge_allows_only_declared_host_methods() {
        let metadata = bridge_test_metadata();

        require_ui_bridge_host_method(&metadata, "assistant.panel", "books.list").unwrap();
        let error = require_ui_bridge_host_method(&metadata, "assistant.panel", "database.update")
            .unwrap_err();
        assert!(matches!(error, TingError::PermissionDenied(_)));
    }

    #[test]
    fn ui_bridge_rejects_non_ui_sources() {
        let metadata = bridge_test_metadata();

        let error = require_ui_bridge_capability(&metadata, "background.task", "background.task")
            .unwrap_err();
        assert!(matches!(error, TingError::PermissionDenied(_)));
    }

    #[test]
    fn unbridged_client_invocation_only_allows_content_processors() {
        let content: PluginCapability = serde_json::from_value(json!({
            "id":"content","kind":"content_processor","extensions":["txt"],
            "operations":["probe","open","close","cancel"]
        }))
        .unwrap();
        assert!(require_unbridged_client_capability(&content).is_ok());
        for cap in bridge_test_metadata().capabilities {
            assert!(require_unbridged_client_capability(&cap).is_err());
        }
    }

    #[test]
    fn plugin_capability_not_found_returns_not_found_error() {
        let error = plugin_capability_not_found("demo@1.0.0", "missing.capability");

        assert!(matches!(error, TingError::NotFound(_)));
        assert_eq!(error.status_code(), StatusCode::NOT_FOUND);
    }
}

mod installation {
    use super::super::installation::unverified_plugin_install_confirmation;
    use crate::plugin::installer::tr_package;

    #[test]
    fn unknown_publisher_confirmation_uses_the_signed_package_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("unknown.tr");
        let manifest = serde_json::json!({
            "id": "unknown-test", "name": "Unknown test", "version": "2.0.0",
            "min_core_version": "2.0.0", "author": "Test",
            "description": {"en": "Test"}, "runtime": "javascript",
            "entry_point": "plugin.js", "dependencies": [],
            "permissions": [
                {"type": "network_access", "domain": "*"},
                {"type": "file_write", "path": "data/output"},
                {"type": "books_write"},
            ],
            "capabilities": [
                {"kind": "plugin_store", "id": "test.store", "operations": ["list_plugins"]}
            ],
        });
        tr_package::write_test_signed_package(
            &package,
            serde_yaml::to_string(&manifest).unwrap().as_bytes(),
        );
        let review = unverified_plugin_install_confirmation(&package, false, None)
            .unwrap()
            .unwrap();
        assert_eq!(review.permissions.len(), 3);
        assert_eq!(review.runtime.as_deref(), Some("javascript"));
        assert_eq!(review.capabilities.len(), 1);
        assert_eq!(review.verification_status, "untrusted");
        assert!(!review.package_changed);
        assert_eq!(review.package_sha256.len(), 64);
        assert!(
            unverified_plugin_install_confirmation(&package, true, Some(&review.package_sha256),)
                .unwrap()
                .is_none()
        );
        // Legacy blanket acceptance cannot skip review of the actual package.
        assert!(
            unverified_plugin_install_confirmation(&package, true, None)
                .unwrap()
                .unwrap()
                .package_changed
        );

        let mut changed = manifest;
        changed["permissions"] = serde_json::json!([]);
        tr_package::write_test_signed_package(
            &package,
            serde_yaml::to_string(&changed).unwrap().as_bytes(),
        );
        let new_review =
            unverified_plugin_install_confirmation(&package, true, Some(&review.package_sha256))
                .unwrap()
                .unwrap();
        assert!(new_review.package_changed);
        assert!(new_review.permissions.is_empty());
        assert_ne!(new_review.package_sha256, review.package_sha256);
    }
}

mod routes {
    use super::super::routes::*;
    use crate::core::app::error::{Result, TingError};
    use crate::core::security::signing::{
        normalize_plugin_route_sign_path, sign_plugin_route_request,
    };
    use crate::plugin::types::PluginCapability;
    use axum::{
        body::to_bytes,
        http::{Method, StatusCode, Uri},
    };
    use base64::Engine;
    use serde_json::json;

    fn validate_public_plugin_route_access(
        capability: &PluginCapability,
        method: &Method,
        route_path: &str,
        uri: &Uri,
        signing_key: &[u8; 32],
    ) -> Result<()> {
        validate_public_plugin_route_access_with_revocations(
            capability,
            method,
            route_path,
            uri,
            signing_key,
            None,
        )
    }
    fn test_route(auth: &str) -> PluginCapability {
        serde_json::from_value(json!({"kind":"http_route","id":"test.route",
                "route":{"method":"GET","path":"/rss/{id}","auth":auth}}))
        .unwrap()
    }

    #[test]
    fn plugin_route_path_strips_api_prefixes() {
        let v1_uri: Uri = "/api/v1/plugin-routes/rss/main.xml?token=abc"
            .parse()
            .unwrap();
        let compat_uri: Uri = "/api/plugin-routes/assistant/chat".parse().unwrap();
        let public_uri: Uri = "/api/v1/public/plugin-routes/rss/main.xml".parse().unwrap();

        assert_eq!(plugin_route_path_from_uri(&v1_uri), "/rss/main.xml");
        assert_eq!(plugin_route_path_from_uri(&compat_uri), "/assistant/chat");
        assert_eq!(plugin_route_path_from_uri(&public_uri), "/rss/main.xml");
    }

    #[test]
    fn plugin_route_public_access_requires_explicit_auth_policy() {
        let private_capability = test_route("user");
        assert!(!plugin_route_allows_public_access(&private_capability));
        let private_capability = test_route("public_or_signed");
        assert!(plugin_route_allows_public_access(&private_capability));
    }

    #[test]
    fn signed_plugin_route_requires_valid_signature() {
        let capability = test_route("signed");

        let key = [7_u8; 32];
        let unsigned_uri: Uri = "/api/v1/public/plugin-routes/rss/main.xml".parse().unwrap();
        let unsigned_error = validate_public_plugin_route_access(
            &capability,
            &Method::GET,
            "/rss/main.xml",
            &unsigned_uri,
            &key,
        )
        .unwrap_err();
        assert!(matches!(unsigned_error, TingError::PermissionDenied(_)));

        let expires = chrono::Utc::now().timestamp() + 60;
        let signature = sign_plugin_route_request(&key, "GET", "/rss/main.xml", expires, None);
        let signed_uri: Uri = format!(
            "/api/v1/public/plugin-routes/rss/main.xml?expires={}&signature={}",
            expires, signature
        )
        .parse()
        .unwrap();
        validate_public_plugin_route_access(
            &capability,
            &Method::GET,
            "/rss/main.xml",
            &signed_uri,
            &key,
        )
        .unwrap();
    }

    #[test]
    fn signed_plugin_route_can_bind_user_context() {
        let capability = test_route("signed");

        let key = [9_u8; 32];
        let expires = chrono::Utc::now().timestamp() + 60;
        let signature =
            sign_plugin_route_request(&key, "GET", "/rss/main.xml", expires, Some("user-1"));
        let signed_uri: Uri = format!(
            "/api/v1/public/plugin-routes/rss/main.xml?expires={}&user=user-1&signature={}",
            expires, signature
        )
        .parse()
        .unwrap();
        validate_public_plugin_route_access(
            &capability,
            &Method::GET,
            "/rss/main.xml",
            &signed_uri,
            &key,
        )
        .unwrap();
        assert_eq!(
            signed_plugin_route_user(&signed_uri).as_deref(),
            Some("user-1")
        );

        let tampered_uri: Uri = format!(
            "/api/v1/public/plugin-routes/rss/main.xml?expires={}&user=user-2&signature={}",
            expires, signature
        )
        .parse()
        .unwrap();
        assert!(
            validate_public_plugin_route_access(
                &capability,
                &Method::GET,
                "/rss/main.xml",
                &tampered_uri,
                &key,
            )
            .is_err()
        );
    }

    #[test]
    fn expired_plugin_route_signature_is_rejected() {
        let capability = test_route("signed");

        let key = [3_u8; 32];
        let expires = chrono::Utc::now().timestamp() - 60;
        let signature = sign_plugin_route_request(&key, "GET", "/rss/main.xml", expires, None);
        let uri: Uri = format!(
            "/api/v1/public/plugin-routes/rss/main.xml?expires={}&signature={}",
            expires, signature
        )
        .parse()
        .unwrap();

        let error = validate_public_plugin_route_access(
            &capability,
            &Method::GET,
            "/rss/main.xml",
            &uri,
            &key,
        )
        .unwrap_err();
        assert!(matches!(error, TingError::PermissionDenied(_)));
    }

    #[test]
    fn plugin_route_sign_path_strips_known_prefixes() {
        assert_eq!(
            normalize_plugin_route_sign_path("/api/v1/plugin-routes/rss/main.xml?x=1"),
            "/rss/main.xml"
        );
        assert_eq!(
            normalize_plugin_route_sign_path("api/public/plugin-routes/tools/ping"),
            "/tools/ping"
        );
    }

    #[tokio::test]
    async fn plugin_route_result_builds_http_response() {
        let response = plugin_route_result_to_response(json!({
            "status": 200,
            "headers": {
                "content-type": "application/rss+xml; charset=utf-8"
            },
            "body": "<rss version=\"2.0\"></rss>"
        }))
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["content-type"],
            "application/rss+xml; charset=utf-8"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"<rss version=\"2.0\"></rss>");
    }

    #[test]
    fn plugin_route_body_supports_base64() {
        let body = plugin_route_response_body(&json!({
            "body_base64": base64::engine::general_purpose::STANDARD.encode([0, 1, 2, 3])
        }))
        .unwrap();

        assert_eq!(body, vec![0, 1, 2, 3]);
    }

    #[test]
    fn plugin_route_result_rejects_invalid_status() {
        let error = plugin_route_result_to_response(json!({ "status": 9999 })).unwrap_err();

        assert!(matches!(error, TingError::PluginExecutionError(_)));
    }
}
