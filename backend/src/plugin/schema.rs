//! Schema validation at manifest loading and invocation boundaries.
use crate::core::error::{Result, TingError};
use crate::plugin::types::PluginCapability;
use serde_json::Value;

pub(crate) fn validate_capability_schemas(capability: &PluginCapability) -> Result<()> {
    let schemas: Vec<&Value> = match capability {
        PluginCapability::ToolProvider(cap) => cap
            .tools
            .iter()
            .flat_map(|tool| [&tool.input_schema, &tool.output_schema])
            .collect(),
        PluginCapability::TaskHandler(cap) => cap
            .tasks
            .iter()
            .flat_map(|task| [&task.input_schema, &task.output_schema])
            .collect(),
        PluginCapability::EventHandler(cap) => {
            cap.events.iter().map(|event| &event.schema).collect()
        }
        PluginCapability::MetadataProvider(cap) => cap.filters_schema.iter().collect(),
        _ => Vec::new(),
    };
    for schema in schemas {
        ting_plugin_contract::capability::validate_schema(schema).map_err(|_| {
            TingError::PluginLoadError(format!(
                "Invalid bounded schema in capability {}",
                capability.id()
            ))
        })?;
        jsonschema::JSONSchema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .compile(schema)
            .map_err(|_| {
                TingError::PluginLoadError(format!(
                    "Invalid JSON Schema in capability {}",
                    capability.id()
                ))
            })?;
    }
    Ok(())
}

/// Payload diagnostics contain the field path, never the supplied value.
/// Shared contract validation prevents any remote schema resolver from being
/// used, even though the existing backend validator also serves other code.
pub(crate) fn validate_payload(schema: &Value, payload: &Value, label: &str) -> Result<()> {
    ting_plugin_contract::capability::validate_schema(schema).map_err(|error| {
        TingError::PluginExecutionError(format!("Invalid {label} schema: {error}"))
    })?;
    if payload.to_string().len() > 1024 * 1024 {
        return Err(TingError::PluginExecutionError(format!(
            "{label} exceeds 1 MiB"
        )));
    }
    let validator = jsonschema::JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .compile(schema)
        .map_err(|_| TingError::PluginExecutionError(format!("Invalid {label} schema")))?;
    if let Err(mut errors) = validator.validate(payload) {
        let path = errors
            .next()
            .map(|error| error.instance_path.to_string())
            .unwrap_or_default();
        return Err(TingError::PluginExecutionError(format!(
            "Invalid {label} at {}: value does not match the declared schema",
            path.chars().take(256).collect::<String>()
        )));
    }
    Ok(())
}
