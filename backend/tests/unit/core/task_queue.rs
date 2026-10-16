use super::*;

#[test]
fn test_priority_ordering() {
    let high = Task::new(
        "high".to_string(),
        Priority::High,
        TaskPayload::Custom {
            task_type: "test".to_string(),
            data: serde_json::json!({}),
        },
    );

    let low = Task::new(
        "low".to_string(),
        Priority::Low,
        TaskPayload::Custom {
            task_type: "test".to_string(),
            data: serde_json::json!({}),
        },
    );

    let pt_high = PriorityTask { task: high };
    let pt_low = PriorityTask { task: low };

    assert!(pt_high > pt_low);
}

#[test]
fn test_backoff_strategy() {
    let fixed = BackoffStrategy::Fixed(Duration::from_secs(5));
    assert_eq!(fixed.calculate_delay(0), Duration::from_secs(5));
    assert_eq!(fixed.calculate_delay(10), Duration::from_secs(5));

    let exponential = BackoffStrategy::Exponential {
        base: Duration::from_secs(1),
        max: Duration::from_secs(60),
    };
    assert_eq!(exponential.calculate_delay(1), Duration::from_secs(1)); // 1 * 2^0
    assert_eq!(exponential.calculate_delay(2), Duration::from_secs(2)); // 1 * 2^1
    assert_eq!(exponential.calculate_delay(3), Duration::from_secs(4)); // 1 * 2^2
    assert_eq!(exponential.calculate_delay(10), Duration::from_secs(60)); // capped at max
}

#[test]
fn test_task_status_as_str() {
    assert_eq!(TaskStatus::Queued.as_str(), "queued");
    assert_eq!(TaskStatus::Running.as_str(), "running");
    assert_eq!(TaskStatus::Completed.as_str(), "completed");
    assert_eq!(TaskStatus::Failed.as_str(), "failed");
    assert_eq!(TaskStatus::Cancelled.as_str(), "cancelled");
}

#[test]
fn test_task_creation() {
    let task = Task::new(
        "test_task".to_string(),
        Priority::Normal,
        TaskPayload::Custom {
            task_type: "test".to_string(),
            data: serde_json::json!({"key": "value"}),
        },
    );

    assert_eq!(task.name, "test_task");
    assert_eq!(task.priority, Priority::Normal);
    assert_eq!(task.status, TaskStatus::Queued);
    assert_eq!(task.retries, 0);
    assert!(task.error.is_none());
    assert_eq!(task.retry_policy.max_retries, 3);
}

#[test]
fn test_task_with_custom_retry_policy() {
    let task = Task::new(
        "test_task".to_string(),
        Priority::High,
        TaskPayload::Custom {
            task_type: "test".to_string(),
            data: serde_json::json!({}),
        },
    )
    .with_retry_policy(RetryPolicy {
        max_retries: 5,
        backoff: BackoffStrategy::Fixed(Duration::from_secs(10)),
    })
    .with_timeout(Duration::from_secs(300));

    assert_eq!(task.retry_policy.max_retries, 5);
    assert_eq!(task.timeout, Duration::from_secs(300));
}

#[test]
fn retry_policy_distinguishes_transient_failures_from_invalid_requests_and_cancellation() {
    use crate::core::app::error::DeadlineErrorKind;
    let policy = RetryPolicy::default();
    let transient = TingError::timeout(DeadlineErrorKind::Elapsed, "task.test", "Delayed".into());
    assert!(policy.should_retry(&transient, 1));
    assert!(!policy.should_retry(&transient, policy.max_retries));
    for error in [
        TingError::invalid_request(InputErrorKind::InvalidValue, "task.test", "Invalid".into()),
        TingError::timeout(
            DeadlineErrorKind::Cancelled,
            "task.test",
            "Cancelled".into(),
        ),
        TingError::DatabaseError(rusqlite::Error::InvalidQuery),
    ] {
        assert!(!policy.should_retry(&error, 1));
    }
}

#[test]
fn persisted_task_failures_round_trip_classification_and_keep_legacy_history_readable() {
    use crate::core::app::error::{DeadlineErrorKind, ErrorResponse};
    let failure = ErrorResponse::from_error_with_trace_id(
        &TingError::timeout(DeadlineErrorKind::Elapsed, "task.execute", "Delayed".into())
            .with_resource(LookupErrorKind::Book, "book-1"),
        "original-task-trace".into(),
    );
    let mut record = TaskRecord {
        id: "task-1".into(),
        task_type: "test".into(),
        status: "failed".into(),
        payload: None,
        message: None,
        message_key: None,
        message_params: None,
        error: TaskRecord::encode_failure(Some(&failure)).unwrap(),
        retries: 1,
        max_retries: 3,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let restored = record.failure().unwrap();
    assert_eq!(restored.error, "Timeout");
    assert_eq!(restored.trace_id, "original-task-trace");
    assert_eq!(restored.details.unwrap()["resource"]["id"], "book-1");
    record.error = Some("An older task failed".into());
    let historical = record.failure().unwrap();
    assert!(historical.message.contains("An older task failed"));
    assert_eq!(historical.trace_id, "legacy-task-task-1");
    assert_eq!(historical.details.unwrap()["reason"], "unclassified");
    record.error = None;
    assert!(record.failure().is_none());
    assert!(TaskRecord::encode_failure(None).unwrap().is_none());
}
