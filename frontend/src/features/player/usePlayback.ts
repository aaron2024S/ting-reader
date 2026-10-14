import React, { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { usePlayerStore } from '../../core/stores/playerStore';
import { useAuthStore } from '../../core/stores/authStore';
import { useWebSocket } from '../../core/hooks/useWebSocket';
import apiClient from '../../core/api/client';
import { ChapterCatalogue } from '../../core/api/books';
import { BOOKMARK_SEEK_REQUESTED } from '../../core/api/reading';
import type { Chapter } from '../../core/types';
import { sortChaptersForPlayback } from '../../core/utils/chapter';
import { resolvePlaybackDuration } from '../../core/utils/duration';
import { getCoverUrl } from '../../core/utils/image';
import { getRuntimeBaseUrl, getRuntimeUrl } from '../../core/utils/runtimeUrl';
import { PLAYBACK_PREFERENCES_UPDATED, type PlaybackPreferences } from '../../core/utils/playbackPreferences';
import { HlsPlaybackSession, type HlsPlaybackInfo } from './playbackSession';
import { isAppleMobileBrowser, isStrmPath } from './platform';
import { registerMediaSessionControls, updateMediaSessionMetadata, updateMediaSessionPlayback } from './mediaSession';
import { useSleepTimer, useStuckDecodeDetector, useNextChapterPreloader, useProgressSync, getBufferedEndAt } from './playerHelpers';

/** Owns the audio element workflow, source changes, timeline and notification
 * controls. Player views consume commands and snapshots without coordinating
 * network sessions or browser media events themselves. */
export const usePlayback = () => {
  const { t } = useTranslation();
  const { token, activeUrl } = useAuthStore();
  const API_BASE_URL = getRuntimeBaseUrl(activeUrl);
  const toAbsoluteMediaUrl = (url: string) => {
    if (/^https?:\/\//i.test(url)) return url;
    const base = API_BASE_URL || window.location.origin;
    return getRuntimeUrl(url, base);
  };

  const {
    currentBook,
    currentChapter,
    isPlaying,
    currentTime,
    duration,
    setCurrentTime,
    setDuration,
    nextChapter,
    prevChapter,
    playbackSpeed,
    setPlaybackSpeed,
    volume,
    setIsPlaying,
  } = usePlayerStore();

  const { isConnected: isWsConnected, sendProgress: wsSendProgress } = useWebSocket();

  const audioRef = useRef<HTMLAudioElement>(null);
  const [isMuted, setIsMuted] = useState(false);
  const [chapters, setChapters] = useState<Chapter[]>([]);
  useEffect(() => {
    const bookId = currentBook?.id;
    if (!bookId) return;
    const catalogue = new ChapterCatalogue(async (params, signal) =>
      (await apiClient.get(`/api/books/${bookId}/chapters`, { params, signal })).data);
    void catalogue.all().then((items) => {
      if (!items || usePlayerStore.getState().currentBook?.id !== bookId) return;
      const sorted = sortChaptersForPlayback(items);
      setChapters(sorted);
      usePlayerStore.setState({ chapters: sorted });
    }).catch((error: unknown) => console.error('Failed to fetch chapters', error));
    return () => catalogue.dispose();
  }, [currentBook?.id, token, API_BASE_URL]);
  const {
    sleepTimer,
    sleepEpisodes,
    startSleepTimer,
    startEpisodeSleepTimer,
    cancelSleepTimer,
    handleSleepChapterEnded,
  } = useSleepTimer({
    isPlaying,
    onExpire: () => setIsPlaying(false),
  });
  const [error, setError] = useState<string | null>(null);
  const [bufferedTime, setBufferedTime] = useState(0);
  const [autoPreload, setAutoPreload] = useState(false);
  const [retryCount, setRetryCount] = useState(0);
  const [shouldTranscode, setShouldTranscode] = useState(false);
  const [seekOffset, setSeekOffset] = useState<number | null>(null);
  const [streamStartOffset, setStreamStartOffset] = useState<{
    chapterId: string | null;
    offset: number;
  }>({ chapterId: null, offset: 0 });
  const [hlsStreamUrl, setHlsStreamUrl] = useState<string | null>(null);
  const [hlsSessionId, setHlsSessionId] = useState<string | null>(null);
  const [hlsChapterId, setHlsChapterId] = useState<string | null>(null);
  const [hlsSeekOffset, setHlsSeekOffset] = useState(0);
  const [hlsUnavailable, setHlsUnavailable] = useState(false);
  const currentBookId = currentBook?.id;
  const currentChapterId = currentChapter?.id;
  const chapterDuration = currentChapter?.duration;
  const isInitialLoadRef = useRef(true);
  const transcodeFallbackChapterRef = useRef<string | null>(null);
  const hlsRequestIdRef = useRef(0);
  const hlsPendingSeekRef = useRef<number | null>(null);
  const hlsSeekingRef = useRef(false);
  const sourceSwitchingRef = useRef(false);
  const hlsOwnerRef = useRef<HlsPlaybackSession | null>(null);
  // 防止 skip-outro 在同一章节内多次触发 nextChapter。
  const skipOutroChapterRef = useRef<string | null>(null);
  const shouldUseHlsForCurrentChapter =
    isAppleMobileBrowser() &&
    isStrmPath(currentChapter?.path) &&
    shouldTranscode &&
    !hlsUnavailable;
  const isUsingHlsForCurrentChapter =
    shouldUseHlsForCurrentChapter &&
    hlsChapterId === currentChapter?.id &&
    !!hlsStreamUrl;
  const getStreamUrl = (chapterId: string, forPreload = false) => {
    let url = (window as Window & { electronAPI?: unknown }).electronAPI
      ? `ting://stream/${chapterId}?token=${token}&remote=${encodeURIComponent(API_BASE_URL)}`
      : `${getRuntimeUrl(`/api/stream/${chapterId}`, API_BASE_URL)}?token=${token}`;

    if (!forPreload && shouldTranscode && !shouldUseHlsForCurrentChapter) url += "&transcode=mp3";

    const initialOffset =
      streamStartOffset.chapterId === chapterId
        ? streamStartOffset.offset
        : currentChapter?.id === chapterId
          ? Math.max(0, Math.floor(currentTime || 0))
          : 0;
    const explicitSeekOffset = currentChapter?.id === chapterId ? seekOffset : null;
    const requestSeekOffset = explicitSeekOffset !== null ? explicitSeekOffset : initialOffset;
    if (!forPreload && requestSeekOffset > 0) url += `&seek=${Math.floor(requestSeekOffset)}`;
    if (!forPreload && retryCount > 0) url += `&retry=${retryCount}`;
    if (forPreload) url += "&preload=true";
    return url;
  };
  const getTranscodeStartOffset = () => {
    if (!shouldTranscode) return 0;
    if (seekOffset !== null) return Math.max(0, seekOffset);
    return streamStartOffset.chapterId === currentChapter?.id
      ? Math.max(0, streamStartOffset.offset)
      : 0;
  };
  const getMediaOffset = () =>
    isUsingHlsForCurrentChapter ? hlsSeekOffset : getTranscodeStartOffset();

  const tryTranscodeFallback = () => {
    if (!currentChapter || retryCount >= 3) return false;
    if (shouldTranscode && !shouldUseHlsForCurrentChapter) return false;
    if (!shouldTranscode && transcodeFallbackChapterRef.current === currentChapter.id) {
      return true;
    }
    const chapterStartOffset =
      streamStartOffset.chapterId === currentChapter?.id
        ? streamStartOffset.offset
        : 0;
    const fallbackOffset = Math.max(
      0,
      isInitialLoadRef.current ? chapterStartOffset : currentTime,
    );
    setSeekOffset(fallbackOffset);
    setCurrentTime(fallbackOffset);
    if (shouldUseHlsForCurrentChapter) {
      setHlsUnavailable(true);
    }
    transcodeFallbackChapterRef.current = currentChapter.id;
    sourceSwitchingRef.current = true;
    setShouldTranscode(true);
    setRetryCount((prev) => prev + 1);
    isInitialLoadRef.current = true;
    return true;
  };

  // Fetch settings for auto_preload and user preferences
  useEffect(() => {
    const controller = new AbortController();
    const updatePreloadSettings = (event: Event) => {
      controller.abort();
      const settings = (event as CustomEvent<PlaybackPreferences>).detail;
      setAutoPreload(settings.auto_preload);
    };
    window.addEventListener(PLAYBACK_PREFERENCES_UPDATED, updatePreloadSettings);
    apiClient
      .get("/api/settings", { signal: controller.signal })
      .then((res) => {
        if (controller.signal.aborted) return;
        setAutoPreload(!!res.data.auto_preload);

        // Apply user's default playback speed
        if (res.data.playback_speed) {
          setPlaybackSpeed(res.data.playback_speed);
        }

      })
      .catch((err) => {
        if (!controller.signal.aborted) console.error("Failed to fetch playback settings", err);
      });
    return () => {
      controller.abort();
      window.removeEventListener(PLAYBACK_PREFERENCES_UPDATED, updatePreloadSettings);
    };
  }, [setPlaybackSpeed, token, activeUrl]);

  useEffect(() => {
    const chapterId = currentChapter?.id ?? null;
    const offset = chapterId
      ? Math.max(0, Math.floor(usePlayerStore.getState().currentTime || 0))
      : 0;
    const timer = window.setTimeout(
      () => setStreamStartOffset({ chapterId, offset }),
      0,
    );
    return () => window.clearTimeout(timer);
  }, [currentChapter?.id]);

  // Reset all playback state when chapter ID changes
  useEffect(() => {
    isInitialLoadRef.current = true;
    skipOutroChapterRef.current = null;
    hlsRequestIdRef.current += 1;
    const timer = window.setTimeout(() => {
      setError(null);
      setShouldTranscode(false);
      setSeekOffset(null);
      setHlsStreamUrl(null);
      setHlsSessionId(null);
      setHlsChapterId(null);
      setHlsSeekOffset(0);
      setHlsUnavailable(false);
      setBufferedTime(0);
      setRetryCount(0);
      transcodeFallbackChapterRef.current = null;
      if (currentChapter?.duration && currentChapter.duration > 0) {
        setDuration(currentChapter.duration);
        console.log(
          `Chapter changed; duration set immediately: ${currentChapter.duration}s`,
        );
      } else {
        setDuration(0);
        console.log("Chapter changed; waiting for audio metadata duration");
      }
    }, 0);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentChapter?.id]);

  // Update duration display when chapter duration is updated (e.g., after FFprobe)
  // without resetting shouldTranscode or retryCount
  useEffect(() => {
    if (currentChapter?.duration && currentChapter.duration > 0) {
      setDuration(currentChapter.duration);
    }
  }, [currentChapter?.duration, setDuration]);

  useEffect(() => {
    if (!shouldTranscode || !currentBookId || !currentChapterId) return;
    if (resolvePlaybackDuration(chapterDuration, 0, true) > 0) return;

    const bookId = currentBookId;
    const chapterId = currentChapterId;
    const controller = new AbortController();
    let timer: number | undefined;
    let attempts = 0;
    const refreshDuration = async () => {
      try {
        const response = await apiClient.get<Chapter[]>(`/api/books/${bookId}/chapters`, {
          signal: controller.signal,
          timeout: 5000,
        });
        if (controller.signal.aborted) return;
        const chapter = response.data.find((item) => item.id === chapterId);
        const sourceDuration = resolvePlaybackDuration(chapter?.duration, 0, true);
        const state = usePlayerStore.getState();
        if (state.currentBook?.id !== bookId || state.currentChapter?.id !== chapterId) return;
        if (sourceDuration > 0) {
          const updateChapter = (item: Chapter) =>
            item.id === chapterId ? { ...item, duration: sourceDuration } : item;
          setChapters((items) => items.map(updateChapter));
          usePlayerStore.setState((current) => ({
            chapters: current.chapters.map(updateChapter),
            currentChapter: current.currentChapter ? updateChapter(current.currentChapter) : null,
            duration: sourceDuration,
          }));
          return;
        }
      } catch (err) {
        if (controller.signal.aborted) return;
        console.debug("Source duration is not available yet", err);
      }
      // FFprobe can finish after the stream has already loaded its metadata.
      attempts += 1;
      if (!controller.signal.aborted && attempts < 6) {
        timer = window.setTimeout(refreshDuration, Math.min(1000 * 2 ** attempts, 8000));
      }
    };
    timer = window.setTimeout(refreshDuration, 0);
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [shouldTranscode, currentBookId, currentChapterId, chapterDuration, token, API_BASE_URL]);

  useEffect(() => {
    let disposed = false;
    let owner: HlsPlaybackSession | null = null;
    const sessionUrl = (id: string) => getRuntimeUrl(`/api/stream/hls/${id}`, API_BASE_URL || window.location.origin);
    const onPageHide = () => owner?.dispose(true);
    window.addEventListener("pagehide", onPageHide);
    const timer = window.setTimeout(() => {
      hlsPendingSeekRef.current = null;
      hlsSeekingRef.current = false;
      sourceSwitchingRef.current = false;
      if (!currentChapter || !shouldUseHlsForCurrentChapter) {
        setHlsStreamUrl(null);
        setHlsSessionId(null);
        setHlsChapterId(null);
        setHlsSeekOffset(0);
        return;
      }
      const requestId = ++hlsRequestIdRef.current;
      let startAt = Math.max(0, usePlayerStore.getState().currentTime || 0);
      if (isInitialLoadRef.current && currentBook?.skip_intro && startAt < currentBook.skip_intro) {
        startAt = currentBook.skip_intro;
      }
      sourceSwitchingRef.current = true;
      setHlsChapterId(currentChapter.id);
      setHlsStreamUrl(null);
      setHlsSessionId(null);
      setHlsSeekOffset(startAt);
      setCurrentTime(startAt);
      isInitialLoadRef.current = false;
      const params: Record<string, string | number> = { transcode: "hls" };
      if (token) params.token = token;
      if (startAt > 0) params.seek = startAt;
      owner = new HlsPlaybackSession({
        create: async () => (await apiClient.get<HlsPlaybackInfo>(`/api/stream/${currentChapter.id}`, { params })).data,
        seek: async (id, position) => (await apiClient.post<HlsPlaybackInfo>(`${sessionUrl(id)}/seek`, null, {
          params: { seek: position },
        })).data,
        touch: (id) => apiClient.post(`${sessionUrl(id)}/touch`),
        release: (id, pageClosing) => pageClosing
          ? fetch(sessionUrl(id), { method: "DELETE", keepalive: true })
          : apiClient.delete(sessionUrl(id)),
      }, (error) => {
        if (!disposed && typeof error === "object" && error !== null && "response" in error
          && (error.response as { status?: number })?.status === 404) setHlsUnavailable(true);
      });
      hlsOwnerRef.current = owner;
      void owner.open()
        .then(info => {
          if (!info || disposed || requestId !== hlsRequestIdRef.current) return;
          setHlsSessionId(info.session_id);
          setHlsSeekOffset(info.start_offset ?? startAt);
          hlsPendingSeekRef.current = 0;
          setHlsStreamUrl(toAbsoluteMediaUrl(info.playlist_url));
        })
        .catch(err => {
          if (disposed || requestId !== hlsRequestIdRef.current) return;
          console.error("HLS stream initialization failed", err);
          sourceSwitchingRef.current = false;
          setHlsStreamUrl(null);
          setHlsSessionId(null);
          setHlsChapterId(null);
          setHlsUnavailable(true);
        });
    }, 0);
    return () => {
      disposed = true;
      hlsRequestIdRef.current += 1;
      window.clearTimeout(timer);
      window.removeEventListener("pagehide", onPageHide);
      owner?.dispose();
      if (hlsOwnerRef.current === owner) hlsOwnerRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentChapter?.id, currentChapter?.path, shouldUseHlsForCurrentChapter, currentBook?.skip_intro, token, API_BASE_URL]);

  // Reset initial load ref when retrying (to allow resume logic to run again)
  useEffect(() => {
    if (retryCount > 0) {
      isInitialLoadRef.current = true;
    }
  }, [retryCount]);

  const requestAudioPlayback = (audio: HTMLAudioElement) => {
    const requestedSrc = audio.getAttribute("src");
    const playPromise = audio.play();
    if (playPromise === undefined) return;

    playPromise.catch((err) => {
      // A chapter/source switch can abort the previous play request. The new
      // source's canplay event will retry it, so stale failures must not change
      // the playback intent kept in the store.
      const sourceChanged =
        audioRef.current !== audio || audio.getAttribute("src") !== requestedSrc;
      if (sourceChanged || !usePlayerStore.getState().isPlaying) return;

      if (err.name === "AbortError" || err.code === 20) {
        console.log("Playback request interrupted; waiting for media readiness");
        return;
      }
      if (err.name === "NotAllowedError") {
        // Safari/iOS may reject play() when it isn't treated as a direct user gesture.
        setIsPlaying(false);
        setError(t("player.autoplayBlocked"));
        console.warn("Playback blocked by browser policy", err);
        return;
      }
      console.error("Playback failed", err);
      if (!tryTranscodeFallback()) {
        setError(t("player.audioLoadError"));
      }
    });
  };

  // Sync state with audio element
  useEffect(() => {
    if (!audioRef.current || !currentChapter) return;
    if (shouldUseHlsForCurrentChapter && !hlsStreamUrl) return;
    // Reset retry count when chapter changes (this is also handled in another effect, but safe to double check)
    // IMPORTANT: If source changes due to transcoding, we do NOT want to reset retry count immediately here
    // or we might enter a loop.
    // Actually, retryCount is part of the dependency array, so this runs on retry too.

    if (isPlaying) {
      requestAudioPlayback(audioRef.current);
    } else {
      audioRef.current.pause();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    isPlaying,
    currentChapter?.id,
    retryCount,
    shouldTranscode,
    seekOffset,
    hlsStreamUrl,
    hlsSeekOffset,
    t,
  ]);

  // Some browsers may report "playing" while decode hasn't actually advanced.
  useStuckDecodeDetector({
    isPlaying,
    audioRef,
    chapterId: currentChapter?.id,
    shouldTranscode,
    retryCount,
    onStuck: () => {
      if (hlsSeekingRef.current) return;
      if (!tryTranscodeFallback()) setError(t("player.audioLoadError"));
    },
  });

  // Preload and Server-side Cache next chapter logic
  useNextChapterPreloader({
    autoPreload,
    chapters,
    chapterId: currentChapter?.id,
    sourceKey: `${API_BASE_URL}:${token ?? ""}`,
    getNextStreamUrl: (id) => getStreamUrl(id, true),
  });

  // Handle Skip Intro and Outro
  const handleTimeUpdate = () => {
    if (!audioRef.current) return;
    if (isUsingHlsForCurrentChapter && hlsSeekingRef.current) return;

    const rawTime = audioRef.current.currentTime;
    // Server-side seeked streams start from 0 but represent audio at an offset.
    const mediaOffset = getMediaOffset();
    const time = rawTime + mediaOffset;

    // Prevent overwriting persisted progress with 0 on initial load
    // If we are at the very beginning (time < 0.5) but store has significant progress (> 2s),
    // ignore this update until we've resumed properly.
    if (isInitialLoadRef.current && rawTime < 0.5 && currentTime > 2) {
      return;
    }

    // Mark initial load as done if we have successfully played past 1s
    if (isInitialLoadRef.current && rawTime > 1) {
      isInitialLoadRef.current = false;
    }

    setCurrentTime(time);

    // Update buffered time more accurately
    if (audioRef.current.buffered.length > 0) {
      setBufferedTime(
        getBufferedEndAt(audioRef.current.buffered, rawTime) + mediaOffset,
      );
    }

    // Handle Skip Intro
    if (isInitialLoadRef.current && currentBook?.skip_intro) {
      if (time < currentBook.skip_intro) {
        audioRef.current.currentTime = currentBook.skip_intro;
        setCurrentTime(currentBook.skip_intro);
      }
      isInitialLoadRef.current = false;
    }

    // Handle Skip Outro
    if (currentBook?.skip_outro && duration > 0) {
      // Only skip if the chapter is long enough to actually have an outro
      // and we've played at least some of it
      const minChapterDuration =
        (currentBook.skip_intro || 0) + currentBook.skip_outro + 10;
      if (
        duration > minChapterDuration &&
        duration - time <= currentBook.skip_outro
      ) {
        // 每集只触发一次，避免 timeUpdate 反复调用 nextChapter。
        if (skipOutroChapterRef.current !== currentChapter?.id) {
          skipOutroChapterRef.current = currentChapter?.id ?? null;
          advanceToNextChapter();
        }
      }
    }
  };

  const applyPendingHlsSeek = () => {
    const audio = audioRef.current;
    const target = hlsPendingSeekRef.current;
    if (!audio || !isUsingHlsForCurrentChapter || target === null) return;
    const ranges = audio.seekable;
    const canSeek = target === 0 || Array.from({ length: ranges.length }, (_, index) => index)
      .some(index => target >= ranges.start(index) && target < ranges.end(index));
    if (!canSeek) return;
    try {
      audio.currentTime = target;
      hlsPendingSeekRef.current = null;
      hlsSeekingRef.current = false;
    } catch {
      // Metadata may precede the first seekable range. Retry on progress/canplay.
    }
  };

  const handleProgress = () => {
    applyPendingHlsSeek();
    if (audioRef.current && audioRef.current.buffered.length > 0) {
      const rawTime = audioRef.current.currentTime;
      const mediaOffset = getMediaOffset();
      setBufferedTime(
        getBufferedEndAt(audioRef.current.buffered, rawTime) + mediaOffset,
      );
    }
  };

  useEffect(() => {
    if (!audioRef.current) return;
    audioRef.current.playbackRate = playbackSpeed;
  }, [playbackSpeed]);

  useEffect(() => {
    if (!audioRef.current) return;
    audioRef.current.volume = isMuted ? 0 : volume;
  }, [volume, isMuted, currentChapter?.id]);

  // Sync progress to backend via WebSocket (primary) and HTTP (fallback);
  // also flushes once immediately on pause.
  const { flush: flushProgress } = useProgressSync({
    isPlaying,
    bookId: currentBook?.id,
    chapterId: currentChapter?.id,
    currentTime,
    isWsConnected,
    wsSendProgress,
  });

  const updateAudioDuration = () => {
    const audio = audioRef.current;
    if (!audio) return 0;
    const sourceDuration = resolvePlaybackDuration(
      currentChapter?.duration,
      audio.duration,
      shouldTranscode,
    );
    setDuration(sourceDuration);
    return sourceDuration;
  };

  const handleLoadedMetadata = () => {
    if (audioRef.current) {
      applyPendingHlsSeek();
      audioRef.current.volume = isMuted ? 0 : volume;
      const browserDuration = updateAudioDuration();

      // Resume position from store if this is the initial load for this chapter
      if (isInitialLoadRef.current && !isUsingHlsForCurrentChapter) {
        const resumePosition = usePlayerStore.getState().currentTime;
        const mediaOffset = getMediaOffset();
        if (resumePosition > 0) {
          // If progress is very close to the end (e.g., within 2 seconds or > 99%), start from the beginning
          if (
            browserDuration > 0 &&
            (browserDuration - resumePosition < 2 ||
              resumePosition / browserDuration > 0.99)
          ) {
            console.log(
              `Chapter ${currentChapter?.title} completed; starting from the beginning`,
            );
            if (mediaOffset > 0) {
              setSeekOffset(0);
            } else {
              audioRef.current.currentTime = 0;
            }
            setCurrentTime(0);
          } else if (mediaOffset > 0) {
            // The backend already sought before transcoding. This element's
            // timeline starts at zero, so applying the absolute position again
            // can seek past the shortened stream.
            setCurrentTime(mediaOffset);
          } else {
            console.log(
              `Resuming chapter ${currentChapter?.title} at ${resumePosition}s`,
            );
            audioRef.current.currentTime = resumePosition;
          }
        }
      }

      // Ensure playback rate is applied
      audioRef.current.playbackRate = playbackSpeed;

      // Sync duration back only when the chapter lacks one and metadata is valid.
      if (
        !shouldTranscode &&
        currentChapter &&
        (!currentChapter.duration || currentChapter.duration === 0)
      ) {
        if (Number.isFinite(browserDuration) && browserDuration > 0) {
          const audioDuration = audioRef.current.duration;
          if (Number.isFinite(audioDuration) && audioDuration > 0) {
            console.log(
              `Syncing browser duration to server: ${Math.round(audioDuration)}s`,
            );
            apiClient
              .patch(`/api/chapters/${currentChapter.id}`, {
                duration: Math.round(audioDuration),
              })
              .catch((err) => console.error("Failed to sync duration", err));
          }
        }
      }
    }
  };

  const [isSeeking, setIsSeeking] = useState(false);
  const [seekTime, setSeekTime] = useState(0);

  const handleSeek = (e: React.SyntheticEvent<HTMLInputElement>) => {
    const time = parseFloat((e.target as HTMLInputElement).value);
    setSeekTime(time);
    if (!isSeeking) {
      seekToTime(time);
    }
  };

  const handleSeekStart = () => {
    setIsSeeking(true);
    setSeekTime(currentTime);
  };

  const seekToTime = (time: number) => {
    if (!Number.isFinite(time)) return;
    const targetTime = Math.max(
      0,
      duration > 0 ? Math.min(time, duration) : time,
    );

    if (audioRef.current) {
      if (isUsingHlsForCurrentChapter && hlsSessionId) {
        const requestId = ++hlsRequestIdRef.current;
        const relative = targetTime - hlsSeekOffset;
        const ranges = audioRef.current.seekable;
        const locallySeekable = !hlsSeekingRef.current && relative >= 0 &&
          Array.from({ length: ranges.length }, (_, index) => index)
            .some(index => relative >= ranges.start(index) && relative < ranges.end(index));
        setCurrentTime(targetTime);
        isInitialLoadRef.current = false;
        if (locallySeekable) {
          hlsOwnerRef.current?.cancelSeek();
          audioRef.current.currentTime = relative;
          hlsPendingSeekRef.current = null;
          return;
        }
        hlsSeekingRef.current = true;
        hlsPendingSeekRef.current = null;
        // Serialize requests and discard queued stale targets. Response guards
        // alone cannot prevent an older server request stopping a newer process.
        void hlsOwnerRef.current?.seek(targetTime).then(info => {
          if (!info || requestId !== hlsRequestIdRef.current) return;
          const offset = info.start_offset ?? targetTime;
          const playbackTime = info.playback_time ?? 0;
          const nextUrl = toAbsoluteMediaUrl(info.playlist_url);
          setHlsSeekOffset(offset);
          if (nextUrl === hlsStreamUrl && audioRef.current) {
            audioRef.current.currentTime = playbackTime;
            hlsSeekingRef.current = false;
          } else {
            sourceSwitchingRef.current = true;
            hlsPendingSeekRef.current = playbackTime;
            setHlsStreamUrl(nextUrl);
          }
        })
          .catch((err) => {
            if (requestId !== hlsRequestIdRef.current) return;
            hlsSeekingRef.current = false;
            sourceSwitchingRef.current = false;
            console.error("HLS seek failed", err);
            tryTranscodeFallback();
          });
        return;
      }

      // For transcoded streams, native seeking won't work (no Range support)
      // Detect by checking if seekable ranges are empty or if we're in transcode mode
      const isNonSeekable =
        shouldTranscode || audioRef.current.seekable.length === 0;

      if (isNonSeekable && shouldTranscode) {
        // Reload audio with seek parameter (server-side seek via FFmpeg -ss)
        sourceSwitchingRef.current = true;
        setSeekOffset(targetTime);
        setCurrentTime(targetTime);
        isInitialLoadRef.current = false;
      } else {
        audioRef.current.currentTime = targetTime;
        setCurrentTime(targetTime);
      }
    } else {
      setCurrentTime(targetTime);
    }
  };

  const handleSeekEnd = (e: React.SyntheticEvent<HTMLInputElement>) => {
    const time = parseFloat((e.target as HTMLInputElement).value);
    setIsSeeking(false);
    seekToTime(time);
  };

  const onBookmarkSeek = React.useEffectEvent((event: Event) => {
    const request = (event as CustomEvent<{ chapterId: string; position: number }>).detail;
    if (request.chapterId !== currentChapter?.id || !Number.isFinite(request.position)) return;
    seekToTime(request.position);
    setIsPlaying(true);
  });
  useEffect(() => {
    window.addEventListener(BOOKMARK_SEEK_REQUESTED, onBookmarkSeek);
    return () => window.removeEventListener(BOOKMARK_SEEK_REQUESTED, onBookmarkSeek);
  }, []);

  const onMediaPlay = React.useEffectEvent(() => setIsPlaying(true));
  const onMediaPause = React.useEffectEvent(() => setIsPlaying(false));
  const onMediaPrevious = React.useEffectEvent(() => prevChapter());
  const onMediaNext = React.useEffectEvent(() => nextChapter());
  const onMediaSeek = React.useEffectEvent((position: number) => seekToTime(position));
  const hasCurrentChapter = !!currentChapter;

  useEffect(() => {
    if (!hasCurrentChapter || !("mediaSession" in navigator)) return;
    const session = navigator.mediaSession;
    const unregister = registerMediaSessionControls(session, {
      play: () => onMediaPlay(),
      pause: () => onMediaPause(),
      previous: () => onMediaPrevious(),
      next: () => onMediaNext(),
      seek: (position) => onMediaSeek(position),
      getPosition: () => usePlayerStore.getState().currentTime,
    });
    return () => {
      unregister();
      session.metadata = null;
      session.playbackState = "none";
      session.setPositionState?.();
    };
  }, [hasCurrentChapter]);

  useEffect(() => {
    if (!("mediaSession" in navigator)) return;
    const cover = currentBook?.cover_url
      ? getCoverUrl(currentBook.cover_url, currentBook.library_id, currentBook.id)
      : null;
    updateMediaSessionMetadata(navigator.mediaSession, currentChapterId ? {
      title: currentChapter?.title || currentBook?.title || "",
      artist: currentBook?.narrator || currentBook?.author || "",
      album: currentBook?.title || "",
      artwork: cover ? [{ src: new URL(cover, window.location.href).href }] : [],
    } : null);
  }, [
    currentChapterId, currentChapter?.title,
    currentBook?.id, currentBook?.title, currentBook?.author, currentBook?.narrator,
    currentBook?.cover_url, currentBook?.library_id, token, API_BASE_URL,
  ]);

  useEffect(() => {
    if (!hasCurrentChapter || !("mediaSession" in navigator)) return;
    updateMediaSessionPlayback(navigator.mediaSession, isPlaying, duration, currentTime, playbackSpeed);
  }, [hasCurrentChapter, isPlaying, duration, currentTime, playbackSpeed]);

  const audioSrc = !currentChapter ? "" : shouldUseHlsForCurrentChapter
    ? hlsChapterId === currentChapter.id
      ? hlsStreamUrl || ""
      : ""
    : getStreamUrl(currentChapter.id);

  // 切换到下一集并处理按集数睡眠定时：若集数已用完则暂停播放。
  const advanceToNextChapter = () => {
    const stopAfterThis = handleSleepChapterEnded();
    nextChapter();
    if (stopAfterThis) {
      // nextChapter 会同步把新章节置为播放状态；不要依赖 ended/pause 事件
      // 到达顺序或旧的 isPlaying 闭包值，直接阻止下一集自动开始。
      setIsPlaying(false);
    }
  };

  const handleEnded = () => {
    flushProgress(duration);
    // 按集数睡眠：播完设定集数后停在下一条开头，不再继续播放。
    advanceToNextChapter();
  };

  const audioProps: React.AudioHTMLAttributes<HTMLAudioElement> & { ref: React.RefObject<HTMLAudioElement | null> } = {
    ref: audioRef,
    src: audioSrc || undefined,
    onTimeUpdate: handleTimeUpdate,
    onProgress: handleProgress,
    onLoadedMetadata: handleLoadedMetadata,
    onDurationChange: updateAudioDuration,
    onCanPlay: () => {
      applyPendingHlsSeek();
      sourceSwitchingRef.current = false;
      setError(null);
      const audio = audioRef.current;
      if (audio && audio.paused && usePlayerStore.getState().isPlaying) {
        requestAudioPlayback(audio);
      }
    },
    onEnded: handleEnded,
    onPlay: () => {
      setIsPlaying(true);
      if (audioRef.current) audioRef.current.playbackRate = playbackSpeed;
    },
    onPause: () => {
      if (!sourceSwitchingRef.current) setIsPlaying(false);
    },
    onError: (e) => {
      const audio = audioRef.current;
      console.log("Audio error event fired", {
        error: audio?.error,
        code: audio?.error?.code,
        message: audio?.error?.message,
        retryCount,
        shouldTranscode,
      });
      if (audio?.error) {
        if (audio.error.code === 1) {
          console.log("Playback aborted by user action");
          return;
        }
        console.error("Audio element error", audio.error);
      } else {
        console.error("Audio element error (unknown)", e);
      }
      if (tryTranscodeFallback()) return;
      setError(t("player.audioLoadError"));
    },
  };
  return {
    audioProps, chapters, isMuted, setIsMuted, error, bufferedTime, isSeeking,
    seekTime, handleSeek, handleSeekStart, handleSeekEnd, seekToTime, sleepTimer,
    sleepEpisodes, startSleepTimer, startEpisodeSleepTimer, cancelSleepTimer,
  };
};
