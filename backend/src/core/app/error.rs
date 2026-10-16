//! Typed domain failures, owned error chains, and their HTTP presentation.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::fmt;
use ting_plugin_contract::protocol::{PluginError, PluginErrorCode};
use uuid::Uuid;

tokio::task_local! {
    pub(crate) static REQUEST_TRACE_ID: String;
}

fn response_trace_id() -> String {
    REQUEST_TRACE_ID
        .try_with(Clone::clone)
        .unwrap_or_else(|_| Uuid::new_v4().to_string())
}

macro_rules! error_reasons {
    ($($name:ident { $($variant:ident),+ $(,)? })+) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    )+};
}

error_reasons! {
    SystemErrorKind { Initialization }
    ConfigurationErrorKind { Missing, InvalidValue, Persistence, Synchronization, Encoding }
    LookupErrorKind { Plugin, Book, Chapter, Bookmark, Library, Series, Playlist, User, Task, Session, File, Resource }
    PluginErrorKind { Synchronization, AbiMismatch, InvalidManifest, Dependency, Cancelled, Unavailable, UnsupportedOperation, InvalidOutput, Communication, Loading, Runtime }
    DependencyErrorKind { Cycle, VersionMismatch, Conflict, Missing }
    InputErrorKind { MissingField, UnsupportedValue, OutOfRange, Conflict, InvalidPath, InvalidFormat, InvalidValue }
    AuthenticationErrorKind { PasswordProcessing, UserNotFound, MissingCredentials, ExpiredCredentials, TokenCreation, InvalidCredentials }
    AccessErrorKind { UserRequired, CapabilityDenied, AdministratorRequired, OwnershipMismatch, ResourceDenied }
    SecurityErrorKind { PathEscape, Integrity, NetworkTarget, InvalidIdentity }
    LimitErrorKind { Memory, Execution, Concurrency, Handles, Payload }
    DeadlineErrorKind { Cancelled, Elapsed }
    NetworkErrorKind { HttpStatus, Redirect, InvalidResponse, Transport }
    CodecErrorKind { InvalidData }
    TaskErrorKind { Join, Unavailable, InvalidState, Execution, Unclassified }
    EventErrorKind { Delivery }
    ExternalErrorKind { Process, Unavailable, Response }
}

/// The reason controls behavior; the message is only a diagnostic description.
#[derive(Debug)]
pub struct Failure<R> {
    pub reason: R,
    pub operation: &'static str,
    pub message: String,
    pub resource: Option<ErrorResource>,
    source: Option<anyhow::Error>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ErrorResource {
    pub kind: LookupErrorKind,
    pub id: String,
}

impl<R> Failure<R> {
    fn new(reason: R, operation: &'static str, message: String) -> Self {
        Self {
            reason,
            operation,
            message,
            resource: None,
            source: None,
        }
    }
}

impl<R> fmt::Display for Failure<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl<R> std::ops::Deref for Failure<R> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl<R> AsRef<str> for Failure<R> {
    fn as_ref(&self) -> &str {
        &self.message
    }
}

impl<R: fmt::Debug> std::error::Error for Failure<R> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|error| error.as_ref())
    }
}

/// An error's domain is independent of its display text and attached context.
#[derive(Debug, thiserror::Error)]
pub enum TingError {
    #[error("{0}")]
    Shared(#[source] std::sync::Arc<TingError>),
    #[error("{context}: {source}")]
    Context {
        context: String,
        #[source]
        source: Box<TingError>,
    },
    #[error("System initialization failed: {0}")]
    InitializationError(#[source] Failure<SystemErrorKind>),
    #[error("Configuration error: {0}")]
    ConfigError(#[source] Failure<ConfigurationErrorKind>),
    #[error("Database error: {0}")]
    DatabaseError(#[from] rusqlite::Error),
    #[error("Plugin not found: {0}")]
    PluginNotFound(#[source] Failure<LookupErrorKind>),
    #[error("Plugin load failed: {0}")]
    PluginLoadError(#[source] Failure<PluginErrorKind>),
    #[error("Plugin execution error: {0}")]
    PluginExecutionError(#[source] Failure<PluginErrorKind>),
    #[error("Plugin execution error: {0}")]
    LoggedPluginExecutionError(#[source] Failure<PluginErrorKind>),
    #[error("Plugin execution error: {}", .0.message)]
    PluginReported(Box<PluginError>),
    #[error("Plugin dependency error: {0}")]
    DependencyError(#[source] Failure<DependencyErrorKind>),
    #[error("Invalid request: {0}")]
    InvalidRequest(#[source] Failure<InputErrorKind>),
    #[error("Authentication failed: {0}")]
    AuthenticationError(#[source] Failure<AuthenticationErrorKind>),
    #[error("Resource not found: {0}")]
    NotFound(#[source] Failure<LookupErrorKind>),
    #[error("Permission denied: {0}")]
    PermissionDenied(#[source] Failure<AccessErrorKind>),
    #[error("Security violation: {0}")]
    SecurityViolation(#[source] Failure<SecurityErrorKind>),
    #[error("Resource limit exceeded: {0}")]
    ResourceLimitExceeded(#[source] Failure<LimitErrorKind>),
    #[error("Timeout: {0}")]
    Timeout(#[source] Failure<DeadlineErrorKind>),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Network error: {0}")]
    NetworkError(#[source] Failure<NetworkErrorKind>),
    #[error("Network error: {0}")]
    NetworkFailure(#[source] Box<reqwest::Error>),
    #[error("Serialization error: {0}")]
    SerializationError(#[source] Failure<CodecErrorKind>),
    #[error("Deserialization error: {0}")]
    DeserializationError(#[source] Failure<CodecErrorKind>),
    #[error("Validation error: {0}")]
    ValidationError(#[source] Failure<InputErrorKind>),
    #[error("Task error: {0}")]
    TaskError(#[source] Failure<TaskErrorKind>),
    #[error("Event error: {0}")]
    EventError(#[source] Failure<EventErrorKind>),
    #[error("External tool error: {0}")]
    ExternalError(#[source] Failure<ExternalErrorKind>),
    #[error("External service error: {0}")]
    ExternalServiceError(#[source] Failure<ExternalErrorKind>),
}

macro_rules! failure_constructors {
    ($($method:ident => $variant:ident($kind:ty)),+ $(,)?) => {
        impl TingError {
            $(pub fn $method(reason: $kind, operation: &'static str, message: String) -> Self {
                Self::$variant(Failure::new(reason, operation, message))
            })+

            /// Attach an owned cause, retaining its concrete type for downcasting.
            pub fn with_source(self, source: anyhow::Error) -> Self {
                let source = match source.downcast::<reqwest::Error>() {
                    Ok(error) => anyhow::Error::new(error.without_url()),
                    Err(error) => error,
                };
                match self {
                    $(Self::$variant(mut failure) => {
                        failure.source = Some(source);
                        Self::$variant(failure)
                    },)+
                    Self::LoggedPluginExecutionError(mut failure) => {
                        failure.source = Some(source);
                        Self::LoggedPluginExecutionError(failure)
                    }
                    Self::Context { context, source: inner } => Self::Context {
                        context, source: Box::new(inner.with_source(source)),
                    },
                    error => error,
                }
            }

            pub fn with_resource(self, kind: LookupErrorKind, id: impl Into<String>) -> Self {
                let resource = ErrorResource { kind, id: id.into() };
                match self {
                    $(Self::$variant(mut failure) => {
                        failure.resource = Some(resource);
                        Self::$variant(failure)
                    },)+
                    Self::LoggedPluginExecutionError(mut failure) => {
                        failure.resource = Some(resource);
                        Self::LoggedPluginExecutionError(failure)
                    }
                    Self::Context { context, source } => Self::Context {
                        context, source: Box::new(source.with_resource(resource.kind, resource.id)),
                    },
                    error => error,
                }
            }

            /// Export classification, never the raw source chain or plugin details.
            pub fn diagnostic_details(&self) -> serde_json::Value {
                let mut details = match self.root_error() {
                    $(Self::$variant(failure) => serde_json::json!({
                        "reason": failure.reason,
                        "operation": failure.operation,
                        "resource": failure.resource,
                    }),)+
                    Self::LoggedPluginExecutionError(failure) => serde_json::json!({
                        "reason": failure.reason, "operation": failure.operation, "resource": failure.resource,
                    }),
                    Self::PluginReported(error) => serde_json::json!({
                        "reason": error.code, "operation": error.operation,
                        "resource": { "kind": "plugin", "id": error.plugin_id },
                        "capability_id": error.capability_id,
                    }),
                    Self::DatabaseError(_) => serde_json::json!({ "reason": "database" }),
                    Self::IoError(error) => serde_json::json!({ "reason": format!("{:?}", error.kind()) }),
                    Self::NetworkFailure(_) => serde_json::json!({ "reason": "transport" }),
                    Self::Context { .. } | Self::Shared(_) => unreachable!("root_error unwraps wrappers"),
                };
                details["context"] = serde_json::json!(self.context_chain());
                details["retryable"] = serde_json::json!(self.is_retryable());
                details
            }
        }
    };
}

failure_constructors! {
    initialization => InitializationError(SystemErrorKind),
    configuration => ConfigError(ConfigurationErrorKind),
    plugin_not_found => PluginNotFound(LookupErrorKind),
    plugin_load => PluginLoadError(PluginErrorKind),
    plugin_execution => PluginExecutionError(PluginErrorKind),
    dependency => DependencyError(DependencyErrorKind),
    invalid_request => InvalidRequest(InputErrorKind),
    authentication => AuthenticationError(AuthenticationErrorKind),
    not_found => NotFound(LookupErrorKind),
    permission => PermissionDenied(AccessErrorKind),
    security => SecurityViolation(SecurityErrorKind),
    resource_limit => ResourceLimitExceeded(LimitErrorKind),
    timeout => Timeout(DeadlineErrorKind),
    network => NetworkError(NetworkErrorKind),
    serialization => SerializationError(CodecErrorKind),
    deserialization => DeserializationError(CodecErrorKind),
    validation => ValidationError(InputErrorKind),
    task => TaskError(TaskErrorKind),
    event => EventError(EventErrorKind),
    external_tool => ExternalError(ExternalErrorKind),
    external_service => ExternalServiceError(ExternalErrorKind),
}

impl TingError {
    /// Classification shared by Host transports; context never changes a code.
    pub fn plugin_error_code(&self) -> PluginErrorCode {
        match self.root_error() {
            Self::PluginReported(error) => error.code,
            Self::InvalidRequest(_) | Self::ValidationError(_) => PluginErrorCode::InvalidInput,
            Self::AuthenticationError(_)
            | Self::PermissionDenied(_)
            | Self::SecurityViolation(_) => PluginErrorCode::PermissionDenied,
            Self::NotFound(_) | Self::PluginNotFound(_) => PluginErrorCode::NotFound,
            Self::ResourceLimitExceeded(_) => PluginErrorCode::ResourceLimit,
            Self::Timeout(failure) if failure.reason == DeadlineErrorKind::Cancelled => {
                PluginErrorCode::Cancelled
            }
            Self::Timeout(_) => PluginErrorCode::Timeout,
            Self::NetworkError(_) | Self::NetworkFailure(_) => PluginErrorCode::NetworkError,
            Self::SerializationError(_) | Self::DeserializationError(_) => {
                PluginErrorCode::ParseError
            }
            Self::DependencyError(_) => PluginErrorCode::DependencyError,
            Self::PluginLoadError(failure)
            | Self::PluginExecutionError(failure)
            | Self::LoggedPluginExecutionError(failure) => match failure.reason {
                PluginErrorKind::Cancelled => PluginErrorCode::Cancelled,
                PluginErrorKind::Unavailable => PluginErrorCode::PluginUnavailable,
                PluginErrorKind::UnsupportedOperation => PluginErrorCode::UnsupportedOperation,
                PluginErrorKind::InvalidManifest => PluginErrorCode::InvalidManifest,
                PluginErrorKind::InvalidOutput => PluginErrorCode::InvalidOutput,
                PluginErrorKind::AbiMismatch => PluginErrorCode::AbiMismatch,
                PluginErrorKind::Dependency => PluginErrorCode::DependencyError,
                _ => PluginErrorCode::InternalError,
            },
            _ => PluginErrorCode::InternalError,
        }
    }

    pub fn status_code(&self) -> StatusCode {
        match self.root_error() {
            Self::InvalidRequest(_)
            | Self::SerializationError(_)
            | Self::DeserializationError(_)
            | Self::ValidationError(_) => StatusCode::BAD_REQUEST,
            Self::AuthenticationError(_) => StatusCode::UNAUTHORIZED,
            Self::PermissionDenied(_) | Self::SecurityViolation(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) | Self::PluginNotFound(_) => StatusCode::NOT_FOUND,
            Self::Timeout(_) => StatusCode::REQUEST_TIMEOUT,
            Self::ResourceLimitExceeded(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::PluginReported(error) => {
                StatusCode::from_u16(error.http_status(false)).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable names retained in the REST response's `error` field.
    pub fn error_type(&self) -> &'static str {
        match self.root_error() {
            Self::InitializationError(_) => "InitializationError",
            Self::ConfigError(_) => "ConfigError",
            Self::DatabaseError(_) => "DatabaseError",
            Self::PluginNotFound(_) => "PluginNotFound",
            Self::PluginLoadError(_) => "PluginLoadError",
            Self::PluginExecutionError(_)
            | Self::LoggedPluginExecutionError(_)
            | Self::PluginReported(_) => "PluginExecutionError",
            Self::DependencyError(_) => "DependencyError",
            Self::InvalidRequest(_) => "InvalidRequest",
            Self::AuthenticationError(_) => "AuthenticationError",
            Self::NotFound(_) => "NotFound",
            Self::PermissionDenied(_) => "PermissionDenied",
            Self::SecurityViolation(_) => "SecurityViolation",
            Self::ResourceLimitExceeded(_) => "ResourceLimitExceeded",
            Self::Timeout(_) => "Timeout",
            Self::IoError(_) => "IoError",
            Self::NetworkError(_) | Self::NetworkFailure(_) => "NetworkError",
            Self::SerializationError(_) => "SerializationError",
            Self::DeserializationError(_) => "DeserializationError",
            Self::ValidationError(_) => "ValidationError",
            Self::TaskError(_) => "TaskError",
            Self::EventError(_) => "EventError",
            Self::ExternalError(_) => "ExternalError",
            Self::ExternalServiceError(_) => "ExternalServiceError",
            Self::Context { .. } | Self::Shared(_) => unreachable!("root_error unwraps wrappers"),
        }
    }

    pub fn is_retryable(&self) -> bool {
        match self.root_error() {
            Self::PluginReported(error) => error.retryable,
            Self::DatabaseError(rusqlite::Error::SqliteFailure(error, _)) => matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ),
            Self::NetworkError(failure) => failure.reason == NetworkErrorKind::Transport,
            Self::NetworkFailure(error) => error.is_timeout() || error.is_connect(),
            Self::Timeout(failure) => failure.reason == DeadlineErrorKind::Elapsed,
            Self::ResourceLimitExceeded(_) => true,
            _ => false,
        }
    }

    pub fn mark_plugin_execution_logged(self) -> Self {
        match self {
            Self::Context { context, source } => Self::Context {
                context,
                source: Box::new(source.mark_plugin_execution_logged()),
            },
            Self::PluginExecutionError(failure) => Self::LoggedPluginExecutionError(failure),
            error => error,
        }
    }

    pub fn with_plugin_execution_context(self, context: impl AsRef<str>) -> Self {
        self.with_context(context.as_ref().to_owned())
    }

    /// Plugin-supplied error identities describe no authority. Bind them to the
    /// validated call before exposing diagnostics or retaining a failure.
    pub(crate) fn bind_plugin_error(
        self,
        plugin_id: &str,
        capability_id: &str,
        operation: &str,
    ) -> Self {
        let Self::PluginReported(error) = self.root_error() else {
            return self;
        };
        let mut error = (**error).clone();
        error.plugin_id = plugin_id.into();
        error.capability_id = capability_id.into();
        error.operation = operation.into();
        error.request_id = Uuid::new_v4().to_string();
        self.context_chain()
            .into_iter()
            .rev()
            .fold(Self::PluginReported(Box::new(error)), |error, context| {
                error.with_context(context)
            })
    }

    pub fn with_context(self, context: impl Into<String>) -> Self {
        Self::Context {
            context: context.into(),
            source: Box::new(self),
        }
    }

    pub fn root_error(&self) -> &Self {
        match self {
            Self::Context { source, .. } => source.root_error(),
            Self::Shared(source) => source.root_error(),
            error => error,
        }
    }

    pub fn context_chain(&self) -> Vec<&str> {
        match self {
            Self::Context { context, source } => {
                let mut contexts = vec![context.as_str()];
                contexts.extend(source.context_chain());
                contexts
            }
            Self::Shared(source) => source.context_chain(),
            _ => Vec::new(),
        }
    }
}

impl From<reqwest::Error> for TingError {
    fn from(error: reqwest::Error) -> Self {
        Self::NetworkFailure(Box::new(error.without_url()))
    }
}

/// The REST envelope remains stable; structured classification lives in details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    pub trace_id: String,
}

impl ErrorResponse {
    pub fn new(error: String, message: String) -> Self {
        Self::with_trace_id(error, message, response_trace_id())
    }

    pub fn with_trace_id(error: String, message: String, trace_id: String) -> Self {
        Self {
            error,
            message,
            details: None,
            trace_id,
        }
    }

    pub fn with_details(error: String, message: String, details: serde_json::Value) -> Self {
        Self::with_details_and_trace_id(error, message, details, response_trace_id())
    }

    pub fn with_details_and_trace_id(
        error: String,
        message: String,
        details: serde_json::Value,
        trace_id: String,
    ) -> Self {
        Self {
            error,
            message,
            details: Some(details),
            trace_id,
        }
    }

    pub fn from_error(error: &TingError) -> Self {
        Self::from_error_with_trace_id(error, response_trace_id())
    }

    pub fn from_error_with_trace_id(error: &TingError, trace_id: String) -> Self {
        Self::with_details_and_trace_id(
            error.error_type().into(),
            error.to_string(),
            error.diagnostic_details(),
            trace_id,
        )
    }

    pub fn from_error_with_details(error: &TingError, details: serde_json::Value) -> Self {
        Self::from_error_with_details_and_trace_id(error, details, response_trace_id())
    }

    pub fn from_error_with_details_and_trace_id(
        error: &TingError,
        details: serde_json::Value,
        trace_id: String,
    ) -> Self {
        let mut response = Self::from_error_with_trace_id(error, trace_id);
        if let (Some(base), Some(extra)) = (
            response
                .details
                .as_mut()
                .and_then(serde_json::Value::as_object_mut),
            details.as_object(),
        ) {
            for (key, value) in extra {
                base.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        response
    }
}

impl fmt::Display for ErrorResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "[{}] {} (trace_id: {})",
            self.error, self.message, self.trace_id
        )
    }
}

impl IntoResponse for TingError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let error_response = ErrorResponse::from_error(&self);
        let log_params = serde_json::json!({
            "error": self.to_string(), "error_type": self.error_type(), "status_code": status.as_u16(),
            "details": error_response.details,
        });
        let (message_key, warning) = match self.root_error() {
            Self::AuthenticationError(_) => ("http.error.authentication", true),
            Self::PermissionDenied(_) | Self::SecurityViolation(_) => {
                ("http.error.permission", true)
            }
            Self::PluginExecutionError(_) | Self::PluginReported(_) => ("http.error.plugin", false),
            Self::DatabaseError(_) if self.is_retryable() => ("http.error.database_busy", true),
            _ => ("http.error.request", status.is_client_error()),
        };
        if !matches!(self.root_error(), Self::LoggedPluginExecutionError(_)) {
            if warning {
                tracing::warn!(
                    error_type = self.error_type(), error = %self, trace_id = %error_response.trace_id,
                    status_code = %status, message_key, message_params = %log_params, "Request failed"
                );
            } else if message_key == "http.error.plugin" {
                tracing::error!(
                    target: "ting_reader::api::plugin",
                    error_type = self.error_type(), error = %self, trace_id = %error_response.trace_id,
                    status_code = %status, message_key, message_params = %log_params, "Plugin request failed"
                );
            } else {
                tracing::error!(
                    error_type = self.error_type(), error = %self, trace_id = %error_response.trace_id,
                    status_code = %status, message_key, message_params = %log_params, "Request failed"
                );
            }
        }
        (status, Json(error_response)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, TingError>;

pub trait ErrorContext<T> {
    fn context(self, context: impl Into<String>) -> Result<T>;
    fn with_context<F: FnOnce() -> String>(self, context: F) -> Result<T>;
}

impl<T, E: Into<TingError>> ErrorContext<T> for std::result::Result<T, E> {
    fn context(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|error| error.into().with_context(context))
    }

    fn with_context<F: FnOnce() -> String>(self, context: F) -> Result<T> {
        self.map_err(|error| error.into().with_context(context()))
    }
}
