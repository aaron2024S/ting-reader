//! Serve bounded plugin UI assets after validating grants and package paths.

use super::authorization::decode_plugin_client_grant;
use super::capabilities::find_ui_bridge_capability;
use crate::api::state::AppState;
use crate::core::app::error::{Result, TingError};
use crate::db::repository::Repository;
use crate::plugin::types::PluginState;
use axum::{
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
};
use std::path::{Component, Path as FsPath, PathBuf};
use tokio_util::io::ReaderStream;

const MAX_PLUGIN_ASSET_BYTES: u64 = 64 * 1024 * 1024;

const PLUGIN_ASSET_CSP: &str = "default-src 'none'; script-src 'none'; style-src 'none'; img-src 'none'; media-src 'none'; font-src 'none'; connect-src 'none'; child-src 'none'; frame-src 'none'; worker-src 'none'; manifest-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; sandbox";

/// Handler for GET /api/v1/plugin-assets/:client_grant/:id/*path.
pub async fn get_plugin_asset(
    State(state): State<AppState>,
    Path((client_grant, id, asset_path)): Path<(String, String, String)>,
) -> Result<Response> {
    let claims = decode_plugin_client_grant(&state, &client_grant).await?;
    if claims.plugin_id != id {
        return Err(TingError::PermissionDenied(
            "Plugin client grant does not match this asset request".to_string(),
        ));
    }
    let asset_user = state
        .user_repo
        .find_by_id(&claims.sub)
        .await?
        .ok_or_else(|| TingError::PermissionDenied("Plugin asset user no longer exists".into()))?;
    let plugin_info = state
        .plugin_manager
        .list_plugins()
        .await
        .into_iter()
        .find(|plugin| plugin.id == id)
        .ok_or_else(|| TingError::PluginNotFound(id.clone()))?;
    if !plugin_assets_available(plugin_info.state) {
        return Err(TingError::PermissionDenied(format!(
            "Plugin assets are unavailable for inactive plugin {}",
            id
        )));
    }
    let metadata = state
        .plugin_manager
        .get_plugin(&id)
        .map_err(|_| TingError::PluginNotFound(id.clone()))?;
    find_ui_bridge_capability(&metadata, &claims.capability_id)?;
    if metadata.admin_only && asset_user.role != "admin" {
        return Err(TingError::PermissionDenied(
            "Administrator access is required for this plugin asset".to_string(),
        ));
    }

    let plugin_root = state.plugin_manager.get_plugin_package_path(&id).await?;
    let relative_path = normalize_plugin_asset_path(&asset_path)?;

    let canonical_root = tokio::fs::canonicalize(&plugin_root)
        .await
        .map_err(TingError::IoError)?;
    let candidate_path = canonical_root.join(&relative_path);
    let canonical_asset = tokio::fs::canonicalize(&candidate_path)
        .await
        .map_err(|_| TingError::NotFound(format!("Plugin asset not found: {}", asset_path)))?;

    if !canonical_asset.starts_with(&canonical_root) {
        return Err(TingError::PermissionDenied(format!(
            "Plugin asset path escapes plugin package: {}",
            asset_path
        )));
    }

    let metadata = tokio::fs::metadata(&canonical_asset)
        .await
        .map_err(TingError::IoError)?;
    if !metadata.is_file() {
        return Err(TingError::NotFound(format!(
            "Plugin asset is not a file: {}",
            asset_path
        )));
    }
    if metadata.len() > MAX_PLUGIN_ASSET_BYTES {
        return Err(TingError::InvalidRequest(format!(
            "Plugin asset exceeds the {} MiB response limit: {}",
            MAX_PLUGIN_ASSET_BYTES / (1024 * 1024),
            asset_path
        )));
    }

    let content_type = mime_guess::from_path(&canonical_asset)
        .first_or_octet_stream()
        .to_string();
    let file = tokio::fs::File::open(&canonical_asset)
        .await
        .map_err(TingError::IoError)?;
    let body = Body::from_stream(ReaderStream::new(file));

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", &content_type)
        .header("Content-Length", metadata.len().to_string())
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header("X-Frame-Options", "DENY")
        .header("Referrer-Policy", "no-referrer")
        .header("Content-Security-Policy", PLUGIN_ASSET_CSP);
    if let Some(disposition) = plugin_document_disposition(&content_type) {
        response = response.header("Content-Disposition", disposition);
    }

    response.body(body).map_err(|e| {
        TingError::PluginExecutionError(format!("Plugin asset response failed: {}", e))
    })
}

pub(super) fn plugin_document_disposition(content_type: &str) -> Option<&'static str> {
    match content_type.split(';').next().unwrap_or_default().trim() {
        "text/html" => Some("attachment; filename=plugin-ui.html"),
        "application/xhtml+xml" | "application/xml" | "text/xml" => {
            Some("attachment; filename=plugin-document.xml")
        }
        _ => None,
    }
}

pub(super) fn plugin_assets_available(state: PluginState) -> bool {
    matches!(state, PluginState::Active | PluginState::Executing)
}

pub(super) fn normalize_plugin_asset_path(asset_path: &str) -> Result<PathBuf> {
    let asset_path = asset_path.trim_start_matches('/');
    if asset_path.trim().is_empty() {
        return Err(TingError::InvalidRequest(
            "Plugin asset path is required".to_string(),
        ));
    }

    let mut normalized = PathBuf::new();
    for component in FsPath::new(asset_path).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            _ => {
                return Err(TingError::PermissionDenied(format!(
                    "Invalid plugin asset path: {}",
                    asset_path
                )));
            }
        }
    }

    let first_component = normalized
        .components()
        .next()
        .and_then(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .unwrap_or_default();

    if !matches!(first_component, "ui" | "assets") {
        return Err(TingError::PermissionDenied(format!(
            "Plugin assets must live under ui/ or assets/: {}",
            asset_path
        )));
    }

    Ok(normalized)
}
