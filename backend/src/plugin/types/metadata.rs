//! Shared plugin metadata reading
//!
//! Reads and validates plugin.yml/plugin.yaml, producing a PluginMetadata struct.

use semver::Version;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use ting_plugin_contract::manifest::PluginManifest;

use super::{
    LocalizedText, PluginCapability, PluginDependency, PluginMetadata, ScraperCapabilities,
};
use crate::core::error::TingError;
#[cfg(test)]
use ting_plugin_contract::manifest::Permission;

const PLUGIN_MANIFEST_NAMES: [&str; 2] = ["plugin.yml", "plugin.yaml"];
const MAX_PLUGIN_ID_LENGTH: usize = 64;
const MAX_PLUGIN_NAME_LENGTH: usize = 128;
const MAX_PLUGIN_VERSION_LENGTH: usize = 64;
const MAX_PLUGIN_ENTRY_POINT_LENGTH: usize = 240;

pub fn find_plugin_manifest(path: &Path) -> Option<PathBuf> {
    PLUGIN_MANIFEST_NAMES
        .iter()
        .map(|name| path.join(name))
        .find(|candidate| candidate.is_file())
}

pub fn has_plugin_manifest(path: &Path) -> bool {
    find_plugin_manifest(path).is_some()
}

/// Read plugin metadata from a plugin.yml/plugin.yaml file in the given directory.
pub fn read_plugin_metadata(path: &Path) -> Result<PluginMetadata, TingError> {
    let metadata_path = find_plugin_manifest(path).ok_or_else(|| {
        TingError::PluginLoadError(format!("plugin.yml not found in: {}", path.display()))
    })?;

    let content = std::fs::read_to_string(&metadata_path).map_err(|e| {
        TingError::PluginLoadError(format!("Failed to read {}: {}", metadata_path.display(), e))
    })?;

    parse_plugin_metadata_content(&content, &metadata_path.display().to_string())
}

pub fn parse_plugin_metadata_content(
    content: &str,
    manifest_label: &str,
) -> Result<PluginMetadata, TingError> {
    let manifest: PluginManifest = serde_yaml::from_str(content).map_err(|error| {
        TingError::PluginLoadError(format!(
            "Invalid YAML plugin manifest in {}: {}",
            manifest_label, error
        ))
    })?;
    metadata_from_manifest(manifest, manifest_label)
}

pub fn parse_plugin_metadata_value(
    json: Value,
    manifest_label: &str,
) -> Result<PluginMetadata, TingError> {
    let manifest: PluginManifest = serde_json::from_value(json).map_err(|error| {
        TingError::PluginLoadError(format!(
            "Invalid plugin manifest in {}: {}",
            manifest_label, error
        ))
    })?;
    metadata_from_manifest(manifest, manifest_label)
}

fn metadata_from_manifest(
    manifest: PluginManifest,
    manifest_label: &str,
) -> Result<PluginMetadata, TingError> {
    manifest.validate().map_err(|error| {
        TingError::PluginLoadError(format!(
            "Invalid plugin manifest in {}: {}",
            manifest_label, error
        ))
    })?;
    validate_plugin_manifest_identity(
        &manifest.id,
        &manifest.name,
        &manifest.version,
        &manifest.entry_point,
        manifest_label,
    )?;
    for capability in &manifest.capabilities {
        crate::plugin::schema::validate_capability_schemas(capability)?;
    }
    let description_i18n = manifest.description;
    let description = display_text(&description_i18n);
    let supported_extensions = derive_supported_extensions(&manifest.capabilities);
    let scraper = derive_scraper_capabilities(&manifest.capabilities);
    let permissions = manifest.permissions;
    let dependencies = manifest
        .dependencies
        .into_iter()
        .map(|dependency| {
            PluginDependency::new(dependency.plugin_id, dependency.version_requirement)
        })
        .collect();

    Ok(PluginMetadata {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        author: manifest.author,
        description,
        description_i18n,
        license: manifest.license,
        repo: manifest.repo,
        entry_point: manifest.entry_point,
        runtime: Some(manifest.runtime.as_str().to_string()),
        dependencies,
        permissions,
        config_schema: manifest.config_schema,
        min_core_version: Some(manifest.min_core_version),
        min_flutter_version: manifest.min_flutter_version,
        admin_only: manifest.admin_only,
        supported_extensions,
        scraper,
        capabilities: manifest.capabilities,
    })
}

fn validate_plugin_manifest_identity(
    id: &str,
    name: &str,
    version: &str,
    entry_point: &str,
    manifest_label: &str,
) -> Result<(), TingError> {
    validate_plugin_id(id, manifest_label)?;
    validate_plugin_name(name, manifest_label)?;
    validate_plugin_version(version, manifest_label)?;
    validate_plugin_entry_point(entry_point, manifest_label)
}

fn validate_plugin_id(id: &str, manifest_label: &str) -> Result<(), TingError> {
    if id.is_empty() || id != id.trim() {
        return Err(invalid_manifest_field(
            "id",
            manifest_label,
            "must be non-empty and have no surrounding whitespace",
        ));
    }
    if id.len() > MAX_PLUGIN_ID_LENGTH {
        return Err(invalid_manifest_field(
            "id",
            manifest_label,
            &format!("must not exceed {} characters", MAX_PLUGIN_ID_LENGTH),
        ));
    }

    let bytes = id.as_bytes();
    let is_edge_valid = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if !is_edge_valid(bytes[0]) || !is_edge_valid(bytes[bytes.len() - 1]) {
        return Err(invalid_manifest_field(
            "id",
            manifest_label,
            "must start and end with a lowercase ASCII letter or digit",
        ));
    }
    if !bytes.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
    }) || id.contains("..")
        || is_windows_reserved_name(id)
    {
        return Err(invalid_manifest_field(
            "id",
            manifest_label,
            "may contain only lowercase ASCII letters, digits, '-', '_' and '.'",
        ));
    }
    Ok(())
}

fn validate_plugin_name(name: &str, manifest_label: &str) -> Result<(), TingError> {
    if name.is_empty() || name != name.trim() {
        return Err(invalid_manifest_field(
            "name",
            manifest_label,
            "must be non-empty and have no surrounding whitespace",
        ));
    }
    if name.chars().count() > MAX_PLUGIN_NAME_LENGTH {
        return Err(invalid_manifest_field(
            "name",
            manifest_label,
            &format!("must not exceed {} characters", MAX_PLUGIN_NAME_LENGTH),
        ));
    }
    validate_path_segment(name).map_err(|reason| {
        invalid_manifest_field(
            "name",
            manifest_label,
            &format!("is not path-safe: {}", reason),
        )
    })
}

fn validate_plugin_version(version: &str, manifest_label: &str) -> Result<(), TingError> {
    if version.is_empty()
        || version != version.trim()
        || version.chars().count() > MAX_PLUGIN_VERSION_LENGTH
    {
        return Err(invalid_manifest_field(
            "version",
            manifest_label,
            &format!(
                "must be a non-empty SemVer string no longer than {} characters",
                MAX_PLUGIN_VERSION_LENGTH
            ),
        ));
    }
    Version::parse(version).map_err(|error| {
        invalid_manifest_field(
            "version",
            manifest_label,
            &format!("must be valid SemVer: {}", error),
        )
    })?;
    Ok(())
}

fn validate_plugin_entry_point(entry_point: &str, manifest_label: &str) -> Result<(), TingError> {
    if entry_point.is_empty()
        || entry_point != entry_point.trim()
        || entry_point.chars().count() > MAX_PLUGIN_ENTRY_POINT_LENGTH
    {
        return Err(invalid_manifest_field(
            "entry_point",
            manifest_label,
            &format!(
                "must be a non-empty relative path no longer than {} characters",
                MAX_PLUGIN_ENTRY_POINT_LENGTH
            ),
        ));
    }
    if entry_point.contains('\\') {
        return Err(invalid_manifest_field(
            "entry_point",
            manifest_label,
            "must use forward slashes",
        ));
    }

    let path = Path::new(entry_point);
    if path.is_absolute() {
        return Err(invalid_manifest_field(
            "entry_point",
            manifest_label,
            "must stay within the plugin directory",
        ));
    }

    let mut components = 0;
    for component in path.components() {
        let Component::Normal(segment) = component else {
            return Err(invalid_manifest_field(
                "entry_point",
                manifest_label,
                "must not contain parent, current-directory, root, or prefix components",
            ));
        };
        let segment = segment.to_str().ok_or_else(|| {
            invalid_manifest_field("entry_point", manifest_label, "must be valid UTF-8")
        })?;
        validate_path_segment(segment).map_err(|reason| {
            invalid_manifest_field(
                "entry_point",
                manifest_label,
                &format!("contains an unsafe path segment: {}", reason),
            )
        })?;
        components += 1;
    }
    if components == 0 {
        return Err(invalid_manifest_field(
            "entry_point",
            manifest_label,
            "must identify a file",
        ));
    }

    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(
        extension.as_deref(),
        Some("js" | "wasm" | "dll" | "so" | "dylib")
    ) {
        return Err(invalid_manifest_field(
            "entry_point",
            manifest_label,
            "must use a supported .js, .wasm, .dll, .so, or .dylib extension",
        ));
    }
    Ok(())
}

fn validate_path_segment(segment: &str) -> std::result::Result<(), &'static str> {
    if segment.is_empty() || matches!(segment, "." | "..") {
        return Err("empty and dot segments are not allowed");
    }
    if segment
        .chars()
        .any(|character| character.is_control() || r#"<>:\"/\\|?*"#.contains(character))
    {
        return Err("contains control characters or reserved path characters");
    }
    if segment.ends_with([' ', '.']) {
        return Err("must not end with a space or dot");
    }
    if is_windows_reserved_name(segment) {
        return Err("uses a reserved device name");
    }
    Ok(())
}

fn is_windows_reserved_name(value: &str) -> bool {
    let stem = value
        .trim_end_matches([' ', '.'])
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn invalid_manifest_field(field: &str, manifest_label: &str, reason: &str) -> TingError {
    TingError::PluginLoadError(format!(
        "Invalid '{}' field in {}: {}",
        field, manifest_label, reason
    ))
}

pub(crate) fn validate_plugin_instance_id(instance_id: &str) -> Result<(), TingError> {
    let (id, version) = instance_id.rsplit_once('@').ok_or_else(|| {
        TingError::PluginLoadError(format!(
            "Invalid plugin instance ID '{}': expected <id>@<version>",
            instance_id
        ))
    })?;
    validate_plugin_id(id, "plugin instance ID")?;
    validate_plugin_version(version, "plugin instance ID")
}

fn derive_supported_extensions(capabilities: &[PluginCapability]) -> Option<Vec<String>> {
    let mut extensions = Vec::new();

    for capability in capabilities {
        for extension in capability.extensions() {
            push_unique(&mut extensions, extension.clone());
        }
    }

    (!extensions.is_empty()).then_some(extensions)
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn derive_scraper_capabilities(capabilities: &[PluginCapability]) -> Option<ScraperCapabilities> {
    // This is the API's display model, not a second manifest parser.
    capabilities.iter().find_map(|capability| {
        let PluginCapability::MetadataProvider(cap) = capability else {
            return None;
        };
        Some(ScraperCapabilities {
            auto_scrape: cap.auto_scrape,
            search_fields: cap
                .search_fields
                .iter()
                .map(|field| super::ScraperSearchField {
                    key: field.key.clone(),
                    label: display_text(&field.label),
                    label_i18n: Some(field.label.clone()),
                    required: field.required,
                    field_type: Some(field.field_type.as_str().to_string()),
                    placeholder: field.placeholder.as_ref().map(display_text),
                    placeholder_i18n: field.placeholder.clone(),
                    default_from: field.default_from.clone(),
                })
                .collect(),
            result_fields: cap
                .result_fields
                .iter()
                .map(|field| field.key.clone())
                .collect(),
            result_field_labels: cap
                .result_fields
                .iter()
                .map(|field| (field.key.clone(), field.label.clone()))
                .collect(),
        })
    })
}

fn display_text(text: &LocalizedText) -> String {
    text.get("zh")
        .or_else(|| text.get("en"))
        .or_else(|| text.values().next())
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
