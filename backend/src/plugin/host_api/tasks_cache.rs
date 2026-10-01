//! Host task lifecycle and plugin cache access.

use super::{
    PluginHostGateway, PluginHostUser, plugin_task_priority, required_string_param, string_param,
};
use crate::core::app::error::{Result, TingError};
use crate::core::task_queue::{Task, TaskPayload};
use crate::db::models::TaskRecord;
use serde_json::Value;

impl PluginHostGateway {
    pub(super) async fn tasks_create(
        &self,
        plugin_id: &str,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let task_type = required_string_param(params, "task_type")?;
        if matches!(task_type.as_str(), "library_scan" | "write_metadata") {
            return Err(TingError::PermissionDenied(format!(
                "Task type {} is reserved for core workflows",
                task_type
            )));
        }

        let handlers = self
            .plugin_manager
            .find_task_handlers(Some(&task_type))
            .await;
        if !handlers
            .iter()
            .any(|handler| handler.registration.plugin_id == plugin_id)
        {
            return Err(TingError::NotFound(format!(
                "Plugin {} has no task_handler for task type {}",
                plugin_id, task_type
            )));
        }

        let data = params.get("data").cloned().unwrap_or(Value::Null);
        if data.to_string().len() > 1024 * 1024 {
            return Err(TingError::ResourceLimitExceeded(
                "Plugin task data exceeds 1 MiB".into(),
            ));
        }
        let owned_data = serde_json::json!({
            "_ting_task_owner": plugin_id,
            "_ting_task_user": user.id,
            "_ting_task_input": data,
        });
        let name =
            string_param(params, "name").unwrap_or_else(|| format!("plugin_task_{}", task_type));
        let priority = plugin_task_priority(params);
        let task = Task::new(
            name,
            priority,
            TaskPayload::Custom {
                task_type: task_type.clone(),
                data: owned_data,
            },
        );
        let task_id = self.task_queue.submit(task).await?;

        Ok(serde_json::json!({
            "task_id": task_id,
            "task_type": task_type,
            "status": "queued",
            "handler_count": handlers.len(),
        }))
    }

    pub(super) async fn tasks_get(
        &self,
        plugin_id: &str,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let task_id = required_string_param(params, "task_id")?;
        let record = self.task_queue.get_task(&task_id).await?;
        ensure_task_owner(&record, plugin_id, user)?;
        Ok(task_record_value(record))
    }

    pub(super) async fn tasks_cancel(
        &self,
        plugin_id: &str,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let task_id = required_string_param(params, "task_id")?;
        let record = self.task_queue.get_task(&task_id).await?;
        ensure_task_owner(&record, plugin_id, user)?;
        self.task_queue.cancel(&task_id).await?;
        Ok(serde_json::json!({ "task_id": task_id, "status": "cancelled" }))
    }

    pub(super) async fn tasks_report_progress(
        &self,
        plugin_id: &str,
        user: &PluginHostUser,
        params: &Value,
    ) -> Result<Value> {
        let task_id = required_string_param(params, "task_id")?;
        let record = self.task_queue.get_task(&task_id).await?;
        ensure_task_owner(&record, plugin_id, user)?;
        if matches!(record.status.as_str(), "completed" | "failed" | "cancelled") {
            return Err(TingError::InvalidRequest(
                "Cannot report progress for a terminal task".into(),
            ));
        }
        let message = string_param(params, "message");
        let message_key = string_param(params, "message_key");
        if message.is_some() && message_key.is_some() {
            return Err(TingError::InvalidRequest(
                "Choose message or message_key, not both".into(),
            ));
        }
        self.task_queue
            .report_progress(
                &task_id,
                message.as_deref(),
                message_key.as_deref(),
                params.get("message_params").cloned(),
            )
            .await?;
        Ok(serde_json::json!({ "task_id": task_id, "updated": true }))
    }

    pub(super) async fn cache_get(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let key = required_string_param(params, "key")?;
        let item = self.plugin_cache.get(plugin_id, &key).await?;

        Ok(match item {
            Some(item) => serde_json::json!({
                "hit": true,
                "key": item.key,
                "value": item.value,
                "created_at": item.created_at,
                "updated_at": item.updated_at,
            }),
            None => serde_json::json!({
                "hit": false,
                "key": key,
                "value": Value::Null,
            }),
        })
    }

    pub(super) async fn cache_set(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let key = required_string_param(params, "key")?;
        let value = params.get("value").cloned().unwrap_or(Value::Null);
        let item = self.plugin_cache.set(plugin_id, &key, value).await?;

        Ok(serde_json::json!({
            "key": item.key,
            "value": item.value,
            "created_at": item.created_at,
            "updated_at": item.updated_at,
        }))
    }

    pub(super) async fn cache_has(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let key = required_string_param(params, "key")?;
        let exists = self.plugin_cache.has(plugin_id, &key).await?;

        Ok(serde_json::json!({
            "key": key,
            "hit": exists,
        }))
    }

    pub(super) async fn cache_delete(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let key = required_string_param(params, "key")?;
        let deleted = self.plugin_cache.delete(plugin_id, &key).await?;

        Ok(serde_json::json!({
            "key": key,
            "deleted": deleted,
        }))
    }
}

fn ensure_task_owner(record: &TaskRecord, plugin_id: &str, user: &PluginHostUser) -> Result<()> {
    let payload = record
        .payload
        .as_deref()
        .and_then(|payload| serde_json::from_str::<TaskPayload>(payload).ok());
    let Some(TaskPayload::Custom { data, .. }) = payload else {
        return Err(TingError::PermissionDenied(
            "Task is not owned by a plugin".into(),
        ));
    };
    let owner = data
        .get("_ting_task_owner")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let owner_user = data
        .get("_ting_task_user")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if owner != plugin_id || owner_user != user.id {
        return Err(TingError::PermissionDenied(
            "Plugin cannot access this task".into(),
        ));
    }
    Ok(())
}

fn task_record_value(record: TaskRecord) -> Value {
    serde_json::to_value(record).unwrap_or_else(|_| serde_json::json!({}))
}
