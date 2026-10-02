//! Capability discovery, UI bridge policy and client/Host invocation.

use super::authorization::{
    PluginRouteAccess, attach_plugin_invocation_context, plugin_invocation_context,
};
use super::authorization::{is_admin_user, registration_visible_to_user, require_plugin_visible};
use super::authorization::{issue_plugin_client_grant, require_plugin_client_grant};
use crate::api::models::{
    FindContentProcessorsQuery, FindEventHandlersQuery, FindTaskHandlersQuery,
    FindToolProvidersQuery, InvokePluginCapabilityRequest, InvokePluginCapabilityResponse,
    InvokePluginHostRequest, InvokePluginHostResponse, ListPluginCapabilitiesQuery,
    PluginCapabilityRegistrationResponse, ToolProviderRegistrationResponse,
};
use crate::api::state::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{Result, TingError};
use crate::plugin::PluginHostUser;
use crate::plugin::manager::capabilities::RegisteredCapability;
use crate::plugin::types::{PluginCapability, PluginMetadata};
use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use serde_json::Value;

/// Handler for GET /api/v1/plugin-capabilities - List registered capabilities.
pub async fn list_plugin_capabilities(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListPluginCapabilitiesQuery>,
) -> Result<impl IntoResponse> {
    let is_admin = is_admin_user(&user);
    let capabilities = if let Some(kind) = query.kind.as_deref() {
        state.plugin_manager.find_capabilities_by_kind(kind).await
    } else {
        state.plugin_manager.list_capabilities().await
    };

    let mut responses = Vec::new();
    for registration in capabilities
        .into_iter()
        .filter(|registration| registration_visible_to_user(registration, is_admin))
    {
        let is_ui_capability = matches!(registration.capability, PluginCapability::UiExtension(_));
        let client_grant = if is_ui_capability {
            Some(
                issue_plugin_client_grant(
                    &state,
                    &user,
                    &registration.plugin_id,
                    registration.capability.id(),
                )
                .await?,
            )
        } else {
            None
        };
        responses.push(plugin_capability_registration_response(
            registration,
            client_grant,
        ));
    }

    Ok(Json(responses))
}

/// Handler for GET /api/v1/plugin-capabilities/content-processors.
pub async fn find_content_processors(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<FindContentProcessorsQuery>,
) -> Result<impl IntoResponse> {
    let is_admin = is_admin_user(&user);
    let processors = state
        .plugin_manager
        .find_content_processors(&query.extension, query.operation.as_deref())
        .await;

    Ok(Json(
        processors
            .into_iter()
            .filter(|processor| registration_visible_to_user(&processor.registration, is_admin))
            .map(|processor| plugin_capability_registration_response(processor.registration, None))
            .collect::<Vec<_>>(),
    ))
}

/// Handler for GET /api/v1/plugin-capabilities/tools.
pub async fn find_tool_providers(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<FindToolProvidersQuery>,
) -> Result<impl IntoResponse> {
    let is_admin = is_admin_user(&user);
    let providers = state
        .plugin_manager
        .find_tool_providers(query.name.as_deref())
        .await;

    Ok(Json(
        providers
            .into_iter()
            .filter(|provider| registration_visible_to_user(&provider.registration, is_admin))
            .map(|provider| ToolProviderRegistrationResponse {
                plugin_id: provider.registration.plugin_id,
                plugin_name: provider.registration.plugin_name,
                admin_only: provider.registration.admin_only,
                capability: provider.registration.capability,
                tool: provider.tool,
            })
            .collect::<Vec<_>>(),
    ))
}

/// Handler for GET /api/v1/plugin-capabilities/task-handlers.
pub async fn find_task_handlers(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<FindTaskHandlersQuery>,
) -> Result<impl IntoResponse> {
    let is_admin = is_admin_user(&user);
    let handlers = state
        .plugin_manager
        .find_task_handlers(query.task_type.as_deref())
        .await;

    Ok(Json(
        handlers
            .into_iter()
            .filter(|handler| registration_visible_to_user(&handler.registration, is_admin))
            .map(|handler| plugin_capability_registration_response(handler.registration, None))
            .collect::<Vec<_>>(),
    ))
}

/// Handler for GET /api/v1/plugin-capabilities/event-handlers.
pub async fn find_event_handlers(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<FindEventHandlersQuery>,
) -> Result<impl IntoResponse> {
    let is_admin = is_admin_user(&user);
    let handlers = state
        .plugin_manager
        .find_event_handlers(query.event.as_deref())
        .await;

    Ok(Json(
        handlers
            .into_iter()
            .filter(|handler| registration_visible_to_user(&handler.registration, is_admin))
            .map(|handler| plugin_capability_registration_response(handler.registration, None))
            .collect::<Vec<_>>(),
    ))
}

fn plugin_capability_registration_response(
    registration: RegisteredCapability,
    client_grant: Option<String>,
) -> PluginCapabilityRegistrationResponse {
    PluginCapabilityRegistrationResponse {
        plugin_id: registration.plugin_id,
        plugin_name: registration.plugin_name,
        admin_only: registration.admin_only,
        client_grant,
        capability: registration.capability,
    }
}

pub(super) fn plugin_capability_not_found(plugin_id: &str, capability_id: &str) -> TingError {
    TingError::NotFound(format!(
        "Capability {} not found for plugin {}",
        capability_id, plugin_id
    ))
}

pub(super) fn find_ui_bridge_capability(
    metadata: &PluginMetadata,
    ui_capability_id: &str,
) -> Result<PluginCapability> {
    let capability = metadata
        .effective_capabilities()
        .into_iter()
        .find(|capability| capability.id() == ui_capability_id)
        .ok_or_else(|| plugin_capability_not_found(&metadata.id, ui_capability_id))?;
    if !matches!(capability, PluginCapability::UiExtension(_)) {
        return Err(TingError::PermissionDenied(
            "Bridge source must be a UI capability".to_string(),
        ));
    }
    Ok(capability)
}

pub(super) fn require_ui_bridge_capability(
    metadata: &PluginMetadata,
    ui_capability_id: &str,
    target_capability_id: &str,
) -> Result<()> {
    let source = find_ui_bridge_capability(metadata, ui_capability_id)?;
    let declared = source.ui_bridge().is_some_and(|bridge| {
        bridge
            .capabilities
            .iter()
            .any(|id| id == target_capability_id)
    });
    if target_capability_id != source.id() && !declared {
        return Err(TingError::PermissionDenied(
            "Capability is not allowed for this view".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn require_ui_bridge_host_method(
    metadata: &PluginMetadata,
    ui_capability_id: &str,
    method: &str,
) -> Result<()> {
    let source = find_ui_bridge_capability(metadata, ui_capability_id)?;
    let declared = source.ui_bridge().is_some_and(|bridge| {
        bridge
            .host_methods
            .iter()
            .any(|candidate| candidate == method)
    });
    if !declared {
        return Err(TingError::PermissionDenied(
            "Host method is not allowed for this view".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn require_unbridged_client_capability(capability: &PluginCapability) -> Result<()> {
    if capability.kind() == "content_processor" {
        return Ok(());
    }
    Err(TingError::PermissionDenied(
        "Client capability invocation requires a declared UI bridge".to_string(),
    ))
}

/// Handler for POST /api/v1/plugins/:id/capabilities/:capability_id/invoke.
pub async fn invoke_plugin_capability(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, capability_id)): Path<(String, String)>,
    Json(req): Json<InvokePluginCapabilityRequest>,
) -> Result<impl IntoResponse> {
    let metadata = state
        .plugin_manager
        .get_plugin(&id)
        .map_err(|_| TingError::PluginNotFound(id.clone()))?;
    require_plugin_visible(&metadata, &user)?;

    let capability = metadata
        .effective_capabilities()
        .into_iter()
        .find(|capability| capability.id() == capability_id)
        .ok_or_else(|| plugin_capability_not_found(&id, &capability_id))?;

    if let Some(ui_capability_id) = req.ui_capability_id.as_deref() {
        let ui_grant = req.ui_grant.as_deref().ok_or_else(|| {
            TingError::PermissionDenied("UI capability invocation requires a client grant".into())
        })?;
        require_plugin_client_grant(&state, &user, ui_grant, &id, Some(ui_capability_id)).await?;
        require_ui_bridge_capability(&metadata, ui_capability_id, &capability_id)?;
    } else {
        if req.ui_grant.is_some() {
            return Err(TingError::PermissionDenied(
                "A client grant requires ui_capability_id".to_string(),
            ));
        }
        require_unbridged_client_capability(&capability)?;
    }

    let invoke_method = client_capability_operation(&capability, &req.params)?;

    let params = attach_plugin_invocation_context(
        req.params,
        &id,
        &capability_id,
        PluginRouteAccess::Authenticated,
        Some(&user),
    );

    let result = state
        .plugin_manager
        .invoke_capability(
            &id,
            &capability_id,
            &invoke_method,
            params,
            &plugin_invocation_context(Some(&user)),
        )
        .await?;

    Ok(Json(InvokePluginCapabilityResponse { result }))
}

fn client_capability_operation(capability: &PluginCapability, params: &Value) -> Result<String> {
    let fixed = match capability {
        PluginCapability::UiExtension(_) => Some("open"),
        PluginCapability::ToolProvider(_) => Some("invokeTool"),
        _ => None,
    };
    let operation = fixed
        .or_else(|| params.get("operation").and_then(Value::as_str))
        .ok_or_else(|| TingError::ValidationError("A declared operation is required".into()))?;
    if !capability.supports(operation) {
        return Err(TingError::ValidationError(format!(
            "Capability {} does not declare operation {}",
            capability.id(),
            operation
        )));
    }
    Ok(operation.to_string())
}

/// Handler for POST /api/v1/plugin-host/invoke - Invoke a HostGateway method.
pub async fn invoke_plugin_host(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<InvokePluginHostRequest>,
) -> Result<impl IntoResponse> {
    let metadata = state
        .plugin_manager
        .get_plugin(&req.plugin_id)
        .map_err(|_| TingError::PluginNotFound(req.plugin_id.clone()))?;
    require_plugin_visible(&metadata, &user)?;
    require_plugin_client_grant(
        &state,
        &user,
        &req.ui_grant,
        &req.plugin_id,
        Some(&req.ui_capability_id),
    )
    .await?;
    require_ui_bridge_host_method(&metadata, &req.ui_capability_id, &req.method)?;

    let host_user = PluginHostUser {
        id: user.id.clone(),
        username: user.username.clone(),
        role: user.role.clone(),
    };
    let result = if req.method == "config.get" {
        state.config_manager.get_redacted_config(&req.plugin_id)?
    } else {
        state
            .plugin_host_gateway
            .invoke_plugin(
                &req.plugin_id,
                Some(&host_user),
                &req.method,
                req.params,
                None,
            )
            .await?
    };
    Ok(Json(InvokePluginHostResponse { result }))
}
