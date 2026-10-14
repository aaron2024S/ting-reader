export interface HlsPlaybackInfo {
  session_id: string;
  playlist_url: string;
  start_offset?: number;
  playback_time?: number;
}

/** Minute and episode limits are mutually exclusive. Absolute deadlines
 * survive throttled browser timers, while paused time never consumes minutes. */
export class PlaybackSleep {
  private deadline: number | null = null;
  private pausedAt: number | null = null;
  private remainingEpisodes: number | null = null;

  startSeconds(seconds: number, playing: boolean, now = Date.now()) {
    if (!Number.isFinite(seconds) || seconds <= 0) return;
    this.cancel();
    this.deadline = now + seconds * 1000;
    this.pausedAt = playing ? null : now;
  }

  startEpisodes(episodes: number) {
    if (!Number.isInteger(episodes) || episodes <= 0) return;
    this.cancel();
    this.remainingEpisodes = episodes;
  }

  setPlaying(playing: boolean, now = Date.now()) {
    if (this.deadline === null) return;
    if (!playing && this.pausedAt === null) this.pausedAt = now;
    else if (playing && this.pausedAt !== null) {
      this.deadline += now - this.pausedAt;
      this.pausedAt = null;
    }
  }

  snapshot(now = Date.now()) {
    return { seconds: this.deadline === null ? null
      : Math.max(0, Math.ceil((this.deadline - (this.pausedAt ?? now)) / 1000)),
      episodes: this.remainingEpisodes };
  }

  expired(now = Date.now()) {
    if (this.pausedAt !== null || this.snapshot(now).seconds !== 0) return false;
    this.cancel();
    return true;
  }

  chapterEnded() {
    if (this.remainingEpisodes === null) return false;
    if (--this.remainingEpisodes > 0) return false;
    this.cancel();
    return true;
  }

  cancel() {
    this.deadline = null;
    this.pausedAt = null;
    this.remainingEpisodes = null;
  }
}

export interface HlsTransport {
  create(): Promise<HlsPlaybackInfo>;
  seek(session: string, position: number): Promise<HlsPlaybackInfo>;
  touch(session: string): Promise<unknown>;
  release(session: string, pageClosing: boolean): Promise<unknown>;
}

const validateInfo = (info: HlsPlaybackInfo, requireSession: boolean) => {
  if (!info || typeof info.playlist_url !== 'string' || !info.playlist_url
    || requireSession && (typeof info.session_id !== 'string' || !info.session_id)
    || [info.start_offset, info.playback_time].some(
      (value) => value !== undefined && (!Number.isFinite(value) || value < 0),
    )) {
    throw new Error('Invalid HLS playback response');
  }
  return info;
};

/** Owns one chapter's server session, heartbeat and serialized seeks.
 * Disposed owners release late creation responses and never publish late seeks.
 */
export class HlsPlaybackSession {
  private disposed = false;
  private session: string | null = null;
  private heartbeat: ReturnType<typeof setInterval> | undefined;
  private seekRevision = 0;
  private seekQueue: Promise<void> = Promise.resolve();
  private released = false;
  private opening: Promise<HlsPlaybackInfo | null> | null = null;
  private heartbeatPending = false;
  private pageClosing = false;
  private readonly transport: HlsTransport;
  private readonly onHeartbeatFailure: (error: unknown) => void;
  private readonly heartbeatMs: number;

  constructor(
    transport: HlsTransport,
    onHeartbeatFailure: (error: unknown) => void,
    heartbeatMs = 30_000,
  ) {
    this.transport = transport;
    this.onHeartbeatFailure = onHeartbeatFailure;
    this.heartbeatMs = heartbeatMs;
  }

  open(): Promise<HlsPlaybackInfo | null> {
    if (this.disposed) return Promise.resolve(null);
    this.opening ??= this.create();
    return this.opening;
  }

  private async create(): Promise<HlsPlaybackInfo | null> {
    const info = await this.transport.create();
    // A malformed response can still own server resources.
    if (typeof info?.session_id === 'string' && info.session_id) {
      this.session = info.session_id;
    }
    if (this.disposed) {
      this.release(this.pageClosing);
      return null;
    }
    try {
      validateInfo(info, true);
    } catch (error) {
      this.release(false);
      throw error;
    }
    this.heartbeat = setInterval(() => {
      if (!this.disposed && this.session && !this.heartbeatPending) {
        this.heartbeatPending = true;
        void this.transport.touch(this.session).catch((error: unknown) => {
          if (!this.disposed) this.onHeartbeatFailure(error);
        }).finally(() => {
          this.heartbeatPending = false;
        });
      }
    }, this.heartbeatMs);
    return info;
  }

  cancelSeek() {
    this.seekRevision++;
  }

  seek(position: number): Promise<HlsPlaybackInfo | null> {
    if (!Number.isFinite(position) || position < 0) {
      return Promise.reject(new Error('Invalid seek position'));
    }
    const revision = ++this.seekRevision;
    let result: HlsPlaybackInfo | null = null;
    const request = this.seekQueue.then(async () => {
      if (this.disposed || revision !== this.seekRevision || !this.session) return;
      const info = await this.transport.seek(this.session, position);
      if (this.disposed || revision !== this.seekRevision) return;
      result = validateInfo(info, false);
    });
    this.seekQueue = request.catch(() => {});
    return request.then(() => result);
  }

  dispose(pageClosing = false) {
    this.disposed = true;
    this.pageClosing ||= pageClosing;
    this.cancelSeek();
    clearInterval(this.heartbeat);
    this.heartbeat = undefined;
    this.release(pageClosing);
  }

  private release(pageClosing: boolean) {
    if (!this.session || this.released) return;
    this.released = true;
    void this.transport.release(this.session, pageClosing).catch(() => {});
  }
}
