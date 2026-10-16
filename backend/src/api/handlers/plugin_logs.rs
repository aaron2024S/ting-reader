use crate::api::require_admin;
use crate::api::state::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::ExternalErrorKind;
use crate::core::app::error::{Result, TingError};
use crate::core::app::logging::{
    LogEntry, LogQueryFilter, normalized_log_filter as normalized_filter,
    normalized_log_pagination as normalized_pagination, read_log_file as parse_plugin_log_file,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path as StdPath;

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
    common: LogQueryFilter,
    source: Option<String>,
}

impl PluginLogFilters {
    fn from_query(plugin_id: Option<String>, query: &PluginLogsQuery) -> Result<Self> {
        let common = LogQueryFilter::new(
            query.level.as_deref(),
            query.q.as_deref(),
            query.since.as_deref(),
            query.until.as_deref(),
        )?;
        let plugin_id = normalized_filter(plugin_id.as_deref());
        Ok(Self {
            plugin_id,
            common,
            source: normalized_filter(query.source.as_deref()),
        })
    }

    fn matches(&self, log: &LogEntry) -> bool {
        self.common.matches(log)
            && self.plugin_matches(log)
            && self.source.as_ref().is_none_or(|source| {
                log_field(log, "source").is_some_and(|value| value.eq_ignore_ascii_case(source))
            })
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
}

fn default_page() -> usize {
    1
}

fn default_page_size() -> usize {
    100
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
    .map_err(|error| {
        let message = error.to_string();
        TingError::external_tool(
            ExternalErrorKind::Response,
            "filtered_plugin_logs::external_tool",
            message,
        )
        .with_source(anyhow::anyhow!(error))
    })
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
