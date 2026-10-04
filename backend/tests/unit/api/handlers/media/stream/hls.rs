use super::super::hls_session::{HlsSessionManager, SESSION_IDLE_TIMEOUT};
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn manager(dir: &tempfile::TempDir) -> Arc<HlsSessionManager> {
    Arc::new(HlsSessionManager::new(dir.path().to_path_buf()))
}

fn encoder_fixture(mode: &str) -> tokio::process::Child {
    tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "core::audio::ffmpeg::tests::stream_process_fixture",
            "--nocapture",
        ])
        .env("TING_AUDIO_STREAM_TEST_MODE", mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

fn fixture_process(mode: &str) -> HlsProcess {
    HlsProcess {
        child: encoder_fixture(mode),
        input_task: None,
        stderr_task: None,
        input_error: Arc::new(AtomicBool::new(false)),
    }
}

#[test]
fn seek_accepts_only_finite_nonnegative_seconds() {
    assert_eq!(parse_seek(None).unwrap(), 0.0);
    assert_eq!(parse_seek(Some("120.5")).unwrap(), 120.5);
    for invalid in ["-1", "NaN", "inf", "-inf", "1e999", "invalid", ""] {
        assert!(parse_seek(Some(invalid)).is_err(), "{invalid}");
    }
}

#[test]
fn segment_names_cannot_reference_other_files_or_directories() {
    for valid in ["segment_000.ts", "segment_1000.ts"] {
        assert!(valid_segment_filename(valid));
    }
    for invalid in [
        "../segment_000.ts",
        "segment_../secret.ts",
        "segment_.ts",
        "playlist.m3u8",
        "segment_000.ts.tmp",
        "/segment_000.ts",
        "C:\\segment_000.ts",
    ] {
        assert!(!valid_segment_filename(invalid), "{invalid}");
    }
}

#[tokio::test]
async fn concurrent_startup_reservations_are_limited_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&dir);
    let requests = (0..16).map(|_| {
        let manager = manager.clone();
        async move {
            manager
                .create_session("chapter".into(), "library".into(), false)
                .await
        }
    });
    let results = futures::future::join_all(requests).await;
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 3);
    for pending in results.into_iter().flatten() {
        let id = pending.commit();
        manager.close_session(&id).await;
    }
    assert!(
        manager
            .create_session("chapter".into(), "library".into(), false)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn abandoned_initialization_releases_session_and_permit() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&dir);
    let pending = manager
        .create_session("chapter".into(), "library".into(), false)
        .await
        .unwrap();
    let id = pending.id.clone();
    let handle = pending.session.clone();
    drop(pending);
    tokio::time::timeout(Duration::from_secs(5), async {
        // Map removal precedes process/file cleanup and permit release.
        while !handle.lock().await.closed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(manager.get_session(&id).await.is_err());
    let sessions = futures::future::join_all(
        (0..3).map(|_| manager.create_session("chapter".into(), "library".into(), false)),
    )
    .await;
    assert!(sessions.iter().all(Result::is_ok));
}

struct Dropped(Arc<AtomicBool>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn close_reaps_encoder_cancels_input_and_removes_files() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&dir);
    let pending = manager
        .create_session("chapter".into(), "library".into(), false)
        .await
        .unwrap();
    let handle = pending.session.clone();
    let id = pending.commit();
    let generation = handle.lock().await.next_generation(0.0).await.unwrap();
    tokio::fs::write(generation.temp_dir.join("segment_000.ts"), b"audio")
        .await
        .unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Dropped(dropped.clone());
    let (ready, initialized) = tokio::sync::oneshot::channel();
    let input_task = tokio::spawn(async move {
        let _guard = guard;
        ready.send(()).unwrap();
        futures::future::pending::<()>().await;
    });
    initialized.await.unwrap();
    let mut process = fixture_process("running");
    let mut stderr = process.child.stderr.take().unwrap();
    process.input_task = Some(input_task);
    handle.lock().await.process = Some(process);
    manager.close_session(&id).await;
    assert!(dropped.load(Ordering::SeqCst));
    assert!(!generation.temp_dir.exists());
    assert!(manager.get_session(&id).await.is_err());
    tokio::time::timeout(Duration::from_secs(5), stderr.read_to_end(&mut Vec::new()))
        .await
        .unwrap()
        .unwrap();
    // Even a retained session Arc must not retain its reservation after close.
    let replacements = futures::future::join_all(
        (0..3).map(|_| manager.create_session("chapter".into(), "library".into(), false)),
    )
    .await;
    assert!(replacements.iter().all(Result::is_ok));
}

#[tokio::test]
async fn heartbeat_keeps_session_while_idle_cleanup_releases_expired_one() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&dir);
    let idle = manager
        .create_session("chapter".into(), "library".into(), false)
        .await
        .unwrap();
    let active = manager
        .create_session("chapter".into(), "library".into(), false)
        .await
        .unwrap();
    for pending in [&idle, &active] {
        pending.session.lock().await.last_accessed =
            Instant::now() - SESSION_IDLE_TIMEOUT - Duration::from_secs(1);
    }
    let idle_id = idle.commit();
    let active_id = active.commit();
    manager.get_session(&active_id).await.unwrap();
    manager.cleanup_expired().await;
    assert!(manager.get_session(&idle_id).await.is_err());
    assert!(manager.get_session(&active_id).await.is_ok());
    manager.close_session(&active_id).await;
}

#[tokio::test]
async fn seek_generations_do_not_overwrite_in_flight_segments_and_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&dir);
    let pending = manager
        .create_session("chapter".into(), "library".into(), false)
        .await
        .unwrap();
    let mut session = pending.session.lock().await;
    let first = session.next_generation(0.0).await.unwrap();
    let old_segment = first.temp_dir.join("segment_000.ts");
    tokio::fs::write(&old_segment, b"first generation")
        .await
        .unwrap();
    let second = session.next_generation(120.5).await.unwrap();
    tokio::fs::write(second.temp_dir.join("segment_000.ts"), b"second generation")
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(&old_segment).await.unwrap(),
        b"first generation"
    );
    assert_ne!(
        playlist_url(&pending.id, first.seq),
        playlist_url(&pending.id, second.seq)
    );
    for offset in [240.0, 360.0, 480.0] {
        session.next_generation(offset).await.unwrap();
    }
    assert_eq!(session.generations.len(), 3);
    assert!(!first.temp_dir.exists());
    drop(session);
    let id = pending.commit();
    manager.close_session(&id).await;
}

#[tokio::test]
async fn seek_reuses_only_complete_audio_in_the_current_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&dir);
    let pending = manager
        .create_session("chapter".into(), "library".into(), false)
        .await
        .unwrap();
    let mut session = pending.session.lock().await;
    let generation = session.next_generation(120.5).await.unwrap();
    assert!(cached_generation(&session, 120.5).await.is_none());
    tokio::fs::write(
        generation.temp_dir.join("playlist.m3u8"),
        "#EXTM3U\n#EXTINF:4.017,\nsegment_000.ts\n#EXTINF:4.0,\nsegment_001.ts\n",
    )
    .await
    .unwrap();
    let cached = cached_generation(&session, 123.0).await.unwrap();
    assert_eq!(cached.seq, generation.seq);
    assert_eq!(123.0 - cached.start_offset, 2.5);
    for target in [120.4, 128.517, 600.0] {
        assert!(
            cached_generation(&session, target).await.is_none(),
            "{target}"
        );
    }
    let new = session.next_generation(600.0).await.unwrap();
    // An old cache cannot be returned as an unfinished EVENT stream.
    assert!(cached_generation(&session, 123.0).await.is_none());
    assert_ne!(new.seq, generation.seq);
    drop(session);
    manager.close_session(&pending.commit()).await;
}

#[tokio::test]
async fn readiness_requires_a_published_playlist_and_complete_segment() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&dir);
    let pending = manager
        .create_session("chapter".into(), "library".into(), false)
        .await
        .unwrap();
    let mut session = pending.session.lock().await;
    let generation = session.next_generation(0.0).await.unwrap();
    session.process = Some(fixture_process("running"));
    let playlist = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXTINF:4.0,\nsegment_000.ts\n";
    tokio::fs::write(generation.temp_dir.join("playlist.m3u8"), playlist)
        .await
        .unwrap();
    tokio::fs::write(
        generation.temp_dir.join("segment_000.ts.tmp"),
        b"incomplete",
    )
    .await
    .unwrap();
    let result = wait_for_first_segment(
        &mut session,
        &generation.temp_dir,
        Instant::now() + Duration::from_millis(100),
    )
    .await;
    assert!(matches!(result, Err(TingError::Timeout(_))));
    tokio::fs::write(generation.temp_dir.join("segment_000.ts"), b"complete")
        .await
        .unwrap();
    wait_for_first_segment(
        &mut session,
        &generation.temp_dir,
        Instant::now() + Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(playlist_duration(playlist), 4.0);
    drop(session);
    manager.close_session(&pending.commit()).await;
}

#[tokio::test]
async fn decoded_input_failure_is_not_accepted_as_normal_end_of_audio() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&dir);
    let pending = manager
        .create_session("chapter".into(), "library".into(), false)
        .await
        .unwrap();
    let mut session = pending.session.lock().await;
    let generation = session.next_generation(0.0).await.unwrap();
    let process = fixture_process("running");
    process.input_error.store(true, Ordering::Relaxed);
    session.process = Some(process);
    tokio::fs::write(
        generation.temp_dir.join("playlist.m3u8"),
        "#EXTM3U\n#EXTINF:4.0,\nsegment_000.ts\n#EXT-X-ENDLIST\n",
    )
    .await
    .unwrap();
    tokio::fs::write(generation.temp_dir.join("segment_000.ts"), b"partial audio")
        .await
        .unwrap();
    assert!(cached_generation(&session, 1.0).await.is_none());
    assert!(matches!(
        wait_for_first_segment(
            &mut session,
            &generation.temp_dir,
            Instant::now() + Duration::from_secs(1)
        )
        .await,
        Err(TingError::ExternalError(_))
    ));
    drop(session);
    manager.close_session(&pending.commit()).await;
}

#[tokio::test]
async fn failed_encoder_is_not_returned_as_a_ready_session() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&dir);
    let pending = manager
        .create_session("chapter".into(), "library".into(), false)
        .await
        .unwrap();
    let mut session = pending.session.lock().await;
    let generation = session.next_generation(0.0).await.unwrap();
    session.process = Some(fixture_process("failure"));
    let result = wait_for_first_segment(
        &mut session,
        &generation.temp_dir,
        Instant::now() + Duration::from_secs(5),
    )
    .await;
    assert!(matches!(result, Err(TingError::ExternalError(_))));
    drop(session);
    manager.close_session(&pending.commit()).await;
}
