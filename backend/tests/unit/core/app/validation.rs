mod error {
    use crate::core::app::error::*;
    use axum::http::StatusCode;

    #[test]
    fn test_error_status_codes() {
        assert_eq!(
            TingError::invalid_request(
                InputErrorKind::InvalidValue,
                "test_error_status_codes::invalid_request",
                "test".into()
            )
            .status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            TingError::authentication(
                AuthenticationErrorKind::InvalidCredentials,
                "test_error_status_codes::authentication",
                "test".into()
            )
            .status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            TingError::permission(
                AccessErrorKind::ResourceDenied,
                "test_error_status_codes::permission",
                "test".into()
            )
            .status_code(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            TingError::not_found(
                LookupErrorKind::Resource,
                "test_error_status_codes::not_found",
                "test".into()
            )
            .status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            TingError::timeout(
                DeadlineErrorKind::Elapsed,
                "test_error_status_codes::timeout",
                "test".into()
            )
            .status_code(),
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
            TingError::plugin_not_found(
                LookupErrorKind::Plugin,
                "test_error_types::plugin_not_found",
                "test".into()
            )
            .error_type(),
            "PluginNotFound"
        );
        assert_eq!(
            TingError::invalid_request(
                InputErrorKind::InvalidValue,
                "test_error_types::invalid_request",
                "test".into()
            )
            .error_type(),
            "InvalidRequest"
        );
        assert_eq!(
            TingError::security(
                SecurityErrorKind::InvalidIdentity,
                "test_error_types::security",
                "test".into()
            )
            .error_type(),
            "SecurityViolation"
        );
    }

    #[test]
    fn test_error_retryable() {
        assert!(
            TingError::network(
                NetworkErrorKind::Transport,
                "test_error_retryable::network",
                "test".into()
            )
            .is_retryable()
        );
        assert!(
            TingError::timeout(
                DeadlineErrorKind::Elapsed,
                "test_error_retryable::timeout",
                "test".into()
            )
            .is_retryable()
        );
        assert!(
            !TingError::invalid_request(
                InputErrorKind::InvalidValue,
                "test_error_retryable::invalid_request",
                "test".into()
            )
            .is_retryable()
        );
        assert!(
            !TingError::permission(
                AccessErrorKind::ResourceDenied,
                "test_error_retryable::permission",
                "test".into()
            )
            .is_retryable()
        );
    }

    #[test]
    fn database_retries_only_transient_lock_failures() {
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_CONSTRAINT,
        ] {
            let error = TingError::DatabaseError(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            ));
            assert_eq!(
                error.is_retryable(),
                code != rusqlite::ffi::SQLITE_CONSTRAINT
            );
        }
    }

    #[test]
    fn logged_plugin_error_preserves_public_error_type() {
        let error = TingError::plugin_execution(
            PluginErrorKind::Runtime,
            "logged_plugin_error_preserves_public_error_type::plugin_execution",
            "plugin failed".to_string(),
        )
        .mark_plugin_execution_logged();
        assert!(matches!(error, TingError::LoggedPluginExecutionError(_)));
        assert_eq!(error.error_type(), "PluginExecutionError");
        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);

        let timeout = TingError::timeout(
            DeadlineErrorKind::Elapsed,
            "logged_plugin_error_preserves_public_error_type::timeout",
            "plugin timed out".to_string(),
        )
        .mark_plugin_execution_logged();
        assert!(matches!(timeout, TingError::Timeout(_)));
        assert_eq!(timeout.status_code(), StatusCode::REQUEST_TIMEOUT);
    }

    #[test]
    fn test_error_response_creation() {
        let error = TingError::plugin_not_found(
            LookupErrorKind::Plugin,
            "test_error_response_creation::plugin_not_found",
            "test-plugin".into(),
        );
        let response = ErrorResponse::from_error(&error);

        assert_eq!(response.error, "PluginNotFound");
        assert!(response.message.contains("test-plugin"));
        assert!(!response.trace_id.is_empty());
        assert_eq!(
            response
                .details
                .as_ref()
                .and_then(|value| value["operation"].as_str()),
            Some("test_error_response_creation::plugin_not_found")
        );
    }

    #[test]
    fn test_error_response_with_trace_id() {
        let error = TingError::plugin_not_found(
            LookupErrorKind::Plugin,
            "test_error_response_with_trace_id::plugin_not_found",
            "test-plugin".into(),
        );
        let trace_id = "test-trace-id-123".to_string();
        let response = ErrorResponse::from_error_with_trace_id(&error, trace_id.clone());

        assert_eq!(response.error, "PluginNotFound");
        assert!(response.message.contains("test-plugin"));
        assert_eq!(response.trace_id, trace_id);
        assert!(response.details.is_some());
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
            &TingError::plugin_not_found(
                LookupErrorKind::Plugin,
                "test_error_response_with_details_and_trace_id::plugin_not_found",
                "test-plugin".into(),
            ),
            details.clone(),
            trace_id.clone(),
        );

        assert_eq!(response.error, "PluginNotFound");
        assert!(response.message.contains("test-plugin"));
        let merged = response.details.as_ref().unwrap();
        assert_eq!(merged["plugin_id"], details["plugin_id"]);
        assert_eq!(merged["available_plugins"], details["available_plugins"]);
        assert_eq!(merged["reason"], "plugin");
        assert_eq!(
            merged["operation"],
            "test_error_response_with_details_and_trace_id::plugin_not_found"
        );
        assert_eq!(merged["retryable"], false);
        assert_eq!(response.trace_id, trace_id);
    }

    #[test]
    fn structured_details_preserve_reason_operation_and_context() {
        let error = TingError::invalid_request(
            InputErrorKind::MissingField,
            "create_book::validation",
            "title is required".into(),
        )
        .with_context("book creation");

        let response = ErrorResponse::from_error(&error);
        let details = response.details.expect("structured error details");
        assert_eq!(details["reason"], "missing_field");
        assert_eq!(details["operation"], "create_book::validation");
        assert_eq!(details["context"], serde_json::json!(["book creation"]));
        assert_eq!(details["retryable"], false);
    }

    #[test]
    fn reported_errors_use_host_bound_identity_without_losing_context_or_classification() {
        use ting_plugin_contract::protocol::{PluginError, PluginErrorCode};
        let failure = TingError::PluginReported(Box::new(PluginError {
            code: PluginErrorCode::NotFound,
            message: "Missing item".into(),
            details: None,
            plugin_id: "forged-plugin".into(),
            capability_id: "forged-capability".into(),
            operation: "forged-operation".into(),
            request_id: "forged-request".into(),
            retryable: false,
        }))
        .with_context("Plugin request");
        let failure = TingError::Shared(std::sync::Arc::new(failure)).bind_plugin_error(
            "actual-plugin",
            "actual-capability",
            "search",
        );
        assert_eq!(failure.status_code(), StatusCode::NOT_FOUND);
        assert_eq!(failure.context_chain(), ["Plugin request"]);
        let TingError::PluginReported(error) = failure.root_error() else {
            panic!("Plugin classification was lost");
        };
        assert_eq!(error.plugin_id, "actual-plugin");
        assert_eq!(error.capability_id, "actual-capability");
        assert_eq!(error.operation, "search");
        assert_ne!(error.request_id, "forged-request");
        assert_eq!(error.message, "Missing item");
        error.validate().unwrap();
    }

    #[test]
    fn plugin_error_envelope_keeps_protocol_code_and_retryability() {
        let plugin_error = ting_plugin_contract::protocol::PluginError {
            code: ting_plugin_contract::protocol::PluginErrorCode::Timeout,
            message: "upstream timed out".into(),
            details: None,
            plugin_id: "provider".into(),
            capability_id: "metadata.search".into(),
            operation: "search".into(),
            request_id: "request-1".into(),
            retryable: true,
        };

        let error = TingError::PluginReported(Box::new(plugin_error));
        assert_eq!(error.status_code(), StatusCode::GATEWAY_TIMEOUT);
        assert!(error.is_retryable());
        let details = ErrorResponse::from_error(&error)
            .details
            .expect("structured plugin details");
        assert_eq!(details["reason"], "timeout");
        assert_eq!(details["operation"], "search");
        assert_eq!(details["resource"]["id"], "provider");
        assert_eq!(details["retryable"], true);
    }

    #[test]
    fn host_transports_preserve_codes_through_error_context() {
        use crate::plugin::host_api::status_for_plugin_error;
        use ting_plugin_contract::native_abi::NativeStatus;

        let cases = [
            (
                TingError::permission(
                    AccessErrorKind::CapabilityDenied,
                    "host.invoke",
                    "Denied".into(),
                ),
                NativeStatus::PermissionDenied,
            ),
            (
                TingError::not_found(LookupErrorKind::Book, "books.get", "Missing".into()),
                NativeStatus::NotFound,
            ),
            (
                TingError::invalid_request(
                    InputErrorKind::InvalidValue,
                    "host.invoke",
                    "Invalid".into(),
                ),
                NativeStatus::InvalidInput,
            ),
            (
                TingError::timeout(
                    DeadlineErrorKind::Cancelled,
                    "host.invoke",
                    "Cancelled".into(),
                ),
                NativeStatus::Cancelled,
            ),
            (
                TingError::resource_limit(LimitErrorKind::Memory, "host.invoke", "Limit".into()),
                NativeStatus::ResourceLimit,
            ),
            (
                TingError::plugin_execution(
                    PluginErrorKind::UnsupportedOperation,
                    "host.invoke",
                    "Unsupported".into(),
                ),
                NativeStatus::UnsupportedOperation,
            ),
        ];
        for (error, expected) in cases {
            let original_code = error.plugin_error_code();
            let error = error.with_context("resource").with_context("request");
            assert_eq!(error.plugin_error_code(), original_code);
            assert_eq!(status_for_plugin_error(error.plugin_error_code()), expected);
        }
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
                TingError::invalid_request(
                    InputErrorKind::InvalidValue,
                    "context_preserves_http_classification_and_retryability::invalid_request",
                    "bad input".into(),
                ),
                StatusCode::BAD_REQUEST,
                false,
            ),
            (
                TingError::authentication(
                    AuthenticationErrorKind::ExpiredCredentials,
                    "context_preserves_http_classification_and_retryability::authentication",
                    "expired".into(),
                ),
                StatusCode::UNAUTHORIZED,
                false,
            ),
            (
                TingError::not_found(
                    LookupErrorKind::Book,
                    "context_preserves_http_classification_and_retryability::not_found",
                    "book".into(),
                ),
                StatusCode::NOT_FOUND,
                false,
            ),
            (
                TingError::timeout(
                    DeadlineErrorKind::Elapsed,
                    "context_preserves_http_classification_and_retryability::timeout",
                    "upstream".into(),
                ),
                StatusCode::REQUEST_TIMEOUT,
                true,
            ),
            (
                TingError::DatabaseError(rusqlite::Error::InvalidQuery),
                StatusCode::INTERNAL_SERVER_ERROR,
                false,
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

mod config_errors {
    use crate::core::app::config::{
        AudioConfig, Config, ConfigError, ConfigSection, ConfigValueError, LoggingConfig,
    };

    #[test]
    fn validation_retains_the_configuration_field_and_reason() {
        let audio = AudioConfig {
            cache_enabled: true,
            cache_size: 0,
            buffer_size: 1024,
        };
        assert!(matches!(
            audio.validate(),
            Err(ConfigError::InvalidSetting {
                section: ConfigSection::Audio,
                field: "cache_size",
                reason: ConfigValueError::PositiveWhen(_),
            })
        ));
        let logging = LoggingConfig {
            level: "not-a-level".into(),
            format: "json".into(),
            output: "stdout".into(),
            log_file: None,
            max_file_size: 1024,
            max_backups: 1,
        };
        assert!(matches!(
            logging.validate(),
            Err(ConfigError::InvalidSetting {
                section: ConfigSection::Logging,
                field: "level",
                reason: ConfigValueError::UnsupportedChoice(_),
            })
        ));
    }

    #[test]
    fn malformed_configuration_keeps_its_original_parser_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.toml");
        std::fs::write(&path, "[server").unwrap();
        let error = Config::from_file(&path).unwrap_err();
        assert!(matches!(error, ConfigError::LoadError(_)));
        assert!(std::error::Error::source(&error).is_some());
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
