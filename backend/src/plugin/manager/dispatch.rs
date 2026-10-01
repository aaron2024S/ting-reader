use crate::plugin::schema::validate_payload;
use serde_json::Value;
use std::time::{Duration, Instant};

use super::PluginManager;
use crate::core::error::{Result, TingError};
use crate::plugin::logger::{PluginLogLevel, emit_plugin_event};
use crate::plugin::types::{
    PluginCapability, PluginId, PluginInvocationContext, PluginLogSource, PluginState,
};
use ting_plugin_contract::capability::MetadataOperation;
use ting_plugin_contract::format_calls::{FormatCall, FormatOutput};
use ting_plugin_contract::protocol::CallResult;
use ting_plugin_contract::scraper::{
    ChapterDetail, ChapterId, ChapterListRequest, ChapterPage, CoverAssetRef, FetchCoverRequest,
    MediaSourceDescriptor, ScraperResult, SearchMode, SearchPage, SearchRequest, SourceRef,
};

impl PluginManager {
    /// Core-only scope creation. Registry generation and authenticated identity
    /// are captured here, never accepted from plugin control JSON.
    pub async fn resource_scope(
        &self,
        id: &PluginId,
        context: &PluginInvocationContext,
        staging_dir: std::path::PathBuf,
        limits: crate::plugin::resources::ResourceLimits,
    ) -> Result<std::sync::Arc<crate::plugin::resources::ResourceScope>> {
        let registry = self.registry.read().await;
        let entry = registry
            .get(id)
            .ok_or_else(|| TingError::PluginNotFound(id.clone()))?;
        if !matches!(entry.state, PluginState::Active | PluginState::Loaded) {
            return Err(TingError::PluginExecutionError(
                "Plugin is unavailable".into(),
            ));
        }
        let scope = std::sync::Arc::new(crate::plugin::resources::ResourceScope::new(
            id.clone(),
            entry.generation,
            context.user.as_ref().map(|user| user.id.clone()),
            staging_dir,
            limits,
        ));
        entry.track_resource_scope(&scope)?;
        Ok(scope)
    }

    /// Validate the target declaration before reaching a runtime adapter.
    /// Callers supply the capability ID; bare method and tool names are not
    /// sufficient to authorize an invocation.
    pub async fn invoke_capability(
        &self,
        id: &PluginId,
        capability_id: &str,
        operation: &str,
        params: Value,
        context: &PluginInvocationContext,
    ) -> Result<Value> {
        let capability = {
            let registry = self.registry.read().await;
            let entry = registry
                .get(id)
                .ok_or_else(|| TingError::PluginNotFound(id.clone()))?;
            if let Some(scope) = &context.resources {
                let principal = context.user.as_ref().map(|user| user.id.as_str());
                let authorization = if matches!(operation, "close" | "cancel") {
                    scope.authorize_owner(id, entry.generation, principal)
                } else {
                    scope.authorize(id, entry.generation, principal)
                };
                authorization
                    .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
            }
            if matches!(entry.state, PluginState::Failed | PluginState::Unloaded) {
                return Err(TingError::PluginExecutionError(
                    "Plugin is unavailable".into(),
                ));
            }
            entry
                .metadata
                .capabilities
                .iter()
                .find(|cap| cap.id() == capability_id)
                .cloned()
                .ok_or_else(|| {
                    TingError::PluginExecutionError(format!(
                        "Undeclared capability: {capability_id}"
                    ))
                })?
        };
        if !capability.supports(operation) {
            return Err(TingError::PluginExecutionError(format!(
                "Capability {capability_id} does not declare operation {operation}"
            )));
        }
        let output_schema = validate_capability_input(&capability, operation, &params)?;
        let request = params.clone();
        let mut invocation = context.clone();
        if invocation.resources.is_none() {
            invocation.resources = Some(
                self.resource_scope(
                    id,
                    context,
                    self.config.plugin_dir.join("staging"),
                    crate::plugin::resources::ResourceLimits::default(),
                )
                .await?,
            );
        }
        let raw = self
            .invoke_plugin(id, operation, params, &invocation)
            .await?;
        let output = match serde_json::from_value::<CallResult<Value>>(raw)
            .map_err(|_| TingError::PluginExecutionError("Invalid plugin result envelope".into()))?
        {
            CallResult::Success(success) => success.data,
            CallResult::Failure(failure) => {
                failure.error.validate().map_err(|_| {
                    TingError::PluginExecutionError("Invalid plugin error envelope".into())
                })?;
                return Err(TingError::PluginExecutionError(format!(
                    "Plugin reported {:?}: {}",
                    failure.error.code, failure.error.message
                )));
            }
        };
        if let Some(schema) = output_schema {
            validate_payload(&schema, &output, "output")?;
        }
        validate_capability_output(&capability, operation, &request, &output)?;
        Ok(output)
    }

    /// Invoke one validated operation through the runtime adapter trait.
    async fn invoke_plugin(
        &self,
        id: &PluginId,
        method: &str,
        params: Value,
        context: &PluginInvocationContext,
    ) -> Result<Value> {
        let instance = {
            let registry = self.registry.read().await;
            let entry = registry
                .get(id)
                .ok_or_else(|| TingError::PluginNotFound(id.clone()))?;
            entry.instance.clone()
        };

        let started_at = Instant::now();
        let result = instance.invoke(method, params, context).await;

        self.record_plugin_call(
            id,
            method,
            started_at.elapsed(),
            result.as_ref().err(),
            PluginLogLevel::Debug,
        )
        .await;
        result.map_err(TingError::mark_plugin_execution_logged)
    }

    /// Invoke a declared metadata operation using the shared contract.
    pub async fn invoke_metadata(
        &self,
        id: &PluginId,
        operation: MetadataOperation,
        params: Value,
    ) -> Result<Value> {
        let (capability_id, method_name) = {
            let registry = self.registry.read().await;
            let entry = registry
                .get(id)
                .ok_or_else(|| TingError::PluginNotFound(id.clone()))?;
            let metadata_provider = entry
                .metadata
                .effective_capabilities()
                .into_iter()
                .find(|capability| matches!(capability, PluginCapability::MetadataProvider(_)))
                .ok_or_else(|| {
                    TingError::PluginExecutionError(format!(
                        "Plugin {} does not declare metadata_provider capability",
                        id
                    ))
                })?;
            let method_name = operation.as_str();
            if !metadata_provider.supports(method_name) {
                return Err(TingError::PluginExecutionError(format!(
                    "Capability {} does not declare operation {}",
                    metadata_provider.id(),
                    method_name
                )));
            }
            (metadata_provider.id().to_owned(), method_name)
        };
        self.invoke_capability(
            id,
            &capability_id,
            method_name,
            params,
            &PluginInvocationContext::default(),
        )
        .await
    }

    pub(crate) async fn record_plugin_call(
        &self,
        id: &PluginId,
        method: &str,
        duration: Duration,
        error: Option<&TingError>,
        success_level: PluginLogLevel,
    ) {
        let mut registry = self.registry.write().await;
        let Some(entry) = registry.get_mut(id) else {
            return;
        };
        let metadata = entry.metadata.clone();
        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);

        match error {
            Some(error) => {
                let error_type = error.to_string();
                entry.stats.record_failure(Some(error_type.as_str()));
            }
            None => {
                entry.stats.record_success(duration_ms);
            }
        }
        drop(registry);

        let failed = error.is_some();
        let error_message = error.map(ToString::to_string);
        let fields = serde_json::json!({
            "op": "plugin.invoke",
            "method": method,
            "duration_ms": duration_ms,
            "status": if failed { "error" } else { "success" },
            "error": error_message,
        });
        emit_plugin_event(
            &metadata,
            PluginLogSource::Runtime,
            if failed {
                PluginLogLevel::Error
            } else {
                success_level
            },
            if failed {
                "Plugin invocation failed"
            } else {
                "Plugin invocation completed"
            },
            Some(&fields),
        );
    }
}

fn validate_capability_input(
    capability: &PluginCapability,
    operation: &str,
    params: &Value,
) -> Result<Option<Value>> {
    let field = |name: &str| {
        params.get(name).ok_or_else(|| {
            TingError::PluginExecutionError(format!("Missing capability input field: {name}"))
        })
    };
    match capability {
        PluginCapability::MetadataProvider(cap) => {
            match operation {
                "search" => {
                    let request: SearchRequest =
                        serde_json::from_value(params.clone()).map_err(|error| {
                            TingError::PluginExecutionError(format!(
                                "Invalid search request: {error}"
                            ))
                        })?;
                    request.validate().map_err(|error| {
                        TingError::PluginExecutionError(format!("Invalid search request: {error}"))
                    })?;
                    if request.context.as_ref().is_some_and(|context| {
                        !matches!(context.mode, SearchMode::Search) && !cap.aggregate_auto_scrape
                    }) {
                        return Err(TingError::PluginExecutionError(
                            "Aggregate search context requires aggregate_auto_scrape".into(),
                        ));
                    }
                    for field in &cap.search_fields {
                        if !field.required {
                            continue;
                        }
                        let present = match field.key.as_str() {
                            "title" => request
                                .title
                                .as_deref()
                                .is_some_and(|text| !text.trim().is_empty()),
                            "author" => request
                                .author
                                .as_deref()
                                .is_some_and(|text| !text.trim().is_empty()),
                            "narrator" => request
                                .narrator
                                .as_deref()
                                .is_some_and(|text| !text.trim().is_empty()),
                            key => request.filters.get(key).is_some_and(|value| {
                                !value.is_null()
                                    && value.as_str().is_none_or(|text| !text.trim().is_empty())
                            }),
                        };
                        if !present {
                            return Err(TingError::PluginExecutionError(format!(
                                "Required search field {} is empty",
                                field.key
                            )));
                        }
                    }
                    if request
                        .title
                        .as_deref()
                        .is_some_and(|text| text.trim().is_empty())
                        || request
                            .author
                            .as_deref()
                            .is_some_and(|text| text.trim().is_empty())
                        || request
                            .narrator
                            .as_deref()
                            .is_some_and(|text| text.trim().is_empty())
                    {
                        return Err(TingError::PluginExecutionError(
                            "Empty search fields must use null".into(),
                        ));
                    }
                    if let Some(schema) = &cap.filters_schema {
                        validate_payload(
                            schema,
                            &serde_json::Value::Object(request.filters),
                            "search filters",
                        )?;
                    } else if !request.filters.is_empty() {
                        return Err(TingError::PluginExecutionError(
                            "Search filters are not declared by this capability".into(),
                        ));
                    }
                }
                "detail" => {
                    let reference: SourceRef =
                        serde_json::from_value(params.clone()).map_err(|error| {
                            TingError::PluginExecutionError(format!(
                                "Invalid detail reference: {error}"
                            ))
                        })?;
                    reference.validate().map_err(|error| {
                        TingError::PluginExecutionError(format!(
                            "Invalid detail reference: {error}"
                        ))
                    })?;
                }
                "list_chapters" => {
                    let page: ChapterListRequest =
                        serde_json::from_value(params.clone()).map_err(|error| {
                            TingError::PluginExecutionError(format!(
                                "Invalid chapter list request: {error}"
                            ))
                        })?;
                    page.validate().map_err(|error| {
                        TingError::PluginExecutionError(format!(
                            "Invalid chapter list request: {error}"
                        ))
                    })?;
                }
                "chapter_detail" | "resolve_audio" => {
                    let chapter: ChapterId =
                        serde_json::from_value(params.clone()).map_err(|error| {
                            TingError::PluginExecutionError(format!(
                                "Invalid chapter request: {error}"
                            ))
                        })?;
                    chapter.validate().map_err(|error| {
                        TingError::PluginExecutionError(format!("Invalid chapter request: {error}"))
                    })?;
                }
                "fetch_cover" => {
                    let request: FetchCoverRequest = serde_json::from_value(params.clone())
                        .map_err(|error| {
                            TingError::PluginExecutionError(format!(
                                "Invalid cover request: {error}"
                            ))
                        })?;
                    request.source.validate().map_err(|error| {
                        TingError::PluginExecutionError(format!("Invalid cover request: {error}"))
                    })?;
                }
                _ => {}
            }
            Ok(None)
        }
        PluginCapability::FormatHandler(cap) => {
            let call: FormatCall = serde_json::from_value(serde_json::json!({
                "operation": operation, "input": params
            }))
            .map_err(|error| {
                TingError::PluginExecutionError(format!("Invalid format call: {error}"))
            })?;
            call.validate(cap).map_err(|error| {
                TingError::PluginExecutionError(format!("Invalid format call: {error}"))
            })?;
            Ok(None)
        }
        PluginCapability::ToolProvider(cap) => {
            let name = field("tool_name")?.as_str().ok_or_else(|| {
                TingError::PluginExecutionError("tool_name must be a string".into())
            })?;
            let tool = cap
                .tools
                .iter()
                .find(|tool| tool.name == name)
                .ok_or_else(|| {
                    TingError::PluginExecutionError(
                        "Tool is not declared by this capability".into(),
                    )
                })?;
            validate_payload(&tool.input_schema, field("params")?, "tool input")?;
            Ok(Some(tool.output_schema.clone()))
        }
        PluginCapability::TaskHandler(cap) => {
            let name = field("task_type")?.as_str().ok_or_else(|| {
                TingError::PluginExecutionError("task_type must be a string".into())
            })?;
            let task = cap
                .tasks
                .iter()
                .find(|task| task.task_type == name)
                .ok_or_else(|| {
                    TingError::PluginExecutionError(
                        "Task type is not declared by this capability".into(),
                    )
                })?;
            validate_payload(&task.input_schema, field("data")?, "task input")?;
            Ok(Some(task.output_schema.clone()))
        }
        PluginCapability::EventHandler(cap) => {
            let name = field("event")?
                .as_str()
                .ok_or_else(|| TingError::PluginExecutionError("event must be a string".into()))?;
            let event = cap
                .events
                .iter()
                .find(|event| event.name == name)
                .ok_or_else(|| {
                    TingError::PluginExecutionError(
                        "Event is not declared by this capability".into(),
                    )
                })?;
            validate_payload(&event.schema, field("data")?, "event input")?;
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn validate_capability_output(
    capability: &PluginCapability,
    operation: &str,
    request: &Value,
    output: &Value,
) -> Result<()> {
    if let PluginCapability::MetadataProvider(_) = capability {
        match operation {
            "search" => {
                let page: SearchPage = serde_json::from_value(output.clone()).map_err(|error| {
                    TingError::PluginExecutionError(format!("Invalid search page: {error}"))
                })?;
                page.validate().map_err(|error| {
                    TingError::PluginExecutionError(format!("Invalid search page: {error}"))
                })?;
                let requested: SearchRequest =
                    serde_json::from_value(request.clone()).map_err(|error| {
                        TingError::PluginExecutionError(format!("Invalid search request: {error}"))
                    })?;
                if page.page != requested.page {
                    return Err(TingError::PluginExecutionError(
                        "Search page must match the requested page".into(),
                    ));
                }
                if !requested.chapter_candidates.is_empty()
                    && page.items.iter().any(|item| {
                        !item.chapter_titles.is_empty()
                            && item.chapter_titles.len() != requested.chapter_candidates.len()
                    })
                {
                    return Err(TingError::PluginExecutionError(
                        "Cleaned chapter titles must match the supplied candidates".into(),
                    ));
                }
            }
            "detail" => {
                let result: ScraperResult =
                    serde_json::from_value(output.clone()).map_err(|error| {
                        TingError::PluginExecutionError(format!("Invalid detail result: {error}"))
                    })?;
                result.validate().map_err(|error| {
                    TingError::PluginExecutionError(format!("Invalid detail result: {error}"))
                })?;
            }
            "list_chapters" => {
                let page: ChapterPage =
                    serde_json::from_value(output.clone()).map_err(|error| {
                        TingError::PluginExecutionError(format!("Invalid chapter page: {error}"))
                    })?;
                page.validate().map_err(|error| {
                    TingError::PluginExecutionError(format!("Invalid chapter page: {error}"))
                })?;
                let requested: ChapterListRequest = serde_json::from_value(request.clone())
                    .map_err(|error| {
                        TingError::PluginExecutionError(format!(
                            "Invalid chapter list request: {error}"
                        ))
                    })?;
                if page.page != requested.page {
                    return Err(TingError::PluginExecutionError(
                        "Chapter page must match the requested page".into(),
                    ));
                }
            }
            "chapter_detail" => {
                let detail: ChapterDetail =
                    serde_json::from_value(output.clone()).map_err(|error| {
                        TingError::PluginExecutionError(format!("Invalid chapter detail: {error}"))
                    })?;
                detail.validate().map_err(|error| {
                    TingError::PluginExecutionError(format!("Invalid chapter detail: {error}"))
                })?;
            }
            "resolve_audio" => {
                let source: MediaSourceDescriptor = serde_json::from_value(output.clone())
                    .map_err(|error| {
                        TingError::PluginExecutionError(format!("Invalid media source: {error}"))
                    })?;
                source.validate().map_err(|error| {
                    TingError::PluginExecutionError(format!("Invalid media source: {error}"))
                })?;
            }
            "fetch_cover" => {
                let cover: CoverAssetRef =
                    serde_json::from_value(output.clone()).map_err(|error| {
                        TingError::PluginExecutionError(format!("Invalid cover resource: {error}"))
                    })?;
                cover.validate().map_err(|error| {
                    TingError::PluginExecutionError(format!("Invalid cover resource: {error}"))
                })?;
            }
            _ => {}
        }
    }
    if let PluginCapability::FormatHandler(_) = capability {
        let call: FormatCall = serde_json::from_value(serde_json::json!({
            "operation": operation, "input": request
        }))
        .map_err(|error| {
            TingError::PluginExecutionError(format!("Invalid format request: {error}"))
        })?;
        let response: FormatOutput = serde_json::from_value(serde_json::json!({
            "operation": operation,
            "data": output,
        }))
        .map_err(|error| {
            TingError::PluginExecutionError(format!("Invalid format response: {error}"))
        })?;
        response.validate_for(&call).map_err(|error| {
            TingError::PluginExecutionError(format!("Invalid format response: {error}"))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manager::{FailedPlugin, PluginConfig, PluginEntry};
    use crate::plugin::types::{Plugin, PluginMetadata};
    use std::sync::Arc;

    #[test]
    fn metadata_search_requires_the_formal_request_and_result() {
        let capability: PluginCapability = serde_json::from_value(serde_json::json!({
            "kind": "metadata_provider", "id": "metadata.search", "operations": ["search"],
            "auto_scrape": false, "search_fields": [{
                "key": "title", "label": {"en": "Title"}, "required": false, "type": "text"
            }], "result_fields": [{"key": "title", "label": {"en": "Title"}}]
        }))
        .unwrap();
        let request = serde_json::json!({
            "title": "Book", "author": null, "narrator": null,
            "page": 1, "page_size": 20, "filters": {},
            "chapter_candidates": [], "context": null
        });
        assert!(validate_capability_input(&capability, "search", &request).is_ok());
        assert!(
            validate_capability_input(
                &capability,
                "search",
                &serde_json::json!({
                    "query": "Book", "page": 1, "page_size": 20, "filters": {}
                })
            )
            .is_err()
        );
        let mut output = serde_json::to_value(ting_plugin_contract::scraper::SearchPage {
            items: vec![ting_plugin_contract::scraper::ScraperResult {
                id: None,
                source_url: None,
                title: "Book".into(),
                author: None,
                narrator: None,
                cover_url: None,
                intro: None,
                subtitle: None,
                publisher: None,
                language: None,
                genre: None,
                published_year: None,
                published_date: None,
                isbn: None,
                asin: None,
                explicit: None,
                abridged: None,
                tags: Vec::new(),
                duration: None,
                score: None,
                chapter_title_template: None,
                chapter_titles: Vec::new(),
            }],
            page: 1,
            page_size: 20,
            total: None,
            has_more: None,
        })
        .unwrap();
        assert!(validate_capability_output(&capability, "search", &request, &output).is_ok());
        output["items"][0]["artist"] = serde_json::json!("old alias");
        assert!(validate_capability_output(&capability, "search", &request, &output).is_err());
    }

    #[test]
    fn format_call_validates_declared_operation_request_and_response() {
        let capability: PluginCapability = serde_json::from_value(serde_json::json!({
            "kind": "format_handler",
            "id": "special.audio",
            "extensions": ["special"],
            "operations": ["probe", "get_metadata_read_size", "extract_metadata"]
        }))
        .unwrap();
        let input = serde_json::json!({
            "input": "host:opaque",
            "extension_hint": "special",
            "mime_hint": null,
            "prefix_bytes": 4096
        });
        assert!(validate_capability_input(&capability, "probe", &input).is_ok());
        let more = serde_json::json!({"kind": "need_more", "total_bytes": 8192});
        assert!(validate_capability_output(&capability, "probe", &input, &more).is_ok());
        let repeated = serde_json::json!({"kind": "need_more", "total_bytes": 4096});
        assert!(validate_capability_output(&capability, "probe", &input, &repeated).is_err());
        assert!(validate_capability_input(
            &capability,
            "probe",
            &serde_json::json!({"input": "host:opaque", "prefix_bytes": 4096, "file_path": "C:/secret"})
        )
        .is_err());
        assert!(validate_capability_input(&capability, "open_decrypt", &input).is_err());
    }

    #[tokio::test]
    async fn record_plugin_call_updates_listed_stats() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = PluginManager::new(PluginConfig {
            plugin_dir: temp_dir.path().join("plugins"),
            enable_hot_reload: false,
            max_memory_per_plugin: 128 * 1024 * 1024,
            max_execution_time: Duration::from_secs(30),
        })
        .unwrap();

        let metadata = PluginMetadata::new(
            "stats-plugin".to_string(),
            "Stats Plugin".to_string(),
            "1.0.0".to_string(),
            "Ting Reader".to_string(),
            "Stats plugin".to_string(),
            "plugin.js".to_string(),
        );
        let plugin_id = metadata.instance_id();
        let instance =
            Arc::new(FailedPlugin::new(metadata.clone(), "unused".to_string())) as Arc<dyn Plugin>;

        manager
            .registry
            .write()
            .await
            .insert(plugin_id.clone(), PluginEntry::new(metadata, instance));

        manager
            .record_plugin_call(
                &plugin_id,
                "search",
                Duration::from_millis(25),
                None,
                PluginLogLevel::Info,
            )
            .await;
        manager
            .record_plugin_call(
                &plugin_id,
                "search",
                Duration::from_millis(5),
                Some(&TingError::PluginExecutionError("boom".to_string())),
                PluginLogLevel::Info,
            )
            .await;

        let plugins = manager.list_plugins().await;
        let stats_plugin = plugins
            .iter()
            .find(|plugin| plugin.id == plugin_id)
            .expect("stats plugin should be listed");

        assert_eq!(stats_plugin.total_calls, 2);
        assert_eq!(stats_plugin.successful_calls, 1);
        assert_eq!(stats_plugin.failed_calls, 1);
    }

    #[tokio::test]
    async fn invoke_plugin_records_failure_for_unsupported_runtime() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = PluginManager::new(PluginConfig {
            plugin_dir: temp_dir.path().join("plugins"),
            enable_hot_reload: false,
            max_memory_per_plugin: 128 * 1024 * 1024,
            max_execution_time: Duration::from_secs(30),
        })
        .unwrap();

        let metadata = PluginMetadata::new(
            "unsupported-plugin".to_string(),
            "Unsupported Plugin".to_string(),
            "1.0.0".to_string(),
            "Ting Reader".to_string(),
            "Unsupported plugin".to_string(),
            "plugin.js".to_string(),
        );
        let plugin_id = metadata.instance_id();
        let instance =
            Arc::new(FailedPlugin::new(metadata.clone(), "unused".to_string())) as Arc<dyn Plugin>;

        manager
            .registry
            .write()
            .await
            .insert(plugin_id.clone(), PluginEntry::new(metadata, instance));

        let result = manager
            .invoke_plugin(
                &plugin_id,
                "anything",
                serde_json::json!({}),
                &PluginInvocationContext::default(),
            )
            .await;

        assert!(result.is_err());

        let plugins = manager.list_plugins().await;
        let plugin = plugins
            .iter()
            .find(|plugin| plugin.id == plugin_id)
            .expect("plugin should be listed");

        assert_eq!(plugin.total_calls, 1);
        assert_eq!(plugin.failed_calls, 1);
    }
}
