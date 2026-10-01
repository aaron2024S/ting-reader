//! Host event publication.

use super::{PluginHostGateway, PluginHostUser, required_string_param};
use crate::core::app::error::{Result, TingError};
use crate::plugin::types::PluginInvocationContext;
use serde_json::Value;

impl PluginHostGateway {
    pub(super) async fn events_publish(
        &self,
        plugin_id: &str,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let event = required_string_param(params, "event")?;
        validate_event_name(&event)?;
        let plugin_name = plugin_id.split('@').next().unwrap_or(plugin_id);
        if !event.starts_with(&format!("{plugin_name}.")) {
            return Err(TingError::PermissionDenied(
                "Plugins may only publish events in their own namespace".into(),
            ));
        }
        let data = params.get("data").cloned().unwrap_or(Value::Null);
        if data.to_string().len() > 1024 * 1024 {
            return Err(TingError::ResourceLimitExceeded(
                "Event data exceeds 1 MiB".into(),
            ));
        }
        let handlers = self.plugin_manager.find_event_handlers(Some(&event)).await;
        let mut delivered = Vec::new();
        let mut failures = Vec::new();
        for handler in handlers {
            let target_plugin_id = handler.registration.plugin_id.clone();
            let capability_id = handler.registration.capability.id().to_string();
            let result = self
                .plugin_manager
                .invoke_capability(
                    &target_plugin_id,
                    &capability_id,
                    "handle",
                    serde_json::json!({
                        "event": event,
                        "data": data,
                        "publisher_plugin_id": plugin_id,
                    }),
                    &PluginInvocationContext {
                        user: Some(user.clone()),
                        resources: None,
                    },
                )
                .await;
            match result {
                Ok(_) => delivered.push(serde_json::json!({
                    "plugin_id": target_plugin_id,
                    "capability_id": capability_id,
                })),
                Err(error) => failures.push(serde_json::json!({
                    "plugin_id": target_plugin_id,
                    "capability_id": capability_id,
                    "error": error.to_string(),
                })),
            }
        }
        Ok(serde_json::json!({
            "event": event,
            "delivered": delivered,
            "failures": failures,
        }))
    }
}

fn validate_event_name(event: &str) -> Result<()> {
    if event.is_empty()
        || event.len() > 128
        || !event
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(TingError::InvalidRequest("Invalid event name".into()));
    }
    Ok(())
}
