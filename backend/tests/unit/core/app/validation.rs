mod error {
    use crate::core::app::error::*;
    use axum::http::StatusCode;

    #[test]
    fn test_error_status_codes() {
        assert_eq!(
            TingError::InvalidRequest("test".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            TingError::AuthenticationError("test".into()).status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            TingError::PermissionDenied("test".into()).status_code(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            TingError::NotFound("test".into()).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            TingError::Timeout("test".into()).status_code(),
            StatusCode::REQUEST_TIMEOUT
        );
        assert_eq!(
            TingError::DatabaseError(rusqlite::Error::InvalidQuery).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn test_error_types() {
        assert_eq!(
            TingError::PluginNotFound("test".into()).error_type(),
            "PluginNotFound"
        );
        assert_eq!(
            TingError::InvalidRequest("test".into()).error_type(),
            "InvalidRequest"
        );
        assert_eq!(
            TingError::SecurityViolation("test".into()).error_type(),
            "SecurityViolation"
        );
    }

    #[test]
    fn test_error_retryable() {
        assert!(TingError::NetworkError("test".into()).is_retryable());
        assert!(TingError::Timeout("test".into()).is_retryable());
        assert!(!TingError::InvalidRequest("test".into()).is_retryable());
        assert!(!TingError::PermissionDenied("test".into()).is_retryable());
    }

    #[test]
    fn logged_plugin_error_preserves_public_error_type() {
        let error = TingError::PluginExecutionError("plugin failed".to_string())
            .mark_plugin_execution_logged();
        assert!(matches!(error, TingError::LoggedPluginExecutionError(_)));
        assert_eq!(error.error_type(), "PluginExecutionError");
        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);

        let timeout =
            TingError::Timeout("plugin timed out".to_string()).mark_plugin_execution_logged();
        assert!(matches!(timeout, TingError::Timeout(_)));
        assert_eq!(timeout.status_code(), StatusCode::REQUEST_TIMEOUT);
    }

    #[test]
    fn test_error_response_creation() {
        let error = TingError::PluginNotFound("test-plugin".into());
        let response = ErrorResponse::from_error(&error);

        assert_eq!(response.error, "PluginNotFound");
        assert!(response.message.contains("test-plugin"));
        assert!(!response.trace_id.is_empty());
        assert!(response.details.is_none());
    }

    #[test]
    fn test_error_response_with_trace_id() {
        let error = TingError::PluginNotFound("test-plugin".into());
        let trace_id = "test-trace-id-123".to_string();
        let response = ErrorResponse::from_error_with_trace_id(&error, trace_id.clone());

        assert_eq!(response.error, "PluginNotFound");
        assert!(response.message.contains("test-plugin"));
        assert_eq!(response.trace_id, trace_id);
        assert!(response.details.is_none());
    }

    #[test]
    fn test_error_response_with_details() {
        let details = serde_json::json!({
            "plugin_id": "test-plugin",
            "available_plugins": ["plugin1", "plugin2"]
        });

        let response = ErrorResponse::with_details(
            "PluginNotFound".into(),
            "Plugin not found".into(),
            details.clone(),
        );

        assert_eq!(response.error, "PluginNotFound");
        assert_eq!(response.message, "Plugin not found");
        assert_eq!(response.details, Some(details));
    }

    #[test]
    fn test_error_response_with_details_and_trace_id() {
        let details = serde_json::json!({
            "plugin_id": "test-plugin",
            "available_plugins": ["plugin1", "plugin2"]
        });
        let trace_id = "test-trace-id-456".to_string();

        let response = ErrorResponse::from_error_with_details_and_trace_id(
            &TingError::PluginNotFound("test-plugin".into()),
            details.clone(),
            trace_id.clone(),
        );

        assert_eq!(response.error, "PluginNotFound");
        assert!(response.message.contains("test-plugin"));
        assert_eq!(response.details, Some(details));
        assert_eq!(response.trace_id, trace_id);
    }

    #[test]
    fn test_error_context() {
        let result: std::result::Result<(), std::io::Error> = Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        ));

        let contexted = result.context("Failed to read plugin metadata");

        assert!(contexted.is_err());
        let err = contexted.unwrap_err();
        assert!(err.to_string().contains("Failed to read plugin metadata"));
        assert!(err.to_string().contains("file not found"));
    }
}

mod time {

    use crate::core::app::time::{DEFAULT_TIME_ZONE, parse_time_zone};

    #[test]
    fn accepts_iana_time_zones() {
        assert_eq!(parse_time_zone("Asia/Shanghai").unwrap(), "Asia/Shanghai");
    }

    #[test]
    fn rejects_unknown_time_zones() {
        assert!(parse_time_zone("Mars/Olympus").is_err());
    }

    #[test]
    fn default_is_utc() {
        assert_eq!(DEFAULT_TIME_ZONE, "UTC");
    }
}
