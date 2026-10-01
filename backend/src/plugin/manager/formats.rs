//! Trusted format selection and local resource grants. The core routes by
//! declared extensions and probe results; special file parsing stays in plugins.

use super::PluginManager;
use crate::core::app::error::{Result, TingError};
use crate::plugin::host_api::resources::{ResourceLimits, ResourceScope, ResourceSource};
use crate::plugin::types::{PluginCapability, PluginInvocationContext};
use std::path::Path;
use std::sync::Arc;
use ting_plugin_contract::format::FormatOperation;
use ting_plugin_contract::format_calls::{MAX_METADATA_READ_BYTES, ProbeResult, ResourceId};
use ting_plugin_contract::format_calls::{MetadataPatch, WriteMetadataResult};
use ting_plugin_contract::format_registry::{FormatProviderKey, FormatRegistry};

pub struct SelectedFormat {
    pub plugin_id: String,
    pub capability_id: String,
    pub input: ResourceId,
    pub scope: Arc<ResourceScope>,
}

pub struct ExtractedFormat {
    pub metadata: ting_plugin_contract::format::FormatMetadata,
    pub cover_bytes: Option<Vec<u8>>,
    pub cover_mime: Option<String>,
}

impl ExtractedFormat {
    /// Scanner adapter after the scoped asset has been copied by the Host.
    /// The metadata schema itself remains shared across all runtimes.
    pub fn into_scanner_json(self, cover_dir: Option<&Path>) -> Result<serde_json::Value> {
        let mut result = serde_json::to_value(&self.metadata).map_err(|error| {
            TingError::PluginExecutionError(format!("Invalid metadata: {error}"))
        })?;
        let cover_url = match self.metadata.cover {
            Some(ting_plugin_contract::format::AssetRef::RemoteUrl { url }) => Some(url),
            Some(ting_plugin_contract::format::AssetRef::HostAsset { .. }) => {
                let (Some(dir), Some(bytes)) = (cover_dir, self.cover_bytes) else {
                    return Ok(result);
                };
                let ext = match self.cover_mime.as_deref() {
                    Some("image/png") if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => "png",
                    Some("image/webp")
                        if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") =>
                    {
                        "webp"
                    }
                    Some("image/jpeg") if bytes.starts_with(b"\xff\xd8") => "jpg",
                    _ => {
                        return Err(TingError::PluginExecutionError(
                            "Invalid plugin cover image".into(),
                        ));
                    }
                };
                std::fs::create_dir_all(dir).map_err(TingError::IoError)?;
                let target = dir.join(format!("cover.{ext}"));
                if !target.exists() {
                    let stage = dir.join(format!("{}.cover-staging", uuid::Uuid::new_v4()));
                    std::fs::write(&stage, bytes).map_err(TingError::IoError)?;
                    if let Err(error) = std::fs::rename(&stage, &target) {
                        let _ = std::fs::remove_file(stage);
                        return Err(TingError::IoError(error));
                    }
                }
                Some(target.to_string_lossy().replace('\\', "/"))
            }
            None => None,
        };
        if let Some(cover_url) = cover_url {
            result["cover_url"] = serde_json::Value::String(cover_url);
        }
        Ok(result)
    }
}

impl SelectedFormat {
    pub fn context(
        &self,
        principal: Option<crate::plugin::PluginHostUser>,
    ) -> PluginInvocationContext {
        PluginInvocationContext {
            user: principal,
            resources: Some(Arc::clone(&self.scope)),
        }
    }
}

impl PluginManager {
    async fn format_registry(
        &self,
        required_operation: Option<FormatOperation>,
    ) -> Result<FormatRegistry> {
        let mut registry = FormatRegistry::default();
        for entry in self.registry.read().await.values() {
            if !matches!(entry.state, crate::plugin::types::PluginState::Active) {
                continue;
            }
            for cap in &entry.metadata.capabilities {
                if let PluginCapability::FormatHandler(declaration) = cap
                    && required_operation.is_none_or(|operation| declaration.supports(operation))
                {
                    registry
                        .register(&entry.metadata.instance_id(), declaration.clone())
                        .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
                }
            }
        }
        Ok(registry)
    }

    async fn select_probed_format(
        &self,
        extension: &str,
        principal: Option<crate::plugin::PluginHostUser>,
        registry: &FormatRegistry,
        grants: Vec<(FormatProviderKey, Arc<ResourceScope>, ResourceId)>,
    ) -> Result<Option<SelectedFormat>> {
        let mut results = Vec::with_capacity(grants.len());
        for (key, scope, input) in &grants {
            let length = scope
                .stat(input)
                .map_err(|error| TingError::PluginExecutionError(error.to_string()))?
                .length
                .unwrap_or(0);
            let mut available = length.min(4096);
            let result = loop {
                if available < 10 {
                    break ProbeResult::NoMatch;
                }
                let output = self
                    .invoke_capability(
                        &key.plugin_id,
                        &key.capability_id,
                        "probe",
                        serde_json::json!({
                            "input": input,
                            "extension_hint": extension.to_ascii_lowercase(),
                            "mime_hint": null,
                            "prefix_bytes": available,
                        }),
                        &PluginInvocationContext {
                            user: principal.clone(),
                            resources: Some(Arc::clone(scope)),
                        },
                    )
                    .await?;
                let probe: ProbeResult = serde_json::from_value(output).map_err(|error| {
                    TingError::PluginExecutionError(format!("Invalid probe response: {error}"))
                })?;
                match probe {
                    ProbeResult::NeedMore { total_bytes }
                        if total_bytes > available && total_bytes <= MAX_METADATA_READ_BYTES =>
                    {
                        available = total_bytes.min(length);
                        if available < total_bytes {
                            break ProbeResult::NoMatch;
                        }
                        scope
                            .extend_readable_end(input, available)
                            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
                    }
                    ProbeResult::NeedMore { .. } => {
                        return Err(TingError::PluginExecutionError(
                            "Format probe exceeded its byte budget or failed to advance".into(),
                        ));
                    }
                    final_result => break final_result,
                }
            };
            results.push((key.clone(), result));
        }

        let selected = registry
            .select(extension, &results, None)
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        let Some(selected) = selected else {
            return Ok(None);
        };
        let (_, scope, input) = grants
            .into_iter()
            .find(|(key, _, _)| key == &selected)
            .ok_or_else(|| TingError::PluginExecutionError("Selected format unavailable".into()))?;
        let FormatProviderKey {
            plugin_id,
            capability_id,
        } = selected;
        Ok(Some(SelectedFormat {
            plugin_id,
            capability_id,
            scope,
            input,
        }))
    }

    pub async fn has_format_operation(
        &self,
        path: &Path,
        operation: FormatOperation,
    ) -> Result<bool> {
        let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        let registry = self.format_registry(Some(operation)).await?;
        Ok(!registry.candidates(extension).is_empty())
    }

    pub async fn select_local_format(
        &self,
        path: &Path,
        principal: Option<crate::plugin::PluginHostUser>,
    ) -> Result<Option<SelectedFormat>> {
        self.select_local_format_with_operation(
            path,
            path,
            principal,
            FormatOperation::ExtractMetadata,
        )
        .await
    }

    pub async fn select_local_format_with_operation(
        &self,
        format_path: &Path,
        source_path: &Path,
        principal: Option<crate::plugin::PluginHostUser>,
        required_operation: FormatOperation,
    ) -> Result<Option<SelectedFormat>> {
        let extension = format_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");
        let registry = self.format_registry(Some(required_operation)).await?;
        let candidates = registry.candidates(extension);
        if candidates.is_empty() {
            return Ok(None);
        }
        let mut grants = Vec::with_capacity(candidates.len());
        for key in candidates {
            let scope = self
                .resource_scope(
                    &key.plugin_id,
                    &PluginInvocationContext {
                        user: principal.clone(),
                        resources: None,
                    },
                    self.config.plugin_dir.join("staging"),
                    ResourceLimits::default(),
                )
                .await?;
            let input = scope
                .grant_file(source_path, 4096, None)
                .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
            grants.push((key, scope, input));
        }
        self.select_probed_format(extension, principal, &registry, grants)
            .await
    }

    pub async fn select_source_format(
        &self,
        format_path: &Path,
        source: Arc<dyn ResourceSource>,
        length: u64,
        principal: Option<crate::plugin::PluginHostUser>,
        required_operation: FormatOperation,
    ) -> Result<Option<SelectedFormat>> {
        let extension = format_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");
        let registry = self.format_registry(Some(required_operation)).await?;
        let candidates = registry.candidates(extension);
        if candidates.is_empty() {
            return Ok(None);
        }
        let mut grants = Vec::with_capacity(candidates.len());
        for key in candidates {
            let scope = self
                .resource_scope(
                    &key.plugin_id,
                    &PluginInvocationContext {
                        user: principal.clone(),
                        resources: None,
                    },
                    self.config.plugin_dir.join("staging"),
                    ResourceLimits::default(),
                )
                .await?;
            let input = scope
                .grant_source(Arc::clone(&source), 4096.min(length), 0)
                .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
            grants.push((key, scope, input));
        }
        self.select_probed_format(extension, principal, &registry, grants)
            .await
    }

    pub async fn extract_local_format_metadata(
        &self,
        path: &Path,
        extract_cover: bool,
    ) -> Result<Option<ExtractedFormat>> {
        let Some(selected) = self.select_local_format(path, None).await? else {
            return Ok(None);
        };
        self.extract_selected_format_metadata(selected, extract_cover)
            .await
    }

    /// Extract metadata directly from a Host-owned source. This is the
    /// remote/scanner counterpart of `extract_local_format_metadata`; the
    /// plugin receives a bounded resource handle and never needs a temporary
    /// filesystem path.
    pub async fn extract_source_format_metadata(
        &self,
        format_path: &Path,
        source: Arc<dyn ResourceSource>,
        length: u64,
        principal: Option<crate::plugin::PluginHostUser>,
        extract_cover: bool,
    ) -> Result<Option<ExtractedFormat>> {
        let Some(selected) = self
            .select_source_format(
                format_path,
                source,
                length,
                principal.clone(),
                FormatOperation::ExtractMetadata,
            )
            .await?
        else {
            return Ok(None);
        };
        self.extract_selected_format_metadata(selected, extract_cover)
            .await
    }

    async fn extract_selected_format_metadata(
        &self,
        selected: SelectedFormat,
        extract_cover: bool,
    ) -> Result<Option<ExtractedFormat>> {
        selected
            .scope
            .extend_readable_end(
                &selected.input,
                MAX_METADATA_READ_BYTES.min(
                    selected
                        .scope
                        .stat(&selected.input)
                        .map_err(|error| TingError::PluginExecutionError(error.to_string()))?
                        .length
                        .unwrap_or(0),
                ),
            )
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        let result = self
            .invoke_capability(
                &selected.plugin_id,
                &selected.capability_id,
                "extract_metadata",
                serde_json::json!({ "input": selected.input, "extract_cover": extract_cover }),
                &selected.context(None),
            )
            .await?;
        let metadata: ting_plugin_contract::format::FormatMetadata = serde_json::from_value(result)
            .map_err(|error| {
                TingError::PluginExecutionError(format!("Invalid format metadata: {error}"))
            })?;
        metadata
            .validate()
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        let (cover_bytes, cover_mime) =
            if let Some(ting_plugin_contract::format::AssetRef::HostAsset { id }) =
                metadata.cover.as_ref()
            {
                let resource = ting_plugin_contract::format_calls::ResourceId(id.clone());
                let stat = selected
                    .scope
                    .stat(&resource)
                    .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
                let length = stat
                    .length
                    .filter(|length| *length <= MAX_METADATA_READ_BYTES)
                    .ok_or_else(|| {
                        TingError::PluginExecutionError("Invalid format cover length".into())
                    })?;
                let mut bytes = Vec::with_capacity(length as usize);
                while bytes.len() < length as usize {
                    let (chunk, eof) = selected
                        .scope
                        .read_at(
                            &resource,
                            bytes.len() as u64,
                            (length as usize - bytes.len()).min(
                                ting_plugin_contract::format_calls::MAX_MEDIA_CHUNK_BYTES as usize,
                            ),
                        )
                        .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
                    if chunk.is_empty() && !eof {
                        return Err(TingError::PluginExecutionError(
                            "Empty format cover chunk".into(),
                        ));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                (Some(bytes), stat.mime_type)
            } else {
                (None, None)
            };
        Ok(Some(ExtractedFormat {
            metadata,
            cover_bytes,
            cover_mime,
        }))
    }

    pub async fn write_local_format_metadata(
        &self,
        path: &Path,
        patch: MetadataPatch,
        cover_path: Option<&Path>,
    ) -> Result<bool> {
        let Some(selected) = self.select_local_format(path, None).await? else {
            return Ok(false);
        };
        let capability = self
            .registry
            .read()
            .await
            .get(&selected.plugin_id)
            .and_then(|entry| {
                entry
                    .metadata
                    .capabilities
                    .iter()
                    .find(|cap| cap.id() == selected.capability_id)
                    .cloned()
            })
            .ok_or_else(|| {
                TingError::PluginExecutionError("Format declaration unavailable".into())
            })?;
        if !capability.supports("write_metadata") {
            return Ok(false);
        }
        let stat = selected
            .scope
            .stat(&selected.input)
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        let revision = stat.revision.ok_or_else(|| {
            TingError::PluginExecutionError("Format source revision unavailable".into())
        })?;
        let length = stat.length.unwrap_or(0);
        selected
            .scope
            .extend_readable_end(&selected.input, length)
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        let mut patch = patch;
        if let Some(path) = cover_path {
            let resource = selected
                .scope
                .grant_file(path, MAX_METADATA_READ_BYTES, None)
                .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
            patch.cover = ting_plugin_contract::format_calls::PatchField::Set(
                ting_plugin_contract::format::AssetRef::HostAsset { id: resource.0 },
            );
        }
        let output = selected
            .scope
            .create_output(None)
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        let result = self
            .invoke_capability(
                &selected.plugin_id,
                &selected.capability_id,
                "write_metadata",
                serde_json::json!({
                    "input": selected.input, "output": output,
                    "source_revision": revision, "patch": patch,
                }),
                &selected.context(None),
            )
            .await?;
        let result: WriteMetadataResult = serde_json::from_value(result).map_err(|error| {
            TingError::PluginExecutionError(format!("Invalid format write: {error}"))
        })?;
        let output_stat = selected
            .scope
            .stat(&output)
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        if output_stat.length != Some(result.bytes_written) {
            return Err(TingError::PluginExecutionError(
                "Format write length mismatch".into(),
            ));
        }
        selected
            .scope
            .commit_local_output(&output, &selected.input, path, &revision)
            .map_err(|error| TingError::PluginExecutionError(error.to_string()))?;
        Ok(true)
    }
}
