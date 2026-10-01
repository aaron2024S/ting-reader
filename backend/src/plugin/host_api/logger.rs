//! Host plugin logging and lifecycle-event records.

use crate::plugin::types::{PluginLogContext, PluginLogSource, PluginLogger, PluginMetadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginLogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl PluginLogLevel {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "debug" => Some(Self::Debug),
            "info" | "log" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct DefaultPluginLogger {
    context: PluginLogContext,
}

impl DefaultPluginLogger {
    /// Legacy constructor retained for callers that do not yet have metadata.
    /// New runtime integrations should use [`Self::from_metadata`] or
    /// [`Self::from_context`] so every field is supplied by the host.
    pub fn new(plugin_name: String) -> Self {
        Self {
            context: PluginLogContext {
                plugin_id: plugin_name.clone(),
                plugin_instance_id: plugin_name.clone(),
                plugin_name,
                plugin_version: "unknown".to_string(),
                runtime: "unknown".to_string(),
                source: PluginLogSource::Code,
            },
        }
    }

    pub fn from_metadata(metadata: &PluginMetadata) -> Self {
        Self::from_context(PluginLogContext::from_metadata(
            metadata,
            PluginLogSource::Code,
        ))
    }

    pub fn from_context(context: PluginLogContext) -> Self {
        Self { context }
    }

    pub fn context(&self) -> &PluginLogContext {
        &self.context
    }

    pub fn log(
        &self,
        level: PluginLogLevel,
        message: &str,
        fields: Option<&serde_json::Value>,
    ) -> String {
        emit_plugin_log(&self.context, level, message, fields)
    }
}

impl PluginLogger for DefaultPluginLogger {
    fn debug(&self, message: &str) {
        self.log(PluginLogLevel::Debug, message, None);
    }
    fn info(&self, message: &str) {
        self.log(PluginLogLevel::Info, message, None);
    }
    fn warn(&self, message: &str) {
        self.log(PluginLogLevel::Warn, message, None);
    }
    fn error(&self, message: &str) {
        self.log(PluginLogLevel::Error, message, None);
    }
}

pub fn emit_plugin_log(
    context: &PluginLogContext,
    level: PluginLogLevel,
    message: &str,
    fields: Option<&serde_json::Value>,
) -> String {
    let event_id = uuid::Uuid::new_v4().to_string();
    let operation = fields
        .and_then(|fields| fields.get("op"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    macro_rules! emit {
        ($macro:path) => {
            if let Some(fields) = fields {
                $macro!(
                    target: "ting_reader::plugin::logger",
                    event_id = %event_id,
                    plugin_id = %context.plugin_id,
                    plugin_instance_id = %context.plugin_instance_id,
                    plugin = %context.plugin_name,
                    plugin_version = %context.plugin_version,
                    runtime = %context.runtime,
                    source = context.source.as_str(),
                    op = operation,
                    plugin_fields = %fields,
                    "{}",
                    message
                );
            } else {
                $macro!(
                    target: "ting_reader::plugin::logger",
                    event_id = %event_id,
                    plugin_id = %context.plugin_id,
                    plugin_instance_id = %context.plugin_instance_id,
                    plugin = %context.plugin_name,
                    plugin_version = %context.plugin_version,
                    runtime = %context.runtime,
                    source = context.source.as_str(),
                    op = operation,
                    "{}",
                    message
                );
            }
        };
    }

    match level {
        PluginLogLevel::Debug => emit!(tracing::debug),
        PluginLogLevel::Info => emit!(tracing::info),
        PluginLogLevel::Warn => emit!(tracing::warn),
        PluginLogLevel::Error => emit!(tracing::error),
    }

    event_id
}

pub fn emit_plugin_event(
    metadata: &PluginMetadata,
    source: PluginLogSource,
    level: PluginLogLevel,
    message: &str,
    fields: Option<&serde_json::Value>,
) -> String {
    emit_plugin_log(
        &PluginLogContext::from_metadata(metadata, source),
        level,
        message,
        fields,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

    struct CapturedGuard(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedGuard {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CapturedWriter {
        type Writer = CapturedGuard;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedGuard(self.0.clone())
        }
    }

    #[test]
    fn structured_plugin_log_uses_host_bound_identity() {
        let writer = CapturedWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(writer.clone())
            .finish();
        let logger = DefaultPluginLogger::from_context(PluginLogContext {
            plugin_id: "stable-id".to_string(),
            plugin_instance_id: "stable-id@1.2.3".to_string(),
            plugin_name: "Display Name".to_string(),
            plugin_version: "1.2.3".to_string(),
            runtime: "javascript".to_string(),
            source: PluginLogSource::Code,
        });

        tracing::subscriber::with_default(subscriber, || {
            logger.log(
                PluginLogLevel::Info,
                "structured message",
                Some(&serde_json::json!({ "answer": 42, "op": "books.search" })),
            );
        });

        let output = String::from_utf8(writer.0.lock().unwrap().clone()).unwrap();
        let event: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        let fields = event.get("fields").unwrap();

        assert_eq!(
            fields.get("plugin_id").and_then(|v| v.as_str()),
            Some("stable-id")
        );
        assert_eq!(
            fields.get("plugin_version").and_then(|v| v.as_str()),
            Some("1.2.3")
        );
        assert_eq!(
            fields.get("plugin_instance_id").and_then(|v| v.as_str()),
            Some("stable-id@1.2.3")
        );
        assert_eq!(
            fields.get("runtime").and_then(|v| v.as_str()),
            Some("javascript")
        );
        assert_eq!(fields.get("source").and_then(|v| v.as_str()), Some("code"));
        assert_eq!(
            fields.get("op").and_then(|v| v.as_str()),
            Some("books.search")
        );
        assert!(
            fields
                .get("plugin_fields")
                .and_then(|v| v.as_str())
                .is_some_and(|value| value.contains("\"answer\":42"))
        );
        let event_id = fields.get("event_id").and_then(|v| v.as_str()).unwrap();
        assert!(uuid::Uuid::parse_str(event_id).is_ok());
    }

    #[test]
    fn plugin_log_level_accepts_legacy_console_names() {
        assert_eq!(PluginLogLevel::parse("log"), Some(PluginLogLevel::Info));
        assert_eq!(PluginLogLevel::parse("warning"), Some(PluginLogLevel::Warn));
        assert_eq!(PluginLogLevel::parse("invalid"), None);
    }
}
