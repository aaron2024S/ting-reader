//! Uploaded/store packages share one review, installation and cleanup workflow.

use super::authorization::{
    is_admin_user, plugin_visible_to_user, require_plugin_system_write, require_plugin_visible,
};
use crate::api::models::{
    InstallPluginResponse, InstallStorePluginRequest, UnverifiedPluginInstallResponse,
};
use crate::api::require_admin;
use crate::api::state::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{Result, TingError};
use crate::plugin::installer::confirmation::inspect_install_confirmation;
use crate::plugin::installer::tr_package;
use crate::plugin::types::metadata::parse_plugin_metadata_content;
use axum::{
    Json,
    extract::{Multipart, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::path::Path as FsPath;
use uuid::Uuid;

/// Handler for POST /api/v1/plugins/install - Install a plugin
pub async fn install_plugin(
    State(state): State<AppState>,
    user: AuthUser,
    mut multipart: Multipart,
) -> Result<Response> {
    require_plugin_system_write(&user)?;

    let temp_dir = std::env::temp_dir().join("ting-reader-uploads");
    if !temp_dir.exists() {
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .map_err(TingError::IoError)?;
    }

    let temp_path = temp_dir.join(format!("plugin-{}.tr", Uuid::new_v4()));
    let mut file_saved = false;
    let mut accept_unverified = false;
    let mut confirmed_package_sha256 = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| TingError::InvalidRequest(e.to_string()))?
    {
        let field_name = field.name().map(ToOwned::to_owned);
        if field_name.as_deref() == Some("file") {
            let data = field
                .bytes()
                .await
                .map_err(|e| TingError::InvalidRequest(e.to_string()))?;
            tokio::fs::write(&temp_path, data)
                .await
                .map_err(TingError::IoError)?;
            file_saved = true;
        } else if field_name.as_deref() == Some("accept_unverified") {
            let value = field
                .text()
                .await
                .map_err(|e| TingError::InvalidRequest(e.to_string()))?;
            accept_unverified = matches!(value.trim(), "true" | "1" | "yes" | "on");
        } else if field_name.as_deref() == Some("confirmed_package_sha256") {
            confirmed_package_sha256 = Some(
                field
                    .text()
                    .await
                    .map_err(|error| TingError::InvalidRequest(error.to_string()))?,
            );
        }
    }

    if !file_saved {
        return Err(TingError::InvalidRequest("No file uploaded".to_string()));
    }

    install_reviewed_package(
        &state,
        &user,
        &temp_path,
        accept_unverified,
        confirmed_package_sha256.as_deref(),
        "",
    )
    .await
}

/// Shared upload/store workflow: review, privilege validation, install and cleanup.
/// The caller owns the temporary uploaded or downloaded package.
pub(super) async fn install_reviewed_package(
    state: &AppState,
    user: &AuthUser,
    package_path: &FsPath,
    accept_unverified: bool,
    confirmed_package_sha256: Option<&str>,
    success_suffix: &str,
) -> Result<Response> {
    let result = async {
        if let Some(response) = unverified_plugin_install_confirmation(
            package_path,
            accept_unverified,
            confirmed_package_sha256,
        )? {
            return Ok((StatusCode::PRECONDITION_REQUIRED, Json(response)).into_response());
        }

        validate_plugin_install_privilege(package_path, user)?;
        let plugin_id = state
            .plugin_manager
            .install_plugin_package(package_path)
            .await?;
        Ok((
            StatusCode::CREATED,
            Json(InstallPluginResponse {
                message: format!(
                    "Plugin {} installed successfully{}",
                    plugin_id, success_suffix
                ),
                plugin_id,
            }),
        )
            .into_response())
    }
    .await;

    let _ = tokio::fs::remove_file(package_path).await;
    result
}

fn unverified_plugin_warning(plugin_name: &str) -> String {
    format!(
        "{}由未知发布者提供，未经Ting Reader验证。单击同意，即表示你同意全权负责因使用该插件而可能导致的任何设备损坏或数据丢失。",
        plugin_name
    )
}

pub(super) fn unverified_plugin_install_confirmation(
    package_path: &FsPath,
    accept_unverified: bool,
    confirmed_package_sha256: Option<&str>,
) -> Result<Option<UnverifiedPluginInstallResponse>> {
    let Some(review) =
        inspect_install_confirmation(package_path, accept_unverified, confirmed_package_sha256)?
    else {
        return Ok(None);
    };
    let metadata = review.metadata;

    Ok(Some(UnverifiedPluginInstallResponse {
        requires_confirmation: true,
        verification_status: review.signature_status.label().to_string(),
        plugin_id: metadata.id.clone(),
        plugin_name: metadata.name.clone(),
        plugin_version: metadata.version.to_string(),
        publisher: "未知发布者".to_string(),
        warning: unverified_plugin_warning(&metadata.name),
        runtime: metadata.runtime.clone(),
        permissions: metadata.permissions.clone(),
        capabilities: metadata.effective_capabilities(),
        package_sha256: review.package_sha256,
        package_changed: review.package_changed,
    }))
}

fn validate_plugin_install_privilege(package_path: &FsPath, user: &AuthUser) -> Result<()> {
    if package_path.is_dir() {
        let metadata = crate::plugin::types::metadata::read_plugin_metadata(package_path)?;
        return require_plugin_visible(&metadata, user);
    }

    if tr_package::has_tr_magic(package_path)? {
        let metadata_content = tr_package::read_manifest_file(package_path, "plugin.yml")?;
        let metadata = parse_plugin_metadata_content(&metadata_content, "plugin.yml")?;
        return require_plugin_visible(&metadata, user);
    }

    Ok(())
}

/// Handler for GET /api/v1/store/plugins - Get list of plugins from store
pub async fn get_store_plugins(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<StorePluginsQuery>,
) -> Result<impl IntoResponse> {
    let is_admin = is_admin_user(&user);
    let plugins = if query.refresh.unwrap_or(false) {
        require_plugin_system_write(&user)?;
        state.plugin_manager.refresh_store_plugins().await?
    } else {
        state.plugin_manager.get_store_plugins().await?
    }
    .into_iter()
    .filter(|plugin| plugin_visible_to_user(plugin.admin_only, is_admin))
    .collect::<Vec<_>>();
    Ok(Json(plugins))
}

#[derive(Debug, Default, Deserialize)]
pub struct StorePluginsQuery {
    #[serde(default)]
    refresh: Option<bool>,
}

/// Handler for POST /api/v1/store/cache/clear - Clear plugin store cache
pub async fn clear_plugin_cache(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse> {
    require_plugin_system_write(&user)?;
    state.plugin_manager.clear_store_cache().await;
    Ok(Json(serde_json::json!({
        "message": "Plugin cache cleared successfully"
    })))
}

/// Handler for POST /api/v1/store/install - Install a plugin from store
pub async fn install_store_plugin(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<InstallStorePluginRequest>,
) -> Result<Response> {
    require_plugin_system_write(&user)?;

    if let Some(plugin) = state
        .plugin_manager
        .get_store_plugins()
        .await?
        .into_iter()
        .find(|plugin| plugin.id == req.plugin_id)
        && plugin.admin_only
    {
        require_admin(&user)?;
    }

    let temp_path = state
        .plugin_manager
        .download_plugin_from_store(&req.plugin_id)
        .await?;

    install_reviewed_package(
        &state,
        &user,
        &temp_path,
        req.accept_unverified,
        req.confirmed_package_sha256.as_deref(),
        " from store",
    )
    .await
}
