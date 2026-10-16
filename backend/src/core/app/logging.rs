//! Structured logging system
//!
//! This module provides structured logging with:
//! - JSON and text format support
//! - Configurable log levels
//! - Log rotation with size limits
//! - Integration with tracing ecosystem

use crate::core::app::config::LoggingConfig;
use crate::core::app::error::InputErrorKind;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer,
    filter::{FilterExt, filter_fn},
    fmt,
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

const PLUGIN_LOG_TARGET: &str = "ting_reader::plugin::logger";

pub(crate) const MAX_LOG_PAGE_SIZE: usize = 500;

pub(crate) fn normalized_log_pagination(page: usize, page_size: usize) -> (usize, usize) {
    (page.max(1), page_size.clamp(1, MAX_LOG_PAGE_SIZE))
}

pub(crate) fn normalized_log_filter(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Shared level/text/time rules for system logs and plugin logs.
/// Module and plugin identity rules remain with their respective handlers.
#[derive(Debug, Clone)]
pub(crate) struct LogQueryFilter {
    level: Option<String>,
    query: Option<String>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
}

impl LogQueryFilter {
    pub(crate) fn new(
        level: Option<&str>,
        query: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> crate::core::app::error::Result<Self> {
        use crate::core::app::error::TingError;
        let parse = |name, value: Option<&str>| {
            normalized_log_filter(value)
                .map(|value| {
                    chrono::DateTime::parse_from_rfc3339(&value)
                        .map(|value| value.with_timezone(&chrono::Utc))
                        .map_err(|_| {
                            TingError::invalid_request(
                                InputErrorKind::InvalidValue,
                                "new::invalid_request",
                                format!("{name} must be an RFC 3339 timestamp"),
                            )
                        })
                })
                .transpose()
        };
        let since = parse("since", since)?;
        let until = parse("until", until)?;
        if since.zip(until).is_some_and(|(since, until)| since > until) {
            return Err(TingError::invalid_request(
                InputErrorKind::InvalidValue,
                "new::invalid_request",
                "since must not be later than until".into(),
            ));
        }
        Ok(Self {
            level: normalized_log_filter(level),
            query: normalized_log_filter(query).map(|value| value.to_lowercase()),
            since,
            until,
        })
    }

    pub(crate) fn matches(&self, log: &LogEntry) -> bool {
        if self
            .level
            .as_ref()
            .is_some_and(|level| !log.level.eq_ignore_ascii_case(level))
        {
            return false;
        }
        if let Some(query) = &self.query {
            let fields = log
                .fields
                .as_ref()
                .map(serde_json::Value::to_string)
                .unwrap_or_default();
            if ![
                log.message.as_str(),
                log.raw_message.as_deref().unwrap_or_default(),
                log.message_key.as_deref().unwrap_or_default(),
                log.module.as_str(),
                fields.as_str(),
            ]
            .iter()
            .any(|value| value.to_lowercase().contains(query))
            {
                return false;
            }
        }
        if self.since.is_none() && self.until.is_none() {
            return true;
        }
        let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&log.timestamp) else {
            return false;
        };
        let timestamp = timestamp.with_timezone(&chrono::Utc);
        self.since.is_none_or(|since| timestamp >= since)
            && self.until.is_none_or(|until| timestamp <= until)
    }
}

/// In-memory log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub module: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_params: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
}

impl LogEntry {
    fn from_tracing_json(json: serde_json::Value) -> Self {
        let text = |key| {
            json.get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let fields = json.get("fields");
        let message = fields
            .and_then(|fields| fields.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let message_key = fields
            .and_then(|fields| fields.get("message_key"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        let decode_value = |value: &serde_json::Value| {
            if value.is_object() {
                Some(value.clone())
            } else {
                value
                    .as_str()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            }
        };
        let message_params = fields
            .and_then(|fields| fields.get("message_params"))
            .and_then(decode_value)
            .filter(serde_json::Value::is_object);
        let fields = fields
            .and_then(serde_json::Value::as_object)
            .and_then(|fields| {
                let mut fields = fields.clone();
                for key in ["message", "message_key", "message_params"] {
                    fields.remove(key);
                }
                if let Some(value) = fields.get("plugin_fields").and_then(decode_value) {
                    fields.insert("plugin_fields".into(), value);
                }
                (!fields.is_empty()).then_some(serde_json::Value::Object(fields))
            });
        Self {
            timestamp: text("timestamp"),
            level: text("level"),
            module: text("target"),
            raw_message: (!message.is_empty()).then(|| message.clone()),
            message,
            message_key,
            message_params,
            fields,
            task_id: None,
            task_status: None,
            task_type: None,
        }
    }
}

pub(crate) fn read_log_file(path: &Path, logs: &mut Vec<LogEntry>) {
    use std::io::{BufRead, BufReader};
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    for line in BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
    {
        if let Ok(json) = serde_json::from_str(&line) {
            logs.push(LogEntry::from_tracing_json(json));
        }
    }
}

/// Logger instance that manages the logging system
pub struct Logger {
    _guards: Vec<Option<WorkerGuard>>,
}

impl Logger {
    /// Initialize the logging system based on configuration
    ///
    /// This sets up the global tracing subscriber with the specified format,
    /// level, and output destination.
    pub fn init(config: &LoggingConfig, data_dir: &Path) -> Result<Self> {
        // Parse log level
        let level = parse_log_level(&config.level)?;

        // Create env filter with the configured level
        let env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level.as_str()));

        let mut guards = Vec::new();

        // Create the appropriate writer and guard based on output configuration
        let (writer, guard) = match config.output.as_str() {
            "stdout" => {
                let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stdout());
                (non_blocking, Some(guard))
            }
            "file" => {
                let log_file = config
                    .log_file
                    .as_ref()
                    .context("log_file must be specified when output is 'file'")?;

                // Create parent directory if it doesn't exist
                if let Some(parent) = log_file.parent() {
                    std::fs::create_dir_all(parent).context("Failed to create log directory")?;
                }

                // Create rolling file appender
                let file_appender =
                    create_rolling_appender(log_file, config.max_file_size, config.max_backups)?;

                let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
                (non_blocking, Some(guard))
            }
            _ => {
                anyhow::bail!("Invalid output configuration: {}", config.output);
            }
        };
        guards.push(guard);

        // Create the formatting layer based on format configuration
        let fmt_layer = match config.format.as_str() {
            "json" => fmt::layer()
                .json()
                .with_writer(writer)
                .with_current_span(true)
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_target(true)
                .with_file(true)
                .with_line_number(true)
                .boxed(),
            "text" => fmt::layer()
                .with_writer(writer)
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_target(true)
                .with_file(true)
                .with_line_number(true)
                .boxed(),
            _ => {
                anyhow::bail!("Invalid format configuration: {}", config.format);
            }
        };

        // Setup API JSON log layer
        let api_log_dir = data_dir.join("logs");
        std::fs::create_dir_all(&api_log_dir).context("Failed to create api log dir")?;
        let api_log_file = api_log_dir.join("system.json");
        let api_appender = create_rolling_appender(
            &api_log_file,
            5 * 1024 * 1024, // 5 MB
            3,               // 3 backups
        )?;
        let (api_writer, api_guard) = tracing_appender::non_blocking(api_appender);
        guards.push(Some(api_guard));

        let api_layer = fmt::layer()
            .json()
            .with_writer(api_writer)
            .with_target(true)
            .boxed();

        let plugin_log_file = api_log_dir.join("plugins.json");
        let plugin_appender = create_rolling_appender(&plugin_log_file, 5 * 1024 * 1024, 3)?;
        let (plugin_writer, plugin_guard) = tracing_appender::non_blocking(plugin_appender);
        guards.push(Some(plugin_guard));
        let plugin_layer = fmt::layer()
            .json()
            .with_writer(plugin_writer)
            .with_target(true)
            .boxed();

        // Create env filter with the configured level for api layer
        let api_env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level.as_str()));
        let plugin_env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level.as_str()));

        // Initialize the global subscriber
        tracing_subscriber::registry()
            .with(fmt_layer.with_filter(env_filter))
            .with(api_layer.with_filter(
                api_env_filter.and(filter_fn(|metadata| metadata.target() != PLUGIN_LOG_TARGET)),
            ))
            .with(plugin_layer.with_filter(
                plugin_env_filter.and(filter_fn(|metadata| metadata.target() == PLUGIN_LOG_TARGET)),
            ))
            .try_init()
            .context("Failed to initialize tracing subscriber")?;

        tracing::info!(
            level = %config.level,
            format = %config.format,
            output = %config.output,
            message_key = "logging.initialized",
            "Logging initialized"
        );

        Ok(Logger { _guards: guards })
    }
}

/// Parse log level string to tracing Level
fn parse_log_level(level: &str) -> Result<Level> {
    match level.to_lowercase().as_str() {
        "debug" => Ok(Level::DEBUG),
        "info" => Ok(Level::INFO),
        "warn" => Ok(Level::WARN),
        "error" => Ok(Level::ERROR),
        _ => anyhow::bail!("Invalid log level: {}", level),
    }
}

/// Create a rolling file appender with size-based rotation
fn create_rolling_appender(
    log_file: &Path,
    max_file_size: usize,
    max_backups: usize,
) -> Result<RollingFileAppender> {
    let directory = log_file
        .parent()
        .context("Log file must have a parent directory")?;

    let filename = log_file
        .file_name()
        .context("Log file must have a filename")?
        .to_str()
        .context("Log filename must be valid UTF-8")?;

    Ok(RollingFileAppender::new(
        directory.to_path_buf(),
        filename.to_string(),
        max_file_size,
        max_backups,
    ))
}

/// Rolling file appender that rotates based on file size
pub struct RollingFileAppender {
    directory: std::path::PathBuf,
    filename: String,
    max_file_size: usize,
    max_backups: usize,
    current_file: std::sync::Mutex<Option<std::fs::File>>,
    current_size: std::sync::atomic::AtomicUsize,
}

impl RollingFileAppender {
    /// Create a new rolling file appender
    pub fn new(
        directory: std::path::PathBuf,
        filename: String,
        max_file_size: usize,
        max_backups: usize,
    ) -> Self {
        Self {
            directory,
            filename,
            max_file_size,
            max_backups,
            current_file: std::sync::Mutex::new(None),
            current_size: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Get the current log file path
    fn current_path(&self) -> std::path::PathBuf {
        self.directory.join(&self.filename)
    }

    /// Get the backup file path for a given index
    fn backup_path(&self, index: usize) -> std::path::PathBuf {
        self.directory.join(format!("{}.{}", self.filename, index))
    }

    /// Rotate log files
    fn rotate(&self) -> std::io::Result<()> {
        // Close current file
        let mut file_guard = self.current_file.lock().unwrap();
        *file_guard = None;
        drop(file_guard);

        // Rotate existing backups
        for i in (1..self.max_backups).rev() {
            let from = self.backup_path(i);
            let to = self.backup_path(i + 1);

            if from.exists() {
                if to.exists() {
                    std::fs::remove_file(&to)?;
                }
                std::fs::rename(&from, &to)?;
            }
        }

        // Move current file to backup.1
        let current = self.current_path();
        if current.exists() {
            let backup = self.backup_path(1);
            if backup.exists() {
                std::fs::remove_file(&backup)?;
            }
            std::fs::rename(&current, &backup)?;
        }

        // Reset size counter
        self.current_size
            .store(0, std::sync::atomic::Ordering::SeqCst);

        Ok(())
    }

    /// Get or create the current log file
    fn get_file(&self) -> std::io::Result<std::sync::MutexGuard<'_, Option<std::fs::File>>> {
        let mut file_guard = self.current_file.lock().unwrap();

        if file_guard.is_none() {
            let path = self.current_path();
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;

            // Get current file size
            let metadata = file.metadata()?;
            self.current_size
                .store(metadata.len() as usize, std::sync::atomic::Ordering::SeqCst);

            *file_guard = Some(file);
        }

        Ok(file_guard)
    }
}

impl std::io::Write for RollingFileAppender {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Check if rotation is needed
        let current_size = self.current_size.load(std::sync::atomic::Ordering::SeqCst);
        if current_size + buf.len() > self.max_file_size {
            self.rotate()?;
        }

        // Write to file
        let mut file_guard = self.get_file()?;
        let file = file_guard.as_mut().unwrap();
        let written = file.write(buf)?;

        // Update size counter
        self.current_size
            .fetch_add(written, std::sync::atomic::Ordering::SeqCst);

        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut file_guard = self.get_file()?;
        if let Some(file) = file_guard.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/core/app/logging.rs"]
mod tests;
