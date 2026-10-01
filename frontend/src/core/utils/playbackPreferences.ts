export const PLAYBACK_PREFERENCES_UPDATED = "ting-reader:playback-preferences-updated";
const PLAYER_VOLUME_KEY = "ting-reader.player-volume.v1";

export const chapterProgressPercent = (position: number, duration: number) =>
  Number.isFinite(position) && duration > 0
    ? Math.max(0, Math.min(100, Math.round(position / duration * 100)))
    : 0;

export const loadPlayerVolume = () => {
  try {
    const stored = localStorage.getItem(PLAYER_VOLUME_KEY);
    if (stored === null) return 1;
    const value = Number(stored);
    return Number.isFinite(value) ? Math.max(0, Math.min(1, value)) : 1;
  } catch {
    return 1;
  }
};

export const coverProgressPercent = (value = 0) => {
  if (!Number.isFinite(value) || value <= 0) return 0;
  if (value >= 100) return 100;
  return Math.max(1, Math.min(99, Math.round(value)));
};

export const savePlayerVolume = (volume: number) => {
  try {
    localStorage.setItem(PLAYER_VOLUME_KEY, String(Math.max(0, Math.min(1, volume))));
  } catch {
    // Playback remains usable when browser storage is unavailable.
  }
};

export interface PlaybackPreferences {
  auto_preload: boolean;
  auto_cache: boolean;
}

export const notifyPlaybackPreferences = (settings: PlaybackPreferences) => {
  window.dispatchEvent(new CustomEvent<PlaybackPreferences>(
    PLAYBACK_PREFERENCES_UPDATED, { detail: settings },
  ));
};
