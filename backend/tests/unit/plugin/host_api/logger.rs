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
