use super::hls::{
    cached_generation, playlist_url, start_generation, valid_segment_filename, validate_seek,
};
use crate::api::state::AppState;
use crate::core::app::error::{Result, TingError};
use crate::db::repository::Repository;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use tokio_util::io::ReaderStream;

pub async fn get_hls_playlist(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Response> {
    serve_file(&state, &session_id, None, "playlist.m3u8").await
}

pub async fn get_hls_segment(
    State(state): State<AppState>,
    Path((session_id, filename)): Path<(String, String)>,
) -> Result<Response> {
    serve_file(&state, &session_id, None, &filename).await
}

pub async fn get_hls_file(
    State(state): State<AppState>,
    Path((session_id, generation, filename)): Path<(String, u32, String)>,
) -> Result<Response> {
    serve_file(&state, &session_id, Some(generation), &filename).await
}

async fn serve_file(
    state: &AppState,
    session_id: &str,
    seq: Option<u32>,
    filename: &str,
) -> Result<Response> {
    let is_playlist = filename == "playlist.m3u8";
    if !is_playlist && !valid_segment_filename(filename) {
        return Err(TingError::InvalidRequest("Invalid HLS filename".into()));
    }
    let session = state.hls_session_manager.get_session(session_id).await?;
    let mut session = session.lock().await;
    if session.closed {
        return Err(TingError::NotFound("HLS session closed".into()));
    }
    let generation = if let Some(seq) = seq {
        session.generations.iter().find(|g| g.seq == seq)
    } else {
        session.generations.back()
    }
    .cloned()
    .ok_or_else(|| TingError::NotFound("HLS generation not found".into()))?;
    if is_playlist
        && session
            .generations
            .back()
            .is_some_and(|g| g.seq == generation.seq)
        && let Some(process) = &session.process
    {
        process.check_input()?;
    }
    if is_playlist
        && session
            .generations
            .back()
            .is_some_and(|g| g.seq == generation.seq)
        && let Some(process) = session.process.as_mut()
        && let Some(status) = process.child.try_wait()?
        && !status.success()
    {
        return Err(TingError::ExternalError(format!(
            "HLS FFmpeg exited with {status}"
        )));
    }
    let path = generation.temp_dir.join(filename);
    if is_playlist {
        let content = tokio::fs::read_to_string(&path).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                TingError::NotFound("HLS playlist not ready".into())
            } else {
                TingError::IoError(error)
            }
        })?;
        // EVENT has a growing timeline; explicitly start at zero, not live edge.
        let content = content.replacen(
            "#EXTM3U",
            "#EXTM3U\n#EXT-X-START:TIME-OFFSET=0,PRECISE=YES",
            1,
        );
        return Ok((
            [
                (header::CONTENT_TYPE, "application/vnd.apple.mpegurl"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            content,
        )
            .into_response());
    }
    let file = tokio::fs::File::open(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            TingError::NotFound("HLS segment not found".into())
        } else {
            TingError::IoError(error)
        }
    })?;
    let length = file.metadata().await?.len();
    drop(session);
    Ok((
        [
            (header::CONTENT_TYPE, "video/mp2t".to_string()),
            (header::CONTENT_LENGTH, length.to_string()),
            (header::CACHE_CONTROL, "private, max-age=120".to_string()),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}

#[derive(Debug, serde::Deserialize)]
pub struct SeekQuery {
    pub seek: Option<f64>,
}

pub async fn seek_hls_stream(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(params): Query<SeekQuery>,
) -> Result<Response> {
    let target = params.seek.unwrap_or(0.0);
    validate_seek(target)?;
    let handle = state.hls_session_manager.get_session(&session_id).await?;
    let mut session = handle.lock().await;
    if session.closed {
        return Err(TingError::NotFound("HLS session closed".into()));
    }
    let cached = cached_generation(&session, target).await;
    let reused = cached.is_some();
    let result = async {
        if let Some(generation) = cached {
            Ok(generation)
        } else {
            let chapter = state
                .chapter_repo
                .find_by_id(&session.chapter_id)
                .await?
                .ok_or_else(|| TingError::NotFound("Chapter not found".into()))?;
            let library = state
                .library_repo
                .find_by_id(&session.library_id)
                .await?
                .ok_or_else(|| TingError::NotFound("Library not found".into()))?;
            start_generation(&state, &mut session, &chapter, &library, target).await
        }
    }
    .await;
    session.last_accessed = std::time::Instant::now();
    drop(session);
    let generation = match result {
        Ok(generation) => generation,
        Err(error) => {
            state.hls_session_manager.close_session(&session_id).await;
            return Err(error);
        }
    };
    Ok(axum::Json(serde_json::json!({
        "status": "seeked",
        "seek_time": target,
        "seq": generation.seq,
        "start_offset": generation.start_offset,
        "playback_time": target - generation.start_offset,
        "reused": reused,
        "playlist_url": playlist_url(&session_id, generation.seq)
    }))
    .into_response())
}

/// UUIDs are bearer credentials, like the playlist/segment URLs themselves.
pub async fn close_hls_stream(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> StatusCode {
    state.hls_session_manager.close_session(&session_id).await;
    StatusCode::NO_CONTENT
}

pub async fn touch_hls_stream(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<StatusCode> {
    state.hls_session_manager.get_session(&session_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
