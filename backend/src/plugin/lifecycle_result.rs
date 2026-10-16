//! Shared validation of lifecycle result envelopes across plugin runtimes.

use crate::core::app::error::PluginErrorKind;
use crate::core::app::error::{Result, TingError};
use serde_json::Value;
use ting_plugin_contract::protocol::CallResult;

pub(crate) fn require_successful_lifecycle_result(value: Value, operation: &str) -> Result<()> {
    match serde_json::from_value::<CallResult<Value>>(value).map_err(|error| {
        TingError::plugin_execution(
            PluginErrorKind::InvalidOutput,
            "require_successful_lifecycle_result::plugin_execution",
            format!("Invalid {operation} result envelope"),
        )
        .with_source(anyhow::Error::new(error))
    })? {
        CallResult::Success(_) => Ok(()),
        CallResult::Failure(error) => {
            error.error.validate().map_err(|_| {
                TingError::plugin_execution(
                    PluginErrorKind::InvalidOutput,
                    "require_successful_lifecycle_result::plugin_execution",
                    format!("Invalid {operation} error envelope"),
                )
            })?;
            Err(TingError::PluginReported(Box::new(error.error)))
        }
    }
}
