//! Host HTML document parsing and selection.

use super::{PluginHostGateway, required_string_param, usize_param};
use crate::core::app::error::{InputErrorKind, LimitErrorKind, LookupErrorKind, PluginErrorKind};
use crate::core::app::error::{Result, TingError};
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
const MAX_HTML_HANDLES: usize = 1024;
const MAX_HTML_MATCHES: usize = 500;
const MAX_SELECTOR_BYTES: usize = 512;

impl PluginHostGateway {
    pub(super) async fn html_parse(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let html = params.get("html").and_then(Value::as_str).ok_or_else(|| {
            TingError::invalid_request(
                InputErrorKind::MissingField,
                "html_parse::invalid_request",
                "html.parse requires html".into(),
            )
        })?;
        if html.len() > MAX_HTML_BYTES {
            return Err(TingError::resource_limit(
                LimitErrorKind::Payload,
                "html_parse::resource_limit",
                "HTML document exceeds 2 MiB".into(),
            ));
        }
        let document_id = Uuid::new_v4().to_string();
        let key = handle_key(plugin_id, &document_id);
        let mut documents = self.html_documents.write().map_err(|_| {
            TingError::plugin_execution(
                PluginErrorKind::Unavailable,
                "html_parse::plugin_execution",
                "HTML handle store unavailable".into(),
            )
        })?;
        if documents.len() >= MAX_HTML_HANDLES {
            return Err(TingError::resource_limit(
                LimitErrorKind::Handles,
                "html_parse::resource_limit",
                "Too many HTML handles".into(),
            ));
        }
        documents.insert(
            key,
            serde_json::json!({
                "kind": "document",
                "html": html,
            })
            .to_string(),
        );
        Ok(serde_json::json!({
            "document_id": document_id,
            "bytes": html.len(),
        }))
    }

    pub(super) async fn html_select(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let document_id = required_string_param(params, "document_id")?;
        let selector_text = bounded_selector(params)?;
        let html = self.load_document(plugin_id, &document_id)?;
        let document = Html::parse_document(&html);
        let selector = Selector::parse(&selector_text).map_err(|_| {
            TingError::invalid_request(
                InputErrorKind::InvalidValue,
                "html_select::invalid_request",
                "Invalid HTML selector".into(),
            )
        })?;
        let limit = usize_param(params, "limit")
            .unwrap_or(100)
            .clamp(1, MAX_HTML_MATCHES);
        let mut matches = Vec::new();
        let mut documents = self.html_documents.write().map_err(|_| {
            TingError::plugin_execution(
                PluginErrorKind::Unavailable,
                "html_select::plugin_execution",
                "HTML handle store unavailable".into(),
            )
        })?;
        for element in document.select(&selector).take(limit) {
            if documents.len() >= MAX_HTML_HANDLES {
                return Err(TingError::resource_limit(
                    LimitErrorKind::Handles,
                    "html_select::resource_limit",
                    "Too many HTML handles".into(),
                ));
            }
            let selection_id = Uuid::new_v4().to_string();
            let node = node_value(element);
            documents.insert(
                handle_key(plugin_id, &selection_id),
                serde_json::json!({
                    "kind": "selection",
                    "document_id": document_id,
                    "node": node,
                })
                .to_string(),
            );
            matches.push(serde_json::json!({
                "selection_id": selection_id,
                "tag": node["tag"],
                "text": node["text"],
                "attrs": node["attrs"],
            }));
        }
        Ok(serde_json::json!({
            "document_id": document_id,
            "selector": selector_text,
            "matches": matches,
        }))
    }

    pub(super) async fn html_text(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let handle = required_string_param(params, "selection_id")
            .or_else(|_| required_string_param(params, "document_id"))?;
        let stored = self.load_handle(plugin_id, &handle)?;
        let kind = stored
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let text = if kind == "selection" {
            stored
                .pointer("/node/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        } else if kind == "document" {
            let html = stored
                .get("html")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let document = Html::parse_document(html);
            normalized_text(document.root_element().text().collect::<Vec<_>>().join(" "))
        } else {
            return Err(TingError::invalid_request(
                InputErrorKind::UnsupportedValue,
                "html_text::invalid_request",
                "Unknown HTML handle".into(),
            ));
        };
        Ok(serde_json::json!({ "text": text }))
    }

    pub(super) async fn html_attr(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let selection_id = required_string_param(params, "selection_id")?;
        let name = required_string_param(params, "name")?;
        let stored = self.load_handle(plugin_id, &selection_id)?;
        if stored.get("kind").and_then(Value::as_str) != Some("selection") {
            return Err(TingError::invalid_request(
                InputErrorKind::MissingField,
                "html_attr::invalid_request",
                "html.attr requires a selection_id".into(),
            ));
        }
        Ok(serde_json::json!({
            "name": name,
            "value": stored.pointer("/node/attrs")
                .and_then(|attrs| attrs.get(&name))
                .cloned()
                .unwrap_or(Value::Null),
        }))
    }

    pub(super) async fn html_extract(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let document_id = required_string_param(params, "document_id")?;
        let selector_text = bounded_selector(params)?;
        let html = self.load_document(plugin_id, &document_id)?;
        let document = Html::parse_document(&html);
        let selector = Selector::parse(&selector_text).map_err(|_| {
            TingError::invalid_request(
                InputErrorKind::InvalidValue,
                "html_extract::invalid_request",
                "Invalid HTML selector".into(),
            )
        })?;
        let fields = params
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                TingError::invalid_request(
                    InputErrorKind::MissingField,
                    "html_extract::invalid_request",
                    "html.extract requires fields".into(),
                )
            })?;
        if fields.len() > 32 {
            return Err(TingError::resource_limit(
                LimitErrorKind::Payload,
                "html_extract::resource_limit",
                "HTML extraction supports at most 32 fields".into(),
            ));
        }
        let limit = usize_param(params, "limit")
            .unwrap_or(100)
            .clamp(1, MAX_HTML_MATCHES);
        let mut rows = Vec::new();
        for element in document.select(&selector).take(limit) {
            let mut row = serde_json::Map::new();
            for field in fields {
                let name = field
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .ok_or_else(|| {
                        TingError::invalid_request(
                            InputErrorKind::MissingField,
                            "html_extract::invalid_request",
                            "HTML extraction field name is required".into(),
                        )
                    })?;
                let value = if let Some(attribute) = field.get("attr").and_then(Value::as_str) {
                    element
                        .value()
                        .attr(attribute)
                        .map(|value| Value::String(value.to_string()))
                        .unwrap_or(Value::Null)
                } else if let Some(field_selector) = field.get("selector").and_then(Value::as_str) {
                    let field_selector = Selector::parse(field_selector).map_err(|_| {
                        TingError::invalid_request(
                            InputErrorKind::InvalidValue,
                            "html_extract::invalid_request",
                            "Invalid HTML field selector".into(),
                        )
                    })?;
                    element
                        .select(&field_selector)
                        .next()
                        .map(|child| {
                            Value::String(normalized_text(
                                child.text().collect::<Vec<_>>().join(" "),
                            ))
                        })
                        .unwrap_or(Value::Null)
                } else {
                    Value::String(normalized_text(
                        element.text().collect::<Vec<_>>().join(" "),
                    ))
                };
                row.insert(name.to_string(), value);
            }
            rows.push(Value::Object(row));
        }
        Ok(serde_json::json!({
            "document_id": document_id,
            "selector": selector_text,
            "items": rows,
        }))
    }

    pub(super) async fn html_close(&self, plugin_id: &str, params: &Value) -> Result<Value> {
        let document_id = required_string_param(params, "document_id")?;
        let prefix = handle_key(plugin_id, &document_id);
        let mut documents = self.html_documents.write().map_err(|_| {
            TingError::plugin_execution(
                PluginErrorKind::Unavailable,
                "html_close::plugin_execution",
                "HTML handle store unavailable".into(),
            )
        })?;
        let before = documents.len();
        documents.retain(|key, value| {
            if key == &prefix {
                return false;
            }
            !value.contains(&format!("\"document_id\":\"{}\"", document_id))
        });
        Ok(serde_json::json!({
            "document_id": document_id,
            "closed": documents.len() != before,
        }))
    }

    fn load_document(&self, plugin_id: &str, document_id: &str) -> Result<String> {
        let value = self.load_handle(plugin_id, document_id)?;
        if value.get("kind").and_then(Value::as_str) != Some("document") {
            return Err(TingError::invalid_request(
                InputErrorKind::MissingField,
                "load_document::invalid_request",
                "HTML operation requires a document handle".into(),
            ));
        }
        value
            .get("html")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                TingError::invalid_request(
                    InputErrorKind::InvalidValue,
                    "load_document::invalid_request",
                    "HTML document is unavailable".into(),
                )
            })
    }

    fn load_handle(&self, plugin_id: &str, handle: &str) -> Result<Value> {
        let key = handle_key(plugin_id, handle);
        let documents = self.html_documents.read().map_err(|_| {
            TingError::plugin_execution(
                PluginErrorKind::Unavailable,
                "load_handle::plugin_execution",
                "HTML handle store unavailable".into(),
            )
        })?;
        let raw = documents.get(&key).ok_or_else(|| {
            TingError::not_found(
                LookupErrorKind::Resource,
                "load_handle::not_found",
                "HTML handle does not exist".into(),
            )
        })?;
        serde_json::from_str(raw).map_err(|_| {
            TingError::plugin_execution(
                PluginErrorKind::Runtime,
                "load_handle::plugin_execution",
                "HTML handle is corrupted".into(),
            )
        })
    }
}

fn bounded_selector(params: &Value) -> Result<String> {
    let selector = required_string_param(params, "selector")?;
    if selector.len() > MAX_SELECTOR_BYTES {
        return Err(TingError::resource_limit(
            LimitErrorKind::Payload,
            "bounded_selector::resource_limit",
            "HTML selector exceeds 512 bytes".into(),
        ));
    }
    Ok(selector)
}

fn handle_key(plugin_id: &str, handle: &str) -> String {
    format!("{plugin_id}:{handle}")
}

fn node_value(element: ElementRef<'_>) -> Value {
    let attrs: BTreeMap<String, String> = element
        .value()
        .attrs()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    serde_json::json!({
        "tag": element.value().name(),
        "text": normalized_text(element.text().collect::<Vec<_>>().join(" ")),
        "attrs": attrs,
    })
}

fn normalized_text(text: String) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
