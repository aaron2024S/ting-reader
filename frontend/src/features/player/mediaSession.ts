interface MediaSessionControls {
  play: () => void;
  pause: () => void;
  previous: () => void;
  next: () => void;
  seek: (position: number) => void;
  getPosition: () => number;
}

export const registerMediaSessionControls = (
  session: MediaSession,
  controls: MediaSessionControls,
) => {
  const handlers: Partial<Record<MediaSessionAction, MediaSessionActionHandler>> = {
    play: controls.play,
    pause: controls.pause,
    previoustrack: controls.previous,
    nexttrack: controls.next,
    seekbackward: ({ seekOffset }) =>
      controls.seek(controls.getPosition() - (seekOffset ?? 15)),
    seekforward: ({ seekOffset }) =>
      controls.seek(controls.getPosition() + (seekOffset ?? 30)),
    seekto: ({ seekTime }) => {
      if (seekTime !== undefined && Number.isFinite(seekTime)) {
        controls.seek(seekTime);
      }
    },
  };
  const registered: MediaSessionAction[] = [];
  for (const [action, handler] of Object.entries(handlers)) {
    try {
      session.setActionHandler(action as MediaSessionAction, handler);
      registered.push(action as MediaSessionAction);
    } catch {
      // Browsers may implement only a subset of Media Session actions.
    }
  }
  return () => {
    for (const action of registered) {
      session.setActionHandler(action, null);
    }
  };
};

export const updateMediaSessionMetadata = (
  session: MediaSession,
  metadata: MediaMetadataInit | null,
) => {
  if (!metadata) {
    session.metadata = null;
  } else if (typeof MediaMetadata !== 'undefined') {
    session.metadata = new MediaMetadata(metadata);
  }
};

export const updateMediaSessionPlayback = (
  session: MediaSession,
  isPlaying: boolean,
  duration: number,
  position: number,
  playbackRate: number,
) => {
  session.playbackState = isPlaying ? 'playing' : 'paused';
  if (typeof session.setPositionState !== 'function') return;

  if (!Number.isFinite(duration) || duration <= 0) {
    // Clear the previous chapter's timeline until the source duration is known.
    session.setPositionState();
    return;
  }
  session.setPositionState({
    duration,
    position: Number.isFinite(position) ? Math.max(0, Math.min(position, duration)) : 0,
    playbackRate: Number.isFinite(playbackRate) && playbackRate > 0 ? playbackRate : 1,
  });
};
