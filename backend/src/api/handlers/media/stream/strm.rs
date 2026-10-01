use super::{StreamQuery, handle_hls_request};
use crate::api::handlers::AppState;
use crate::core::app::error::{Result, TingError};
use crate::db::models::{Book, Chapter, Library};
use crate::db::repository::Repository;
use axum::{
    body::Body,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::process::Stdio;
use tokio::io::AsyncReadExt;

pub(super) async fn handle_strm_stream(
    state: AppState,
    chapter_id: &str,
    chapter: Chapter,
    book: Book,
    library: Library,
    params: &StreamQuery,
) -> Result<Response> {
    use axum::http::header;
    // Read the URL from the file
    let url = if library.library_type == "local" {
        let file = tokio::fs::File::open(&chapter.path)
            .await
            .map_err(TingError::IoError)?;
        read_strm_url(file).await?
    } else if library.library_type == "webdav" {
        let (reader, _) = state
            .storage_service
            .get_webdav_reader(&library, &chapter.path, None, state.encryption_key.as_ref())
            .await
            .map_err(|e| TingError::NotFound(format!("Failed to read strm file: {}", e)))?;

        read_strm_url(reader).await?
    } else {
        let (reader, _) = state
            .storage_service
            .get_http_reader(&chapter.path, None)
            .await
            .map_err(|e| TingError::NotFound(format!("Failed to read strm URL: {}", e)))?;

        read_strm_url(reader).await?
    };

    validate_strm_url(&url)?;
    tracing::debug!(chapter_id, "Handling strm file");

    // Handle Transcoding Request for .strm files
    // Frontend will request transcoding via &transcode=mp3 when playback fails
    // Android app uses &transcode=hls which is handled by the general HLS handler below
    if let Some(format) = &params.transcode {
        // HLS transcoding for strm files is handled by the general HLS handler
        if format == "hls" {
            tracing::info!("strm file requested HLS transcoding; delegating to HLS handler");
            return handle_hls_request(
                state,
                chapter,
                book,
                library,
                true, // is_strm
                params.seek.clone(),
            )
            .await;
        }

        tracing::info!(chapter_id, format, "Transcoding strm audio");

        let content_type = match format.as_str() {
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            _ => {
                return Err(TingError::InvalidRequest(
                    "Unsupported transcode format".to_string(),
                ));
            }
        };

        // 优先使用数据库中的时长，避免重复调用 FFprobe
        let duration_seconds = if let Some(db_duration) = chapter.duration {
            if db_duration > 0 {
                tracing::debug!("Using database duration: {} seconds", db_duration);
                Some(db_duration as f64)
            } else {
                None
            }
        } else {
            None
        };

        // 只有在数据库中没有时长时才使用 FFprobe
        let duration_seconds = if duration_seconds.is_none() {
            tracing::info!("No duration in database; using FFprobe to get audio duration...");

            // Add delay to avoid overwhelming the server
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;

            let duration_output = crate::core::audio::AudioService::ffprobe_command()?
                .arg("-v")
                .arg("error")
                .arg("-show_entries")
                .arg("format=duration")
                .arg("-of")
                .arg("default=noprint_wrappers=1:nokey=1")
                .arg(&url)
                .output()
                .await;

            if let Ok(output) = duration_output {
                if output.status.success() {
                    let duration_str = String::from_utf8_lossy(&output.stdout);
                    let dur = duration_str.trim().parse::<f64>().ok();

                    if let Some(d) = dur {
                        tracing::info!("FFprobe detected duration: {:.2} seconds", d);

                        // 更新数据库中的时长
                        if let Ok(Some(mut chapter_record)) =
                            state.chapter_repo.find_by_id(chapter_id).await
                        {
                            let new_duration = d.round() as i32;
                            tracing::info!(
                                "Updated chapter duration in database: {} seconds",
                                new_duration
                            );
                            chapter_record.duration = Some(new_duration);
                            let _ = state.chapter_repo.update(&chapter_record).await;
                        }
                    }

                    dur
                } else {
                    let error = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!(
                        message_key = "ffprobe.duration_failed",
                        message_params = %serde_json::json!({ "error": error.to_string() }),
                        error = %error,
                        "FFprobe duration detection failed"
                    );
                    None
                }
            } else {
                tracing::warn!(message_key = "ffprobe.run_failed", "Failed to run FFprobe");
                None
            }
        } else {
            duration_seconds
        };

        tracing::info!("Using bundled FFmpeg to read directly from URL");

        // Build FFmpeg command to transcode directly from URL
        // This allows seeking support
        let mut cmd = crate::core::audio::AudioService::ffmpeg_command()?;
        cmd.arg("-y").arg("-loglevel").arg("warning");

        // Add seek parameter if present (must be before -i for input seeking)
        if let Some(seek_time) = &params.seek {
            cmd.arg("-ss").arg(seek_time);
            tracing::info!("Seeking to position: {}", seek_time);
        }

        // Use URL as input directly (FFmpeg will handle HTTP/HTTPS)
        cmd.arg("-i").arg(&url);

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

    Ok(redirect_strm_url(url))
}

fn redirect_strm_url(url: String) -> Response {
    use axum::http::header;
    tracing::info!("Redirecting strm playback to origin");
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, url),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".to_string()),
            (
                header::ACCESS_CONTROL_ALLOW_METHODS,
                "GET, HEAD, OPTIONS".to_string(),
            ),
            (
                header::ACCESS_CONTROL_EXPOSE_HEADERS,
                "Content-Length, Content-Range, Accept-Ranges".to_string(),
            ),
            (
                "Cross-Origin-Resource-Policy".parse().unwrap(),
                "cross-origin".to_string(),
            ),
        ],
        Body::empty(),
    )
        .into_response()
}

async fn read_strm_url(reader: impl tokio::io::AsyncRead + Unpin) -> Result<String> {
    let mut content = String::new();
    reader.take(65_537).read_to_string(&mut content).await?;
    if content.len() > 65_536 {
        return Err(TingError::InvalidRequest(
            "strm file exceeds 64 KiB".to_string(),
        ));
    }
    Ok(content.trim().to_string())
}

fn validate_strm_url(url: &str) -> Result<()> {
    let parsed_url = reqwest::Url::parse(url)
        .map_err(|_| TingError::InvalidRequest("Invalid strm URL".to_string()))?;
    if !matches!(parsed_url.scheme(), "http" | "https") || parsed_url.host_str().is_none() {
        return Err(TingError::InvalidRequest("Invalid strm URL".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strm_url_validation_accepts_credentials_and_signed_urls() {
        for url in [
            "https://audio.example.test/chapter.mp3",
            "https://user:pass@audio.example.test/chapter.mp3",
            "https://user@audio.example.test/chapter.mp3",
            "https://audio.example.test/chapter.mp3?token=abc&signature=xyz",
        ] {
            validate_strm_url(url).unwrap();
        }
        for url in ["javascript:alert(1)", "file:///chapter.mp3", "not a URL"] {
            assert!(validate_strm_url(url).is_err());
        }
    }

    #[tokio::test]
    async fn strm_direct_redirect_does_not_fetch_origin() {
        // An unreachable source still yields a redirect, including URL credentials.
        for url in [
            "http://127.0.0.1:9/never-requested.mp3",
            "http://user:pass@127.0.0.1:9/never-requested.mp3",
        ] {
            let response = redirect_strm_url(url.into());
            assert_eq!(response.status(), StatusCode::FOUND);
            assert_eq!(response.headers()["location"], url);
            assert_eq!(response.headers()["cache-control"], "private, no-store");
            assert_eq!(response.headers()["access-control-allow-origin"], "*");
            assert_eq!(
                response.headers()["access-control-allow-methods"],
                "GET, HEAD, OPTIONS"
            );
            assert!(
                axum::body::to_bytes(response.into_body(), 1)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn strm_reader_rejects_oversize_even_with_whitespace() {
        let oversized = format!("https://audio.example.test/a.mp3{}", " ".repeat(65_536));
        assert!(read_strm_url(oversized.as_bytes()).await.is_err());
        assert_eq!(
            read_strm_url(b" https://audio.example.test/a.mp3\n".as_slice())
                .await
                .unwrap(),
            "https://audio.example.test/a.mp3",
        );
    }

    #[tokio::test]
    async fn credentialed_strm_redirect_preserves_url_without_requesting_media() {
        use axum::{Router, routing::get};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let recorded = hits.clone();
        let app = Router::new().route(
            "/audio.mp3",
            get(move || {
                recorded.fetch_add(1, Ordering::SeqCst);
                async { "audio" }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let url = format!("http://user:p%40ss@{address}/audio.mp3?signature=a%2Fb");
        validate_strm_url(&url).unwrap();
        let response = redirect_strm_url(url.clone());
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers()["location"], url);
        assert!(
            axum::body::to_bytes(response.into_body(), 1)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        server.abort();
    }
}
