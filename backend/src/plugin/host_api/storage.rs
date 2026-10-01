//! Host access to plugin configuration and user-scoped persistent storage.

use super::{PluginHostGateway, PluginHostUser, required_string_param, string_param};
use crate::core::app::error::{Result, TingError};
use serde_json::Value;

fn storage_key(user: &PluginHostUser, key: &str) -> String {
    format!("user:{}:{}", user.id, key)
}

impl PluginHostGateway {
    pub(super) async fn config_get(&self, plugin_id: &str) -> Result<Value> {
        self.config_manager
            .get_config(&plugin_id.to_string())
            .map_err(|error| TingError::ConfigError(error.to_string()))
    }

    pub(super) async fn storage_get(
        &self,
        plugin_id: &str,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let key = required_string_param(params, "key")?;
        let item = self
            .plugin_storage
            .get(plugin_id, &storage_key(user, &key))
            .await?;
        Ok(serde_json::json!({
            "key": key,
            "value": item.map(|item| item.value),
        }))
    }

    pub(super) async fn storage_set(
        &self,
        plugin_id: &str,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let key = required_string_param(params, "key")?;
        let value = params
            .get("value")
            .cloned()
            .ok_or_else(|| TingError::InvalidRequest("storage.set requires value".into()))?;
        let item = self
            .plugin_storage
            .set(plugin_id, &storage_key(user, &key), value)
            .await?;
        Ok(serde_json::json!({
            "key": key,
            "value": item.value,
            "updated_at": item.updated_at,
        }))
    }

    pub(super) async fn storage_delete(
        &self,
        plugin_id: &str,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let key = required_string_param(params, "key")?;
        let deleted = self
            .plugin_storage
            .delete(plugin_id, &storage_key(user, &key))
            .await?;
        Ok(serde_json::json!({ "key": key, "deleted": deleted }))
    }

    pub(super) async fn storage_list(
        &self,
        plugin_id: &str,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let prefix = string_param(params, "prefix").unwrap_or_default();
        let prefix = format!("user:{}:{}", user.id, prefix);
        let keys = self.plugin_storage.list(plugin_id, Some(&prefix)).await?;
        let visible_prefix = format!("user:{}:", user.id);
        Ok(serde_json::json!({
            "keys": keys.into_iter()
                .filter_map(|key| key.strip_prefix(&visible_prefix).map(str::to_string))
                .collect::<Vec<_>>()
        }))
    }
}
