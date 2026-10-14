use super::*;
use std::path::PathBuf;

#[test]
fn shared_log_reader_preserves_localization_and_structured_fields() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("logs.json");
    let json = serde_json::json!({
        "timestamp": "2026-10-14T12:00:00+08:00", "level": "ERROR", "target": "plugin",
        "fields": {
            "message": "failed", "message_key": "plugin.failed",
            "message_params": "{\"name\":\"demo\"}",
            "plugin_fields": "{\"duration_ms\":42}", "plugin_id": "demo"
        }
    });
    std::fs::write(&file, format!("broken record\n{json}\n")).unwrap();
    let mut logs = Vec::new();
    read_log_file(&file, &mut logs);
    assert_eq!(logs.len(), 1);
    let log = &logs[0];
    assert_eq!(log.message_key.as_deref(), Some("plugin.failed"));
    assert_eq!(log.message_params.as_ref().unwrap()["name"], "demo");
    let fields = log.fields.as_ref().unwrap();
    assert_eq!(fields["plugin_fields"]["duration_ms"], 42);
    assert!(fields.get("message").is_none());
    let filter = LogQueryFilter::new(
        Some(" error "),
        Some("PLUGIN.FAILED"),
        Some("2026-10-14T03:00:00Z"),
        Some("2026-10-14T05:00:00Z"),
    )
    .unwrap();
    assert!(filter.matches(log));
    assert!(
        !LogQueryFilter::new(None, None, Some("2026-10-14T05:00:00Z"), None)
            .unwrap()
            .matches(log)
    );
    assert!(LogQueryFilter::new(None, None, Some("invalid"), None).is_err());
}

#[test]
fn malformed_log_parameters_remain_recoverable_without_hiding_the_message() {
    for params in [serde_json::json!("{bad"), serde_json::json!([])] {
        let log = LogEntry::from_tracing_json(serde_json::json!({
            "fields": { "message": "original message", "message_params": params,
                "plugin_fields": "invalid JSON" }
        }));
        assert_eq!(log.raw_message.as_deref(), Some("original message"));
        assert!(log.message_params.is_none());
        assert_eq!(log.fields.unwrap()["plugin_fields"], "invalid JSON");
    }
}

#[test]
fn test_parse_log_level() {
    assert!(matches!(parse_log_level("debug"), Ok(Level::DEBUG)));
    assert!(matches!(parse_log_level("info"), Ok(Level::INFO)));
    assert!(matches!(parse_log_level("warn"), Ok(Level::WARN)));
    assert!(matches!(parse_log_level("error"), Ok(Level::ERROR)));
    assert!(parse_log_level("invalid").is_err());
}

#[test]
fn test_rolling_appender_paths() {
    let appender =
        RollingFileAppender::new(PathBuf::from("/tmp/logs"), "test.log".to_string(), 1024, 5);

    assert_eq!(appender.current_path(), PathBuf::from("/tmp/logs/test.log"));
    assert_eq!(
        appender.backup_path(1),
        PathBuf::from("/tmp/logs/test.log.1")
    );
    assert_eq!(
        appender.backup_path(2),
        PathBuf::from("/tmp/logs/test.log.2")
    );
}
