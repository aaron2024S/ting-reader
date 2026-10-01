//! Authoritative infrastructure and business Host dispatch for every runtime.
//! Runtime adapters only transport JSON and binary buffers into this module.

use crate::core::error::{Result, TingError};
use crate::plugin::resources::ResourceScope;
use crate::plugin::{PluginHostGateway, PluginHostUser};
use serde_json::Value;
use std::sync::Arc;
use ting_plugin_contract::manifest::Permission;

/// Resource scopes and principals are Host-owned, never taken from JSON.
pub(crate) async fn invoke(
    plugin_id: &str,
    permissions: Option<&[Permission]>,
    user: Option<&PluginHostUser>,
    resources: Option<&Arc<ResourceScope>>,
    gateway: Option<&PluginHostGateway>,
    method: &str,
    input: Value,
) -> Result<Value> {
    if method.len() > 128
        || serde_json::to_vec(&input).map_or(true, |json| json.len() > 1024 * 1024)
    {
        return Err(TingError::ResourceLimitExceeded(
            "Host control input exceeds 1 MiB".into(),
        ));
    }
    if method.starts_with("resources.") {
        let scope = resources
            .ok_or_else(|| TingError::PermissionDenied("No resource scope for this call".into()))?;
        return scope
            .invoke(method, input)
            .map_err(|error| TingError::PluginExecutionError(error.to_string()));
    }
    let gateway = gateway.ok_or_else(|| {
        TingError::PluginExecutionError("Host business services are unavailable".into())
    })?;
    let owned_permissions;
    let permissions = match permissions {
        Some(permissions) => permissions,
        None => {
            owned_permissions = gateway_permissions(gateway, plugin_id)?;
            &owned_permissions
        }
    };
    PluginHostGateway::authorize(plugin_id, permissions, method)?;
    if method == "http.request" {
        return http_request(permissions, &input, resources).await;
    }
    gateway
        .invoke_with_permissions(plugin_id, permissions, user, method, input, resources)
        .await
}

fn gateway_permissions(gateway: &PluginHostGateway, plugin_id: &str) -> Result<Vec<Permission>> {
    gateway.plugin_permissions(plugin_id)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HostHttpRequest {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

pub(crate) async fn http_request(
    permissions: &[Permission],
    params: &Value,
    resources: Option<&Arc<crate::plugin::resources::ResourceScope>>,
) -> Result<Value> {
    let scope = resources.ok_or_else(|| {
        TingError::PermissionDenied("HTTP requests require a Host resource scope".into())
    })?;
    let request: HostHttpRequest = serde_json::from_value(params.clone())
        .map_err(|_| TingError::InvalidRequest("Invalid HTTP request parameters".into()))?;
    if request.url.len() > 4096
        || request
            .body
            .as_ref()
            .is_some_and(|body| body.len() > 1024 * 1024)
    {
        return Err(TingError::InvalidRequest(
            "HTTP request exceeds its size limit".into(),
        ));
    }
    let method = request
        .method
        .as_deref()
        .unwrap_or("GET")
        .to_ascii_uppercase();
    if !matches!(
        method.as_str(),
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
    ) {
        return Err(TingError::InvalidRequest(
            "Unsupported HTTP request method".into(),
        ));
    }
    let domains = permissions
        .iter()
        .filter_map(|permission| match permission {
            Permission::NetworkAccess { domain } => Some(domain.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let options = serde_json::json!({
        "method": method, "headers": request.headers.unwrap_or_default(),
        "body": request.body, "timeout_ms": request.timeout_ms.unwrap_or(30_000),
    });
    let cancellation = scope.cancellation_token();
    let response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(TingError::PluginExecutionError("HTTP request cancelled".into())),
        response = crate::plugin::http::request(&request.url, Some(&options), &domains) => {
            response.map_err(|error| TingError::NetworkError(error.to_string()))?
        }
    };
    let status = response.status;
    let headers = response.headers;
    let body = response.body;
    if body.len() > 8 * 1024 * 1024 {
        return Err(TingError::ResourceLimitExceeded(
            "HTTP response exceeds 8 MiB".into(),
        ));
    }
    let length = body.len();
    let resource = scope
        .grant_bytes(Arc::from(body), headers.get("content-type").cloned())
        .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
    Ok(
        serde_json::json!({"resource": resource, "length": length, "status": status, "headers": headers}),
    )
}
