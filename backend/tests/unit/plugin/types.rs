mod manifest {
    use super::super::metadata::*;
    use crate::plugin::types::{Permission, PluginCapability};
    use serde_json::Value;

    fn valid_manifest_value() -> Value {
        serde_json::json!({
            "id": "safe-plugin",
            "name": "Safe Plugin",
            "version": "1.2.3",
            "runtime": "javascript",
            "entry_point": "dist/plugin.js",
            "author": "Ting Reader",
            "description": {"en": "Safe plugin"},
            "min_core_version": "2.0.0",
            "capabilities": [{"id": "safe.tools", "kind": "tool_provider", "invoke": "invokeTool", "tools": [{"name": "books.search", "description": {"en": "Search books"}, "input_schema": {"type": "object"}, "output_schema": {"type": "object"}, "side_effects": false}]}]
        })
    }

    #[test]
    fn rejects_unsafe_manifest_identity_fields() {
        let cases = [
            ("id", "../../escape"),
            ("id", "safe@1"),
            ("version", "../1.0.0"),
            ("version", "1.0"),
            ("name", "../shared"),
            ("name", "CON"),
            ("entry_point", "../plugin.js"),
            ("entry_point", "/tmp/plugin.js"),
            ("entry_point", r"C:\outside\plugin.js"),
        ];

        for (field, value) in cases {
            let mut manifest = valid_manifest_value();
            manifest[field] = Value::String(value.to_string());
            let error = parse_plugin_metadata_value(manifest, "malicious-plugin.yml")
                .expect_err("unsafe manifest identity should be rejected");
            assert!(
                error.to_string().contains(field),
                "unexpected error for {field}: {error}"
            );
        }

        let mut missing_id = valid_manifest_value();
        missing_id.as_object_mut().unwrap().remove("id");
        let error = parse_plugin_metadata_value(missing_id, "missing-id.yml").unwrap_err();
        assert!(error.to_string().contains("id"));
    }

    #[test]
    fn accepts_safe_nested_entry_point_and_semver() {
        let metadata =
            parse_plugin_metadata_value(valid_manifest_value(), "safe-plugin.yml").unwrap();
        assert_eq!(metadata.instance_id(), "safe-plugin@1.2.3");
        assert_eq!(metadata.entry_point, "dist/plugin.js");
    }

    #[test]
    fn rejects_unsafe_plugin_instance_ids() {
        for instance_id in [
            "../../outside@1.0.0",
            "safe-plugin@../1.0.0",
            "safe-plugin",
            "safe-plugin@1.0",
        ] {
            assert!(
                validate_plugin_instance_id(instance_id).is_err(),
                "unsafe instance id was accepted: {instance_id}"
            );
        }
        validate_plugin_instance_id("safe-plugin@1.0.0").unwrap();
    }

    #[test]
    fn parses_declared_capabilities() {
        let manifest = r#"
    id: ai-assistant
    name: AI Assistant
    version: 1.0.0
    runtime: javascript
    entry_point: assistant.js
    author: Ting Reader
    description: {en: AI assistant}
    min_core_version: 2.0.0
    capabilities:
      - id: assistant.float
        kind: ui_extension
        slots: [global.floating_action]
        contexts: [global]
        title: {en: Assistant}
        icon: message-circle
        render:
          mode: web_container
          entry: ui/assistant.html
          bridge: {capabilities: [], host_methods: []}
    "#;

        let metadata = parse_plugin_metadata_content(manifest, "test-plugin.yml").unwrap();

        assert_eq!(metadata.capabilities.len(), 1);
        assert_eq!(metadata.capabilities[0].id(), "assistant.float");
        let PluginCapability::UiExtension(cap) = &metadata.capabilities[0] else {
            panic!("expected UI");
        };
        assert_eq!(cap.slots[0].as_str(), "global.floating_action");
        assert_eq!(cap.icon.as_deref(), Some("message-circle"));
        assert_eq!(cap.render.mode.as_str(), "web_container");
    }

    #[test]
    fn parses_admin_only_plugin_metadata() {
        let manifest = r#"
    id: admin-tool
    name: Admin Tool
    version: 1.0.0
    runtime: javascript
    entry_point: plugin.js
    author: Ting Reader
    description: {en: Admin tool}
    min_core_version: 2.0.0
    admin_only: true
    capabilities:
      - id: admin.panel
        kind: ui_extension
        slots: [global.panel]
        contexts: [global]
        title: {en: Admin}
        render: {mode: action, bridge: {capabilities: [], host_methods: []}}
    "#;

        let metadata = parse_plugin_metadata_content(manifest, "test-plugin.yml").unwrap();

        assert!(metadata.admin_only);
    }

    #[test]
    fn parses_host_gateway_permissions_without_values() {
        let manifest = r#"
    id: rss-feed
    name: RSS Feed
    version: 1.0.0
    runtime: javascript
    entry_point: rss.js
    author: Ting Reader
    description: {en: RSS feed plugin}
    min_core_version: 2.0.0
    capabilities:
      - id: rss.feed
        kind: http_route
        route: {method: GET, path: /rss/feed, auth: user}
    permissions:
      - type: books_read
      - type: chapters_read
      - type: progress_read
      - type: media_read_url
      - type: plugin_route_sign
      - type: network_access
        domain: example.com
    "#;

        let metadata = parse_plugin_metadata_content(manifest, "test-plugin.yml").unwrap();

        assert!(metadata.permissions.contains(&Permission::BooksRead));
        assert!(metadata.permissions.contains(&Permission::ChaptersRead));
        assert!(metadata.permissions.contains(&Permission::ProgressRead));
        assert!(metadata.permissions.contains(&Permission::MediaReadUrl));
        assert!(metadata.permissions.contains(&Permission::PluginRouteSign));
        assert!(metadata.permissions.contains(&Permission::NetworkAccess {
            domain: "example.com".into()
        }));
    }

    #[test]
    fn derives_metadata_provider_declarations_from_capability() {
        let manifest = r#"
    id: metadata-source
    name: Metadata Source
    version: 1.0.0
    runtime: javascript
    entry_point: plugin.js
    author: Ting Reader
    description: {en: Metadata source}
    min_core_version: 2.0.0
    capabilities:
      - id: metadata.search
        kind: metadata_provider
        operations: [search]
        auto_scrape: true
        search_fields:
          - key: title
            label: {en: Title}
            type: text
            required: true
            default_from: book.title
        result_fields:
          - key: title
            label: {en: Title}
    "#;

        let metadata = parse_plugin_metadata_content(manifest, "test-plugin.yml").unwrap();
        assert_eq!(metadata.supported_extensions, None);
        let scraper = metadata.scraper.unwrap();
        assert_eq!(scraper.search_fields.len(), 1);
        assert_eq!(scraper.result_fields, vec!["title"]);
    }
}

mod capabilities {
    use super::super::*;

    #[test]
    fn effective_capabilities_uses_declared_capabilities_only() {
        let metadata = PluginMetadata::new(
            "assistant".to_string(),
            "Assistant".to_string(),
            "1.0.0".to_string(),
            "Ting Reader".to_string(),
            "Assistant".to_string(),
            "plugin.js".to_string(),
        );

        assert!(metadata.effective_capabilities().is_empty());

        let mut metadata = metadata;
        metadata.capabilities.push(
            serde_json::from_value(serde_json::json!({
                "id":"assistant.ui", "kind":"ui_extension", "slots":["global.panel"],
                "contexts":["global"], "title":{"en":"Assistant"},
                "render":{"mode":"action","bridge":{"capabilities":[],"host_methods":[]}}
            }))
            .unwrap(),
        );

        let capabilities = metadata.effective_capabilities();

        assert_eq!(capabilities.len(), 1);
        assert_eq!(capabilities[0].id(), "assistant.ui");
        assert_eq!(capabilities[0].kind(), "ui_extension");
        assert!(capabilities[0].supports("open"));
    }
}

mod scraper {
    use super::super::scraper::*;

    #[test]
    fn test_search_result_serialization() {
        let result = SearchResult {
            items: vec![BookItem {
                id: Some("123".to_string()),
                source_url: None,
                title: "Test Book".to_string(),
                author: "Test Author".to_string(),
                cover_url: Some("https://example.com/cover.jpg".to_string()),
                intro: Some("Test intro".to_string()),
                narrator: None,
                subtitle: None,
                published_year: None,
                published_date: None,
                publisher: None,
                isbn: None,
                asin: None,
                language: None,
                genre: None,
                explicit: None,
                abridged: None,
                tags: vec![],
                duration: None,
                chapter_title_template: None,
                chapter_titles: vec![],
                score: None,
            }],
            total: Some(100),
            has_more: Some(true),
            page: 1,
            page_size: 20,
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: SearchResult = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.items.len(), 1);
        assert_eq!(deserialized.total, Some(100));
        assert_eq!(deserialized.page, 1);
    }

    #[test]
    fn test_book_detail_serialization() {
        let detail = BookDetail {
            id: Some("456".to_string()),
            title: "Test Book".to_string(),
            author: "Test Author".to_string(),
            narrator: Some("Test Narrator".to_string()),
            cover_url: Some("https://example.com/cover.jpg".to_string()),
            subtitle: None,
            published_year: None,
            published_date: None,
            publisher: None,
            isbn: None,
            asin: None,
            language: None,
            explicit: false,
            abridged: false,
            intro: "Full introduction".to_string(),
            tags: vec!["fiction".to_string(), "sci-fi".to_string()],
            genre: None,
            chapter_count: 50,
            duration: Some(36000),
            chapter_title_template: None,
            chapter_titles: vec!["Chapter 1".to_string(), "Chapter 2".to_string()],
        };

        let json = serde_json::to_string(&detail).unwrap();
        let deserialized: BookDetail = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id.as_deref(), Some("456"));
        assert_eq!(deserialized.chapter_count, 50);
        assert_eq!(deserialized.tags.len(), 2);
        assert_eq!(deserialized.chapter_titles.len(), 2);
    }

    #[test]
    fn test_chapter_serialization() {
        let chapter = Chapter {
            id: "789".to_string(),
            title: "Chapter 1".to_string(),
            index: 0,
            duration: Some(1800),
            is_free: true,
        };

        let json = serde_json::to_string(&chapter).unwrap();
        let deserialized: Chapter = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "789");
        assert_eq!(deserialized.index, 0);
        assert!(deserialized.is_free);
    }

    #[test]
    fn test_chapter_default_is_free() {
        let json = r#"{"id":"789","title":"Chapter 1","index":0}"#;
        let chapter: Chapter = serde_json::from_str(json).unwrap();

        assert!(chapter.is_free);
    }
}
