use super::*;

fn query() -> LogsQuery {
    LogsQuery {
        level: None,
        module: None,
        q: None,
        since: None,
        until: None,
        page: 1,
        page_size: 50,
    }
}

fn log_entry(
    timestamp: &str,
    level: &str,
    module: &str,
    message: &str,
    fields: Option<serde_json::Value>,
) -> LogEntry {
    LogEntry {
        timestamp: timestamp.to_string(),
        level: level.to_string(),
        module: module.to_string(),
        message: message.to_string(),
        raw_message: Some(message.to_string()),
        message_key: None,
        message_params: None,
        fields,
        task_id: None,
        task_status: None,
        task_type: None,
    }
}

#[test]
fn default_filters_keep_existing_audit_and_error_behavior() {
    let filters = LogFilters::from_query(&query()).unwrap();
    let audit = log_entry(
        "2026-08-22T01:00:00Z",
        "INFO",
        "audit::login",
        "login",
        None,
    );
    let system_info = log_entry(
        "2026-08-22T01:00:01Z",
        "INFO",
        "ting_reader::api",
        "request completed",
        None,
    );
    let system_error = log_entry(
        "2026-08-22T01:00:02Z",
        "ERROR",
        "ting_reader::api",
        "request failed",
        None,
    );
    let plugin_request_error = log_entry(
        "2026-08-22T01:00:03Z",
        "ERROR",
        "ting_reader::api::plugin",
        "plugin request failed",
        None,
    );
    assert!(filters.matches(&audit));
    assert!(!filters.matches(&system_info));
    assert!(filters.matches(&system_error));
    assert!(!filters.matches(&plugin_request_error));
}

#[test]
fn query_filter_matches_system_fields() {
    let mut query = query();
    query.module = Some("all".to_string());
    query.q = Some("needle".to_string());
    query.since = Some("2026-08-22T00:00:00Z".to_string());
    query.until = Some("2026-08-22T02:00:00Z".to_string());
    let filters = LogFilters::from_query(&query).unwrap();
    let entry = log_entry(
        "2026-08-22T01:00:00Z",
        "INFO",
        "ting_reader::api",
        "contains needle",
        Some(serde_json::json!({
            "operation": "needle-operation"
        })),
    );

    assert!(filters.matches(&entry));
}

#[test]
fn invalid_or_reversed_time_ranges_are_rejected() {
    let mut invalid = query();
    invalid.since = Some("not-a-timestamp".to_string());
    assert!(LogFilters::from_query(&invalid).is_err());

    let mut reversed = query();
    reversed.since = Some("2026-08-22T02:00:00Z".to_string());
    reversed.until = Some("2026-08-22T01:00:00Z".to_string());
    assert!(LogFilters::from_query(&reversed).is_err());
}

#[test]
fn pagination_is_bounded_and_overflow_safe() {
    assert_eq!(normalized_pagination(0, 0), (1, 1));
    assert_eq!(normalized_pagination(2, 100), (2, 100));
    assert_eq!(
        normalized_pagination(usize::MAX, usize::MAX),
        (usize::MAX, MAX_LOG_PAGE_SIZE)
    );
    assert_eq!(
        usize::MAX
            .saturating_sub(1)
            .saturating_mul(MAX_LOG_PAGE_SIZE),
        usize::MAX
    );
}
