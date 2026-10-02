//! Shared validation of lifecycle result envelopes across plugin runtimes.

use crate::core::app::error::{Result, TingError};
use serde_json::Value;
use ting_plugin_contract::protocol::CallResult;

pub(crate) fn require_successful_lifecycle_result(value: Value, operation: &str) -> Result<()> {
    match serde_json::from_value::<CallResult<Value>>(value).map_err(|_| {
        TingError::PluginExecutionError(format!("Invalid {operation} result envelope"))
    })? {
        CallResult::Success(_) => Ok(()),
        CallResult::Failure(error) => {
            error.error.validate().map_err(|_| {
                TingError::PluginExecutionError(format!("Invalid {operation} error envelope"))
            })?;
            Err(TingError::PluginExecutionError(format!(
                "Plugin {operation} failed: {}",
                error.error.message
            )))
        }
    }
}
