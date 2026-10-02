use crate::api::require_admin;
use crate::api::state::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{Result, TingError};
use crate::core::app::logging::LogEntry;
use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path as StdPath;

const MAX_PLUGIN_LOG_PAGE_SIZE: usize = 500;

#[derive(Debug, Deserialize)]
pub struct PluginLogsQuery {
    pub plugin_id: Option<String>,
    pub level: Option<String>,
    pub source: Option<String>,
    pub q: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

#[derive(Debug, Serialize)]
pub struct PluginLogsResponse {
    pub logs: Vec<LogEntry>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

#[derive(Debug, Clone)]
struct PluginLogFilters {
    plugin_id: Option<String>,
    level: Option<String>,
    source: Option<String>,
    query: Option<String>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

impl PluginLogFilters {
    fn from_query(plugin_id: Option<String>, query: &PluginLogsQuery) -> Result<Self> {
        let since = parse_time_filter("since", query.since.as_deref())?;
        let until = parse_time_filter("until", query.until.as_deref())?;
        // The UI sends an empty plugin_id for the all-plugins option.
        // Treat blank values as no plugin filter instead of matching an empty ID.
        let plugin_id = plugin_id
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if since.zip(until).is_some_and(|(since, until)| since > until) {
            return Err(TingError::InvalidRequest(
                "since must not be later than until".to_string(),
            ));
        }

        Ok(Self {
            plugin_id,
            level: normalized_filter(query.level.as_deref()),
            source: normalized_filter(query.source.as_deref()),
            query: normalized_filter(query.q.as_deref()).map(|value| value.to_lowercase()),
            since,
            until,
        })
    }

    fn matches(&self, log: &LogEntry) -> bool {
        self.plugin_matches(log)
            && self
                .level
                .as_ref()
                .is_none_or(|level| log.level.eq_ignore_ascii_case(level))
            && self.source.as_ref().is_none_or(|source| {
                log_field(log, "source")
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(source))
            })
            && self.query_matches(log)
            && self.time_matches(log)
    }

    fn plugin_matches(&self, log: &LogEntry) -> bool {
        let Some(plugin_id) = self.plugin_id.as_deref() else {
            return true;
        };
        [
            log_field(log, "plugin_id"),
            log_field(log, "plugin_instance_id"),
        ]
        .into_iter()
        .flatten()
        .any(|candidate| {
            plugin_id_without_version(candidate) == plugin_id_without_version(plugin_id)
        })
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

fn default_page() -> usize {
    1
}

fn default_page_size() -> usize {
    100
}

fn normalized_pagination(page: usize, page_size: usize) -> (usize, usize) {
    (page.max(1), page_size.clamp(1, MAX_PLUGIN_LOG_PAGE_SIZE))
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
        .map_err(|_| TingError::InvalidRequest(format!("{name} must be an RFC 3339 timestamp")))
}

fn plugin_id_without_version(plugin_id: &str) -> &str {
    plugin_id
        .rsplit_once('@')
        .filter(|(id, version)| !id.is_empty() && !version.is_empty())
        .map(|(id, _)| id)
        .unwrap_or(plugin_id)
}

fn safe_filename_component(value: &str) -> String {
    let value = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if value.is_empty() {
        "plugin".to_string()
    } else {
        value
    }
}

fn log_field<'a>(log: &'a LogEntry, key: &str) -> Option<&'a str> {
    log.fields.as_ref()?.get(key)?.as_str()
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

fn parse_plugin_log_file(path: &StdPath, logs: &mut Vec<LogEntry>) {
    let Ok(file) = File::open(path) else {
        return;
    };

    for line in BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
    {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let fields = json.get("fields");
        let raw_message = fields
            .and_then(|value| value.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let message_key = fields
            .and_then(|value| value.get("message_key"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        let message_params = fields.and_then(parse_message_params);
        let structured_fields = fields.and_then(|value| {
            value.as_object().and_then(|fields| {
                let mut map = fields.clone();
                map.remove("message");
                map.remove("message_key");
                map.remove("message_params");
                if let Some(plugin_fields) = map
                    .get("plugin_fields")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
                {
                    map.insert("plugin_fields".to_string(), plugin_fields);
                }
                (!map.is_empty()).then_some(serde_json::Value::Object(map))
            })
        });

        logs.push(LogEntry {
            timestamp: json
                .get("timestamp")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            level: json
                .get("level")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            module: json
                .get("target")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            message: raw_message.clone(),
            raw_message: (!raw_message.is_empty()).then_some(raw_message),
            message_key,
            message_params,
            fields: structured_fields,
            task_id: None,
            task_status: None,
            task_type: None,
        });
    }
}

fn read_plugin_logs(data_dir: &StdPath) -> Vec<LogEntry> {
    let log_dir = data_dir.join("logs");
    let mut logs = Vec::new();
    for index in (1..=3).rev() {
        parse_plugin_log_file(&log_dir.join(format!("plugins.json.{index}")), &mut logs);
    }
    parse_plugin_log_file(&log_dir.join("plugins.json"), &mut logs);
    logs
}

async fn filtered_plugin_logs(
    state: &AppState,
    plugin_id: Option<String>,
    query: &PluginLogsQuery,
) -> Result<Vec<LogEntry>> {
    let filters = PluginLogFilters::from_query(plugin_id, query)?;
    let data_dir = state.config.read().await.storage.data_dir.clone();
    tokio::task::spawn_blocking(move || {
        let mut logs = read_plugin_logs(&data_dir)
            .into_iter()
            .filter(|log| filters.matches(log))
            .collect::<Vec<_>>();
        logs.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
        logs
    })
    .await
    .map_err(|error| TingError::ExternalError(error.to_string()))
}

pub async fn get_plugin_logs(
    State(state): State<AppState>,
    user: AuthUser,
    Path(plugin_id): Path<String>,
    Query(query): Query<PluginLogsQuery>,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    let logs = filtered_plugin_logs(&state, Some(plugin_id), &query).await?;
    let total = logs.len();
    let (page, page_size) = normalized_pagination(query.page, query.page_size);
    let start = page.saturating_sub(1).saturating_mul(page_size);
    let end = start.saturating_add(page_size).min(total);
    let logs = if start < total {
        logs[start..end].to_vec()
    } else {
        Vec::new()
    };

    Ok(Json(PluginLogsResponse {
        logs,
        total,
        page,
        page_size,
    }))
}

pub async fn get_all_plugin_logs(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<PluginLogsQuery>,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    let logs = filtered_plugin_logs(&state, query.plugin_id.clone(), &query).await?;
    let total = logs.len();
    let (page, page_size) = normalized_pagination(query.page, query.page_size);
    let start = page.saturating_sub(1).saturating_mul(page_size);
    let end = start.saturating_add(page_size).min(total);
    let logs = if start < total {
        logs[start..end].to_vec()
    } else {
        Vec::new()
    };

    Ok(Json(PluginLogsResponse {
        logs,
        total,
        page,
        page_size,
    }))
}

pub async fn export_plugin_logs(
    State(state): State<AppState>,
    user: AuthUser,
    Path(plugin_id): Path<String>,
    Query(query): Query<PluginLogsQuery>,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    let logs = filtered_plugin_logs(&state, Some(plugin_id.clone()), &query).await?;
    let mut output = String::new();
    for log in logs {
        let fields = log
            .fields
            .as_ref()
            .map(|value| format!(" {value}"))
            .unwrap_or_default();
        output.push_str(&format!(
            "[{}] [{}] {}{}\n",
            log.timestamp, log.level, log.message, fields
        ));
    }

    let filename = format!(
        "plugin_{}_logs_{}.txt",
        safe_filename_component(plugin_id_without_version(&plugin_id)),
        Utc::now().format("%Y%m%d_%H%M%S")
    );
    let headers = [
        (
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8".to_string(),
        ),
        (
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        ),
    ];
    Ok((headers, output).into_response())
}

pub async fn export_all_plugin_logs(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<PluginLogsQuery>,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    let logs = filtered_plugin_logs(&state, query.plugin_id.clone(), &query).await?;
    let mut output = String::new();
    for log in logs {
        let fields = log
            .fields
            .as_ref()
            .map(|value| format!(" {value}"))
            .unwrap_or_default();
        output.push_str(&format!(
            "[{}] [{}] {}{}\n",
            log.timestamp, log.level, log.message, fields
        ));
    }

    let filename = format!("plugin_logs_{}.txt", Utc::now().format("%Y%m%d_%H%M%S"));
    let headers = [
        (
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8".to_string(),
        ),
        (
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        ),
    ];
    Ok((headers, output).into_response())
}

#[cfg(test)]
#[path = "../../../tests/unit/api/handlers/plugin_logs.rs"]
mod tests;
