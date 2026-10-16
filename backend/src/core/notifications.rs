use crate::core::app::error::{
    CodecErrorKind, ExternalErrorKind, InputErrorKind, NetworkErrorKind,
};
use crate::core::app::error::{ErrorResponse, Result, TingError};
use crate::db::models::NotificationWebhook;
use crate::db::repository::NotificationWebhookRepository;
use crate::plugin::manager::PluginManager;
use lazy_static::lazy_static;
use regex::{Captures, Regex};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_BODY_TEMPLATE: &str = "{{json:payload}}";
const MAX_RESPONSE_BODY_CHARS: usize = 4096;

lazy_static! {
    static ref TEMPLATE_VARIABLE_RE: Regex =
        Regex::new(r"\{\{\s*(?:(json)\s*:)?\s*([A-Za-z0-9_.]+)\s*\}\}")
            .expect("valid webhook template regex");
}

#[derive(Debug, Clone, Serialize)]
pub struct NotificationEventPayload {
    pub event: String,
    pub title: String,
    pub message: String,
    pub data: Value,
    pub occurred_at: String,
}

impl NotificationEventPayload {
    pub fn new(
        event: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
        data: Value,
    ) -> Self {
        Self {
            event: event.into(),
            title: title.into(),
            message: message.into(),
            data,
            occurred_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    pub fn test_payload() -> Self {
        Self::new(
            "webhook.test",
            "听悦测试通知",
            "如果你看到这条消息，说明 Webhook 配置正常。",
            serde_json::json!({
                "username": "admin",
                "book_title": "示例有声书",
                "chapter_title": "第一章"
            }),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WebhookDeliveryResult {
    pub success: bool,
    pub status: u16,
    pub response_body: String,
    pub rendered_body: String,
    pub error: Option<ErrorResponse>,
}

pub fn default_body_template() -> String {
    DEFAULT_BODY_TEMPLATE.to_string()
}

pub fn parse_headers(raw: &str) -> Result<HashMap<String, String>> {
    serde_json::from_str(raw).map_err(|error| {
        let message = format!("Invalid webhook headers: {error}");
        TingError::deserialization(
            CodecErrorKind::InvalidData,
            "parse_headers::deserialization",
            message,
        )
        .with_source(anyhow::anyhow!(error))
    })
}

pub fn validate_headers(headers: &HashMap<String, String>) -> Result<()> {
    for (name, value) in headers {
        HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            TingError::validation(
                InputErrorKind::InvalidValue,
                "validate_headers::validation",
                format!("Invalid webhook header name: {name}"),
            )
        })?;
        HeaderValue::from_str(value).map_err(|_| {
            TingError::validation(
                InputErrorKind::InvalidValue,
                "validate_headers::validation",
                format!("Invalid value for webhook header: {name}"),
            )
        })?;
    }
    Ok(())
}

pub fn render_template(template: &str, payload: &NotificationEventPayload) -> Result<String> {
    let mut render_error = None;
    let rendered = TEMPLATE_VARIABLE_RE.replace_all(template, |captures: &Captures<'_>| {
        let json_encoded = captures.get(1).is_some();
        let variable = captures
            .get(2)
            .map(|value| value.as_str())
            .unwrap_or_default();

        match resolve_template_variable(variable, payload) {
            Some(value) => {
                if json_encoded {
                    serde_json::to_string(&value).unwrap_or_else(|error| {
                        render_error = Some(format!(
                            "Failed to encode template variable {variable}: {error}"
                        ));
                        String::new()
                    })
                } else {
                    raw_template_value(&value)
                }
            }
            None => {
                render_error = Some(format!("Unknown webhook template variable: {variable}"));
                String::new()
            }
        }
    });

    if let Some(error) = render_error {
        return Err(TingError::validation(
            InputErrorKind::InvalidValue,
            "render_template::validation",
            error,
        ));
    }

    Ok(rendered.into_owned())
}

pub fn validate_template(template: &str) -> Result<String> {
    let template = template.trim();
    if template.is_empty() {
        return Err(TingError::validation(
            InputErrorKind::InvalidValue,
            "validate_template::validation",
            "Webhook body template cannot be empty".to_string(),
        ));
    }

    render_template(template, &NotificationEventPayload::test_payload())?;
    Ok(template.to_string())
}

pub fn dispatch_notification_event(
    repo: Arc<NotificationWebhookRepository>,
    payload: NotificationEventPayload,
) {
    let event = payload.event.clone();
    tokio::spawn(async move {
        if let Err(error) = dispatch_notification_event_inner(repo, payload).await {
            tracing::warn!(
                target: "audit::notification",
                message_key = "notification.webhook.dispatch_failed",
                message_params = %serde_json::json!({
                    "event": event.as_str(),
                    "error": error.to_string(),
                }),
                event = %event,
                error = %error,
                "Webhook notification dispatch failed"
            );
        }
    });
}

pub fn dispatch_application_event(
    repo: Arc<NotificationWebhookRepository>,
    plugin_manager: Arc<PluginManager>,
    payload: NotificationEventPayload,
) {
    dispatch_notification_event(repo, payload.clone());
    dispatch_plugin_event_handlers(plugin_manager, payload);
}

pub fn dispatch_plugin_event_handlers(
    plugin_manager: Arc<PluginManager>,
    payload: NotificationEventPayload,
) {
    let event = payload.event.clone();
    tokio::spawn(async move {
        let handlers = plugin_manager.find_event_handlers(Some(&event)).await;
        for handler in handlers {
            let params = serde_json::json!({
                "event": payload.event,
                "title": payload.title,
                "message": payload.message,
                "data": payload.data,
                "occurred_at": payload.occurred_at,
                "capability_id": handler.registration.capability.id(),
                "plugin_id": handler.registration.plugin_id,
            });

            if let Err(error) = plugin_manager
                .invoke_capability(
                    &handler.registration.plugin_id,
                    handler.registration.capability.id(),
                    "handle",
                    params,
                    &crate::plugin::types::PluginInvocationContext::default(),
                )
                .await
            {
                tracing::warn!(
                    target: "audit::plugin",
                    message_key = "plugin.event_handler.failed",
                    message_params = %serde_json::json!({
                        "event": event.as_str(),
                        "plugin_id": handler.registration.plugin_id.as_str(),
                        "capability_id": handler.registration.capability.id(),
                        "error": error.to_string(),
                    }),
                    event = %event,
                    plugin_id = %handler.registration.plugin_id,
                    capability_id = %handler.registration.capability.id(),
                    error = %error,
                    "Plugin event handler failed"
                );
            }
        }
    });
}

async fn dispatch_notification_event_inner(
    repo: Arc<NotificationWebhookRepository>,
    payload: NotificationEventPayload,
) -> Result<()> {
    let webhooks = repo.find_enabled_for_event(&payload.event).await?;
    if webhooks.is_empty() {
        return Ok(());
    }

    let client = build_client()?;

    for webhook in webhooks {
        match deliver_webhook_with_client(&client, &webhook, &payload).await {
            Ok(result) if result.success => {
                tracing::info!(
                    target: "audit::notification",
                    message_key = "notification.webhook.sent",
                    message_params = %serde_json::json!({
                        "webhook_id": webhook.id.as_str(),
                        "webhook_name": webhook.name.as_str(),
                        "event": payload.event.as_str(),
                        "status": result.status,
                    }),
                    webhook_id = %webhook.id,
                    webhook_name = %webhook.name,
                    event = %payload.event,
                    status = result.status,
                    "Webhook notification sent"
                );
            }
            Ok(result) => {
                let error_text = result
                    .error
                    .as_ref()
                    .map(|error| error.message.as_str())
                    .unwrap_or("Unknown webhook error");
                tracing::warn!(
                    target: "audit::notification",
                    message_key = "notification.webhook.status_failed",
                    message_params = %serde_json::json!({
                        "webhook_id": webhook.id.as_str(),
                        "webhook_name": webhook.name.as_str(),
                        "event": payload.event.as_str(),
                        "status": result.status,
                        "error": error_text,
                    }),
                    webhook_id = %webhook.id,
                    webhook_name = %webhook.name,
                    event = %payload.event,
                    status = result.status,
                    error = error_text,
                    failure = ?result.error,
                    "Webhook notification returned failure"
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "audit::notification",
                    message_key = "notification.webhook.request_failed",
                    message_params = %serde_json::json!({
                        "webhook_id": webhook.id.as_str(),
                        "webhook_name": webhook.name.as_str(),
                        "event": payload.event.as_str(),
                        "error": error.to_string(),
                    }),
                    webhook_id = %webhook.id,
                    webhook_name = %webhook.name,
                    event = %payload.event,
                    error = %error,
                    "Webhook notification request failed"
                );
            }
        }
    }

    Ok(())
}

pub async fn deliver_webhook(
    webhook: &NotificationWebhook,
    payload: &NotificationEventPayload,
) -> Result<WebhookDeliveryResult> {
    let client = build_client()?;
    deliver_webhook_with_client(&client, webhook, payload).await
}

async fn deliver_webhook_with_client(
    client: &reqwest::Client,
    webhook: &NotificationWebhook,
    payload: &NotificationEventPayload,
) -> Result<WebhookDeliveryResult> {
    let rendered_body = render_template(&webhook.body_template, payload)?;
    let configured_headers = parse_headers(&webhook.headers)?;
    validate_headers(&configured_headers)?;

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HeaderName::from_static("x-ting-event"),
        HeaderValue::from_str(&payload.event).map_err(|_| {
            TingError::validation(
                InputErrorKind::InvalidValue,
                "deliver_webhook_with_client::validation",
                "Webhook event cannot be used as a header".to_string(),
            )
        })?,
    );

    if let Some(secret) = webhook.secret.as_deref().filter(|value| !value.is_empty()) {
        headers.insert(
            HeaderName::from_static("x-ting-webhook-secret"),
            HeaderValue::from_str(secret).map_err(|_| {
                TingError::validation(
                    InputErrorKind::InvalidValue,
                    "deliver_webhook_with_client::validation",
                    "Webhook secret contains invalid characters".to_string(),
                )
            })?,
        );
    }

    for (name, value_template) in configured_headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            TingError::validation(
                InputErrorKind::InvalidValue,
                "deliver_webhook_with_client::validation",
                format!("Invalid webhook header name: {name}"),
            )
        })?;
        let rendered_value = render_template(&value_template, payload)?;
        let value = HeaderValue::from_str(&rendered_value).map_err(|_| {
            TingError::validation(
                InputErrorKind::InvalidValue,
                "deliver_webhook_with_client::validation",
                format!("Invalid value for webhook header: {name}"),
            )
        })?;
        headers.insert(name, value);
    }

    let response = client
        .post(&webhook.url)
        .headers(headers)
        .body(rendered_body.clone())
        .send()
        .await
        .map_err(|error| {
            let message = error.to_string();
            TingError::network(
                NetworkErrorKind::Transport,
                "deliver_webhook_with_client::network",
                message,
            )
            .with_source(anyhow::anyhow!(error))
        })?;

    let status = response.status();
    let response_body = response.text().await.map_err(|error| {
        let message = error.to_string();
        TingError::network(
            NetworkErrorKind::Transport,
            "deliver_webhook_with_client::network",
            message,
        )
        .with_source(anyhow::anyhow!(error))
    })?;
    let response_body = truncate_chars(&response_body, MAX_RESPONSE_BODY_CHARS);

    let error = webhook_failure(status, &response_body);
    let success = error.is_none();

    Ok(WebhookDeliveryResult {
        success,
        status: status.as_u16(),
        response_body,
        rendered_body,
        error,
    })
}

fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| {
            let message = error.to_string();
            TingError::external_tool(
                ExternalErrorKind::Response,
                "build_client::external_tool",
                message,
            )
            .with_source(anyhow::anyhow!(error))
        })
}

fn resolve_template_variable(variable: &str, payload: &NotificationEventPayload) -> Option<Value> {
    if variable == "notification" {
        return Some(Value::String(format!(
            "{}\n{}",
            payload.title, payload.message
        )));
    }

    let payload_value = serde_json::to_value(payload).ok()?;
    if variable == "payload" {
        return Some(payload_value);
    }

    let mut current = &payload_value;
    for segment in variable.split('.') {
        current = current.get(segment)?;
    }
    Some(current.clone())
}

fn raw_template_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message} (errcode: {code})")]
struct WebhookBusinessError {
    code: i64,
    message: String,
}

fn detect_business_error(response_body: &str) -> Option<WebhookBusinessError> {
    let value = serde_json::from_str::<Value>(response_body).ok()?;
    let errcode = value.get("errcode")?.as_i64()?;
    if errcode == 0 {
        return None;
    }

    let message = value
        .get("errmsg")
        .and_then(Value::as_str)
        .unwrap_or("Webhook service returned an error");
    Some(WebhookBusinessError {
        code: errcode,
        message: message.into(),
    })
}

fn webhook_failure(status: reqwest::StatusCode, response_body: &str) -> Option<ErrorResponse> {
    if !status.is_success() {
        let error = TingError::network(
            NetworkErrorKind::HttpStatus,
            "webhook.deliver",
            format!("HTTP {}", status.as_u16()),
        );
        return Some(ErrorResponse::from_error_with_details(
            &error,
            serde_json::json!({ "upstream_status": status.as_u16() }),
        ));
    }
    detect_business_error(response_body).map(|source| {
        let code = source.code;
        let error = TingError::external_service(
            ExternalErrorKind::Response,
            "webhook.deliver",
            source.to_string(),
        )
        .with_source(source.into());
        ErrorResponse::from_error_with_details(
            &error,
            serde_json::json!({ "upstream_status": status.as_u16(), "upstream_code": code }),
        )
    })
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
#[path = "../../tests/unit/core/notifications.rs"]
mod tests;
