use super::super::LibraryScanner;
use crate::core::StorageService;
use crate::db::models::Library;
use crate::plugin::host_api::resources::{ResourceError, ResourceResult, ResourceSource};
use id3::TagLike;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use tracing::debug;
use uuid::Uuid;

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
        library: &crate::db::models::Library,
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
        // Returns: (album, title, author, narrator, cover_url, duration)

        // Handle .strm files: download the file, read the URL inside, then FFprobe that URL
        let ext = Path::new(file_url)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("tmp")
            .to_lowercase();
        let fallback_title = Path::new(file_url)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("chapter")
            .to_string();
        if ext == "strm" {
            let decoded_url = self.decode_url_path(file_url);
            let filename = decoded_url.split('/').next_back().unwrap_or("chapter");
            let title = filename
                .strip_suffix(".strm")
                .unwrap_or(filename)
                .to_string();

            if let Some(storage) = &self.storage_service {
                let key = self.encryption_key.as_deref().unwrap_or(&[0u8; 32]);

                // Download .strm file content
                let temp_dir = std::env::temp_dir();
                let temp_filename = format!("ting_scan_strm_{}.strm", Uuid::new_v4());
                let temp_path = temp_dir.join(&temp_filename);

                if let Ok((mut reader, _)) = storage
                    .get_webdav_reader(library, file_url, None, key)
                    .await
                    && let Ok(mut file) = tokio::fs::File::create(&temp_path).await
                {
                    let _ = tokio::io::copy(&mut reader, &mut file).await;
                }

                // Read the URL from the .strm file
                let url = match tokio::fs::read_to_string(&temp_path).await {
                    Ok(content) => content.trim().to_string(),
                    Err(e) => {
                        tracing::error!(
                            path = %file_url,
                            error = %e,
                            message_key = "strm.file.read_failed",
                            message_params = %serde_json::json!({
                                "path": file_url,
                                "error": e.to_string(),
                            }),
                            "Failed to read strm file"
                        );
                        let _ = tokio::fs::remove_file(&temp_path).await;
                        return (String::new(), title, None, None, None, 0);
                    }
                };

                let _ = tokio::fs::remove_file(&temp_path).await;

                if url.is_empty() || !url.starts_with("http") {
                    tracing::warn!(
                        path = %file_url,
                        url = %url,
                        message_key = "strm.url.invalid",
                        message_params = %serde_json::json!({ "path": file_url }),
                        "strm file contains invalid URL"
                    );
                    return (String::new(), title, None, None, None, 0);
                }

                let duration = if let Ok(mut ffprobe) =
                    crate::core::audio::AudioService::ffprobe_command()
                {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                    match ffprobe
                            .arg("-v").arg("error")
                            .arg("-user_agent").arg("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
                            .arg("-headers").arg("Accept: */*")
                            .arg("-show_entries").arg("format=duration")
                            .arg("-of").arg("default=noprint_wrappers=1:nokey=1")
                            .arg(&url)
                            .output()
                            .await
                    {
                            Ok(output) if output.status.success() => {
                                let duration_str = String::from_utf8_lossy(&output.stdout);
                                match duration_str.trim().parse::<f64>() {
                                    Ok(dur) => {
                                        let duration_secs = dur.round() as i32;
                                        tracing::info!("WebDAV strm file {} duration: {} seconds", title, duration_secs);
                                        duration_secs
                                    }
                                    Err(_) => {
                                        tracing::warn!(
                                            output = %duration_str,
                                            message_key = "ffprobe.output_parse_failed",
                                            "Failed to parse FFprobe output"
                                        );
                                        0
                                    }
                                }
                            }
                            Ok(output) => {
                                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                                tracing::warn!(
                                    error = %stderr,
                                    message_key = "ffprobe.duration_failed",
                                    message_params = %serde_json::json!({ "error": stderr }),
                                    "FFprobe duration detection failed"
                                );
                                0
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    message_key = "ffprobe.run_failed",
                                    message_params = %serde_json::json!({ "error": e.to_string() }),
                                    "Failed to run FFprobe"
                                );
                                0
                            }
                        }
                } else {
                    tracing::warn!(
                        message_key = "ffprobe.missing",
                        "FFprobe not found; duration will be set to zero"
                    );
                    0
                };

                return (String::new(), title, None, None, None, duration);
            } else {
                return (String::new(), fallback_title, None, None, None, 0);
            }
        }

        if let Some(storage) = &self.storage_service {
            let key = self.encryption_key.as_deref().unwrap_or(&[0u8; 32]);

            // Special-format plugins inspect a Host-owned remote resource.
            // This keeps bounded reads and cancellation in the common
            // ResourceScope path instead of copying the source to a temp file.
            if let Ok((_, remote_length)) = storage
                .get_webdav_reader(library, file_url, Some((0, 1)), key)
                .await
                && remote_length > 0
                && let Ok(Some(extracted)) = self
                    .plugin_manager
                    .extract_source_format_metadata(
                        Path::new(file_url),
                        Arc::new(WebDavFormatSource {
                            storage: Arc::clone(storage),
                            library: library.clone(),
                            path: file_url.to_string(),
                            key: *key,
                            length: remote_length,
                            runtime: tokio::runtime::Handle::current(),
                        }),
                        remote_length,
                        None,
                        extract_cover,
                    )
                    .await
                && let Ok(result) = extracted.into_scanner_json(cover_target_dir)
            {
                let album = result
                    .get("album")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let title = result
                    .get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let author = result
                    .get("album_artist")
                    .or_else(|| result.get("artist"))
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .map(ToOwned::to_owned);
                let narrator = result
                    .get("narrator")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .map(ToOwned::to_owned);
                let cover = result
                    .get("cover_url")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .map(ToOwned::to_owned);
                let duration = result
                    .get("duration")
                    .and_then(|value| value.as_f64())
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .map(|value| value.round() as i32)
                    .unwrap_or_default();
                return (album, title, author, narrator, cover, duration);
            }

            // A declared format handler owns the special-format path. If its
            // remote resource call fails, do not silently copy the file to a
            // temporary path and bypass the plugin resource contract.
            let special_format_declared = self
                .plugin_manager
                .has_format_operation(
                    Path::new(file_url),
                    ting_plugin_contract::format::FormatOperation::ExtractMetadata,
                )
                .await
                .unwrap_or(false);
            if special_format_declared {
                tracing::warn!(
                    path = %file_url,
                    message_key = "format.remote_plugin_failed",
                    "Declared remote format plugin did not extract metadata"
                );
                return (String::new(), fallback_title, None, None, None, 0);
            }

            // Determine temp file path
            let temp_dir = std::env::temp_dir();
            let temp_filename = format!("ting_scan_{}.{}", Uuid::new_v4(), ext);
            let temp_path = temp_dir.join(&temp_filename);

            // 1. Probe Header
            // We need enough bytes to detect ID3v2 header and size.
            // ID3v2 header is 10 bytes. Size is encoded in bytes 6-9 (Synchsafe integer).
            // Let's probe 64KB first, usually enough for metadata, but maybe not cover.
            let probe_size = 64 * 1024;
            let mut required_size = probe_size as u64; // Default fallback
            let mut probe_data = Vec::with_capacity(probe_size);

            if let Ok((mut reader, _)) = storage
                .get_webdav_reader(library, file_url, Some((0, probe_size as u64)), key)
                .await
            {
                let mut buf = vec![0u8; probe_size];
                if let Ok(n) = reader.read(&mut buf).await {
                    probe_data.extend_from_slice(&buf[..n]);
                }
            }

            if !probe_data.is_empty() {
                // Check for ID3v2 header
                if probe_data.len() >= 10 && &probe_data[0..3] == b"ID3" {
                    // Parse ID3v2 size
                    // Size is 4 bytes (6-9), each byte uses 7 bits (MSB is 0)
                    let size_bytes = &probe_data[6..10];
                    let tag_size = ((size_bytes[0] as u32) << 21)
                        | ((size_bytes[1] as u32) << 14)
                        | ((size_bytes[2] as u32) << 7)
                        | (size_bytes[3] as u32);

                    // Total size = Header (10) + Tag Size + Footer (10, optional but we ignore for read size)
                    // We need to download at least this much to get full ID3 tag including cover
                    let total_id3_size = 10
                        + tag_size as u64
                        + if probe_data[3] == 4 && probe_data[5] & 0x10 != 0 {
                            10
                        } else {
                            0
                        };
                    if total_id3_size > required_size {
                        required_size = total_id3_size;
                        debug!("Detected ID3v2 tag size: {} bytes", required_size);
                    }
                }
            }
            required_size =
                required_size.min(ting_plugin_contract::format_calls::MAX_METADATA_READ_BYTES);

            // 2. Download required data
            if let Ok(mut file) = tokio::fs::File::create(&temp_path).await {
                // Write probe data
                if file.write_all(&probe_data).await.is_ok() {
                    // Download rest if needed
                    if required_size > probe_data.len() as u64 {
                        let start = probe_data.len() as u64;
                        let end = required_size;

                        if let Ok((reader, _)) = storage
                            .get_webdav_reader(library, file_url, Some((start, end)), key)
                            .await
                        {
                            let _ = tokio::io::copy(&mut reader.take(end - start), &mut file).await;
                        }
                    }

                    // Extract metadata
                    let mut album = String::new();
                    let mut title = String::new();
                    let mut author = None;
                    let mut narrator = None;
                    let mut duration = 0;
                    let mut cover_url = None;

                    let ext = file_url.split('.').next_back().unwrap_or("").to_lowercase();
                    let mut use_ffprobe = false;

                    // 策略：优先使用格式插件处理所有格式（包括 MP3/M4A/WMA/FLAC 等）
                    // 格式插件使用 lofty 等库，对部分文件支持更好，不会报 "end of stream" 错误

                    // 1. 查找支持该格式的插件
                    let mut plugin_handled = false;

                    if let Ok(Some(extracted)) = self
                        .plugin_manager
                        .extract_local_format_metadata(&temp_path, false)
                        .await
                        && let Ok(result) = extracted.into_scanner_json(None)
                    {
                        tracing::debug!(
                            "Using format plugin {} to process {} file",
                            "format_handler",
                            ext
                        );

                        // 提取元数据
                        if let Some(a) = result.get("album").and_then(|v| v.as_str())
                            && !a.trim().is_empty()
                        {
                            album = a.to_string();
                        }
                        if let Some(t) = result.get("title").and_then(|v| v.as_str())
                            && !t.trim().is_empty()
                        {
                            title = t.to_string();
                        }
                        if let Some(au) = result
                            .get("album_artist")
                            .or_else(|| result.get("artist"))
                            .and_then(|v| v.as_str())
                            && !au.trim().is_empty()
                        {
                            author = Some(au.to_string());
                        }
                        if let Some(n) = result.get("narrator").and_then(|v| v.as_str())
                            && !n.trim().is_empty()
                        {
                            narrator = Some(n.to_string());
                        }
                        if let Some(dur) = result.get("duration").and_then(|v| v.as_f64()) {
                            duration = dur.round() as i32;
                            if duration > 0 {
                                tracing::debug!(
                                    "Format plugin {} detected duration: {} seconds",
                                    "format_handler",
                                    duration
                                );
                            }
                        }
                        if let Some(c) = result.get("cover_url").and_then(|v| v.as_str())
                            && !c.trim().is_empty()
                        {
                            cover_url = Some(c.to_string());
                        }

                        plugin_handled = true;
                    }

                    // 2. 如果没有插件处理，且是 MP3 文件，尝试使用 ID3 库（对部分文件支持好）
                    if !plugin_handled
                        && ext == "mp3"
                        && let Ok(tag) = id3::Tag::read_from_path(&temp_path)
                    {
                        debug!("Using ID3 library to process MP3 file");
                        if let Some(t) = tag.album()
                            && !t.trim().is_empty()
                        {
                            album = t.to_string();
                        }
                        if let Some(t) = tag.title()
                            && !t.trim().is_empty()
                        {
                            title = t.to_string();
                        }

                        // Author logic: Album Artist > Artist
                        if let Some(t) = tag.album_artist()
                            && !t.trim().is_empty()
                        {
                            author = Some(t.to_string());
                        }

                        if let Some(t) = tag.artist()
                            && !t.trim().is_empty()
                        {
                            if author.is_none() {
                                author = Some(t.to_string());
                            } else if author.as_deref() != Some(t) {
                                narrator = Some(t.to_string());
                            }
                        }

                        if let Some(d) = tag.duration() {
                            duration = (d / 1000) as i32;
                        }
                    }

                    // 注意：不再调用 extract_chapter_metadata，因为它使用 Symphonia
                    // 对部分文件会报 "end of stream" 错误

                    // 3. 验证时长合理性：与文件大小估算比对
                    if duration > 0 {
                        // 获取文件大小（通过之前的 WebDAV 请求已经获取）
                        // 我们可以发起一个 HEAD 请求或者使用 Range 请求获取
                        if let Ok((_, file_size)) = storage
                            .get_webdav_reader(library, file_url, Some((0, 1)), key)
                            .await
                            && file_size > 0
                        {
                            // 根据文件大小和格式估算时长
                            let estimated_duration =
                                self.estimate_duration_by_size(file_size, &ext);

                            if estimated_duration > 0 {
                                let diff_ratio = (duration as f64 - estimated_duration as f64)
                                    .abs()
                                    / estimated_duration as f64;

                                if diff_ratio > 0.15 {
                                    // 差距超过15%，时长可能不准确，需要用 FFprobe 验证
                                    tracing::warn!(
                                        message_key = "webdav.duration.mismatch",
                                        message_params = %serde_json::json!({
                                            "file_url": file_url,
                                            "duration": duration,
                                            "estimated_duration": estimated_duration,
                                            "diff_percent": diff_ratio * 100.0,
                                        }),
                                        file_url = %file_url,
                                        duration = duration,
                                        estimated_duration = estimated_duration,
                                        diff_percent = diff_ratio * 100.0,
                                        "WebDAV duration differs from size estimate; using FFprobe"
                                    );
                                    use_ffprobe = true;
                                } else {
                                    tracing::debug!(
                                        "WebDAV file {} duration verified (detected: {}s, estimated: {}s, diff: {:.1}%)",
                                        file_url,
                                        duration,
                                        estimated_duration,
                                        diff_ratio * 100.0
                                    );
                                }
                            }
                        }
                    } else {
                        // 无法从部分文件中获取时长，需要 FFprobe
                        use_ffprobe = true;
                        tracing::debug!(
                            "Could not get {} duration from partial file; using FFprobe",
                            ext
                        );
                    }

                    // 4. Use FFprobe when needed (fallback or validation)
                    if use_ffprobe {
                        if let Ok(mut ffprobe) = crate::core::audio::AudioService::ffprobe_command()
                        {
                            // Add small delay before FFprobe to avoid overwhelming the server
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                            // Build WebDAV URL with authentication
                            let webdav_url = if file_url.starts_with("http://")
                                || file_url.starts_with("https://")
                            {
                                url::Url::parse(file_url).ok()
                            } else {
                                None
                            };

                            if let Some(mut url) = webdav_url {
                                // Add authentication to URL if present
                                if let (Some(username), Some(password)) =
                                    (&library.username, &library.password)
                                {
                                    let decrypted_password =
                                        crate::core::security::crypto::decrypt(password, key)
                                            .unwrap_or_else(|_| password.clone());
                                    url.set_username(username).ok();
                                    url.set_password(Some(&decrypted_password)).ok();
                                }

                                let url_str = url.to_string();

                                match ffprobe
                                    .arg("-v")
                                    .arg("error")
                                    .arg("-show_entries")
                                    .arg("format=duration")
                                    .arg("-of")
                                    .arg("default=noprint_wrappers=1:nokey=1")
                                    .arg(&url_str)
                                    .output()
                                    .await
                                {
                                    Ok(output) if output.status.success() => {
                                        let duration_str = String::from_utf8_lossy(&output.stdout);
                                        if let Ok(dur) = duration_str.trim().parse::<f64>() {
                                            duration = dur.round() as i32;
                                            debug!(
                                                "FFprobe detected WebDAV file duration: {} seconds ({})",
                                                duration, ext
                                            );
                                        }
                                    }
                                    Ok(output) => {
                                        debug!(
                                            "FFprobe failed to get WebDAV duration: {}",
                                            String::from_utf8_lossy(&output.stderr)
                                        );
                                    }
                                    Err(e) => {
                                        debug!("Failed to run FFprobe: {}", e);
                                    }
                                }
                            }
                        } else {
                            debug!("FFprobe not found; keeping estimated WebDAV duration");
                        }
                    }

                    if !extract_cover {
                        cover_url = None;
                    }

                    // Manually extract cover here since we have the temp file
                    let mut final_cover_url = cover_url;

                    if extract_cover && final_cover_url.is_none() {
                        // Decide target directory
                        let (target_dir, use_hash_name) = if let Some(dir) = cover_target_dir {
                            (dir.to_path_buf(), false) // Use fixed name "cover.ext" inside dir
                        } else {
                            // Fallback to old behavior: temp/covers/{hash}.ext
                            let cache_dir = Path::new("./temp/covers");
                            if !cache_dir.exists() {
                                let _ = std::fs::create_dir_all(cache_dir);
                            }
                            (cache_dir.to_path_buf(), true)
                        };

                        // Ensure directory exists
                        if !target_dir.exists() {
                            let _ = std::fs::create_dir_all(&target_dir);
                        }

                        // Check if cover file already exists (for non-hash mode)
                        if !use_hash_name {
                            let cover_extensions = ["jpg", "jpeg", "png", "webp", "gif"];
                            for ext in &cover_extensions {
                                let cover_path = target_dir.join(format!("cover.{}", ext));
                                if cover_path.exists() {
                                    debug!(
                                        "Cover file already exists at {:?}, skipping extraction",
                                        cover_path
                                    );
                                    final_cover_url =
                                        Some(cover_path.to_string_lossy().replace('\\', "/"));
                                    break;
                                }
                            }
                        }

                        // Only extract if we didn't find an existing cover
                        if final_cover_url.is_none() {
                            // First try plugin-based extraction (supports M4A, etc.)
                            if let Ok(Some(extracted)) = self
                                .plugin_manager
                                .extract_local_format_metadata(&temp_path, true)
                                .await
                                && let Ok(result) = extracted.into_scanner_json(Some(&target_dir))
                                && let Some(c) = result.get("cover_url").and_then(|v| v.as_str())
                                && !c.trim().is_empty()
                            {
                                final_cover_url = Some(c.to_string());
                            }

                            // Fallback to ID3 extraction (for MP3)
                            if final_cover_url.is_none()
                                && let Ok(tag) = id3::Tag::read_from_path(&temp_path)
                                && let Some(picture) = tag.pictures().next()
                            {
                                let ext = match picture.mime_type.as_str() {
                                    "image/png" => "png",
                                    "image/webp" => "webp",
                                    "image/gif" => "gif",
                                    _ => "jpg",
                                };

                                let target_path = if use_hash_name {
                                    // Generate hash from parent URL
                                    let parent_url = if let Some(idx) = file_url.rfind('/') {
                                        &file_url[..idx]
                                    } else {
                                        file_url
                                    };
                                    let mut hasher = Sha256::new();
                                    hasher.update(parent_url.as_bytes());
                                    let book_hash = format!("{:x}", hasher.finalize());
                                    target_dir.join(format!("{}.{}", book_hash, ext))
                                } else {
                                    target_dir.join(format!("cover.{}", ext))
                                };

                                // Only write if not exists
                                if !target_path.exists()
                                    && std::fs::write(&target_path, &picture.data).is_ok()
                                {
                                    debug!("Saved WebDAV cover from ID3 to {:?}", target_path);
                                }
                                final_cover_url =
                                    Some(target_path.to_string_lossy().replace('\\', "/"));
                            }
                        }
                    }

                    // Cleanup
                    let _ = tokio::fs::remove_file(&temp_path).await;

                    return (album, title, author, narrator, final_cover_url, duration);
                }
            }

            // Ensure cleanup on failure
            if temp_path.exists() {
                let _ = tokio::fs::remove_file(&temp_path).await;
            }
        }

        (String::new(), String::new(), None, None, None, 0)
    }
}
