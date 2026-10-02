//! Plugin inventory, lifecycle operations and configuration endpoints.

use super::authorization::{
    is_admin_user, plugin_visible_to_user, require_plugin_system_write, require_plugin_visible,
};
use crate::api::models::{
    PluginConfigResponse, PluginDependencyResponse, PluginDetailResponse, PluginInfoResponse,
    PluginStatsResponse, ReloadPluginResponse, UninstallPluginResponse, UpdatePluginConfigRequest,
    UpdatePluginConfigResponse,
};
use crate::api::state::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{Result, TingError};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};

/// Handler for GET /api/v1/plugins - List plugins visible to the current user
pub async fn list_plugins(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse> {
    let is_admin = is_admin_user(&user);
    let plugins = state.plugin_manager.list_plugins().await;

    let plugin_responses: Vec<PluginInfoResponse> = plugins
        .into_iter()
        .filter(|info| plugin_visible_to_user(info.admin_only, is_admin))
        .map(|info| PluginInfoResponse {
            id: info.id,
            name: info.name,
            version: info.version,
            runtime: info.runtime,
            author: Some(info.author),
            description: Some(info.description),
            description_i18n: info.description_i18n,
            is_enabled: true, // All loaded plugins are enabled
            state: format!("{:?}", info.state).to_lowercase(),
            error: info.error,
            stats: Some(PluginStatsResponse {
                total_calls: info.total_calls,
                successful_calls: info.successful_calls,
                failed_calls: info.failed_calls,
                avg_execution_time_ms: 0.0, // Not available in PluginInfo
            }),
            config_schema: info.config_schema,
            permissions: Some(info.permissions),
            license: info.license,
            repo: info.repo,
            min_core_version: info.min_core_version,
            min_flutter_version: info.min_flutter_version,
            admin_only: info.admin_only,
            scraper: info.scraper,
            capabilities: info.capabilities,
        })
        .collect();

    Ok(Json(plugin_responses))
}

/// Handler for GET /api/v1/plugins/:id - Get plugin details
pub async fn get_plugin_detail(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let plugin = state.plugin_manager.get_plugin(&id)?;
    let metadata = plugin;
    require_plugin_visible(&metadata, &user)?;

    let plugins = state.plugin_manager.list_plugins().await;
    let plugin_info = plugins
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| TingError::PluginNotFound(id.clone()))?;

    let response = PluginDetailResponse {
        id: plugin_info.id.clone(),
        name: metadata.name.clone(),
        version: metadata.version.to_string(),
        runtime: metadata.runtime.clone(),
        author: Some(metadata.author.clone()),
        description: Some(metadata.description.clone()),
        description_i18n: metadata.description_i18n.clone(),
        license: metadata.license.clone(),
        repo: metadata.repo.clone(),
        min_core_version: metadata.min_core_version.clone(),
        min_flutter_version: metadata.min_flutter_version.clone(),
        admin_only: metadata.admin_only,
        is_enabled: true, // All loaded plugins are enabled
        state: format!("{:?}", plugin_info.state).to_lowercase(),
        error: plugin_info.error.clone(),
        entry_point: metadata.entry_point.clone(),
        dependencies: metadata
            .dependencies
            .iter()
            .map(|dep| PluginDependencyResponse {
                plugin_name: dep.plugin_name.clone(),
                version_requirement: dep.version_requirement.to_string(),
            })
            .collect(),
        permissions: metadata.permissions.clone(),
        supported_extensions: metadata.supported_extensions.clone(),
        config_schema: metadata.config_schema.clone(),
        scraper: metadata.scraper.clone(),
        capabilities: metadata.effective_capabilities(),
        stats: Some(PluginStatsResponse {
            total_calls: plugin_info.total_calls,
            successful_calls: plugin_info.successful_calls,
            failed_calls: plugin_info.failed_calls,
            avg_execution_time_ms: 0.0, // Not available in PluginInfo
        }),
    };

    Ok(Json(response))
}

/// Handler for POST /api/v1/plugins/:id/reload - Reload a plugin
pub async fn reload_plugin(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    require_plugin_system_write(&user)?;

    let metadata = state
        .plugin_manager
        .get_plugin(&id)
        .map_err(|_| TingError::PluginNotFound(id.clone()))?;
    require_plugin_visible(&metadata, &user)?;

    state.plugin_manager.reload_plugin(&id).await?;

    Ok(Json(ReloadPluginResponse {
        message: format!("Plugin {} reloaded successfully", id),
    }))
}

/// Handler for DELETE /api/v1/plugins/:id - Uninstall a plugin
pub async fn uninstall_plugin(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    require_plugin_system_write(&user)?;

    let metadata = state
        .plugin_manager
        .get_plugin(&id)
        .map_err(|_| TingError::PluginNotFound(id.clone()))?;
    require_plugin_visible(&metadata, &user)?;

    state.plugin_manager.uninstall_plugin(&id).await?;

    Ok((
        StatusCode::OK,
        Json(UninstallPluginResponse {
            message: format!("Plugin {} uninstalled successfully", id),
        }),
    ))
}

/// Handler for GET /api/v1/plugins/:id/config - Get plugin configuration
pub async fn get_plugin_config(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let metadata = state
        .plugin_manager
        .get_plugin(&id)
        .map_err(|_| TingError::PluginNotFound(id.clone()))?;
    require_plugin_visible(&metadata, &user)?;

    if let Some(ref schema) = metadata.config_schema {
        let defaults = crate::plugin::config::schema_defaults(schema);
        state.config_manager.ensure_config(
            id.clone(),
            metadata.name.clone(),
            Some(schema.clone()),
            defaults,
        )?;
    }

    let config = state
        .config_manager
        .get_redacted_config(&id)
        .unwrap_or_else(|_| serde_json::json!({}));

    Ok(Json(PluginConfigResponse {
        plugin_id: id,
        config,
    }))
}

/// Handler for PUT /api/v1/plugins/:id/config - Update plugin configuration
pub async fn update_plugin_config(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<UpdatePluginConfigRequest>,
) -> Result<impl IntoResponse> {
    require_plugin_system_write(&user)?;

    let metadata = state
        .plugin_manager
        .get_plugin(&id)
        .map_err(|_| TingError::PluginNotFound(id.clone()))?;
    require_plugin_visible(&metadata, &user)?;

    // Auto-initialize or sync config schema before preserving encrypted fields.
    if let Some(ref schema) = metadata.config_schema {
        let defaults = crate::plugin::config::schema_defaults(schema);
        state.config_manager.ensure_config(
            id.clone(),
            metadata.name.clone(),
            Some(schema.clone()),
            defaults,
        )?;
    }

    let config = state
        .config_manager
        .merge_preserved_sensitive_fields(&id, req.config)?;
    state.config_manager.update_config(&id, config)?;
    state.plugin_manager.reload_plugin(&id).await?;

    Ok(Json(UpdatePluginConfigResponse {
        message: format!("Plugin {} configuration updated successfully", id),
    }))
}
