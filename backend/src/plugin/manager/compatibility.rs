//! Core-version and plugin-dependency preflight validation.

use super::PluginManager;
use crate::core::app::error::{DependencyErrorKind, LookupErrorKind, PluginErrorKind};
use crate::core::app::error::{Result, TingError};
use crate::plugin::types::{PluginId, PluginMetadata, PluginState};
use semver::{Version, VersionReq};

impl PluginManager {
    pub(crate) fn validate_core_compatibility(metadata: &PluginMetadata) -> Result<()> {
        Self::validate_core_compatibility_for_version(metadata, env!("CARGO_PKG_VERSION"))
    }

    pub(super) fn validate_core_compatibility_for_version(
        metadata: &PluginMetadata,
        current_core_version: &str,
    ) -> Result<()> {
        let min_core_version = metadata
            .min_core_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                TingError::plugin_load(
                    PluginErrorKind::InvalidManifest,
                    "validate_core_compatibility_for_version::plugin_load",
                    format!(
                        "Plugin {} must declare min_core_version >= {} for the new plugin contract",
                        metadata.instance_id(),
                        ting_plugin_contract::MIN_PLUGIN_CORE_VERSION
                    ),
                )
            })?;

        let current = parse_core_version(current_core_version, "current core version")?;
        let required = parse_core_version(min_core_version, "min_core_version")?;
        let contract_floor = parse_core_version(
            ting_plugin_contract::MIN_PLUGIN_CORE_VERSION,
            "minimum plugin contract core version",
        )?;

        if required < contract_floor {
            return Err(TingError::plugin_load(
                PluginErrorKind::InvalidManifest,
                "validate_core_compatibility_for_version::plugin_load",
                format!(
                    "Plugin {} declares min_core_version {}, but the new plugin contract requires >= {}",
                    metadata.instance_id(),
                    min_core_version,
                    contract_floor
                ),
            ));
        }

        if current < required {
            return Err(TingError::plugin_load(
                PluginErrorKind::InvalidManifest,
                "validate_core_compatibility_for_version::plugin_load",
                format!(
                    "Plugin {} requires Ting Reader core >= {}, current core version is {}",
                    metadata.instance_id(),
                    min_core_version,
                    current_core_version
                ),
            ));
        }

        Ok(())
    }

    pub(crate) async fn validate_plugin_dependencies(
        &self,
        metadata: &PluginMetadata,
        ignored_plugin_id: Option<&PluginId>,
    ) -> Result<()> {
        let registry = self.registry.read().await;
        let mut missing_deps = Vec::new();

        for dep in &metadata.dependencies {
            let matching_plugins: Vec<_> = registry
                .iter()
                .filter(|(id, entry)| {
                    ignored_plugin_id
                        .map(|ignored| ignored != *id)
                        .unwrap_or(true)
                        && entry.state != PluginState::Failed
                        && Self::metadata_matches_dependency(&entry.metadata, dep)
                })
                .collect();

            if matching_plugins.is_empty() {
                missing_deps.push(format!("{} ({})", dep.plugin_name, dep.version_requirement));
            }
        }

        if !missing_deps.is_empty() {
            return Err(TingError::dependency(
                DependencyErrorKind::Missing,
                "validate_plugin_dependencies::dependency",
                format!(
                    "Missing or incompatible dependencies for plugin {}: {}",
                    metadata.instance_id(),
                    missing_deps.join(", ")
                ),
            ));
        }

        Ok(())
    }

    pub(crate) async fn find_plugin_dependents(
        &self,
        plugin_id: &PluginId,
    ) -> Result<Vec<PluginId>> {
        let registry = self.registry.read().await;
        let target = registry.get(plugin_id).ok_or_else(|| {
            TingError::plugin_not_found(
                LookupErrorKind::Plugin,
                "find_plugin_dependents::plugin_not_found",
                plugin_id.clone(),
            )
        })?;

        let dependents = registry
            .iter()
            .filter(|(id, entry)| {
                *id != plugin_id
                    && entry.state != PluginState::Failed
                    && entry
                        .metadata
                        .dependencies
                        .iter()
                        .any(|dep| Self::metadata_matches_dependency(&target.metadata, dep))
            })
            .map(|(id, _)| id.clone())
            .collect();

        Ok(dependents)
    }

    pub(crate) fn metadata_matches_dependency(
        metadata: &PluginMetadata,
        dependency: &crate::plugin::types::PluginDependency,
    ) -> bool {
        let dependency_name = dependency.plugin_name.trim();
        if dependency_name.is_empty() {
            return false;
        }

        let name_matches = metadata.id == dependency_name
            || metadata.name == dependency_name
            || metadata.instance_id() == dependency_name;

        name_matches
            && version_requirement_matches(&metadata.version, &dependency.version_requirement)
    }
}

pub(super) fn parse_core_version(version: &str, label: &str) -> Result<Version> {
    Version::parse(version).map_err(|e| {
        let message = format!("Invalid {} '{}': {}", label, version, e);
        TingError::plugin_load(
            PluginErrorKind::InvalidOutput,
            "parse_core_version::plugin_load",
            message,
        )
        .with_source(anyhow::anyhow!(e))
    })
}

pub(super) fn version_requirement_matches(version: &str, requirement: &str) -> bool {
    let Ok(version) = Version::parse(version.trim().trim_start_matches('v')) else {
        return false;
    };

    let requirement = requirement.trim();
    if requirement.is_empty() || requirement == "*" {
        return true;
    }

    VersionReq::parse(requirement)
        .map(|req| req.matches(&version))
        .unwrap_or_else(|_| version.to_string() == requirement.trim_start_matches('v'))
}

pub(super) fn plugin_version_is_same_or_newer(installed: &str, candidate: &str) -> bool {
    match (
        Version::parse(installed.trim().trim_start_matches('v')),
        Version::parse(candidate.trim().trim_start_matches('v')),
    ) {
        (Ok(installed), Ok(candidate)) => installed >= candidate,
        _ => installed.trim().trim_start_matches('v') == candidate.trim().trim_start_matches('v'),
    }
}
