use super::super::LibraryScanner;
use crate::core::StorageService;
use crate::core::audio::{AudioService, metadata::AudioInput};
use crate::db::models::Library;
use crate::plugin::host_api::resources::{ResourceError, ResourceResult, ResourceSource};
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

pub(super) fn audio_metadata_enabled(library: &Library) -> bool {
    !library
        .scraper_config
        .as_deref()
        .and_then(|config| serde_json::from_str::<crate::db::models::ScraperConfig>(config).ok())
        .is_some_and(|config| config.cloud_mode)
}

struct WebDavFormatSource {
    storage: Arc<StorageService>,
    library: Library,
    path: String,
    key: [u8; 32],
    length: u64,
    runtime: tokio::runtime::Handle,
}

impl ResourceSource for WebDavFormatSource {
    fn stat(&self) -> ting_plugin_contract::resources::ResourceStat {
        ting_plugin_contract::resources::ResourceStat {
            length: Some(self.length),
            mime_type: None,
            readable: true,
            writable: false,
            seekable: true,
            revision: None,
            finished: true,
        }
    }

    fn read_at(
        &self,
        offset: u64,
        max_bytes: usize,
        cancel: &CancellationToken,
    ) -> ResourceResult<Vec<u8>> {
        let storage = Arc::clone(&self.storage);
        let library = self.library.clone();
        let path = self.path.clone();
        let key = self.key;
        let runtime = self.runtime.clone();
        let token = cancel.clone();
        std::thread::spawn(move || {
            runtime.block_on(async move {
                let (mut reader, _) = storage
                    .get_webdav_reader(
                        &library,
                        &path,
                        Some((offset, offset.saturating_add(max_bytes as u64))),
                        &key,
                    )
                    .await
                    .map_err(|_| ResourceError {
                        code: ting_plugin_contract::protocol::PluginErrorCode::NetworkError,
                        message: "Remote format source read failed",
                    })?;
                let mut output = vec![0; max_bytes];
                let mut length = 0;
                while length < output.len() {
                    let read = tokio::select! {
                        _ = token.cancelled() => {
                            return Err(ResourceError {
                                code: ting_plugin_contract::protocol::PluginErrorCode::Cancelled,
                                message: "Resource scope cancelled",
                            });
                        }
                        read = reader.read(&mut output[length..]) => read.map_err(|_| ResourceError {
                            code: ting_plugin_contract::protocol::PluginErrorCode::NetworkError,
                            message: "Remote format source read failed",
                        })?,
                    };
                    if read == 0 {
                        break;
                    }
                    length += read;
                }
                output.truncate(length);
                Ok(output)
            })
        })
        .join()
        .map_err(|_| ResourceError {
            code: ting_plugin_contract::protocol::PluginErrorCode::InternalError,
            message: "Remote format source worker failed",
        })?
    }
}

impl LibraryScanner {
    pub(crate) async fn extract_webdav_metadata(
        &self,
        library: &Library,
        file_url: &str,
        cover_target_dir: Option<&Path>,
        extract_cover: bool,
    ) -> (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i32,
    ) {
        let path = Path::new(file_url);
        let fallback = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("chapter")
            .to_string();
        if !audio_metadata_enabled(library) {
            return (String::new(), fallback, None, None, None, 0);
        }
        let Some(storage) = &self.storage_service else {
            return (String::new(), fallback, None, None, None, 0);
        };
        let key = self.encryption_key.as_deref().unwrap_or(&[0; 32]);
        let input = match storage.webdav_metadata_input(library, file_url, key) {
            Ok(input) => input,
            Err(error) => {
                tracing::warn!(error = %error, "Invalid WebDAV metadata source");
                return (String::new(), fallback, None, None, None, 0);
            }
        };

        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"))
        {
            let result = async {
                let (reader, _) = storage
                    .get_webdav_reader(library, file_url, None, key)
                    .await?;
                let mut bytes = Vec::new();
                reader.take(64 * 1024 + 1).read_to_end(&mut bytes).await?;
                if bytes.len() > 64 * 1024 {
                    return Err(crate::core::app::error::TingError::InvalidRequest(
                        "STRM file is too large".into(),
                    ));
                }
                let url = std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|value| url::Url::parse(value.trim()).ok())
                    .filter(|url| matches!(url.scheme(), "http" | "https"))
                    .ok_or_else(|| {
                        crate::core::app::error::TingError::InvalidRequest(
                            "Invalid STRM URL".into(),
                        )
                    })?;
                AudioService::read_file_metadata(&AudioInput::remote(url, None), None).await
            }
            .await;
            let duration = result
                .map(|metadata| metadata.duration.round() as i32)
                .unwrap_or_default();
            return (String::new(), fallback, None, None, None, duration);
        }

        // Special formats keep their bounded Host resource contract.
        if self
            .plugin_manager
            .has_format_operation(
                path,
                ting_plugin_contract::format::FormatOperation::ExtractMetadata,
            )
            .await
            .unwrap_or(false)
        {
            if let Ok((_, length)) = storage
                .get_webdav_reader(library, file_url, Some((0, 1)), key)
                .await
                && length > 0
                && let Ok(Some(extracted)) = self
                    .plugin_manager
                    .extract_source_format_metadata(
                        path,
                        Arc::new(WebDavFormatSource {
                            storage: Arc::clone(storage),
                            library: library.clone(),
                            path: file_url.into(),
                            key: *key,
                            length,
                            runtime: tokio::runtime::Handle::current(),
                        }),
                        length,
                        None,
                        extract_cover,
                    )
                    .await
                && let Ok(result) = extracted.into_scanner_json(cover_target_dir)
            {
                let string = |key| {
                    result
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .map(ToOwned::to_owned)
                };
                return (
                    string("album").unwrap_or_default(),
                    string("title").unwrap_or_default(),
                    string("album_artist").or_else(|| string("artist")),
                    string("narrator"),
                    string("cover_url"),
                    result
                        .get("duration")
                        .and_then(serde_json::Value::as_f64)
                        .filter(|value| value.is_finite() && *value >= 0.0)
                        .map(|value| value.round() as i32)
                        .unwrap_or_default(),
                );
            }
            tracing::warn!(path = %file_url, message_key = "format.remote_plugin_failed",
                "Declared remote format plugin did not extract metadata");
            return (String::new(), fallback, None, None, None, 0);
        }

        // A fixed prefix cannot represent MP4 containers with a trailing moov atom.
        let result = async {
            let cache_dir = if extract_cover {
                Some(if let Some(dir) = cover_target_dir {
                    dir.to_path_buf()
                } else {
                    let parent = file_url
                        .rsplit_once('/')
                        .map_or(file_url, |(parent, _)| parent);
                    crate::core::books::metadata_writer::remote_metadata_dir(parent)?
                })
            } else {
                None
            };
            AudioService::read_file_metadata(&input, cache_dir.as_deref()).await
        }
        .await;
        match result {
            Ok(metadata) => {
                let (author, narrator) = metadata.author_narrator();
                (
                    metadata.album.unwrap_or_default(),
                    metadata.title.unwrap_or_default(),
                    author,
                    narrator,
                    metadata.cover_url,
                    metadata.duration.round() as i32,
                )
            }
            Err(error) => {
                tracing::warn!(path = %file_url, error = %error, "Could not read WebDAV audio metadata");
                (String::new(), fallback, None, None, None, 0)
            }
        }
    }
}
