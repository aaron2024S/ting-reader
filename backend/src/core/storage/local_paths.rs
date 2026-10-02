use crate::core::app::config::Config;
use crate::core::app::error::{Result, TingError};
use crate::db::models::Library;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthorizedRoot {
    pub path: PathBuf,
    pub source: String,
    pub readable: bool,
    pub writable: bool,
}

pub fn discover_authorized_roots(config: &Config) -> Vec<AuthorizedRoot> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    // Try reading from the .accessible_paths file generated dynamically by config_callback
    let mut accessible_paths = None;
    let paths_file = config.storage.data_dir.join(".accessible_paths");
    if paths_file.is_file()
        && let Ok(content) = std::fs::read_to_string(&paths_file)
    {
        accessible_paths = Some(content);
    }

    // Fallback to environment variable if file is missing or empty
    let paths_val = accessible_paths
        .unwrap_or_else(|| std::env::var("TRIM_DATA_ACCESSIBLE_PATHS").unwrap_or_default());

    if !paths_val.trim().is_empty() {
        for path in split_trim_accessible_paths(&paths_val) {
            push_authorized_root(&mut roots, &mut seen, path, "fnos");
        }
    }

    for path in &config.storage.local_library_roots {
        push_authorized_root(&mut roots, &mut seen, path.clone(), "config");
    }

    push_authorized_root(
        &mut roots,
        &mut seen,
        config.storage.local_storage_root.clone(),
        "legacy_storage",
    );

    roots
}

pub fn resolve_local_library_path(input: &str, config: &Config) -> Result<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(TingError::ValidationError(
            "Local library path cannot be empty".to_string(),
        ));
    }

    let raw_path = Path::new(trimmed);
    let candidate = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        config.storage.local_storage_root.join(raw_path)
    };

    let canonical_path = canonicalize_existing_directory(&candidate, trimmed)?;
    let roots = discover_authorized_roots(config);
    ensure_path_inside_authorized_roots(&canonical_path, &roots)?;

    Ok(canonical_path)
}

pub fn resolve_existing_local_library_root(library: &Library, config: &Config) -> Result<PathBuf> {
    if library.library_type != "local" {
        return Err(TingError::InvalidRequest(format!(
            "Library {} is not a local library",
            library.id
        )));
    }

    resolve_local_library_path(&library.url, config)
}

pub fn resolve_authorized_root(input: &str, config: &Config) -> Result<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(TingError::ValidationError(
            "Storage root cannot be empty".to_string(),
        ));
    }

    let requested = canonicalize_existing_directory(Path::new(trimmed), trimmed)?;
    let roots = discover_authorized_roots(config);
    if roots.iter().any(|root| root.path == requested) {
        return Ok(requested);
    }

    Err(TingError::SecurityViolation(format!(
        "Storage root '{}' is not authorized",
        trimmed
    )))
}

pub fn resolve_storage_folder_target(
    root: Option<&str>,
    sub_path: &str,
    config: &Config,
) -> Result<(PathBuf, PathBuf)> {
    let root_path = if let Some(root) = root.map(str::trim).filter(|value| !value.is_empty()) {
        resolve_authorized_root(root, config)?
    } else {
        let legacy_root = absolute_path(&config.storage.local_storage_root)?;
        if !legacy_root.exists() {
            std::fs::create_dir_all(&legacy_root)?;
        }
        canonicalize_existing_directory(&legacy_root, &legacy_root.display().to_string())?
    };

    let relative = normalize_relative_sub_path(sub_path)?;
    let candidate = root_path.join(&relative);
    let target = canonicalize_existing_directory(&candidate, sub_path)?;
    ensure_path_inside_root(&root_path, &target)?;

    Ok((root_path, target))
}

pub fn path_to_display_string(path: &Path) -> String {
    strip_windows_verbatim_prefix(&path.to_string_lossy()).replace('\\', "/")
}

pub fn ensure_path_inside_authorized_roots(path: &Path, roots: &[AuthorizedRoot]) -> Result<()> {
    if roots
        .iter()
        .any(|root| path == root.path || path.starts_with(&root.path))
    {
        return Ok(());
    }

    Err(TingError::SecurityViolation(format!(
        "Path '{}' is outside authorized local roots",
        path.display()
    )))
}

pub fn ensure_path_inside_root(root: &Path, path: &Path) -> Result<()> {
    if path == root || path.starts_with(root) {
        return Ok(());
    }

    Err(TingError::SecurityViolation(
        "Path escapes the authorized root".to_string(),
    ))
}

pub fn normalize_relative_sub_path(value: &str) -> Result<PathBuf> {
    let trimmed = value.trim();
    if trimmed.contains("..") {
        return Err(TingError::ValidationError(
            "Invalid path: contains '..'".to_string(),
        ));
    }

    let raw = Path::new(trimmed);
    if raw.is_absolute() {
        return Err(TingError::ValidationError(
            "Invalid path: sub_path must be relative".to_string(),
        ));
    }

    let mut normalized = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(TingError::ValidationError(
                    "Invalid path: sub_path cannot escape the root".to_string(),
                ));
            }
        }
    }

    Ok(normalized)
}

fn strip_windows_verbatim_prefix(value: &str) -> String {
    if let Some(rest) = value
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| value.strip_prefix(r"//?/UNC/"))
    {
        return format!("//{}", rest);
    }

    value
        .strip_prefix(r"\\?\")
        .or_else(|| value.strip_prefix(r"//?/"))
        .unwrap_or(value)
        .to_string()
}

fn push_authorized_root(
    roots: &mut Vec<AuthorizedRoot>,
    seen: &mut HashSet<PathBuf>,
    path: PathBuf,
    source: &str,
) {
    let Ok(abs_path) = absolute_path(&path) else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(&abs_path) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let Ok(canonical) = std::fs::canonicalize(&abs_path) else {
        return;
    };
    if !seen.insert(canonical.clone()) {
        return;
    }

    roots.push(AuthorizedRoot {
        readable: std::fs::read_dir(&canonical).is_ok(),
        writable: !metadata.permissions().readonly(),
        path: canonical,
        source: source.to_string(),
    });
}

fn canonicalize_existing_directory(path: &Path, display: &str) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            TingError::ValidationError(format!("Path '{}' does not exist", display))
        } else {
            TingError::IoError(e)
        }
    })?;

    if !canonical.is_dir() {
        return Err(TingError::ValidationError(format!(
            "Path '{}' is not a directory",
            display
        )));
    }

    Ok(canonical)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn split_trim_accessible_paths(value: &str) -> Vec<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if trimmed.contains(';') {
        return std::env::split_paths(trimmed).collect();
    }

    if cfg!(windows) && looks_like_windows_path(trimmed) {
        return std::env::split_paths(trimmed).collect();
    }

    trimmed
        .split(':')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn looks_like_windows_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

#[cfg(test)]
#[path = "../../../tests/unit/core/storage/local_paths.rs"]
mod tests;
