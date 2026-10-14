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
        assert_eq!(err.error_type(), "IoError");
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn context_preserves_http_classification_and_retryability() {
        let cases = [
            (
                TingError::InvalidRequest("bad input".into()),
                StatusCode::BAD_REQUEST,
                false,
            ),
            (
                TingError::AuthenticationError("expired".into()),
                StatusCode::UNAUTHORIZED,
                false,
            ),
            (
                TingError::NotFound("book".into()),
                StatusCode::NOT_FOUND,
                false,
            ),
            (
                TingError::Timeout("upstream".into()),
                StatusCode::REQUEST_TIMEOUT,
                true,
            ),
            (
                TingError::DatabaseError(rusqlite::Error::InvalidQuery),
                StatusCode::INTERNAL_SERVER_ERROR,
                true,
            ),
        ];
        for (error, status, retryable) in cases {
            let category = error.error_type();
            let result: Result<()> = Err(error);
            let error = result
                .context("operation")
                .with_context(|| "request".into())
                .unwrap_err();
            assert_eq!(error.status_code(), status);
            assert_eq!(error.error_type(), category);
            assert_eq!(error.is_retryable(), retryable);
            assert!(error.to_string().contains("request: operation:"));
        }
    }

    #[tokio::test]
    async fn network_context_retains_source_without_credentials_or_query_tokens() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = reqwest::Client::new()
            .get(format!(
                "http://user:private-password@{address}/media?token=private-token"
            ))
            .timeout(std::time::Duration::from_secs(1))
            .send()
            .await
            .context("WebDAV request failed")
            .unwrap_err();
        assert_eq!(error.error_type(), "NetworkError");
        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(error.is_retryable());
        let diagnostic = error.to_string();
        assert!(!diagnostic.contains("private-password"));
        assert!(!diagnostic.contains("private-token"));
        assert!(std::error::Error::source(&error).is_some());
    }
}

mod security_config {
    use crate::core::app::config::SecurityConfig;

    #[test]
    fn existing_configuration_gets_bounded_auth_limits_without_trusting_proxies() {
        let mut value = serde_json::json!({
            "enable_auth": false, "api_key": "", "jwt_secret": "",
            "allowed_origins": ["*"], "rate_limit_requests": 100,
            "rate_limit_window": 60, "enable_hsts": false, "hsts_max_age": 31536000,
        });
        let config: SecurityConfig = serde_json::from_value(value.clone()).unwrap();
        assert!(config.rate_limit_enabled);
        assert_eq!(config.rate_limit_max_clients, 10000);
        assert!(config.trusted_proxies.is_empty());
        config.validate().unwrap();
        value["rate_limit_max_clients"] = serde_json::json!(0);
        assert!(
            serde_json::from_value::<SecurityConfig>(value)
                .unwrap()
                .validate()
                .is_err()
        );
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
