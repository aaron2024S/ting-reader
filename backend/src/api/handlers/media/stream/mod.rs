mod decrypt;
mod hls;
mod hls_serve;
mod hls_session;
pub(crate) mod preload;
mod strm;

use crate::api::handlers::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::error::{Result, TingError};
use crate::core::signing::{constant_time_eq, sign_media_stream_request, signature_has_expired};
use crate::db::models::{Chapter, Library};
use crate::db::repository::Repository;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
pub(crate) use decrypt::create_decrypted_stream;
pub use hls::handle_hls_request;
pub use hls_serve::{get_hls_playlist, get_hls_segment, seek_hls_stream};
pub use hls_session::HlsSessionManager;
use std::process::Stdio;
use ting_plugin_contract::format::FormatOperation;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

/// Query parameters for stream chapter
#[derive(Debug, serde::Deserialize)]
pub struct StreamQuery {
    pub token: Option<String>,
    pub transcode: Option<String>,
    pub seek: Option<String>,
    pub download: Option<String>,
    pub preload: Option<bool>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SignedStreamQuery {
    pub expires: Option<i64>,
    pub user: Option<String>,
    pub signature: Option<String>,
    pub transcode: Option<String>,
    pub seek: Option<String>,
    pub download: Option<String>,
}

fn stream_mime_type_from_path(path: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        // WebKit prefers audio/mp4 for m4a/mp4 audio streams.
        "m4a" | "mp4" => "audio/mp4".to_string(),
        "mp3" => "audio/mpeg".to_string(),
        "aac" => "audio/aac".to_string(),
        "flac" => "audio/flac".to_string(),
        "ogg" => "audio/ogg".to_string(),
        "opus" => "audio/opus".to_string(),
        "wav" => "audio/wav".to_string(),
        _ => mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string(),
    }
}

fn is_download_query(params: &StreamQuery) -> bool {
    let Some(value) = params.download.as_deref() else {
        return false;
    };
    matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

async fn get_remote_media_reader(
    state: &AppState,
    library: &Library,
    path: &str,
    range: Option<(u64, u64)>,
) -> Result<(Box<dyn tokio::io::AsyncRead + Send + Unpin>, u64)> {
    if library.library_type == "webdav" {
        return state
            .storage_service
            .get_webdav_reader(library, path, range, state.encryption_key.as_ref())
            .await
            .map_err(|e| TingError::NotFound(format!("Remote WebDAV media not found: {}", e)));
    }

    if library.library_type == "rss" || path.starts_with("http://") || path.starts_with("https://")
    {
        return state
            .storage_service
            .get_http_reader(path, range)
            .await
            .map_err(|e| TingError::NotFound(format!("Remote media not found: {}", e)));
    }

    Err(TingError::ValidationError(format!(
        "Unsupported remote library type '{}'",
        library.library_type
    )))
}

struct PluginTranscodeOptions<'a> {
    format: &'a str,
    content_type: &'a str,
    seek: Option<&'a str>,
}

async fn transcode_plugin_stream(
    state: &AppState,
    chapter: &Chapter,
    library: &Library,
    options: PluginTranscodeOptions<'_>,
) -> Result<axum::response::Response> {
    let PluginTranscodeOptions {
        format,
        content_type,
        seek,
    } = options;
    let (plugin_stream, _, _, _, _, _, _) =
        create_decrypted_stream(state, chapter, library, None).await?;

    let mut cmd = crate::core::audio::AudioService::ffmpeg_command()?;
    cmd.arg("-y").arg("-loglevel").arg("error");
    if let Some(seek_time) = seek {
        cmd.arg("-ss").arg(seek_time);
    }
    cmd.arg("-i").arg("pipe:0");

    if format == "mp3" {
        cmd.arg("-fflags")
            .arg("+genpts+igndts")
            .arg("-avoid_negative_ts")
            .arg("make_zero")
            .arg("-acodec")
            .arg("libmp3lame")
            .arg("-b:a")
            .arg("128k")
            .arg("-ac")
            .arg("2")
            .arg("-ar")
            .arg("44100")
            .arg("-vn")
            .arg("-map")
            .arg("0:a:0")
            .arg("-f")
            .arg("mp3");
    } else if format == "wav" {
        cmd.arg("-vn").arg("-map").arg("0:a:0").arg("-f").arg("wav");
    } else {
        cmd.arg("-f").arg(format);
    }

    cmd.arg("pipe:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    tracing::info!(
        chapter_id = %chapter.id,
        format = %format,
        "Using format plugin decoded stream for transcoded output"
    );

    let mut child = cmd.spawn().map_err(TingError::IoError)?;

    let mut stdin = child.stdin.take().ok_or_else(|| {
        TingError::IoError(std::io::Error::other("Failed to capture ffmpeg stdin"))
    })?;

    tokio::spawn(async move {
        use futures::StreamExt;

        let mut stream = plugin_stream;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if let Err(error) = stdin.write_all(&bytes).await {
                        tracing::debug!(
                            "Plugin decoded stream write to FFmpeg interrupted: {}",
                            error
                        );
                        break;
                    }
                }
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        message_key = "media.plugin_stream.read_failed",
                        message_params = %serde_json::json!({ "error": error.to_string() }),
                        "Plugin decode stream read failed"
                    );
                    break;
                }
            }
        }
        let _ = stdin.shutdown().await;
    });

    if let Some(mut stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut buffer = String::new();
            if stderr.read_to_string(&mut buffer).await.is_ok() && !buffer.is_empty() {
                tracing::warn!("FFmpeg stderr: {}", buffer);
            }
        });
    }

    let stream = crate::core::audio::AudioService::output_stream(child)?;
    let body = Body::from_stream(stream);
    use axum::http::header;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (
                "Cross-Origin-Resource-Policy".parse().unwrap(),
                "cross-origin".to_string(),
            ),
        ],
        body,
    )
        .into_response())
}

/// Handler for GET /api/stream/:chapterId - Stream chapter audio
pub async fn stream_chapter(
    State(state): State<AppState>,
    Path(chapter_id): Path<String>,
    Query(params): Query<StreamQuery>,
    method: axum::http::Method,
    headers: axum::http::HeaderMap,
    user: Option<AuthUser>,
) -> Result<axum::response::Response> {
    use axum::http::header;

    if let Some(_token) = &params.token {
        // Token validation would go here
    }

    let is_head_request = method == axum::http::Method::HEAD;

    let chapter = state
        .chapter_repo
        .find_by_id(&chapter_id)
        .await?
        .ok_or_else(|| TingError::NotFound(format!("Chapter {} not found", chapter_id)))?;

    let book = state
        .book_repo
        .find_by_id(&chapter.book_id)
        .await?
        .ok_or_else(|| TingError::NotFound(format!("Book {} not found", chapter.book_id)))?;

    ensure_user_can_stream_book(&state, user.as_ref(), &book.id).await?;

    let library = state
        .library_repo
        .find_by_id(&book.library_id)
        .await?
        .ok_or_else(|| TingError::NotFound(format!("Library {} not found", book.library_id)))?;

    let ext = std::path::Path::new(&chapter.path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let is_download_request = is_download_query(&params);

    // STRM playback redirects to the source unless transcoding was requested.
    if ext == "strm" {
        if !is_download_request {
            preload::cancel_auto_preload(&state, user.as_ref()).await;
        }
        return strm::handle_strm_stream(state, &chapter_id, chapter, book, library, &params).await;
    }
    if params.transcode.is_some() && !is_download_request {
        preload::cancel_auto_preload(&state, user.as_ref()).await;
    }
    // Handle HLS Transcoding Request
    if let Some(format) = &params.transcode
        && format == "hls"
    {
        tracing::info!("Requested HLS transcoding: {}", chapter.path);
        return handle_hls_request(
            state,
            chapter,
            book,
            library,
            ext == "strm",
            params.seek.clone(),
        )
        .await;
    }

    // Handle Transcoding Request
    if let Some(format) = &params.transcode {
        tracing::info!("Requested transcoding: {} -> {}", chapter.path, format);

        let content_type = match format.as_str() {
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            _ => {
                return Err(TingError::InvalidRequest(
                    "Unsupported transcode format".to_string(),
                ));
            }
        };

        let cache_path = state.cache_manager.get_cache_path(&chapter_id);
        let plugin_handles_format = state
            .plugin_manager
            .has_format_operation(
                std::path::Path::new(&chapter.path),
                FormatOperation::OpenDecrypt,
            )
            .await?;

        // Check if we can use direct URL transcoding (for WebDAV or cached files).
        // Plugin-backed formats must be decoded/decrypted before FFmpeg sees them.
        let can_use_direct_url =
            library.library_type != "local" && !cache_path.exists() && !plugin_handles_format;

        if can_use_direct_url {
            // WebDAV files: Use direct URL transcoding (same as STRM)
            // Build the WebDAV URL with authentication
            let mut webdav_url =
                if chapter.path.starts_with("http://") || chapter.path.starts_with("https://") {
                    // Parse existing URL
                    url::Url::parse(&chapter.path)
                        .map_err(|e| TingError::ValidationError(e.to_string()))?
                } else {
                    // Construct URL from library config
                    let base_url = url::Url::parse(&library.url)
                        .map_err(|e| TingError::ValidationError(e.to_string()))?;
                    let mut url = base_url.clone();

                    let root = library.root_path.as_str();
                    let root = if root.is_empty() { "/" } else { root };
                    let root_trimmed = root.trim_matches('/');
                    let rel_trimmed = chapter.path.trim_matches('/');
                    let full_path_str = if root_trimmed.is_empty() {
                        rel_trimmed.to_string()
                    } else {
                        format!("{}/{}", root_trimmed, rel_trimmed)
                    };

                    let decoded_path = urlencoding::decode(&full_path_str)
                        .map_err(|e| TingError::ValidationError(e.to_string()))?;

                    {
                        let mut segments = url
                            .path_segments_mut()
                            .map_err(|_| TingError::ValidationError("Invalid URL".to_string()))?;
                        for segment in decoded_path.split('/') {
                            if !segment.is_empty() {
                                segments.push(segment);
                            }
                        }
                    }

                    url
                };

            // Add authentication to URL if present
            if let (Some(username), Some(password)) = (&library.username, &library.password) {
                let decrypted_password =
                    crate::core::crypto::decrypt(password, state.encryption_key.as_ref())
                        .unwrap_or_else(|_| password.clone());
                webdav_url.set_username(username).ok();
                webdav_url.set_password(Some(&decrypted_password)).ok();
            }

            let webdav_url_str = webdav_url.to_string();

            tracing::info!("Transcoding from direct URL: {}", webdav_url_str);

            // Get duration using FFprobe

            // Add delay to avoid overwhelming the server
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;

            let duration_output = crate::core::audio::AudioService::ffprobe_command()?
                .arg("-v")
                .arg("error")
                .arg("-show_entries")
                .arg("format=duration")
                .arg("-of")
                .arg("default=noprint_wrappers=1:nokey=1")
                .arg(&webdav_url_str)
                .output()
                .await;

            let duration_seconds = if let Ok(output) = duration_output {
                if output.status.success() {
                    let duration_str = String::from_utf8_lossy(&output.stdout);
                    duration_str.trim().parse::<f64>().ok()
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(dur) = duration_seconds {
                tracing::info!("Audio duration: {:.2} seconds", dur);

                // Update chapter duration in database if significantly different
                if let Ok(Some(mut chapter_record)) =
                    state.chapter_repo.find_by_id(&chapter_id).await
                {
                    let db_duration = chapter_record.duration.unwrap_or(0);
                    let new_duration = dur.round() as i32;
                    if (db_duration - new_duration).abs() > 2 {
                        tracing::info!(
                            "Updated chapter duration: {} -> {} seconds",
                            db_duration,
                            new_duration
                        );
                        chapter_record.duration = Some(new_duration);
                        let _ = state.chapter_repo.update(&chapter_record).await;
                    }
                }
            }

            // Build FFmpeg command to transcode directly from URL
            let mut cmd = crate::core::audio::AudioService::ffmpeg_command()?;
            cmd.arg("-y").arg("-loglevel").arg("warning");

            // Add seek parameter if present (must be before -i for input seeking)
            if let Some(seek_time) = &params.seek {
                cmd.arg("-ss").arg(seek_time);
                tracing::info!("Seeking to position: {}", seek_time);
            }

            // Use URL as input directly (FFmpeg will handle HTTP/HTTPS)
            cmd.arg("-i").arg(&webdav_url_str);

            // Add transcoding parameters
            if format == "mp3" {
                cmd.arg("-fflags")
                    .arg("+genpts+igndts")
                    .arg("-avoid_negative_ts")
                    .arg("make_zero")
                    .arg("-acodec")
                    .arg("libmp3lame")
                    .arg("-b:a")
                    .arg("128k")
                    .arg("-ac")
                    .arg("2")
                    .arg("-ar")
                    .arg("44100")
                    .arg("-vn")
                    .arg("-map")
                    .arg("0:a:0")
                    .arg("-f")
                    .arg("mp3");
            } else if format == "wav" {
                cmd.arg("-vn").arg("-map").arg("0:a:0").arg("-f").arg("wav");
            }

            cmd.arg("pipe:1"); // Output to stdout

            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());

            tracing::info!("Starting FFmpeg process reading directly from URL...");

            // Spawn FFmpeg process
            let mut child = cmd.spawn().map_err(TingError::IoError)?;

            let stderr = child.stderr.take();

            // Log FFmpeg errors
            if let Some(mut stderr) = stderr {
                tokio::spawn(async move {
                    let mut buffer = String::new();
                    use tokio::io::AsyncReadExt;
                    if stderr.read_to_string(&mut buffer).await.is_ok() && !buffer.is_empty() {
                        tracing::warn!("FFmpeg stderr: {}", buffer);
                    }
                });
            }

            // Create streaming response from FFmpeg stdout
            let stream = crate::core::audio::AudioService::output_stream(child)?;
            let body = Body::from_stream(stream);

            // Build response with duration header if available
            if let Some(dur) = duration_seconds {
                return Ok((
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, content_type.to_string()),
                        (
                            "Cross-Origin-Resource-Policy".parse().unwrap(),
                            "cross-origin".to_string(),
                        ),
                        ("X-Audio-Duration".parse().unwrap(), dur.to_string()),
                    ],
                    body,
                )
                    .into_response());
            } else {
                return Ok((
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, content_type.to_string()),
                        (
                            "Cross-Origin-Resource-Policy".parse().unwrap(),
                            "cross-origin".to_string(),
                        ),
                    ],
                    body,
                )
                    .into_response());
            }
        }

        if plugin_handles_format {
            return transcode_plugin_stream(
                &state,
                &chapter,
                &library,
                PluginTranscodeOptions {
                    format,
                    content_type,
                    seek: params.seek.as_deref(),
                },
            )
            .await;
        }

        // Streaming MP3 output may not expose a finite duration to clients.
        // Recover missing metadata from the full local/cache source before seeking.
        let mut duration_seconds = chapter
            .duration
            .filter(|duration| *duration > 0)
            .map(f64::from);
        if duration_seconds.is_none() {
            let source_path = if cache_path.exists() {
                Some(cache_path.as_path())
            } else if library.library_type == "local" {
                Some(std::path::Path::new(&chapter.path))
            } else {
                None
            };
            if let Some(source_path) = source_path {
                match crate::core::audio::AudioService::probe_duration(source_path).await {
                    Ok(Some(duration)) => {
                        duration_seconds = Some(duration);
                        if let Ok(Some(mut chapter_record)) =
                            state.chapter_repo.find_by_id(&chapter_id).await
                            && chapter_record.duration.unwrap_or(0) <= 0
                        {
                            chapter_record.duration = Some(duration.round() as i32);
                            if let Err(error) = state.chapter_repo.update(&chapter_record).await {
                                tracing::warn!(
                                    chapter_id = %chapter_id,
                                    %error,
                                    "Failed to save transcoded source duration"
                                );
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => tracing::warn!(
                        chapter_id = %chapter_id,
                        %error,
                        "Failed to detect transcoded source duration"
                    ),
                }
            }
        }

        let mut cmd = crate::core::audio::AudioService::ffmpeg_command()?;
        cmd.arg("-y").arg("-loglevel").arg("error");

        if let Some(seek_time) = &params.seek {
            cmd.arg("-ss").arg(seek_time);
        }

        cmd.arg("-i");

        // Input Source
        if cache_path.exists() {
            cmd.arg(cache_path.to_string_lossy().as_ref());
        } else if library.library_type == "local" {
            cmd.arg(&chapter.path);
        } else {
            // Pipe input
            cmd.arg("-");
            cmd.stdin(Stdio::piped());
        }

        if format == "mp3" {
            cmd.arg("-fflags")
                .arg("+genpts+igndts")
                .arg("-avoid_negative_ts")
                .arg("make_zero")
                .arg("-acodec")
                .arg("libmp3lame")
                .arg("-b:a")
                .arg("128k")
                .arg("-ac")
                .arg("2")
                .arg("-ar")
                .arg("44100")
                .arg("-vn")
                .arg("-map")
                .arg("0:a:0");
        }

        cmd.arg("-f").arg(format).arg("-");

        cmd.stdout(Stdio::piped());

        let mut child = cmd.spawn().map_err(TingError::IoError)?;

        // Handle input pipe if needed (Only if we are using the fallback pipe logic)
        let use_pipe = !cache_path.exists() && library.library_type != "local";
        if use_pipe
            && child.stdin.is_some()
            && let Some(mut stdin) = child.stdin.take()
        {
            // Get reader
            let (mut reader, _) =
                get_remote_media_reader(&state, &library, &chapter.path, None).await?;

            tokio::spawn(async move {
                if let Err(e) = tokio::io::copy(&mut reader, &mut stdin).await {
                    tracing::error!(
                        error = %e,
                        message_key = "media.ffmpeg.pipe_failed",
                        message_params = %serde_json::json!({ "error": e.to_string() }),
                        "Failed to pipe input to FFmpeg"
                    );
                }
            });
        }

        let stream = crate::core::audio::AudioService::output_stream(child)?;
        let body = Body::from_stream(stream);

        let mut response = (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type.to_string()),
                (
                    "Cross-Origin-Resource-Policy".parse().unwrap(),
                    "cross-origin".to_string(),
                ),
            ],
            body,
        )
            .into_response();
        if let Some(duration) = duration_seconds {
            response
                .headers_mut()
                .insert("X-Audio-Duration", duration.to_string().parse().unwrap());
        }
        return Ok(response);
    }

    // ... existing code ...

    if !is_download_request && !params.preload.unwrap_or(false) {
        preload::maybe_spawn_auto_preload(&state, user.as_ref(), &book, &chapter_id, &library)
            .await;
    }
    let cached = preload::cached_chapter(&state, &chapter_id).await;
    let cached_size_unknown = cached
        .as_ref()
        .is_some_and(|entry| entry.total_size.is_none());
    let plugin_handles_format = if cached.is_some() {
        state
            .plugin_manager
            .has_format_operation(
                std::path::Path::new(&chapter.path),
                FormatOperation::OpenDecrypt,
            )
            .await?
    } else {
        false
    };
    // 1. Serve a bounded memory prefix, then stream the source remainder.
    if let Some(entry) = cached
        && !plugin_handles_format
    {
        let tail_state = state.clone();
        let tail_library = library.clone();
        let tail_path = chapter.path.clone();
        let tail_chapter_id = chapter_id.clone();
        let response = preload::prefix_response(
            entry,
            &state.audio_streamer,
            headers
                .get(header::RANGE)
                .and_then(|value| value.to_str().ok()),
            stream_mime_type_from_path(&chapter.path),
            is_head_request,
            move |start, end| async move {
                let cache_path = tail_state.cache_manager.get_cache_path(&tail_chapter_id);
                if cache_path.exists() || tail_library.library_type == "local" {
                    let path = if cache_path.exists() {
                        cache_path
                    } else {
                        std::path::PathBuf::from(tail_path)
                    };
                    let (file, size) = tail_state
                        .storage_service
                        .get_local_reader(&path, Some((start, end)))
                        .await?;
                    Ok((
                        Box::new(file) as Box<dyn tokio::io::AsyncRead + Send + Unpin>,
                        size,
                    ))
                } else {
                    get_remote_media_reader(
                        &tail_state,
                        &tail_library,
                        &tail_path,
                        Some((start, end)),
                    )
                    .await
                }
            },
        )?;
        if let Some(response) = response {
            tracing::debug!(target: "media", chapter_id = %chapter_id, "Serving preloaded prefix with streaming remainder");
            return Ok(response);
        }
    }

    // 2. Check Disk Cache
    let cache_path = state.cache_manager.get_cache_path(&chapter_id);
    if cache_path.exists() {
        tracing::debug!(target: "media", chapter_id = %chapter_id, "Serving from disk cache");

        // Check if we need to use a format plugin even for cached files (source file is cached)
        let plugin_handles_format = state
            .plugin_manager
            .has_format_operation(
                std::path::Path::new(&chapter.path),
                FormatOperation::OpenDecrypt,
            )
            .await?;

        if plugin_handles_format {
            // If a plugin handles this format, we use the cached file as the source for the plugin logic
            // instead of serving it directly.
            tracing::info!(
                chapter_id = %chapter_id,
                "Cached file requires format plugin processing"
            );

            // Fall through to the plugin handling logic below
            // We need to make sure the logic below knows to use the cache_path as source
            // This is handled by the `if cache_path.exists()` checks in the plugin block
        } else {
            let file_size = tokio::fs::metadata(&cache_path).await?.len();
            let mime_type = stream_mime_type_from_path(&chapter.path);

            let range_header = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
            if let Some(range_str) = range_header
                && let Ok(range) = state
                    .audio_streamer
                    .parse_range_header(range_str, file_size)
            {
                let content_length = range.end - range.start;
                let mut file = tokio::fs::File::open(&cache_path).await?;
                file.seek(std::io::SeekFrom::Start(range.start)).await?;
                let body = if is_head_request {
                    Body::empty()
                } else {
                    Body::from_stream(ReaderStream::new(file.take(content_length)))
                };

                return Ok((
                    StatusCode::PARTIAL_CONTENT,
                    [
                        (header::CONTENT_TYPE, mime_type.clone()),
                        (header::CONTENT_LENGTH, content_length.to_string()),
                        (
                            header::CONTENT_RANGE,
                            format!("bytes {}-{}/{}", range.start, range.end - 1, file_size),
                        ),
                        (header::ACCEPT_RANGES, "bytes".to_string()),
                        (
                            "Cross-Origin-Resource-Policy".parse().unwrap(),
                            "cross-origin".to_string(),
                        ),
                    ],
                    body,
                )
                    .into_response());
            }

            let file = tokio::fs::File::open(&cache_path).await?;
            let stream = ReaderStream::new(file);
            let body = Body::from_stream(stream);
            return Ok((
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime_type),
                    (header::CONTENT_LENGTH, file_size.to_string()),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (
                        "Cross-Origin-Resource-Policy".parse().unwrap(),
                        "cross-origin".to_string(),
                    ),
                ],
                if is_head_request { Body::empty() } else { body },
            )
                .into_response());
        }
    }

    // 3. Not cached. Fetch from source.
    tracing::debug!(target: "media", chapter_id = %chapter_id, "Serving from source stream");

    // Determine if we need to use a format plugin
    // Instead of hardcoding extensions, we ask the plugin manager if any loaded plugin supports this extension
    let plugin_handles_format = state
        .plugin_manager
        .has_format_operation(
            std::path::Path::new(&chapter.path),
            FormatOperation::OpenDecrypt,
        )
        .await?;

    if plugin_handles_format {
        tracing::info!(chapter_id = %chapter_id, "Processing file with format plugin");

        let range_header = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
        let (stream, mime_type, output_extension, content_length, start, end, logic_size) =
            create_decrypted_stream(
                &state,
                &chapter,
                &library,
                range_header.map(|s| s.to_string()),
            )
            .await?;
        let download_extension = output_extension.unwrap_or_else(|| {
            if mime_type.contains("mpeg") || mime_type.contains("mp3") {
                "mp3".to_string()
            } else if mime_type.contains("flac") {
                "flac".to_string()
            } else if mime_type.contains("ogg") {
                "ogg".to_string()
            } else if mime_type.contains("wav") {
                "wav".to_string()
            } else {
                "m4a".to_string()
            }
        });

        let body = Body::from_stream(stream);

        if range_header.is_some() {
            let content_length = content_length.ok_or_else(|| {
                TingError::PluginExecutionError(
                    "Format stream returned a range without a known length".into(),
                )
            })?;
            let logic_size = logic_size.ok_or_else(|| {
                TingError::PluginExecutionError(
                    "Format stream returned a range without a known length".into(),
                )
            })?;
            let end_inclusive = if end > 0 { end.saturating_sub(1) } else { 0 };
            return Ok((
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_TYPE, mime_type.to_string()),
                    (header::CONTENT_LENGTH, content_length.to_string()),
                    (
                        header::CONTENT_RANGE,
                        format!("bytes {}-{}/{}", start, end_inclusive, logic_size),
                    ),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (
                        "X-Download-Extension".parse().unwrap(),
                        download_extension.clone(),
                    ),
                    (
                        "Cross-Origin-Resource-Policy".parse().unwrap(),
                        "cross-origin".to_string(),
                    ),
                ],
                if is_head_request { Body::empty() } else { body },
            )
                .into_response());
        } else {
            if let Some(content_length) = content_length {
                return Ok((
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, mime_type.to_string()),
                        (header::CONTENT_LENGTH, content_length.to_string()),
                        (header::ACCEPT_RANGES, "bytes".to_string()),
                        ("X-Download-Extension".parse().unwrap(), download_extension),
                        (
                            "Cross-Origin-Resource-Policy".parse().unwrap(),
                            "cross-origin".to_string(),
                        ),
                    ],
                    if is_head_request { Body::empty() } else { body },
                )
                    .into_response());
            }
            return Ok((
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime_type.to_string()),
                    ("X-Download-Extension".parse().unwrap(), download_extension),
                    (
                        "Cross-Origin-Resource-Policy".parse().unwrap(),
                        "cross-origin".to_string(),
                    ),
                ],
                if is_head_request { Body::empty() } else { body },
            )
                .into_response());
        }
    }

    // Non-encrypted: Stream directly
    let range_header = headers.get(header::RANGE).and_then(|v| v.to_str().ok());

    // A suffix range needs the total length before its starting offset is known.
    // Keep the full reader if the source cannot advertise that length.
    let mut remote_probe = None;
    let mut range = if let Some(r) = range_header {
        if library.library_type == "local" {
            let file_size = tokio::fs::metadata(std::path::Path::new(&chapter.path))
                .await?
                .len();
            let parsed = state.audio_streamer.parse_range_header(r, file_size)?;
            Some((parsed.start, parsed.end))
        } else if cached_size_unknown || r.starts_with("bytes=-") {
            let (reader, total) =
                get_remote_media_reader(&state, &library, &chapter.path, None).await?;
            if total > 0 {
                let parsed = state.audio_streamer.parse_range_header(r, total)?;
                Some((parsed.start, parsed.end))
            } else {
                remote_probe = Some((reader, total));
                None
            }
        } else {
            let (start, end) = r
                .strip_prefix("bytes=")
                .and_then(|value| value.split_once('-'))
                .ok_or_else(|| TingError::InvalidRequest("Invalid Range header".into()))?;
            let start = start
                .parse::<u64>()
                .map_err(|_| TingError::InvalidRequest("Invalid Range start".into()))?;
            let end = if end.is_empty() {
                0
            } else {
                let inclusive_end = end
                    .parse::<u64>()
                    .map_err(|_| TingError::InvalidRequest("Invalid Range end".into()))?;
                if inclusive_end < start {
                    return Err(TingError::InvalidRequest("Invalid Range bounds".into()));
                }
                inclusive_end.saturating_add(1)
            };
            Some((start, end))
        }
    } else {
        None
    };

    let (mut reader, mut total_size) = if library.library_type == "local" {
        let (f, size) = state
            .storage_service
            .get_local_reader(std::path::Path::new(&chapter.path), range)
            .await
            .map_err(|e| TingError::NotFound(format!("Local file not found: {}", e)))?;
        (
            Box::new(f) as Box<dyn tokio::io::AsyncRead + Send + Unpin>,
            size,
        )
    } else if let Some(probe) = remote_probe {
        probe
    } else {
        get_remote_media_reader(&state, &library, &chapter.path, range).await?
    };

    // A source without a total length cannot resolve suffix/open-ended ranges.
    // Ignore Range and stream the full representation without inventing a length.
    if total_size == 0 && range.is_some() && library.library_type != "local" {
        (reader, total_size) =
            get_remote_media_reader(&state, &library, &chapter.path, None).await?;
        range = None;
    }

    if range.is_some() && total_size > 0 {
        let parsed = state
            .audio_streamer
            .parse_range_header(range_header.unwrap(), total_size)?;
        range = Some((parsed.start, parsed.end));
    }

    // Calculate actual content length and range for response headers
    let start = range.map(|r| r.0).unwrap_or(0);
    let end = if let Some(r) = range {
        if r.1 > 0 {
            std::cmp::min(r.1, total_size)
        } else {
            total_size
        }
    } else {
        total_size
    };

    let content_length = end.saturating_sub(start);

    // For local files, we need to limit the reader if a specific end was requested
    if total_size > 0 && content_length < total_size.saturating_sub(start) {
        reader = Box::new(reader.take(content_length));
    }

    // Convert AsyncRead to Stream
    let stream = ReaderStream::new(reader);
    let body = Body::from_stream(stream);

    let mime_type = stream_mime_type_from_path(&chapter.path);

    if range.is_some() {
        let content_range = format!("bytes {}-{}/{}", start, end.saturating_sub(1), total_size);

        Ok((
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, mime_type),
                (header::CONTENT_LENGTH, content_length.to_string()),
                (header::CONTENT_RANGE, content_range),
                (header::ACCEPT_RANGES, "bytes".to_string()),
                (
                    "Cross-Origin-Resource-Policy".parse().unwrap(),
                    "cross-origin".to_string(),
                ),
            ],
            if is_head_request { Body::empty() } else { body },
        )
            .into_response())
    } else {
        let mut response = (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, mime_type),
                (header::ACCEPT_RANGES, "bytes".to_string()),
                (
                    "Cross-Origin-Resource-Policy".parse().unwrap(),
                    "cross-origin".to_string(),
                ),
            ],
            if is_head_request { Body::empty() } else { body },
        )
            .into_response();
        if total_size > 0 || library.library_type == "local" {
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, total_size.into());
        }
        Ok(response)
    }
}

/// Handler for GET /api/v1/public/media/:chapterId - Stream signed public chapter audio.
pub async fn stream_signed_chapter(
    State(state): State<AppState>,
    Path(chapter_id): Path<String>,
    Query(params): Query<SignedStreamQuery>,
    method: axum::http::Method,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Response> {
    validate_signed_stream_query(&state, &chapter_id, &params)?;

    let user_id = params.user.as_deref().unwrap_or_default();
    let signed_user = state.user_repo.find_by_id(user_id).await?.ok_or_else(|| {
        TingError::PermissionDenied("Signed media route user not found".to_string())
    })?;
    let auth_user = AuthUser {
        user_id: signed_user.id.clone(),
        id: signed_user.id,
        username: signed_user.username,
        role: signed_user.role,
    };

    stream_chapter(
        State(state),
        Path(chapter_id),
        Query(StreamQuery {
            token: None,
            transcode: params.transcode,
            seek: params.seek,
            download: params.download,
            preload: None,
        }),
        method,
        headers,
        Some(auth_user),
    )
    .await
}

fn validate_signed_stream_query(
    state: &AppState,
    chapter_id: &str,
    params: &SignedStreamQuery,
) -> Result<()> {
    let expires = params
        .expires
        .ok_or_else(|| TingError::PermissionDenied("Missing signed media expiry".to_string()))?;
    if signature_has_expired(expires) {
        return Err(TingError::PermissionDenied(
            "Signed media URL has expired".to_string(),
        ));
    }

    let user_id = params
        .user
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| TingError::PermissionDenied("Missing signed media user".to_string()))?;
    let signature = params
        .signature
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| TingError::PermissionDenied("Missing signed media signature".to_string()))?;
    let download = signed_download_flag(params.download.as_deref());
    let expected = sign_media_stream_request(
        state.encryption_key.as_ref(),
        chapter_id,
        expires,
        user_id,
        params.transcode.as_deref(),
        params.seek.as_deref(),
        download,
    );

    if !constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
        return Err(TingError::PermissionDenied(
            "Invalid signed media signature".to_string(),
        ));
    }

    Ok(())
}

async fn ensure_user_can_stream_book(
    state: &AppState,
    user: Option<&AuthUser>,
    book_id: &str,
) -> Result<()> {
    let user = user.ok_or_else(|| TingError::AuthenticationError("用户未认证".to_string()))?;
    let is_admin = user.role == "admin";
    let can_access = state
        .book_repo
        .check_access(book_id, &user.id, is_admin)
        .await?;
    if can_access {
        Ok(())
    } else {
        Err(TingError::PermissionDenied(format!(
            "User cannot access book {}",
            book_id
        )))
    }
}

fn signed_download_flag(value: Option<&str>) -> bool {
    value
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}
