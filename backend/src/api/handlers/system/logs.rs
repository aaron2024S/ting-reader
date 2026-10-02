use crate::api::state::AppState;
use crate::core::app::error::{Result, TingError};
use crate::core::app::logging::LogEntry;
use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path as StdPath;

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub level: Option<String>,
    pub module: Option<String>,
    pub q: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

fn default_page() -> usize {
    1
}
fn default_page_size() -> usize {
    50
}

const MAX_LOG_PAGE_SIZE: usize = 500;

fn normalized_pagination(page: usize, page_size: usize) -> (usize, usize) {
    (page.max(1), page_size.clamp(1, MAX_LOG_PAGE_SIZE))
}

#[derive(Debug, Clone)]
struct LogFilters {
    level: Option<String>,
    module: Option<String>,
    query: Option<String>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

impl LogFilters {
    fn from_query(query: &LogsQuery) -> Result<Self> {
        let since = parse_time_filter("since", query.since.as_deref())?;
        let until = parse_time_filter("until", query.until.as_deref())?;
        if since.zip(until).is_some_and(|(since, until)| since > until) {
            return Err(TingError::InvalidRequest(
                "since must not be later than until".to_string(),
            ));
        }

        Ok(Self {
            level: normalized_filter(query.level.as_deref()),
            module: normalized_filter(query.module.as_deref()),
            query: normalized_filter(query.q.as_deref()).map(|value| value.to_lowercase()),
            since,
            until,
        })
    }

    fn matches(&self, log: &LogEntry) -> bool {
        self.level
            .as_ref()
            .is_none_or(|level| log.level.eq_ignore_ascii_case(level))
            && self.module_matches(log)
            && self.query_matches(log)
            && self.time_matches(log)
    }

    fn module_matches(&self, log: &LogEntry) -> bool {
        match self.module.as_deref() {
            Some(module) if module.eq_ignore_ascii_case("audit") => {
                log.module.starts_with("audit::")
                    || (log.level.eq_ignore_ascii_case("error")
                        && !log.module.starts_with("ting_reader::api::plugin"))
            }
            Some(module) if module.eq_ignore_ascii_case("all") => true,
            Some(module) => log
                .module
                .to_lowercase()
                .starts_with(&module.to_lowercase()),
            None => {
                log.module.starts_with("audit::")
                    || (log.level.eq_ignore_ascii_case("error")
                        && !log.module.starts_with("ting_reader::api::plugin"))
            }
        }
    }

    fn query_matches(&self, log: &LogEntry) -> bool {
        let Some(query) = self.query.as_deref() else {
            return true;
        };
        let fields = log
            .fields
            .as_ref()
            .map(serde_json::Value::to_string)
            .unwrap_or_default();
        [
            log.message.as_str(),
            log.raw_message.as_deref().unwrap_or_default(),
            log.message_key.as_deref().unwrap_or_default(),
            log.module.as_str(),
            fields.as_str(),
        ]
        .iter()
        .any(|value| value.to_lowercase().contains(query))
    }

    fn time_matches(&self, log: &LogEntry) -> bool {
        if self.since.is_none() && self.until.is_none() {
            return true;
        }
        let Ok(timestamp) = DateTime::parse_from_rfc3339(&log.timestamp) else {
            return false;
        };
        let timestamp = timestamp.with_timezone(&Utc);
        self.since.is_none_or(|since| timestamp >= since)
            && self.until.is_none_or(|until| timestamp <= until)
    }
}

fn normalized_filter(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_time_filter(name: &str, value: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| Some(timestamp.with_timezone(&Utc)))
        .map_err(|_| TingError::InvalidRequest(format!("{} must be an RFC 3339 timestamp", name)))
}

#[derive(Debug, Serialize)]
pub struct LogsResponse {
    pub logs: Vec<LogEntry>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

fn parse_message_params(fields: &serde_json::Value) -> Option<serde_json::Value> {
    let value = fields.get("message_params")?;
    if value.is_object() {
        return Some(value.clone());
    }
    value
        .as_str()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .filter(|parsed| parsed.is_object())
}

fn parse_log_file(path: &StdPath, logs: &mut Vec<LogEntry>) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let reader = BufReader::new(file);

    for line in reader.lines().map_while(std::result::Result::ok) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
            let timestamp = json
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let level = json
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let module = json
                .get("target")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let (message, raw_message, message_key, message_params) =
                if let Some(fields) = json.get("fields") {
                    let raw_message = fields.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    let message_key = fields
                        .get("message_key")
                        .and_then(|v| v.as_str())
                        .map(ToOwned::to_owned);
                    let message_params = parse_message_params(fields);
                    (
                        raw_message.to_string(),
                        Some(raw_message.to_string()).filter(|value| !value.is_empty()),
                        message_key,
                        message_params,
                    )
                } else {
                    (String::new(), None, None, None)
                };
            let fields = json.get("fields").and_then(|value| {
                value.as_object().and_then(|fields| {
                    let mut map = fields.clone();
                    map.remove("message");
                    map.remove("message_key");
                    map.remove("message_params");
                    if map.is_empty() {
                        None
                    } else {
                        Some(serde_json::Value::Object(map))
                    }
                })
            });

            logs.push(LogEntry {
                timestamp,
                level,
                module,
                message,
                raw_message,
                message_key,
                message_params,
                fields,
                task_id: None,
                task_status: None,
                task_type: None,
            });
        }
    }
}

fn read_api_logs(data_dir: &StdPath) -> Vec<LogEntry> {
    let mut logs = Vec::new();
    let api_log_dir = data_dir.join("logs");

    for i in (1..=3).rev() {
        let path = api_log_dir.join(format!("system.json.{}", i));
        if path.exists() {
            parse_log_file(&path, &mut logs);
        }
    }

    let current_path = api_log_dir.join("system.json");
    if current_path.exists() {
        parse_log_file(&current_path, &mut logs);
    }

    logs
}

/// Handler for GET /api/v1/system/logs - Get system logs
pub async fn get_system_logs(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Query(query): Query<LogsQuery>,
) -> Result<impl IntoResponse> {
    if user.role != "admin" {
        return Err(TingError::PermissionDenied(
            "Admin access required".to_string(),
        ));
    }

    let config = state.config.read().await;
    let data_dir = config.storage.data_dir.clone();
    drop(config);

    let filters = LogFilters::from_query(&query)?;

    // Get all tasks
    let tasks = state.task_queue.list_tasks().await.unwrap_or_default();

    let filtered_logs = tokio::task::spawn_blocking(move || {
        let all_logs = read_api_logs(&data_dir);

        let mut filtered: Vec<LogEntry> = all_logs
            .into_iter()
            .filter(|log| {
                // Ignore duplicate text logs for tasks so we only have one record per task
                log.module != "audit::scan" && log.module != "audit::metadata"
            })
            .collect();

        // Convert tasks to LogEntry and add them
        for task in tasks {
            let module = match task.task_type.as_str() {
                "scan" | "library_scan" | "scrape" => "audit::scan",
                "write_metadata" => "audit::metadata",
                _ => "audit::task",
            };

            let level = if task.status == "failed" {
                "ERROR"
            } else {
                "INFO"
            };

            let (message, raw_message, message_key, message_params) =
                if let Some(key) = task.message_key {
                    let message_params = task
                        .message_params
                        .and_then(|params| serde_json::from_str(&params).ok());
                    (
                        task.message.clone().unwrap_or_default(),
                        task.message.filter(|value| !value.is_empty()),
                        Some(key),
                        message_params,
                    )
                } else if let Some(msg) = task.message {
                    if !msg.is_empty() {
                        (msg.clone(), Some(msg), None, None)
                    } else if let Some(payload) = task.payload {
                        let params = serde_json::json!({ "payload": payload });
                        (
                            String::new(),
                            None,
                            Some("task.execute_with_payload".to_string()),
                            Some(params),
                        )
                    } else {
                        (String::new(), None, Some("task.execute".to_string()), None)
                    }
                } else if let Some(payload) = task.payload {
                    let params = serde_json::json!({ "payload": payload });
                    (
                        String::new(),
                        None,
                        Some("task.execute_with_payload".to_string()),
                        Some(params),
                    )
                } else {
                    (String::new(), None, Some("task.execute".to_string()), None)
                };

            let timestamp = if task.status == "running" {
                // Update running tasks to "now" so they appear at the top, or use updated_at
                Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            } else {
                task.updated_at
            };

            filtered.push(LogEntry {
                timestamp,
                level: level.to_string(),
                module: module.to_string(),
                message,
                raw_message,
                message_key,
                message_params,
                fields: Some(serde_json::json!({
                    "task_id": task.id.clone(),
                    "task_status": task.status.clone(),
                    "task_type": task.task_type.clone(),
                })),
                task_id: Some(task.id),
                task_status: Some(task.status),
                task_type: Some(task.task_type),
            });
        }

        filtered.retain(|log| filters.matches(log));

        // Sort by timestamp descending
        filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        filtered
    })
    .await
    .map_err(|e| TingError::ExternalError(e.to_string()))?;

    let total = filtered_logs.len();
    let (page, page_size) = normalized_pagination(query.page, query.page_size);
    let start = page.saturating_sub(1).saturating_mul(page_size);
    let end = start.saturating_add(page_size).min(total);

    let page_logs = if start < total {
        filtered_logs[start..end].to_vec()
    } else {
        Vec::new()
    };

    Ok(Json(LogsResponse {
        logs: page_logs,
        total,
        page,
        page_size,
    }))
}

/// Handler for GET /api/v1/system/logs/export - Export system logs
pub async fn export_system_logs(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Query(query): Query<LogsQuery>,
) -> Result<impl IntoResponse> {
    if user.role != "admin" {
        return Err(TingError::PermissionDenied(
            "Admin access required".to_string(),
        ));
    }

    let config = state.config.read().await;
    let data_dir = config.storage.data_dir.clone();
    drop(config);

    let filters = LogFilters::from_query(&query)?;

    let filtered_logs = tokio::task::spawn_blocking(move || {
        let all_logs = read_api_logs(&data_dir);

        all_logs
            .into_iter()
            .filter(|log| filters.matches(log))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| TingError::ExternalError(e.to_string()))?;

    let mut output = String::new();
    for log in filtered_logs {
        let fields = log
            .fields
            .as_ref()
            .map(|value| format!(" {}", value))
            .unwrap_or_default();
        output.push_str(&format!(
            "[{}] [{}] [{}] {}{}\n",
            log.timestamp, log.level, log.module, log.message, fields
        ));
    }

    let timestamp = Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let filename = if query
        .level
        .as_deref()
        .is_some_and(|level| level.eq_ignore_ascii_case("error"))
    {
        format!("error_logs_{}.txt", timestamp)
    } else {
        format!("system_logs_{}.txt", timestamp)
    };

    let headers = [
        (
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8".to_string(),
        ),
        (
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        ),
    ];

    Ok((headers, output).into_response())
}

#[derive(Debug, Serialize)]
pub struct ClearSystemLogsResponse {
    pub message: String,
}

/// Handler for DELETE /api/v1/system/logs - Clear system logs
pub async fn clear_system_logs(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    if user.role != "admin" {
        return Err(TingError::PermissionDenied(
            "Admin access required".to_string(),
        ));
    }

    let config = state.config.read().await;
    let data_dir = config.storage.data_dir.clone();
    drop(config);

    tokio::task::spawn_blocking(move || {
        let api_log_dir = data_dir.join("logs");

        for filename in ["system.json", "plugins.json"] {
            for i in 1..=3 {
                let path = api_log_dir.join(format!("{filename}.{i}"));
                if path.exists() {
                    let _ = std::fs::remove_file(path);
                }
            }

            let current_path = api_log_dir.join(filename);
            if current_path.exists() {
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(current_path);
            }
        }
    })
    .await
    .map_err(|e| TingError::ExternalError(e.to_string()))?;

    Ok(Json(ClearSystemLogsResponse {
        message: "System and plugin logs cleared successfully".to_string(),
    }))
}

#[cfg(test)]
#[path = "../../../../tests/unit/api/handlers/system/logs.rs"]
mod tests;
