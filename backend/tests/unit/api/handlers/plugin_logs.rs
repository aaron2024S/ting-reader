use super::*;
use std::io::Write;

fn query() -> PluginLogsQuery {
    PluginLogsQuery {
        plugin_id: None,
        level: None,
        source: None,
        q: None,
        since: None,
        until: None,
        page: 1,
        page_size: 100,
    }
}

fn log(fields: serde_json::Value) -> LogEntry {
    LogEntry {
        timestamp: "2026-08-22T01:00:00Z".to_string(),
        level: "ERROR".to_string(),
        module: "ting_reader::plugin::logger".to_string(),
        message: "needle".to_string(),
        raw_message: Some("needle".to_string()),
        message_key: None,
        message_params: None,
        fields: Some(fields),
        task_id: None,
        task_status: None,
        task_type: None,
    }
}

#[test]
fn plugin_filter_matches_stable_and_versioned_ids() {
    let mut query = query();
    query.source = Some("runtime".to_string());
    let filters = PluginLogFilters::from_query(Some("demo@9.9.9".to_string()), &query).unwrap();
    assert!(filters.matches(&log(serde_json::json!({
        "plugin_id": "demo",
        "plugin_instance_id": "demo@1.0.0",
        "source": "runtime"
    }))));
}

#[test]
fn missing_plugin_filter_matches_all_plugins() {
    let filters = PluginLogFilters::from_query(None, &query()).unwrap();
    assert!(filters.matches(&log(serde_json::json!({
        "plugin_id": "demo",
        "plugin_instance_id": "demo@1.0.0",
        "source": "runtime"
    }))));
}

#[test]
fn blank_plugin_filter_matches_all_plugins() {
    let mut query = query();
    query.plugin_id = Some("   ".to_string());
    let filters = PluginLogFilters::from_query(query.plugin_id.clone(), &query).unwrap();
    assert!(filters.matches(&log(serde_json::json!({
        "plugin_id": "demo",
        "plugin_instance_id": "demo@1.0.0",
        "source": "runtime"
    }))));
}

#[test]
fn invalid_time_ranges_are_rejected() {
    let mut query = query();
    query.since = Some("2026-08-22T02:00:00Z".to_string());
    query.until = Some("2026-08-22T01:00:00Z".to_string());
    assert!(PluginLogFilters::from_query(Some("demo".to_string()), &query).is_err());
}

#[test]
fn export_filename_components_are_sanitized() {
    assert_eq!(safe_filename_component("demo-plugin"), "demo-plugin");
    assert_eq!(safe_filename_component("../bad\"name"), ".._bad_name");
    assert_eq!(safe_filename_component(""), "plugin");
}

#[test]
fn plugin_fields_are_returned_as_structured_json() {
    let temp_dir = tempfile::tempdir().unwrap();
    let log_path = temp_dir.path().join("plugins.json");
    let mut file = File::create(&log_path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": "2026-08-24T12:00:00Z",
            "level": "INFO",
            "target": "ting_reader::plugin::logger",
            "fields": {
                "message": "Plugin invocation completed",
                "plugin_id": "demo",
                "plugin_fields": "{\"op\":\"plugin.invoke\",\"duration_ms\":42}"
            }
        })
    )
    .unwrap();

    let mut logs = Vec::new();
    parse_plugin_log_file(&log_path, &mut logs);

    assert_eq!(logs.len(), 1);
    assert_eq!(
        logs[0]
            .fields
            .as_ref()
            .and_then(|fields| fields.get("plugin_fields"))
            .and_then(|fields| fields.get("duration_ms"))
            .and_then(serde_json::Value::as_u64),
        Some(42)
    );
}
