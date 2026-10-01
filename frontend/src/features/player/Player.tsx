import React, { useRef, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLocation } from "react-router";
import { usePlayerStore } from "../../core/stores/playerStore";
import { useAuthStore } from "../../core/stores/authStore";
import { useWebSocket } from "../../core/hooks/useWebSocket";
import apiClient from "../../core/api/client";
import { BOOKMARK_SEEK_REQUESTED } from "../../core/api/reading";
import type { Chapter } from "../../core/types";
import { sortChaptersForPlayback } from "../../core/utils/chapter";
import { setAlpha, toSolidColor, isTooLight } from "../../core/utils/color";
import { useBookshelfCoverShape } from "../../core/hooks/useBookshelfCoverShape";
import {
  PLAYBACK_PREFERENCES_UPDATED,
  type PlaybackPreferences,
} from "../../core/utils/playbackPreferences";
import ProgressBar from "./ProgressBar";
import {
  isAppleMobileBrowser,
  isMiniPlayerHiddenPath,
  isStrmPath,
} from "./platform";
import { getRuntimeBaseUrl, getRuntimePathname, getRuntimeUrl } from "../../core/utils/runtimeUrl";
import PlayerSettingsModal from "./PlayerSettingsModal";
import BookmarkManagerModal from "./BookmarkManagerModal";
import ChapterListDrawer from "./ChapterListDrawer";
import {
  useIsDarkMode,
  useThemeColorSync,
  useChapterGroups,
  useSleepTimer,
  useOutsideClickClose,
  useWidgetFullscreen,
  useStuckDecodeDetector,
  useNextChapterPreloader,
  useProgressSync,
  getPlayerCoverSizes,
  getBufferedEndAt,
  formatPlayerTime,
  getChapterProgressText,
  CHAPTERS_PER_GROUP,
} from "./playerHelpers";
import {
  CollapsedPlayerView,
  ExpandedPlayerHeader,
  ExpandedMainControls,
  ExpandedBottomControls,
  ExpandedProgressSection,
  ExpandedCoverAndMeta,
  MiniPlayerBookInfo,
  MiniPlayerDesktopControls,
  MiniPlayerDesktopExtras,
  MiniPlayerMobileControls,
} from "./PlayerPieces";

const Player: React.FC = () => {
  const { t } = useTranslation();
  const coverShape = useBookshelfCoverShape();
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
    togglePlay,
    currentTime,
    duration,
    setCurrentTime,
    setDuration,
    nextChapter,
    prevChapter,
    playbackSpeed,
    setPlaybackSpeed,
    volume,
    setVolume,
    themeColor,
    playChapter,
    setIsPlaying,
    isExpanded,
    setIsExpanded,
    isCollapsed,
    setIsCollapsed,
    isSeriesEditing,
  } = usePlayerStore();

  const { isConnected: isWsConnected, sendProgress: wsSendProgress } = useWebSocket();

  const audioRef = useRef<HTMLAudioElement>(null);
  const location = useLocation();
  const [isMuted, setIsMuted] = useState(false);
  const [showChapters, setShowChapters] = useState(false);
  const [showSleepTimer, setShowSleepTimer] = useState(false);
  const [showVolumeControl, setShowVolumeControl] = useState(false);
  const [showSpeedControl, setShowSpeedControl] = useState(false);
  const [showBookmarks, setShowBookmarks] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [currentGroupIndex, setCurrentGroupIndex] = useState(0);
  const [activeTab, setActiveTab] = useState<"main" | "extra">("main");
  const scrollRef = useRef<HTMLDivElement>(null);
  const volumeControlRef = useRef<HTMLDivElement>(null);
  const speedControlRef = useRef<HTMLDivElement>(null);

  const scrollGroups = (direction: "left" | "right") => {
    if (scrollRef.current) {
      const scrollAmount = 200;
      scrollRef.current.scrollBy({
        left: direction === "left" ? -scrollAmount : scrollAmount,
        behavior: "smooth",
      });
    }
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const [chapters, setChapters] = useState<any[]>([]);
  const [customMinutes, setCustomMinutes] = useState("");
  const [customEpisodes, setCustomEpisodes] = useState("");
  const [editSkipIntro, setEditSkipIntro] = useState(0);
  const [editSkipOutro, setEditSkipOutro] = useState(0);

  const isDark = useIsDarkMode();

  const effectiveThemeColor =
    themeColor && !isTooLight(themeColor) ? themeColor : undefined;
  const {
    collapsed: collapsedCoverSizeClass,
    mini: miniCoverSizeClass,
    expanded: expandedCoverSizeClass,
  } = getPlayerCoverSizes(coverShape);
  // Always use the theme color for the mini player progress bar, even in dark mode
  const miniPlayerThemeColor = effectiveThemeColor;
  // Determine if we should use dark mode text colors (white/gray) for controls
  // In dark mode, we always want bright white/gray for contrast
  const useDarkControls = isDark;

  // Sync theme color from current book (stored value preferred, fallback to FAC extraction).
  useThemeColorSync(currentBook);

  useEffect(() => {
    if (currentBook) {
      setTimeout(() => {
        setEditSkipIntro(currentBook.skip_intro || 0);
        setEditSkipOutro(currentBook.skip_outro || 0);
      }, 0);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentBook?.id]);

  const handleSaveSettings = async () => {
    if (!currentBook) return;
    try {
      await apiClient.patch(`/api/books/${currentBook.id}`, {
        skip_intro: editSkipIntro,
        skip_outro: editSkipOutro,
      });
      // Update local store state if necessary, but currentBook is in store
      usePlayerStore.setState((state) => ({
        currentBook: state.currentBook
          ? {
              ...state.currentBook,
              skip_intro: editSkipIntro,
              skip_outro: editSkipOutro,
            }
          : null,
      }));
      setShowSettings(false);
    } catch (err) {
      console.error("Failed to save player settings", err);
    }
  };

  const { extraChapters, currentChapters, groups } = useChapterGroups(
    chapters,
    activeTab,
  );
  const chaptersPerGroup = CHAPTERS_PER_GROUP;

  const {
    sleepTimer,
    sleepEpisodes,
    startSleepTimer,
    startEpisodeSleepTimer,
    cancelSleepTimer,
    handleSleepChapterEnded,
  } = useSleepTimer({
    isPlaying,
    onExpire: togglePlay,
  });
  const timerMenuRef = useRef<HTMLDivElement>(null);

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
  const isInitialLoadRef = useRef(true);
  const hlsRequestIdRef = useRef(0);
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
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    let url = (window as any).electronAPI
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
    if (shouldTranscode || retryCount >= 3) return;
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
    // Silently retry with transcoding, no need to show error message
    // Keep the retry silent while switching to a compatible audio stream.
    setShouldTranscode(true);
    setRetryCount((prev) => prev + 1);
    isInitialLoadRef.current = true;
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

  // Fetch chapters for the current book
  useEffect(() => {
    if (currentBook?.id) {
      apiClient
        .get(`/api/books/${currentBook.id}/chapters`)
        .then((res) => {
          const sortedChapters = sortChaptersForPlayback(res.data);
          setChapters(sortedChapters);
          setCurrentGroupIndex(0); // Reset group index when book changes
          // Keep chapters in the store so nextChapter can work correctly.
          usePlayerStore.setState({ chapters: sortedChapters });
        })
        .catch((err) => console.error("Failed to fetch chapters", err));
    }
  }, [currentBook?.id]);

  // Fetch chapters on mount if the store is empty but a book is already active.
  useEffect(() => {
    const storeChapters = usePlayerStore.getState().chapters;
    if (currentBook?.id && storeChapters.length === 0) {
      apiClient
        .get(`/api/books/${currentBook.id}/chapters`)
        .then((res) => {
          const sortedChapters = sortChaptersForPlayback(res.data);
          setChapters(sortedChapters);
          usePlayerStore.setState({ chapters: sortedChapters });
        })
        .catch((err) => console.error("Failed to fetch chapters", err));
    }
  }, [currentBook?.id]);

  // Close timer / volume menus when clicking outside.
  useOutsideClickClose([timerMenuRef, volumeControlRef, speedControlRef], (ref) => {
    if (ref === timerMenuRef) setShowSleepTimer(false);
    if (ref === volumeControlRef) setShowVolumeControl(false);
    if (ref === speedControlRef) setShowSpeedControl(false);
  });

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
    let requestId: number | null = null;
    const timer = window.setTimeout(() => {
      if (!currentChapter || !shouldUseHlsForCurrentChapter) {
        setHlsStreamUrl(null);
        setHlsSessionId(null);
        setHlsChapterId(null);
        setHlsSeekOffset(0);
        return;
      }

      requestId = ++hlsRequestIdRef.current;
      let startAt = Math.max(0, usePlayerStore.getState().currentTime || 0);
      if (
        isInitialLoadRef.current &&
        currentBook?.skip_intro &&
        startAt < currentBook.skip_intro
      ) {
        startAt = currentBook.skip_intro;
      }

      setHlsChapterId(currentChapter.id);
      setHlsStreamUrl(null);
      setHlsSessionId(null);
      setHlsSeekOffset(startAt);
      setCurrentTime(startAt);
      isInitialLoadRef.current = false;

      const params: Record<string, string | number> = { transcode: "hls" };
      if (token) params.token = token;
      if (startAt > 0) params.seek = startAt;

      void apiClient
        .get(`/api/stream/${currentChapter.id}`, { params })
        .then((res) => {
          if (requestId !== hlsRequestIdRef.current) return;
          const playlistUrl = res.data?.playlist_url;
          const sessionId = res.data?.session_id;
          if (!playlistUrl || !sessionId) {
            throw new Error("HLS response missing playlist URL or session ID");
          }
          setHlsSessionId(sessionId);
          setHlsStreamUrl(toAbsoluteMediaUrl(playlistUrl));
        })
        .catch((err) => {
          if (requestId !== hlsRequestIdRef.current) return;
          console.error("HLS stream initialization failed", err);
          setHlsStreamUrl(null);
          setHlsSessionId(null);
          setHlsChapterId(null);
          setHlsUnavailable(true);
        });
    }, 0);

    return () => {
      window.clearTimeout(timer);
      if (requestId !== null) hlsRequestIdRef.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    currentChapter?.id,
    currentChapter?.path,
    shouldUseHlsForCurrentChapter,
    currentBook?.skip_intro,
    token,
  ]);

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
      // Don't set user-visible error yet, let onError handler try to recover first
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
    onStuck: isStrmPath(currentChapter?.path)
      ? () => setError(t("player.strmDirectError"))
      : tryTranscodeFallback,
  });

  // Preload and Server-side Cache next chapter logic
  useNextChapterPreloader({
    autoPreload,
    bookId: currentBook?.id,
    chapterId: currentChapter?.id,
    sourceKey: `${API_BASE_URL}:${token ?? ""}`,
    getNextStreamUrl: (id) => getStreamUrl(id, true),
  });

  // Handle Skip Intro and Outro
  const handleTimeUpdate = () => {
    if (!audioRef.current) return;

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

  const handleProgress = () => {
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
  useProgressSync({
    isPlaying,
    bookId: currentBook?.id,
    chapterId: currentChapter?.id,
    currentTime,
    isWsConnected,
    wsSendProgress,
  });

  const handleLoadedMetadata = () => {
    if (audioRef.current) {
      audioRef.current.volume = isMuted ? 0 : volume;
      let browserDuration = audioRef.current.duration;

      // Prefer duration already stored with the chapter.
      if (currentChapter?.duration && currentChapter.duration > 0) {
        browserDuration = currentChapter.duration;
        console.log(`Using chapter duration: ${browserDuration}s`);
      }
      // Use browser metadata only when the chapter has no stored duration.
      else if (
        Number.isFinite(browserDuration) &&
        !isNaN(browserDuration) &&
        browserDuration > 0
      ) {
        console.log(`Using browser-reported duration: ${browserDuration}s`);
      } else {
        console.warn("Unable to determine a valid duration; using 0");
        browserDuration = 0;

        // Transcoded chunked streams may report Infinity/NaN. Refetch the
        // chapter list because the backend stores the real FFprobe duration.
        if (shouldTranscode && currentBook?.id && currentChapter?.id) {
          const fetchBookId = currentBook.id;
          const fetchChapterId = currentChapter.id;
          apiClient
            .get(`/api/books/${fetchBookId}/chapters`)
            .then((res) => {
              const updatedChapters = sortChaptersForPlayback(res.data);
              const updatedChapter = updatedChapters.find(
                (c: Chapter) => c.id === fetchChapterId,
              );
              if (
                updatedChapter &&
                updatedChapter.duration &&
                updatedChapter.duration > 0
              ) {
                setChapters(updatedChapters);
                usePlayerStore.setState({
                  chapters: updatedChapters,
                  currentChapter: updatedChapter,
                });
                setDuration(updatedChapter.duration);
                console.log(
                  `Fetched transcoded audio duration from server: ${updatedChapter.duration}s`,
                );
              }
            })
            .catch((err) =>
              console.error("Failed to fetch transcoded audio duration", err),
            );
        }
      }

      setDuration(browserDuration);

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

  const jumpToBookmark = (chapterId: string, position: number) => {
    if (!currentBook) return;
    const chapter = chapters.find((item) => item.id === chapterId);
    if (!chapter) {
      alert(t('bookmarks.chapterUnavailable', '当前书籍章节尚未加载'));
      return;
    }
    if (currentChapter?.id === chapterId) {
      seekToTime(position);
      usePlayerStore.setState({ isPlaying: true });
    } else {
      playChapter(currentBook, chapters, chapter, position);
    }
    setShowBookmarks(false);
  };

  const [isSeeking, setIsSeeking] = useState(false);
  const [seekTime, setSeekTime] = useState(0);

  const handleSeek = (e: React.SyntheticEvent<HTMLInputElement>) => {
    const time = parseFloat((e.target as HTMLInputElement).value);
    setSeekTime(time);
    if (!isSeeking) {
      if (isUsingHlsForCurrentChapter) {
        setCurrentTime(time);
        return;
      }
      if (audioRef.current) {
        audioRef.current.currentTime = time;
      }
      setCurrentTime(time);
    }
  };

  const handleSeekStart = () => {
    setIsSeeking(true);
    setSeekTime(currentTime);
  };

  const seekToTime = (time: number) => {
    const targetTime = Math.max(
      0,
      duration > 0 ? Math.min(time, duration) : time,
    );

    if (audioRef.current) {
      if (isUsingHlsForCurrentChapter && hlsSessionId) {
        const requestId = ++hlsRequestIdRef.current;
        setHlsSeekOffset(targetTime);
        setCurrentTime(targetTime);
        isInitialLoadRef.current = false;

        apiClient
          .post(`/api/stream/hls/${hlsSessionId}/seek`, null, {
            params: { seek: targetTime },
          })
          .then((res) => {
            if (requestId !== hlsRequestIdRef.current) return;
            const playlistUrl = res.data?.playlist_url;
            if (!playlistUrl) {
              throw new Error("HLS seek response missing playlist URL");
            }
            setHlsStreamUrl(toAbsoluteMediaUrl(playlistUrl));
          })
          .catch((err) => {
            if (requestId !== hlsRequestIdRef.current) return;
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

  const formatTime = formatPlayerTime;
  const getLocalizedChapterProgressText = React.useCallback(
    (chapter: Chapter) =>
      getChapterProgressText(chapter, {
        complete: t("player.playedComplete"),
        percent: (percent) => t("player.playedPercent", { percent }),
      }),
    [t],
  );

  const isHiddenPage = isMiniPlayerHiddenPath(location.pathname);
  const isWidgetMode = getRuntimePathname().startsWith("/widget");

  // Auto collapse player when navigating to hidden pages
  useEffect(() => {
    if (isHiddenPage && isExpanded) {
      setTimeout(() => setIsExpanded(false), 0);
    }
  }, [location.pathname, isExpanded, isHiddenPage, setIsExpanded]);

  useEffect(() => {
    const timer = window.setTimeout(() => setShowVolumeControl(false), 0);
    return () => window.clearTimeout(timer);
  }, [isExpanded]);

  // Fullscreen Logic for Widget
  const { toggleFullscreen, exitExpanded: handleExitExpanded } =
    useWidgetFullscreen({
      isWidgetMode,
      setIsExpanded,
    });

  if (!currentChapter) return null;

  const miniPlayerStyle = !isExpanded
    ? {
        bottom: isWidgetMode ? "0" : "var(--mini-player-offset)",
        height: isWidgetMode
          ? "100%"
          : isCollapsed
            ? "64px"
            : "var(--player-h)",
        left: isWidgetMode ? "0" : undefined,
        right: isWidgetMode ? "0" : undefined,
      }
    : {};

  const audioSrc = shouldUseHlsForCurrentChapter
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
    if (currentBook && currentChapter) {
      const finalPosition = Math.floor(duration);
      // Sync via both WS and HTTP for reliability
      wsSendProgress(currentBook.id, currentChapter.id, finalPosition);
      apiClient
        .post("/api/progress", {
          book_id: currentBook.id,
          chapter_id: currentChapter.id,
          position: finalPosition,
        })
        .catch((err) => console.error("Failed to sync final progress", err));
    }
    // 按集数睡眠：播完设定集数后停在下一条开头，不再继续播放。
    advanceToNextChapter();
  };

  const openChapterList = () => {
    if (currentChapter && chapters.length > 0) {
      const isExtra =
        !!currentChapter.is_extra ||
        /\u756a\u5916|SP|Extra/i.test(currentChapter.title);
      const targetTab = isExtra ? "extra" : "main";
      if (activeTab !== targetTab) setActiveTab(targetTab);

      const targetList = chapters.filter((chapter) => {
        const chapterIsExtra =
          !!chapter.is_extra || /\u756a\u5916|SP|Extra/i.test(chapter.title);
        return chapterIsExtra === isExtra;
      });

      const index = targetList.findIndex(
        (chapter) => chapter.id === currentChapter.id,
      );
      if (index !== -1) {
        const groupIndex = Math.floor(index / chaptersPerGroup);
        setCurrentGroupIndex(groupIndex);

        setTimeout(() => {
          const chapterEl = document.getElementById(
            `player-chapter-${currentChapter.id}`,
          );
          if (chapterEl) {
            chapterEl.scrollIntoView({ block: "center", behavior: "smooth" });
          }

          const groupTab = document.getElementById(
            `player-group-tab-${groupIndex}`,
          );
          const container = scrollRef.current;
          if (groupTab && container) {
            const containerWidth = container.offsetWidth;
            const tabWidth = groupTab.offsetWidth;
            const tabLeft = groupTab.offsetLeft;

            container.scrollTo({
              left: tabLeft - containerWidth / 2 + tabWidth / 2,
              behavior: "smooth",
            });
          }
        }, 100);
      }
    }

    setShowChapters(true);
  };

  return (
    <div
      className={`
        transition-all duration-500 ease-in-out
        ${(isHiddenPage || isSeriesEditing) && !isExpanded ? "translate-y-full opacity-0 pointer-events-none" : ""}
        ${
          isExpanded
            ? "fixed inset-0 z-[110] bg-white dark:bg-slate-950"
            : "absolute left-0 right-0 z-[30] bg-transparent pointer-events-none"
        }
      `}
      style={miniPlayerStyle}
    >
      <audio
        key={currentChapter.id}
        ref={audioRef}
        src={audioSrc || undefined}
        onTimeUpdate={handleTimeUpdate}
        onProgress={handleProgress}
        onLoadedMetadata={handleLoadedMetadata}
        onCanPlay={() => {
          setError(null);
          const audio = audioRef.current;
          if (
            audio &&
            audio.paused &&
            usePlayerStore.getState().isPlaying
          ) {
            requestAudioPlayback(audio);
          }
        }}
        onEnded={handleEnded}
        onPlay={() => {
          setIsPlaying(true);
          if (audioRef.current) {
            audioRef.current.playbackRate = playbackSpeed;
          }
        }}
        onPause={() => setIsPlaying(false)}
        onError={(e) => {
          const audio = audioRef.current;
          console.log("Audio error event fired", {
            error: audio?.error,
            code: audio?.error?.code,
            message: audio?.error?.message,
            retryCount,
            shouldTranscode,
          });

          if (audio && audio.error) {
            // Ignore aborted errors (code 4) ONLY if we are not already trying to recover
            // Actually code 4 is MEDIA_ERR_SRC_NOT_SUPPORTED, which is exactly what we want to catch for WMA
            // Code 1 is MEDIA_ERR_ABORTED

            if (audio.error.code === 1) {
              console.log("Playback aborted by user action");
              return;
            }

            // A network error (or an ambiguous unsupported error on a remote .strm)
            // must not silently turn a direct stream into a server download.
            if (!shouldTranscode && retryCount < 3 &&
                (!isStrmPath(currentChapter?.path) || audio.error.code === 3)) {
              tryTranscodeFallback();
              return;
            }
            console.error("Audio element error", audio.error);
          } else {
            if (!isStrmPath(currentChapter?.path) && !shouldTranscode && retryCount < 3) {
              tryTranscodeFallback();
              return;
            }
            console.error("Audio element error (unknown)", e);
          }
          setError(t(isStrmPath(currentChapter?.path)
            ? "player.strmDirectError" : "player.audioLoadError"));
        }}
      />

      {error && !isExpanded && (
        <div className="absolute top-0 left-4 right-4 bg-red-500 text-white text-[10px] py-1 px-2 text-center rounded-t-lg animate-pulse z-[101]">
          {error}
        </div>
      )}
      {/* Mini Player - Floating Card Style on Mobile */}
      {!isExpanded && (
        <div
          className={`h-full ${isWidgetMode ? "px-0" : "px-2 sm:px-4"} pointer-events-none`}
        >
          {isCollapsed ? (
            <CollapsedPlayerView
              book={currentBook}
              coverSizeClass={collapsedCoverSizeClass}
              themeColor={miniPlayerThemeColor}
              onExpandCollapsed={() => setIsCollapsed(false)}
            />
          ) : (
            /* Normal Mini Player */
            <div
              className={`
              h-full ${isWidgetMode ? "max-w-none rounded-none border-none shadow-none" : "max-w-7xl mx-auto rounded-2xl sm:rounded-3xl shadow-2xl shadow-black/10 border border-slate-200/50 dark:border-slate-800/50"}
              bg-white/95 dark:bg-slate-900/95 backdrop-blur-md 
              flex items-center justify-between gap-3 sm:gap-4 ${isWidgetMode ? "px-3 max-[380px]:flex-col max-[380px]:justify-center max-[380px]:gap-1.5 max-[380px]:py-2" : "px-3 sm:px-6"} pointer-events-auto
              transition-all duration-300
            `}
              style={{
                backgroundColor: isWidgetMode
                  ? undefined
                  : miniPlayerThemeColor
                    ? setAlpha(miniPlayerThemeColor, 0.05)
                    : undefined,
                borderColor: isWidgetMode
                  ? undefined
                  : miniPlayerThemeColor
                    ? setAlpha(miniPlayerThemeColor, 0.2)
                    : undefined,
              }}
            >
              {/* Info */}
              <MiniPlayerBookInfo
                book={currentBook}
                chapterTitle={currentChapter.title}
                coverSizeClass={miniCoverSizeClass}
                isWidgetMode={isWidgetMode}
                onCoverClick={toggleFullscreen}
              />

              {/* Widget Vertical Layout: Progress Bar (Visible only on small widget) */}
              {isWidgetMode && (
                <div className="hidden max-[380px]:block w-full px-1 py-1">
                  <ProgressBar
                    isMini={true}
                    isSeeking={isSeeking}
                    seekTime={seekTime}
                    currentTime={currentTime}
                    duration={duration}
                    bufferedTime={bufferedTime}
                    themeColor={miniPlayerThemeColor}
                    onSeek={handleSeek}
                    onSeekStart={handleSeekStart}
                    onSeekEnd={handleSeekEnd}
                  />
                </div>
              )}

              {/* Controls (Desktop) */}
              <MiniPlayerDesktopControls
                isPlaying={isPlaying}
                currentTime={currentTime}
                duration={duration}
                themeColor={miniPlayerThemeColor}
                effectiveThemeColor={effectiveThemeColor}
                useDarkControls={useDarkControls}
                formatTime={formatTime}
                onPrev={prevChapter}
                onNext={nextChapter}
                onTogglePlay={togglePlay}
                onSeekTo={seekToTime}
                isSeeking={isSeeking}
                seekTime={seekTime}
                bufferedTime={bufferedTime}
                onSeek={handleSeek}
                onSeekStart={handleSeekStart}
                onSeekEnd={handleSeekEnd}
              />

              {/* Mobile Controls - Only visible on small screens */}
              <MiniPlayerMobileControls
                isPlaying={isPlaying}
                isWidgetMode={isWidgetMode}
                themeColor={miniPlayerThemeColor}
                effectiveThemeColor={effectiveThemeColor}
                useDarkControls={useDarkControls}
                currentTime={currentTime}
                duration={duration}
                bufferedTime={bufferedTime}
                isSeeking={isSeeking}
                seekTime={seekTime}
                onSeek={handleSeek}
                onSeekStart={handleSeekStart}
                onSeekEnd={handleSeekEnd}
                onTogglePlay={togglePlay}
                onPrev={prevChapter}
                onNext={nextChapter}
                onSeekTo={seekToTime}
                onCollapse={() => setIsCollapsed(true)}
              />

              {/* Desktop Extra Controls - Visible on Tablet and Desktop */}
              <MiniPlayerDesktopExtras
                volume={volume}
                isMuted={isMuted}
                showVolumeControl={showVolumeControl}
                volumeControlRef={
                  !isExpanded ? volumeControlRef : { current: null }
                }
                themeColor={miniPlayerThemeColor}
                useDarkControls={useDarkControls}
                playbackSpeed={playbackSpeed}
                onToggleVolumeControl={() =>
                  setShowVolumeControl(!showVolumeControl)
                }
                onChangeVolume={setVolume}
                onToggleMuted={() => setIsMuted(!isMuted)}
                onCyclePlaybackSpeed={() =>
                  setPlaybackSpeed(
                    playbackSpeed === 2 ? 1 : playbackSpeed + 0.25,
                  )
                }
                onCollapse={() => setIsCollapsed(true)}
                onExpand={() => setIsExpanded(true)}
              />
            </div>
          )}
        </div>
      )}

      {/* Expanded Player View */}
      {isExpanded && (
        <div
          className="absolute inset-0 flex flex-col p-4 sm:p-8 md:p-12 overflow-y-auto animate-in slide-in-from-bottom duration-500 bg-white dark:bg-slate-950"
            style={{
              paddingTop: "max(env(safe-area-inset-top, 0px), clamp(1rem, 3vw, 3rem))",
              paddingBottom: "max(env(safe-area-inset-bottom, 0px), clamp(1rem, 3vw, 3rem))",
            backgroundColor: isWidgetMode
              ? effectiveThemeColor
                ? toSolidColor(effectiveThemeColor)
                : "#1e293b"
              : effectiveThemeColor
                ? setAlpha(effectiveThemeColor, 0.05)
                : undefined,
          }}
        >
          <ExpandedPlayerHeader
            chapterTitle={currentChapter.title}
            bookTitle={currentBook?.title}
            onExit={handleExitExpanded}
            onOpenSettings={() => setShowSettings(true)}
          />

          <div className="flex-1 flex flex-col items-center justify-center max-w-[520px] mx-auto w-full gap-5 sm:gap-7">
            <ExpandedCoverAndMeta
              book={currentBook}
              chapterTitle={currentChapter.title}
              expandedCoverSizeClass={expandedCoverSizeClass}
              error={error}
            />

            <div className="w-full flex flex-col gap-7 sm:gap-8">
              {/* Progress Bar Section */}
              <ExpandedProgressSection
                currentTime={currentTime}
                duration={duration}
                bufferedTime={bufferedTime}
                isSeeking={isSeeking}
                seekTime={seekTime}
                themeColor={effectiveThemeColor || "#60a5fa"}
                formatTime={formatTime}
                onSeek={handleSeek}
                onSeekStart={handleSeekStart}
                onSeekEnd={handleSeekEnd}
                onSeekTo={seekToTime}
              />

              <ExpandedMainControls
                isPlaying={isPlaying}
                themeColor={effectiveThemeColor}
                onPrev={prevChapter}
                onTogglePlay={togglePlay}
                onNext={nextChapter}
              />

              <ExpandedBottomControls
                playbackSpeed={playbackSpeed}
                showSpeedControl={showSpeedControl}
                speedControlRef={speedControlRef}
                onToggleSpeedControl={() => setShowSpeedControl((value) => !value)}
                onChangePlaybackSpeed={setPlaybackSpeed}
                onOpenBookmarks={() => setShowBookmarks(true)}
                sleepTimer={sleepTimer}
                showSleepTimer={showSleepTimer}
                customMinutes={customMinutes}
                timerMenuRef={timerMenuRef}
                onToggleShowSleepTimer={() =>
                  setShowSleepTimer(!showSleepTimer)
                }
                onSetCustomMinutes={setCustomMinutes}
                onStartSleepTimer={startSleepTimer}
                onCancelSleepTimer={cancelSleepTimer}
                sleepEpisodes={sleepEpisodes}
                customEpisodes={customEpisodes}
                onSetCustomEpisodes={setCustomEpisodes}
                onStartEpisodeSleepTimer={startEpisodeSleepTimer}
                onCloseSleepTimer={() => setShowSleepTimer(false)}
                onOpenChapterList={openChapterList}
              />
            </div>
          </div>

          {/* Settings Modal */}
          {showSettings && (
            <PlayerSettingsModal
              editSkipIntro={editSkipIntro}
              editSkipOutro={editSkipOutro}
              volume={volume}
              onChangeVolume={(value) => {
                setVolume(value);
                if (value > 0) setIsMuted(false);
              }}
              onChangeSkipIntro={setEditSkipIntro}
              onChangeSkipOutro={setEditSkipOutro}
              onClose={() => setShowSettings(false)}
              onSave={handleSaveSettings}
            />
          )}

          {/* Chapter List Drawer */}
          <ChapterListDrawer
            show={showChapters}
            currentBook={currentBook}
            currentChapter={currentChapter}
            currentChapters={currentChapters}
            groups={groups}
            chaptersPerGroup={chaptersPerGroup}
            currentGroupIndex={currentGroupIndex}
            activeTab={activeTab}
            extraChapters={extraChapters}
            isPlaying={isPlaying}
            effectiveThemeColor={effectiveThemeColor}
            scrollRef={scrollRef}
            onClose={() => setShowChapters(false)}
            onSetActiveTab={setActiveTab}
            onSetCurrentGroupIndex={setCurrentGroupIndex}
            onScrollGroups={scrollGroups}
            onPlayChapter={(chapter) =>
              playChapter(currentBook!, currentChapters, chapter)
            }
            formatTime={formatTime}
            getChapterProgressText={getLocalizedChapterProgressText}
          />
          {showBookmarks && currentBook && currentChapter && <BookmarkManagerModal
            key={currentBook.id}
            bookId={currentBook.id}
            bookTitle={currentBook.title}
            chapterId={currentChapter.id}
            position={currentTime}
            onClose={() => setShowBookmarks(false)}
            onJump={jumpToBookmark}
          />}
        </div>
      )}
    </div>
  );
};

export default Player;
