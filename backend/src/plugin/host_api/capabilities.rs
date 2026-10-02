use super::{PluginHostGateway, PluginHostUser, required_string_param};
use crate::core::app::error::{Result, TingError};
use crate::plugin::sandbox::Permission;
use crate::plugin::types::PluginInvocationContext;
use serde_json::Value;
use std::sync::Arc;

impl PluginHostGateway {
    pub(super) async fn capabilities_invoke(
        &self,
        caller_plugin_id: &str,
        user: &PluginHostUser,
        permissions: &[Permission],
        params: &Value,
        _resources: Option<&Arc<super::resources::ResourceScope>>,
    ) -> Result<Value> {
        let target_plugin_id = required_string_param(params, "plugin_id")
            .or_else(|_| required_string_param(params, "target_plugin_id"))?;
        let target_capability_id = required_string_param(params, "capability_id")
            .or_else(|_| required_string_param(params, "target_capability_id"))?;
        let operation = required_string_param(params, "operation")?;
        if operation == "capabilities.invoke" {
            return Err(TingError::InvalidRequest(
                "Recursive capability invocation is not allowed".into(),
            ));
        }
        let input = params.get("params").cloned().unwrap_or(Value::Null);
        if input.to_string().len() > 1024 * 1024 {
            return Err(TingError::ResourceLimitExceeded(
                "Capability input exceeds 1 MiB".into(),
            ));
        }

        let permitted = permissions.iter().any(|permission| {
            matches!(
                permission,
                Permission::CapabilityInvoke {
                    plugin_id,
                    capability_id,
                } if plugin_id == &target_plugin_id && capability_id == &target_capability_id
            )
        });
        if !permitted {
            return Err(TingError::PermissionDenied(format!(
                "Plugin {} is not permitted to invoke {} on {}",
                caller_plugin_id, target_capability_id, target_plugin_id
            )));
        }
        if caller_plugin_id == target_plugin_id {
            return Err(TingError::InvalidRequest(
                "A plugin cannot invoke its own capability through the cross-plugin gateway".into(),
            ));
        }

        let result = self
            .plugin_manager
            .invoke_capability(
                &target_plugin_id,
                &target_capability_id,
                &operation,
                input,
                &PluginInvocationContext {
                    user: Some(user.clone()),
                    resources: None,
                },
            )
            .await?;
        Ok(serde_json::json!({
            "plugin_id": target_plugin_id,
            "capability_id": target_capability_id,
            "operation": operation,
            "result": result,
        }))
    }
}
