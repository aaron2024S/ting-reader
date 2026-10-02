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
use crate::core::app::error::TingError;

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
        crate::plugin::types::schema::validate_capability_schemas(capability)?;
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
