use super::{PluginHostGateway, PluginHostUser, required_string_param, string_param, usize_param};
use crate::core::Config;
use crate::core::error::{Result, TingError};
use crate::core::local_paths::resolve_existing_local_library_root;
use crate::db::models::Library;
use crate::db::repository::Repository;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const MAX_HOST_FILE_READ_BYTES: u64 = 20 * 1024 * 1024;
const MAX_LIBRARY_FILE_LIST_ENTRIES: usize = 500;
const MAX_PRIVATE_FILE_LIST_ENTRIES: usize = 500;

impl PluginHostGateway {
    pub(super) async fn private_file_open(
        &self,
        plugin_id: &str,
        params: &Value,
        resources: Option<&Arc<crate::plugin::resources::ResourceScope>>,
    ) -> Result<Value> {
        let scope = resources.ok_or_else(|| {
            TingError::PermissionDenied("Private file open requires a Host resource scope".into())
        })?;
        let target = self.resolve_private_file_target(plugin_id, params, false)?;
        let canonical = std::fs::canonicalize(&target)?;
        let root = self.private_file_root(plugin_id, params)?;
        ensure_path_inside(&root, &canonical)?;
        let metadata = std::fs::metadata(&canonical)?;
        if !metadata.is_file() || metadata.len() > MAX_HOST_FILE_READ_BYTES {
            return Err(TingError::ResourceLimitExceeded(
                "Private file is not a readable file within the 20 MiB limit".into(),
            ));
        }
        let resource = scope
            .grant_file(
                &canonical,
                metadata.len(),
                mime_guess::from_path(&canonical)
                    .first()
                    .map(|mime| mime.to_string()),
            )
            .map_err(|error| TingError::PermissionDenied(error.to_string()))?;
        Ok(serde_json::json!({
            "resource": resource,
            "length": metadata.len(),
            "path": private_relative_path(&root, &canonical),
        }))
    }

    pub(super) async fn private_file_stat(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let root = self.private_file_root(plugin_id, params)?;
        let target = self.resolve_private_file_target(plugin_id, params, true)?;
        let canonical = std::fs::canonicalize(&target)?;
        ensure_path_inside(&root, &canonical)?;
        let metadata = tokio::fs::metadata(&canonical).await?;
        Ok(serde_json::json!({
            "path": private_relative_path(&root, &canonical),
            "is_file": metadata.is_file(),
            "is_dir": metadata.is_dir(),
            "size": metadata.len(),
            "modified_unix": metadata.modified().ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs()),
        }))
    }

    pub(super) async fn private_file_list(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let root = self.private_file_root(plugin_id, params)?;
        let target = self.resolve_private_file_target(plugin_id, params, true)?;
        let canonical = std::fs::canonicalize(&target)?;
        ensure_path_inside(&root, &canonical)?;
        if !canonical.is_dir() {
            return Err(TingError::InvalidRequest(
                "Private file list target is not a directory".into(),
            ));
        }
        let limit = usize_param(params, "limit")
            .unwrap_or(200)
            .clamp(1, MAX_PRIVATE_FILE_LIST_ENTRIES);
        let mut entries = Vec::new();
        let mut directory = tokio::fs::read_dir(&canonical).await?;
        while let Some(entry) = directory.next_entry().await? {
            let path = std::fs::canonicalize(entry.path())?;
            ensure_path_inside(&root, &path)?;
            let metadata = tokio::fs::metadata(&path).await?;
            entries.push(serde_json::json!({
                "name": entry.file_name().to_string_lossy(),
                "path": private_relative_path(&root, &path),
                "is_file": metadata.is_file(),
                "is_dir": metadata.is_dir(),
                "size": metadata.len(),
            }));
            if entries.len() >= limit {
                break;
            }
        }
        Ok(serde_json::json!({
            "path": private_relative_path(&root, &canonical),
            "entries": entries,
            "limit": limit,
        }))
    }

    pub(super) async fn private_file_remove(
        &self,
        plugin_id: &str,
        params: &Value,
    ) -> Result<Value> {
        let root = self.private_file_root(plugin_id, params)?;
        let target = self.resolve_private_file_target(plugin_id, params, false)?;
        let canonical = std::fs::canonicalize(&target)?;
        ensure_path_inside(&root, &canonical)?;
        if canonical == root {
            return Err(TingError::PermissionDenied(
                "The private file root cannot be removed".into(),
            ));
        }
        let metadata = tokio::fs::metadata(&canonical).await?;
        if metadata.is_dir() {
            tokio::fs::remove_dir_all(&canonical).await?;
        } else {
            tokio::fs::remove_file(&canonical).await?;
        }
        Ok(serde_json::json!({
            "path": private_relative_path(&root, &canonical),
            "deleted": true,
        }))
    }

    pub(super) async fn library_file_list(
        &self,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let (library, root, target, relative_path) =
            self.resolve_library_file_target(user, params, true).await?;
        let metadata = tokio::fs::metadata(&target).await?;
        if !metadata.is_dir() {
            return Err(TingError::InvalidRequest(format!(
                "Library file target is not a directory: {}",
                relative_path
            )));
        }

        let limit = usize_param(params, "limit")
            .unwrap_or(200)
            .clamp(1, MAX_LIBRARY_FILE_LIST_ENTRIES);
        let mut entries = Vec::new();
        let mut dir = tokio::fs::read_dir(&target).await?;
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            entries.push(library_file_entry_value(&root, &path).await?);
            if entries.len() >= limit {
                break;
            }
        }

        Ok(serde_json::json!({
            "library_id": library.id,
            "path": relative_path,
            "entries": entries,
            "limit": limit,
        }))
    }

    pub(super) async fn library_file_stat(
        &self,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let (library, root, target, relative_path) =
            self.resolve_library_file_target(user, params, true).await?;
        let entry = library_file_entry_value(&root, &target).await?;

        Ok(serde_json::json!({
            "library_id": library.id,
            "path": relative_path,
            "entry": entry,
        }))
    }

    pub(super) async fn library_file_open(
        &self,
        user: &PluginHostUser,
        params: &Value,
        resources: Option<&Arc<crate::plugin::resources::ResourceScope>>,
    ) -> Result<Value> {
        let scope = resources.ok_or_else(|| {
            TingError::PermissionDenied("Library file open requires a Host resource scope".into())
        })?;
        let (_, root, target, _) = if string_param(params, "relative_to").as_deref() == Some("book")
        {
            self.resolve_book_file_target(user, params).await?
        } else {
            self.resolve_library_file_target(user, params, false)
                .await?
        };
        let canonical = std::fs::canonicalize(&target)?;
        ensure_path_inside(&root, &canonical)?;
        let metadata = std::fs::metadata(&canonical)?;
        if !metadata.is_file() || metadata.len() > MAX_HOST_FILE_READ_BYTES {
            return Err(TingError::ResourceLimitExceeded(
                "Library file is not a readable file within the 20 MiB limit".into(),
            ));
        }
        let resource = scope
            .grant_file(
                &canonical,
                metadata.len(),
                mime_guess::from_path(&canonical)
                    .first()
                    .map(|mime| mime.to_string()),
            )
            .map_err(|error| TingError::PermissionDenied(error.to_string()))?;
        Ok(serde_json::json!({ "resource": resource, "length": metadata.len() }))
    }

    pub(super) async fn assets_commit(
        &self,
        user: &PluginHostUser,
        params: &Value,
        resources: Option<&Arc<crate::plugin::resources::ResourceScope>>,
    ) -> Result<Value> {
        if !user.is_admin() {
            return Err(TingError::PermissionDenied(
                "Admin access required for assets.commit".into(),
            ));
        }
        let scope = resources.ok_or_else(|| {
            TingError::PermissionDenied("Asset commit requires a Host resource scope".into())
        })?;
        if string_param(params, "relative_to").as_deref() != Some("book") {
            return Err(TingError::InvalidRequest(
                "Asset must belong to a book".into(),
            ));
        }
        let file_name = required_string_param(params, "path")?;
        if !matches!(file_name.as_str(), "cover.png" | "cover.jpg" | "cover.webp") {
            return Err(TingError::InvalidRequest(
                "Unsupported cover asset name".into(),
            ));
        }
        let (_, root, target, relative_path) = self.resolve_book_file_target(user, params).await?;
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
            ensure_canonical_child(&root, parent)?;
        }
        if tokio::fs::try_exists(&target).await? {
            ensure_canonical_child(&root, &target)?;
        }
        let output: ting_plugin_contract::format_calls::ResourceId =
            serde_json::from_value(params.get("resource").cloned().ok_or_else(|| {
                TingError::InvalidRequest("Asset output resource is required".into())
            })?)
            .map_err(|_| TingError::InvalidRequest("Invalid asset output resource".into()))?;
        let stat = scope
            .stat(&output)
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        let length = stat
            .length
            .filter(|size| *size >= 12 && *size <= MAX_HOST_FILE_READ_BYTES)
            .ok_or_else(|| TingError::InvalidRequest("Invalid cover asset size".into()))?;
        let (header, _) = scope
            .read_at(&output, 0, length.min(16) as usize)
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        let valid_image = match file_name.as_str() {
            "cover.png" => header.starts_with(b"\x89PNG\r\n\x1a\n"),
            "cover.jpg" => header.starts_with(b"\xff\xd8\xff"),
            "cover.webp" => header.starts_with(b"RIFF") && header.get(8..12) == Some(b"WEBP"),
            _ => false,
        };
        if !valid_image {
            return Err(TingError::InvalidRequest(
                "Asset bytes do not match the cover format".into(),
            ));
        }
        let bytes = scope
            .commit_asset_output(&output, &target, MAX_HOST_FILE_READ_BYTES)
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        Ok(serde_json::json!({
            "path": relative_path,
            "size": bytes,
            "entry": library_file_entry_value(&root, &target).await?,
        }))
    }

    async fn resolve_library_file_target(
        &self,
        user: &PluginHostUser,
        params: &Value,
        allow_root_path: bool,
    ) -> Result<(Library, PathBuf, PathBuf, String)> {
        let library_id = required_string_param(params, "library_id")?;
        self.ensure_user_can_access_library(user, &library_id)
            .await?;
        let library = self
            .library_repo
            .find_by_id(&library_id)
            .await?
            .ok_or_else(|| {
                TingError::NotFound(format!("Library with id {} not found", library_id))
            })?;
        let root = self.local_library_root(&library)?;
        let requested_path = string_param(params, "path")
            .or_else(|| string_param(params, "relative_path"))
            .unwrap_or_default();
        let relative = normalize_library_relative_path(&requested_path, allow_root_path)?;
        let target = root.join(&relative);

        let canonical = std::fs::canonicalize(&target)?;
        ensure_path_inside(&root, &canonical)?;

        Ok((
            library,
            root,
            target,
            relative.to_string_lossy().replace('\\', "/"),
        ))
    }

    async fn resolve_book_file_target(
        &self,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<(Library, PathBuf, PathBuf, String)> {
        let library_id = required_string_param(params, "library_id")?;
        let book_id = required_string_param(params, "book_id")?;
        self.ensure_user_can_access_library(user, &library_id)
            .await?;
        self.ensure_user_can_access_book(user, &book_id).await?;

        let library = self
            .library_repo
            .find_by_id(&library_id)
            .await?
            .ok_or_else(|| {
                TingError::NotFound(format!("Library with id {} not found", library_id))
            })?;
        let book =
            self.book_repo.find_by_id(&book_id).await?.ok_or_else(|| {
                TingError::NotFound(format!("Book with id {} not found", book_id))
            })?;
        if book.library_id != library.id {
            return Err(TingError::InvalidRequest(format!(
                "Book {} does not belong to library {}",
                book_id, library_id
            )));
        }

        let requested_path = string_param(params, "path")
            .or_else(|| string_param(params, "relative_path"))
            .unwrap_or_default();
        let relative = normalize_library_relative_path(&requested_path, false)?;
        let (root, book_dir) = self.book_file_base_dir(&library, &book.path)?;
        let target = book_dir.join(&relative);
        let relative_path = target.to_string_lossy().replace('\\', "/");

        if let Some(parent) = target.parent()
            && parent.exists()
        {
            ensure_canonical_child(&root, parent)?;
        }

        Ok((library, root, target, relative_path))
    }
}

fn book_file_base_dir_with_root(
    config: &Config,
    library: &Library,
    book_path: &str,
) -> Result<(PathBuf, PathBuf)> {
    if library.library_type == "local" {
        let root = local_library_root_with_config(config, library)?;
        let book_dir = PathBuf::from(book_path);
        let canonical_book_dir = std::fs::canonicalize(&book_dir)?;
        ensure_path_inside(&root, &canonical_book_dir)?;
        return Ok((root, canonical_book_dir));
    }

    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut hasher = Sha256::new();
    hasher.update(book_path.as_bytes());
    let book_hash = format!("{:x}", hasher.finalize());
    Ok((root.clone(), root.join("temp").join(book_hash)))
}

fn local_library_root_with_config(config: &Config, library: &Library) -> Result<PathBuf> {
    if library.library_type == "local" {
        let root = resolve_existing_local_library_root(library, config)?;
        if !root.is_dir() {
            return Err(TingError::InvalidRequest(format!(
                "Library {} root path is not a directory",
                library.id
            )));
        }
        return Ok(root);
    }

    let root = library.root_path.trim();
    if root.is_empty() {
        return Err(TingError::InvalidRequest(format!(
            "Library {} does not expose a local root path",
            library.id
        )));
    }

    let root = std::fs::canonicalize(PathBuf::from(root))?;
    if !root.is_dir() {
        return Err(TingError::InvalidRequest(format!(
            "Library {} root path is not a directory",
            library.id
        )));
    }
    Ok(root)
}

impl PluginHostGateway {
    fn local_library_root(&self, library: &Library) -> Result<PathBuf> {
        local_library_root_with_config(&self.config, library)
    }

    fn book_file_base_dir(&self, library: &Library, book_path: &str) -> Result<(PathBuf, PathBuf)> {
        book_file_base_dir_with_root(&self.config, library, book_path)
    }

    fn private_file_root(&self, plugin_id: &str, params: &Value) -> Result<PathBuf> {
        let scope = string_param(params, "scope").unwrap_or_else(|| "data".to_string());
        let base = match scope.as_str() {
            "data" => self.config.storage.data_dir.join("plugin-data"),
            "temp" => self.config.storage.temp_dir.join("plugin-temp"),
            _ => {
                return Err(TingError::InvalidRequest(
                    "Private file scope must be data or temp".into(),
                ));
            }
        };
        let root = base.join(plugin_id);
        std::fs::create_dir_all(&root)?;
        std::fs::canonicalize(root).map_err(Into::into)
    }

    fn resolve_private_file_target(
        &self,
        plugin_id: &str,
        params: &Value,
        allow_root: bool,
    ) -> Result<PathBuf> {
        let root = self.private_file_root(plugin_id, params)?;
        let requested = string_param(params, "path").unwrap_or_default();
        let relative = normalize_private_relative_path(&requested, allow_root)?;
        let target = root.join(relative);
        if target.exists() {
            let canonical = std::fs::canonicalize(&target)?;
            ensure_path_inside(&root, &canonical)?;
            Ok(canonical)
        } else {
            if target.parent().is_some_and(|parent| parent.exists()) {
                let parent = std::fs::canonicalize(target.parent().unwrap())?;
                ensure_path_inside(&root, &parent)?;
            }
            Ok(target)
        }
    }
}

fn normalize_private_relative_path(value: &str, allow_empty: bool) -> Result<PathBuf> {
    let raw = Path::new(value.trim());
    if raw.is_absolute() {
        return Err(TingError::SecurityViolation(
            "Private file path must be relative".into(),
        ));
    }
    let mut normalized = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(TingError::SecurityViolation(
                    "Private file path cannot escape the plugin root".into(),
                ));
            }
        }
    }
    if !allow_empty && normalized.as_os_str().is_empty() {
        return Err(TingError::InvalidRequest(
            "Private file path is required".into(),
        ));
    }
    Ok(normalized)
}

fn private_relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn normalize_library_relative_path(value: &str, allow_empty: bool) -> Result<PathBuf> {
    let raw = Path::new(value.trim());
    if raw.is_absolute() {
        return Err(TingError::SecurityViolation(
            "Library file path must be relative".to_string(),
        ));
    }

    let mut normalized = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(TingError::SecurityViolation(
                    "Library file path cannot escape the library root".to_string(),
                ));
            }
        }
    }

    if !allow_empty && normalized.as_os_str().is_empty() {
        return Err(TingError::InvalidRequest(
            "Library file path is required".to_string(),
        ));
    }
    Ok(normalized)
}

fn ensure_canonical_child(root: &Path, path: &Path) -> Result<()> {
    let canonical = std::fs::canonicalize(path)?;
    ensure_path_inside(root, &canonical)
}

fn ensure_path_inside(root: &Path, path: &Path) -> Result<()> {
    if path == root || path.starts_with(root) {
        return Ok(());
    }
    Err(TingError::SecurityViolation(
        "Library file path escapes the library root".to_string(),
    ))
}

async fn library_file_entry_value(root: &Path, path: &Path) -> Result<Value> {
    let metadata = tokio::fs::metadata(path).await?;
    let canonical = std::fs::canonicalize(path)?;
    ensure_path_inside(root, &canonical)?;
    let relative_path = canonical
        .strip_prefix(root)
        .unwrap_or(&canonical)
        .to_string_lossy()
        .replace('\\', "/");
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());

    Ok(serde_json::json!({
        "name": file_name,
        "path": relative_path,
        "is_file": metadata.is_file(),
        "is_dir": metadata.is_dir(),
        "size": metadata.len(),
        "modified_unix": modified_at,
    }))
}
