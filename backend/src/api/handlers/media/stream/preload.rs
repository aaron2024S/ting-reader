use crate::api::handlers::AppState;
use crate::auth::middleware::AuthUser;
use crate::db::models::{Book, Chapter, Library};
use ting_plugin_contract::format::FormatOperation;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const PRELOAD_WINDOW_BYTES: usize = 4 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
const CACHE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_CONCURRENT_PRELOADS: usize = 4;

static PRELOAD_PERMITS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CONCURRENT_PRELOADS);

#[derive(Clone, Debug)]
pub struct PreloadedChapter {
    pub(super) data: bytes::Bytes,
    pub(super) total_size: Option<u64>,
    last_access: std::time::Instant,
}

struct TemporaryCacheFile(std::path::PathBuf);

impl Drop for TemporaryCacheFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(super) async fn cancel_auto_preload(state: &AppState, user: Option<&AuthUser>) {
    if let Some(user) = user
        && let Some((_, task)) = state.active_preload_tasks.lock().await.remove(&user.id)
    {
        task.abort();
    }
}

pub(super) async fn maybe_spawn_auto_preload(
    state: &AppState,
    user: Option<&AuthUser>,
    book: &Book,
    chapter_id: &str,
    library: &Library,
) {
    let Some(user) = user else { return };
    let (auto_preload, auto_cache) =
        preload_preferences(state, &user.id, user.role == "admin").await;

    let next = if auto_preload || auto_cache {
        state
            .chapter_repo
            .find_by_book(&book.id)
            .await
            .ok()
            .and_then(|chapters| {
                chapters
                    .iter()
                    .position(|chapter| chapter.id == chapter_id)
                    .and_then(|index| chapters.get(index + 1).cloned())
            })
            .filter(|chapter| !chapter.path.to_ascii_lowercase().ends_with(".strm"))
    } else {
        None
    };

    let mut active = state.active_preload_tasks.lock().await;
    let Some(next) = next else {
        if let Some((_, task)) = active.remove(&user.id) {
            task.abort();
        }
        return;
    };
    if active
        .get(&user.id)
        .is_some_and(|(chapter, task)| chapter == &next.id && !task.is_finished())
    {
        return;
    }
    if let Some((_, task)) = active.remove(&user.id) {
        task.abort();
    }

    let task_state = state.clone();
    let task_library = library.clone();
    let next_id = next.id.clone();
    let user_id = user.id.clone();
    let task_user_id = user_id.clone();
    let is_admin = user.role == "admin";
    let handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let Ok(_permit) = PRELOAD_PERMITS.acquire().await else {
            return;
        };
        // A settings change may complete while this task is waiting for a permit.
        let (auto_preload, auto_cache) =
            preload_preferences(&task_state, &task_user_id, is_admin).await;
        if let Err(error) =
            preload_chapter(&task_state, &task_library, &next, auto_preload, auto_cache).await
        {
            tracing::warn!(chapter_id = %next.id, error = %error, "Next chapter preload failed");
        }
    });
    let task_id = handle.id();
    active.insert(user.id.clone(), (next_id, handle.abort_handle()));
    drop(active);

    let tasks = state.active_preload_tasks.clone();
    tokio::spawn(async move {
        let _ = handle.await;
        let mut active = tasks.lock().await;
        if active
            .get(&user_id)
            .is_some_and(|(_, task)| task.id() == task_id)
        {
            active.remove(&user_id);
        }
    });
}

async fn preload_preferences(state: &AppState, user_id: &str, is_admin: bool) -> (bool, bool) {
    let settings = state
        .settings_repo
        .get_by_user(user_id)
        .await
        .ok()
        .flatten();
    let settings_json = settings
        .and_then(|s| s.settings_json)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let auto_preload = settings_json
        .as_ref()
        .and_then(|s| s.get("auto_preload"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let auto_cache = is_admin
        && settings_json
            .as_ref()
            .and_then(|s| s.get("auto_cache"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

    (auto_preload, auto_cache)
}

async fn preload_chapter(
    state: &AppState,
    library: &Library,
    chapter: &Chapter,
    auto_preload: bool,
    auto_cache: bool,
) -> crate::core::app::error::Result<()> {
    // Format extensions may cache ciphertext or require a streaming transform.
    // Only prefetch raw bytes for formats served directly by the core.
    let auto_preload = auto_preload
        && !state
            .plugin_manager
            .has_format_operation(
                std::path::Path::new(&chapter.path),
                FormatOperation::OpenDecrypt,
            )
            .await?;
    if !auto_preload && !auto_cache {
        return Ok(());
    }
    if auto_preload && !auto_cache && state.preload_cache.read().await.contains_key(&chapter.id) {
        return Ok(());
    }

    let is_local = library.library_type.eq_ignore_ascii_case("local");
    let should_cache_to_disk = auto_cache && !is_local;
    if should_cache_to_disk {
        let cache_path = state.cache_manager.get_cache_path(&chapter.id);
        if !cache_path.exists() {
            let (reader, _) = chapter_reader(state, library, chapter).await?;
            let temp_path =
                cache_path.with_extension(format!("preload-{}.tmp", uuid::Uuid::new_v4()));
            let temporary = TemporaryCacheFile(temp_path.clone());
            let mut writer = tokio::fs::File::create(&temp_path).await?;
            let disk_limit = state.config.read().await.storage.max_disk_usage;
            let written =
                tokio::io::copy(&mut reader.take(disk_limit.saturating_add(1)), &mut writer)
                    .await?;
            if written > disk_limit {
                return Err(crate::core::app::error::TingError::InvalidRequest(
                    "Preload exceeds disk cache limit".to_string(),
                ));
            }
            writer.flush().await?;
            drop(writer);
            tokio::fs::rename(&temp_path, &cache_path).await?;
            drop(temporary);
            let config = state.config.read().await;
            state
                .cache_manager
                .enforce_limits(50, config.storage.max_disk_usage)
                .await
                .map_err(|error| {
                    crate::core::app::error::TingError::IoError(std::io::Error::other(
                        error.to_string(),
                    ))
                })?;
        }
        if auto_preload {
            let file = tokio::fs::File::open(&cache_path).await?;
            let size = file.metadata().await?.len();
            let entry = read_preload_prefix(file, size).await?;
            insert_preload(state, chapter.id.clone(), entry).await;
        }
    } else if auto_preload {
        let (reader, reported_size) = chapter_reader(state, library, chapter).await?;
        let entry = read_preload_prefix(reader, reported_size).await?;
        insert_preload(state, chapter.id.clone(), entry).await;
    }
    Ok(())
}

async fn read_preload_prefix(
    mut reader: impl AsyncRead + Unpin,
    reported_size: u64,
) -> std::io::Result<PreloadedChapter> {
    let mut total_size = (reported_size > 0).then_some(reported_size);
    // Read only the window; a full window is not proof that the source ended.
    // Fixed allocation keeps the in-flight memory budget independent of file size.
    let limit = total_size
        .unwrap_or(PRELOAD_WINDOW_BYTES as u64)
        .min(PRELOAD_WINDOW_BYTES as u64) as usize;
    let mut data = vec![0; limit];
    let mut length = 0;
    while length < data.len() {
        let read = reader.read(&mut data[length..]).await?;
        if read == 0 {
            if total_size.is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Preload source ended before its advertised length",
                ));
            }
            total_size = Some(length as u64);
            data.truncate(length);
            break;
        }
        length += read;
    }
    Ok(PreloadedChapter {
        // Release unused capacity for short sources so the byte budget is accurate.
        data: bytes::Bytes::from(data.into_boxed_slice()),
        total_size,
        last_access: std::time::Instant::now(),
    })
}

async fn chapter_reader(
    state: &AppState,
    library: &Library,
    chapter: &Chapter,
) -> crate::core::app::error::Result<(Box<dyn AsyncRead + Send + Unpin>, u64)> {
    if library.library_type.eq_ignore_ascii_case("local") {
        let (file, size) = state
            .storage_service
            .get_local_reader(std::path::Path::new(&chapter.path), None)
            .await?;
        Ok((Box::new(file), size))
    } else if library.library_type.eq_ignore_ascii_case("webdav") {
        state
            .storage_service
            .get_webdav_reader(library, &chapter.path, None, state.encryption_key.as_ref())
            .await
    } else {
        state
            .storage_service
            .get_http_reader(&chapter.path, None)
            .await
    }
}

async fn insert_preload(state: &AppState, chapter_id: String, entry: PreloadedChapter) {
    if entry.data.len() > PRELOAD_WINDOW_BYTES {
        return;
    }
    let mut cache = state.preload_cache.write().await;
    insert_preload_cache(&mut cache, chapter_id, entry);
}

pub(super) async fn cached_chapter(state: &AppState, chapter_id: &str) -> Option<PreloadedChapter> {
    let mut cache = state.preload_cache.write().await;
    let now = std::time::Instant::now();
    evict_expired_cache(&mut cache, now);
    let entry = cache.get_mut(chapter_id)?;
    entry.last_access = now;
    Some(entry.clone())
}

pub(super) fn prefix_response<F, Fut>(
    entry: PreloadedChapter,
    streamer: &crate::core::audio::AudioStreamer,
    range_header: Option<&str>,
    mime_type: String,
    is_head: bool,
    open_tail: F,
) -> crate::core::app::error::Result<Option<axum::response::Response>>
where
    F: FnOnce(u64, u64) -> Fut + Send + 'static,
    Fut: std::future::Future<
            Output = crate::core::app::error::Result<(Box<dyn AsyncRead + Send + Unpin>, u64)>,
        > + Send
        + 'static,
{
    use axum::body::Body;
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    use futures::{StreamExt, TryStreamExt, stream};

    // Suffix and open-ended ranges require the source's total length. Let the
    // ordinary source path handle requests when preloading could not learn it.
    if range_header.is_some() && entry.total_size.is_none() {
        return Ok(None);
    }
    let (start, end) = if let Some(range) = range_header {
        let total = entry.total_size.unwrap();
        match streamer.parse_range_header(range, total) {
            Ok(range) => (range.start, Some(range.end)),
            Err(_) => {
                return Ok(Some(
                    (
                        StatusCode::RANGE_NOT_SATISFIABLE,
                        [(header::CONTENT_RANGE, format!("bytes */{total}"))],
                        Body::empty(),
                    )
                        .into_response(),
                ));
            }
        }
    } else {
        (0, entry.total_size)
    };

    let prefix_end = end
        .unwrap_or(entry.data.len() as u64)
        .min(entry.data.len() as u64);
    let prefix = entry
        .data
        .slice(start.min(prefix_end) as usize..prefix_end as usize);
    let tail_start = start.max(entry.data.len() as u64);
    let has_tail = end.is_none_or(|end| tail_start < end);
    let body = if is_head {
        Body::empty()
    } else if has_tail {
        let expected_size = entry.total_size;
        let tail = stream::once(async move {
            let (reader, actual_size) = open_tail(tail_start, end.unwrap_or(0))
                .await
                .map_err(std::io::Error::other)?;
            if expected_size.is_some_and(|expected| actual_size > 0 && actual_size != expected) {
                return Err(std::io::Error::other(
                    "Preloaded source length changed before playback",
                ));
            }
            let reader = reader.take(end.map_or(u64::MAX, |end| end - tail_start));
            Ok(tokio_util::io::ReaderStream::new(reader))
        })
        .try_flatten();
        let prefix = stream::once(async move { Ok::<_, std::io::Error>(prefix) });
        // Opening the tail is lazy: the cached bytes reach the client first.
        // Dropping the response cancels the read; no producer task owns the body.
        Body::from_stream(prefix.chain(tail))
    } else {
        Body::from(prefix)
    };
    let mut response = body.into_response();
    *response.status_mut() = if range_header.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, mime_type.parse().unwrap());
    headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    headers.insert(
        "Cross-Origin-Resource-Policy",
        "cross-origin".parse().unwrap(),
    );
    if let Some(end) = end {
        headers.insert(header::CONTENT_LENGTH, (end - start).into());
        if range_header.is_some() {
            headers.insert(
                header::CONTENT_RANGE,
                format!("bytes {start}-{}/{}", end - 1, entry.total_size.unwrap())
                    .parse()
                    .unwrap(),
            );
        }
    }
    Ok(Some(response))
}

pub(crate) fn evict_expired_cache(
    cache: &mut std::collections::HashMap<String, PreloadedChapter>,
    now: std::time::Instant,
) {
    cache.retain(|_, entry| now.duration_since(entry.last_access) < CACHE_IDLE_TIMEOUT);
}

fn insert_preload_cache(
    cache: &mut std::collections::HashMap<String, PreloadedChapter>,
    chapter_id: String,
    entry: PreloadedChapter,
) {
    evict_expired_cache(cache, std::time::Instant::now());
    cache.remove(&chapter_id);
    let mut size: usize = cache.values().map(|entry| entry.data.len()).sum();
    while size + entry.data.len() > MAX_CACHE_BYTES {
        let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.last_access)
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        if let Some(evicted) = cache.remove(&oldest) {
            size -= evicted.data.len();
        }
    }
    cache.insert(chapter_id, entry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{StatusCode, header};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn entry(data: bytes::Bytes, total_size: Option<u64>) -> PreloadedChapter {
        PreloadedChapter {
            data,
            total_size,
            last_access: std::time::Instant::now(),
        }
    }

    #[test]
    fn cache_evicts_oldest_and_never_exceeds_byte_limit() {
        let mut cache = std::collections::HashMap::new();
        for index in 0..9 {
            insert_preload_cache(
                &mut cache,
                index.to_string(),
                entry(
                    bytes::Bytes::from(vec![0; PRELOAD_WINDOW_BYTES]),
                    Some(100 * PRELOAD_WINDOW_BYTES as u64),
                ),
            );
        }
        assert_eq!(cache.len(), 8);
        assert!(!cache.contains_key("0"));
        assert_eq!(
            cache.values().map(|entry| entry.data.len()).sum::<usize>(),
            MAX_CACHE_BYTES
        );
    }

    #[test]
    fn cache_expires_after_idle_timeout() {
        let mut cache = std::collections::HashMap::new();
        let now = std::time::Instant::now();
        cache.insert(
            "old".into(),
            PreloadedChapter {
                data: bytes::Bytes::from_static(b"a"),
                total_size: Some(1),
                last_access: now - CACHE_IDLE_TIMEOUT,
            },
        );
        cache.insert(
            "new".into(),
            entry(bytes::Bytes::from_static(b"b"), Some(1)),
        );
        evict_expired_cache(&mut cache, now);
        assert!(!cache.contains_key("old"));
        assert!(cache.contains_key("new"));
    }

    #[tokio::test]
    async fn preload_keeps_large_chapter_prefix_without_overreading() {
        let data = vec![7; PRELOAD_WINDOW_BYTES + 10];
        for reported_size in [0, data.len() as u64] {
            let mut reader = data.as_slice();
            let cached = read_preload_prefix(&mut reader, reported_size)
                .await
                .unwrap();
            assert_eq!(cached.data.as_ref(), &data[..PRELOAD_WINDOW_BYTES]);
            assert_eq!(reader.len(), 10);
            assert_eq!(
                cached.total_size,
                (reported_size > 0).then_some(reported_size)
            );
        }
    }

    #[tokio::test]
    async fn preload_distinguishes_complete_files_from_unknown_length_windows() {
        for reported_size in [0, 3] {
            let cached = read_preload_prefix(b"abc".as_slice(), reported_size)
                .await
                .unwrap();
            assert_eq!(cached.data, "abc");
            assert_eq!(cached.total_size, Some(3));
        }
        let data = vec![0; PRELOAD_WINDOW_BYTES];
        let cached = read_preload_prefix(data.as_slice(), data.len() as u64)
            .await
            .unwrap();
        assert_eq!(cached.total_size, Some(data.len() as u64));
        let cached = read_preload_prefix(data.as_slice(), 0).await.unwrap();
        assert_eq!(cached.total_size, None);
        assert_eq!(cached.data.len(), PRELOAD_WINDOW_BYTES);
        assert_eq!(
            read_preload_prefix(b"abc".as_slice(), 10)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof,
        );
    }

    #[tokio::test]
    async fn prefix_playback_preserves_full_bytes_and_range_headers() {
        let source = bytes::Bytes::from_static(b"abcdefghijklmnop");
        let streamer = crate::core::audio::AudioStreamer::new(Default::default());
        for (range, expected, tail_offset) in [
            (None, 0..16, Some(5)),
            (Some("bytes=0-2"), 0..3, None),
            (Some("bytes=3-8"), 3..9, Some(5)),
            (Some("bytes=5-7"), 5..8, Some(5)),
            (Some("bytes=9-12"), 9..13, Some(9)),
            (Some("bytes=4-"), 4..16, Some(5)),
            (Some("bytes=-3"), 13..16, Some(13)),
            (Some("bytes=-100"), 0..16, Some(5)),
            (Some("bytes=0-18446744073709551615"), 0..16, Some(5)),
        ] {
            let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorded = calls.clone();
            let tail = source.clone();
            let response = prefix_response(
                entry(source.slice(..5), Some(16)),
                &streamer,
                range,
                "audio/mpeg".into(),
                false,
                move |start, end| async move {
                    recorded.lock().unwrap().push((start, end));
                    Ok((
                        Box::new(std::io::Cursor::new(tail.slice(start as usize..)))
                            as Box<dyn AsyncRead + Send + Unpin>,
                        16,
                    ))
                },
            )
            .unwrap()
            .unwrap();
            assert!(calls.lock().unwrap().is_empty(), "tail must open lazily");
            assert_eq!(
                response.status(),
                if range.is_some() {
                    StatusCode::PARTIAL_CONTENT
                } else {
                    StatusCode::OK
                },
            );
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                (expected.end - expected.start).to_string(),
            );
            if range.is_some() {
                assert_eq!(
                    response.headers()[header::CONTENT_RANGE],
                    format!("bytes {}-{}/16", expected.start, expected.end - 1),
                );
            }
            let body = axum::body::to_bytes(response.into_body(), 100)
                .await
                .unwrap();
            assert_eq!(body, source.slice(expected.clone()), "{range:?}");
            let expected_calls = tail_offset
                .map(|start| vec![(start, expected.end as u64)])
                .unwrap_or_default();
            assert_eq!(*calls.lock().unwrap(), expected_calls, "{range:?}");
        }
    }

    #[tokio::test]
    async fn dropping_prefix_response_and_head_never_start_tail_download() {
        use futures::StreamExt;
        let streamer = crate::core::audio::AudioStreamer::new(Default::default());
        for is_head in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let recorded = calls.clone();
            let response = prefix_response(
                entry(bytes::Bytes::from_static(b"abc"), Some(10)),
                &streamer,
                None,
                "audio/mpeg".into(),
                is_head,
                move |_, _| async move {
                    recorded.fetch_add(1, Ordering::SeqCst);
                    Ok((
                        Box::new(tokio::io::empty()) as Box<dyn AsyncRead + Send + Unpin>,
                        10,
                    ))
                },
            )
            .unwrap()
            .unwrap();
            let mut stream = response.into_body().into_data_stream();
            if is_head {
                assert!(stream.next().await.is_none());
            } else {
                assert_eq!(stream.next().await.unwrap().unwrap(), "abc");
            }
            drop(stream);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn unknown_length_prefix_streams_remainder_without_false_content_length() {
        let streamer = crate::core::audio::AudioStreamer::new(Default::default());
        for tail in [b"def".as_slice(), b"".as_slice()] {
            let response = prefix_response(
                entry(bytes::Bytes::from_static(b"abc"), None),
                &streamer,
                None,
                "audio/mpeg".into(),
                false,
                move |start, end| async move {
                    assert_eq!((start, end), (3, 0));
                    Ok((Box::new(tail) as Box<dyn AsyncRead + Send + Unpin>, 0))
                },
            )
            .unwrap()
            .unwrap();
            assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
            assert!(!response.headers().contains_key(header::CONTENT_RANGE));
            let body = axum::body::to_bytes(response.into_body(), 100)
                .await
                .unwrap();
            assert_eq!(body.as_ref(), [b"abc".as_slice(), tail].concat());
        }
    }

    #[tokio::test]
    async fn changed_source_length_fails_instead_of_splicing_a_different_file() {
        let streamer = crate::core::audio::AudioStreamer::new(Default::default());
        let response = prefix_response(
            entry(bytes::Bytes::from_static(b"abc"), Some(10)),
            &streamer,
            None,
            "audio/mpeg".into(),
            false,
            |_, _| async {
                Ok((
                    Box::new(b"other".as_slice()) as Box<dyn AsyncRead + Send + Unpin>,
                    8,
                ))
            },
        )
        .unwrap()
        .unwrap();
        assert!(
            axum::body::to_bytes(response.into_body(), 100)
                .await
                .is_err()
        );
    }

    #[test]
    fn invalid_cached_ranges_return_416_without_reading_the_source() {
        let streamer = crate::core::audio::AudioStreamer::new(Default::default());
        for range in ["bytes=16-", "bytes=8-7", "bytes=-0", "bytes=0-1,3-4"] {
            let response = prefix_response(
                entry(bytes::Bytes::from_static(b"abcde"), Some(16)),
                &streamer,
                Some(range),
                "audio/mpeg".into(),
                false,
                |_, _| async { panic!("invalid range must not read the source") },
            )
            .unwrap()
            .unwrap();
            assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
            assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */16");
        }
    }
}
