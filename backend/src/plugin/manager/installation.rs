//! Package trust, install/upgrade, preinstalled and store workflows.

use super::PluginManager;
use super::compatibility::plugin_version_is_same_or_newer;
use crate::core::app::error::{Result, TingError};
use crate::plugin::installer::PluginInstaller;
use crate::plugin::installer::tr_package::{self, TrPackageSignatureIdentity};
use crate::plugin::types::metadata::has_plugin_manifest;
use crate::plugin::types::{PluginId, PluginMetadata};
use semver::Version;
use std::path::Path;
use tracing::{error, info, warn};

impl PluginManager {
    /// Uninstall a plugin (Unload and delete files)
    pub async fn uninstall_plugin(&self, plugin_id: &PluginId) -> Result<()> {
        self.uninstall_plugin_internal(plugin_id, true).await
    }

    pub(super) async fn uninstall_plugin_for_upgrade(&self, plugin_id: &PluginId) -> Result<()> {
        self.uninstall_plugin_internal(plugin_id, false).await
    }

    pub(super) async fn uninstall_plugin_internal(
        &self,
        plugin_id: &PluginId,
        delete_cache: bool,
    ) -> Result<()> {
        info!("Uninstalling plugin: {}", plugin_id);

        let dependents = match self.find_plugin_dependents(plugin_id).await {
            Ok(dependents) => dependents,
            Err(TingError::PluginNotFound(_)) => Vec::new(),
            Err(e) => return Err(e),
        };

        if !dependents.is_empty() {
            return Err(TingError::DependencyError(format!(
                "Cannot uninstall plugin {} because these plugins depend on it: {}",
                plugin_id,
                dependents.join(", ")
            )));
        }

        let cached_plugin_path = {
            let cache = self.metadata_cache.read().await;
            cache.get(plugin_id).cloned()
        };

        if let Err(e) = self.unload_plugin(plugin_id).await
            && !matches!(e, TingError::PluginNotFound(_))
        {
            tracing::warn!(
                plugin_id = %plugin_id,
                error = %e,
                message_key = "plugin.unload_during_uninstall_failed",
                message_params = %serde_json::json!({
                    "plugin_id": plugin_id,
                    "error": e.to_string(),
                }),
                "Failed to unload plugin during uninstall"
            );
        }

        // Clean up plugin configuration
        if let Some(cm) = self.config_manager.read().unwrap().as_ref() {
            let _ = cm.delete_config(plugin_id);
        }

        let installer = PluginInstaller::new(
            self.config.plugin_dir.clone(),
            self.config.plugin_dir.join("temp"),
        )?;

        if let Err(e) = installer.uninstall_plugin(plugin_id) {
            warn!(
                "Failed to uninstall plugin using standard ID path: {}. Searching for directory...",
                e
            );

            let mut found = false;
            if let Some(cached_path) = cached_plugin_path.as_ref() {
                let root = tokio::fs::canonicalize(&self.config.plugin_dir).await;
                let target = tokio::fs::canonicalize(cached_path).await;

                match (root, target) {
                    (Ok(root), Ok(target)) if target.starts_with(&root) && target != root => {
                        info!(
                            "Removing cached plugin directory for {}: {}",
                            plugin_id,
                            cached_path.display()
                        );
                        if let Err(e) = tokio::fs::remove_dir_all(cached_path).await {
                            error!(
                                "Failed to remove cached plugin directory {}: {}",
                                cached_path.display(),
                                e
                            );
                            return Err(TingError::IoError(e));
                        }
                        found = true;
                    }
                    (Ok(_), Ok(target)) => {
                        warn!(
                            "Cached plugin path for {} is outside plugin directory: {}",
                            plugin_id,
                            target.display()
                        );
                    }
                    _ => {}
                }
            }

            let mut read_dir = tokio::fs::read_dir(&self.config.plugin_dir)
                .await
                .map_err(TingError::IoError)?;

            if !found {
                while let Some(entry) = read_dir.next_entry().await.map_err(TingError::IoError)? {
                    let path = entry.path();
                    if path.is_dir()
                        && has_plugin_manifest(&path)
                        && let Ok(metadata) = self.read_plugin_metadata(&path)
                        && &metadata.instance_id() == plugin_id
                    {
                        info!(
                            "Found plugin directory for {}: {}",
                            plugin_id,
                            path.display()
                        );
                        if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                            error!(
                                "Failed to remove plugin directory {}: {}",
                                path.display(),
                                e
                            );
                        }
                        found = true;
                        break;
                    }
                }
            }

            if !found {
                return Err(e);
            }
        }

        {
            let mut cache = self.metadata_cache.write().await;
            cache.remove(plugin_id);
        }

        if delete_cache {
            if let Some(gateway) = self.host_gateway_handle().get() {
                gateway.delete_plugin_cache(plugin_id).await?;
            } else {
                warn!(
                    "Plugin cache gateway is unavailable while uninstalling {}",
                    plugin_id
                );
            }
        }

        info!("Plugin uninstalled and files removed: {}", plugin_id);
        Ok(())
    }

    /// Reload a plugin
    /// Install a plugin package
    pub async fn install_plugin_package(&self, package_path: &Path) -> Result<PluginId> {
        let installer = PluginInstaller::new(
            self.config.plugin_dir.clone(),
            self.config.plugin_dir.join("temp"),
        )?;

        let metadata = installer.get_package_metadata(package_path)?;
        Self::validate_core_compatibility(&metadata)?;
        self.validate_plugin_dependencies(&metadata, None).await?;
        self.ensure_install_signature_identity_allowed(package_path, &metadata)
            .await?;
        let target_plugin_id = metadata.instance_id();

        let needs_unload = {
            let registry = self.registry.read().await;
            registry.contains_key(&target_plugin_id)
        };

        let old_versions_to_remove = {
            let registry = self.registry.read().await;
            let mut to_remove = Vec::new();
            for (id, entry) in registry.iter() {
                let is_same_plugin = entry.metadata.id == metadata.id;

                if is_same_plugin && id != &target_plugin_id {
                    to_remove.push((id.clone(), Version::parse(&entry.metadata.version).ok()));
                }
            }
            to_remove
                .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            to_remove
                .into_iter()
                .map(|(id, _version)| id)
                .collect::<Vec<_>>()
        };

        // Preserve old config before uninstalling so we can migrate it to the new version.
        let preserved_old_config: Option<serde_json::Value> = {
            let cm_lock = self.config_manager.read().unwrap();
            cm_lock.as_ref().and_then(|cm| {
                old_versions_to_remove
                    .iter()
                    .find_map(|old_id| cm.get_config(old_id).ok())
            })
        };

        let mut cache_ids_to_migrate = Vec::new();
        for old_id in &old_versions_to_remove {
            info!(
                "Found old version of plugin {}, removing: {}",
                metadata.id, old_id
            );
            if let Err(e) = self.uninstall_plugin_for_upgrade(old_id).await {
                tracing::warn!(
                    plugin_id = %old_id,
                    error = %e,
                    message_key = "plugin.uninstall_old_failed",
                    message_params = %serde_json::json!({
                        "plugin_id": old_id,
                        "error": e.to_string(),
                    }),
                    "Failed to uninstall old plugin version"
                );
            } else {
                cache_ids_to_migrate.push(old_id.clone());
            }
        }

        if needs_unload {
            info!(
                "Plugin {} is already loaded, unloading before re-installation",
                target_plugin_id
            );
            if let Err(e) = self.unload_plugin(&target_plugin_id).await {
                tracing::warn!(
                    plugin_id = %target_plugin_id,
                    error = %e,
                    message_key = "plugin.unload_before_install_failed",
                    message_params = %serde_json::json!({
                        "plugin_id": target_plugin_id,
                        "error": e.to_string(),
                    }),
                    "Failed to unload plugin before install"
                );
            }
        }

        let plugin_id = installer.install_plugin(package_path, |_| Ok(())).await?;

        if let Some(gateway) = self.host_gateway_handle().get()
            && let Some((old_id, stale_cache_ids)) = cache_ids_to_migrate.split_first()
        {
            if let Err(e) = gateway.migrate_plugin_cache(old_id, &plugin_id).await {
                tracing::warn!(
                    old_plugin_id = %old_id,
                    new_plugin_id = %plugin_id,
                    error = %e,
                    message_key = "plugin.cache.migration_failed",
                    message_params = %serde_json::json!({
                        "old_plugin_id": old_id,
                        "new_plugin_id": &plugin_id,
                        "error": e.to_string(),
                    }),
                    "Failed to migrate plugin cache during upgrade"
                );
            }

            for stale_id in stale_cache_ids {
                if let Err(e) = gateway.delete_plugin_cache(stale_id).await {
                    tracing::warn!(
                        plugin_id = %stale_id,
                        error = %e,
                        message_key = "plugin.cache.cleanup_failed",
                        message_params = %serde_json::json!({
                            "plugin_id": stale_id,
                            "error": e.to_string(),
                        }),
                        "Failed to delete obsolete plugin cache after upgrade"
                    );
                }
            }
        }

        // Apply the same schema-preserving migration for install and reload upgrades.
        if let Some(ref old_config) = preserved_old_config
            && let Some(ref schema) = metadata.config_schema
        {
            let manager = self.config_manager.read().map_err(|error| {
                TingError::ConfigError(format!("Failed to acquire config manager: {error}"))
            })?;
            if let Some(manager) = manager.as_ref() {
                manager.migrate_config(
                    plugin_id.clone(),
                    metadata.name.clone(),
                    schema,
                    old_config,
                )?;
            }
        }

        let plugin_path = self.config.plugin_dir.join(&plugin_id);
        if let Err(e) = self.load_plugin(&plugin_path).await {
            tracing::error!(
                plugin_id = %plugin_id,
                error = %e,
                message_key = "plugin.auto_load_after_install_failed",
                message_params = %serde_json::json!({
                    "plugin_id": plugin_id,
                    "error": e.to_string(),
                }),
                "Failed to auto-load plugin after install"
            );
        }

        Ok(plugin_id)
    }

    /// Install bundled plugin packages before regular discovery.
    pub async fn install_preinstalled_packages(&self, preinstalled_dir: &Path) -> Result<()> {
        if preinstalled_dir.as_os_str().is_empty() || !preinstalled_dir.exists() {
            return Ok(());
        }

        if !preinstalled_dir.is_dir() {
            warn!(
                "Preinstalled plugin path is not a directory: {}",
                preinstalled_dir.display()
            );
            return Ok(());
        }

        info!(
            "Scanning preinstalled plugin packages from {}",
            preinstalled_dir.display()
        );

        let installer = PluginInstaller::new(
            self.config.plugin_dir.clone(),
            self.config.plugin_dir.join("temp"),
        )?;
        let mut read_dir = tokio::fs::read_dir(preinstalled_dir)
            .await
            .map_err(TingError::IoError)?;

        while let Some(entry) = read_dir.next_entry().await.map_err(TingError::IoError)? {
            let package_path = entry.path();
            let is_tr_package = package_path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("tr"))
                .unwrap_or(false);
            if !package_path.is_file() || !is_tr_package {
                continue;
            }

            match self
                .install_preinstalled_package(&installer, &package_path)
                .await
            {
                Ok(Some(plugin_id)) => info!(
                    "Installed preinstalled plugin package {} as {}",
                    package_path.display(),
                    plugin_id
                ),
                Ok(None) => info!(
                    "Skipped preinstalled plugin package {} because an equal or newer version is already installed",
                    package_path.display()
                ),
                Err(e) => warn!(
                    "Failed to install preinstalled plugin package {}: {}",
                    package_path.display(),
                    e
                ),
            }
        }

        Ok(())
    }

    pub(super) async fn install_preinstalled_package(
        &self,
        installer: &PluginInstaller,
        package_path: &Path,
    ) -> Result<Option<PluginId>> {
        let metadata = installer.get_package_metadata(package_path)?;
        Self::validate_core_compatibility(&metadata)?;
        self.ensure_install_signature_identity_allowed(package_path, &metadata)
            .await?;

        if self.has_same_or_newer_installed_version(&metadata).await? {
            return Ok(None);
        }

        installer
            .install_plugin(package_path, |_| Ok(()))
            .await
            .map(Some)
    }

    /// Install a plugin from the store
    pub async fn install_plugin_from_store(&self, plugin_id: &str) -> Result<PluginId> {
        let temp_path = self.download_plugin_from_store(plugin_id).await?;

        info!("Installing plugin package from {}", temp_path.display());
        let result = self.install_plugin_package(&temp_path).await;

        if let Err(e) = tokio::fs::remove_file(&temp_path).await {
            tracing::warn!(
                path = %temp_path.display(),
                error = %e,
                message_key = "plugin.temp_file.delete_failed",
                message_params = %serde_json::json!({
                    "path": temp_path.display().to_string(),
                    "error": e.to_string(),
                }),
                "Failed to delete temporary plugin file"
            );
        }

        result
    }

    /// Download a plugin package from the configured store to a temporary file.
    pub async fn download_plugin_from_store(&self, plugin_id: &str) -> Result<std::path::PathBuf> {
        info!("Downloading plugin from store: {}", plugin_id);

        let plugins = self.get_store_plugins().await?;
        let plugin = plugins.iter().find(|p| p.id == plugin_id).ok_or_else(|| {
            TingError::PluginNotFound(format!("Plugin {} not found in store", plugin_id))
        })?;

        let download_url = crate::plugin::store::get_download_url(plugin)?;

        info!(
            plugin_id = %plugin_id,
            download_url = %crate::plugin::store::redacted_download_url(&download_url),
            "Downloading plugin package"
        );

        let temp_dir = self.config.plugin_dir.join("temp");
        if !temp_dir.exists() {
            tokio::fs::create_dir_all(&temp_dir)
                .await
                .map_err(TingError::IoError)?;
        }

        crate::plugin::store::download_plugin(&self.http_client, &download_url, &temp_dir).await
    }

    // ── Lifecycle helpers ──

    pub(super) async fn ensure_install_signature_identity_allowed(
        &self,
        package_path: &Path,
        metadata: &PluginMetadata,
    ) -> Result<()> {
        let candidate_identity = plugin_install_signature_identity(package_path)?;
        let candidate_path = std::fs::canonicalize(package_path).ok();

        let mut read_dir = tokio::fs::read_dir(&self.config.plugin_dir)
            .await
            .map_err(TingError::IoError)?;

        while let Some(entry) = read_dir.next_entry().await.map_err(TingError::IoError)? {
            let path = entry.path();
            if !path.is_dir() || !has_plugin_manifest(&path) {
                continue;
            }

            if let (Some(candidate_path), Ok(installed_path)) =
                (&candidate_path, std::fs::canonicalize(&path))
                && &installed_path == candidate_path
            {
                continue;
            }

            let installed_metadata = match self.read_plugin_metadata(&path) {
                Ok(installed_metadata) => installed_metadata,
                Err(_) => continue,
            };

            if installed_metadata.id != metadata.id {
                continue;
            }

            if !tr_package::has_installed_signature_metadata(&path) {
                continue;
            }

            let installed_identity = tr_package::read_installed_signature_identity(&path)?;
            if !installed_identity.is_compatible_with(&candidate_identity) {
                return Err(TingError::PluginLoadError(format!(
                    "Plugin {} is already installed with signature identity '{}', but the candidate package uses '{}'. Uninstall the existing plugin before installing a package from a different publisher.",
                    metadata.id,
                    installed_identity.label(),
                    candidate_identity.label()
                )));
            }
        }

        Ok(())
    }

    pub(super) async fn has_same_or_newer_installed_version(
        &self,
        candidate: &PluginMetadata,
    ) -> Result<bool> {
        let mut read_dir = tokio::fs::read_dir(&self.config.plugin_dir)
            .await
            .map_err(TingError::IoError)?;

        while let Some(entry) = read_dir.next_entry().await.map_err(TingError::IoError)? {
            let path = entry.path();
            if !path.is_dir()
                || !has_plugin_manifest(&path)
                || !tr_package::has_installed_signature_metadata(&path)
            {
                continue;
            }

            let installed = match self.read_plugin_metadata(&path) {
                Ok(installed) => installed,
                Err(_) => continue,
            };

            if installed.id == candidate.id
                && plugin_version_is_same_or_newer(&installed.version, &candidate.version)
            {
                return Ok(true);
            }
        }

        Ok(false)
    }
}

pub(super) fn plugin_install_signature_identity(
    package_path: &Path,
) -> Result<TrPackageSignatureIdentity> {
    if package_path.is_dir() {
        return tr_package::read_installed_signature_identity(package_path);
    }

    if tr_package::has_tr_magic(package_path)? {
        return tr_package::read_package_signature_identity(package_path);
    }

    Err(TingError::PluginLoadError(format!(
        "{} is not a valid Ting Reader .tr package",
        package_path.display()
    )))
}
