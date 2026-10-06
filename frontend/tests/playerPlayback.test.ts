import assert from 'node:assert/strict';
import { test } from 'node:test';
import { resolvePlaybackDuration } from '../src/core/utils/duration.ts';
import {
  registerMediaSessionControls,
  updateMediaSessionMetadata,
  updateMediaSessionPlayback,
} from '../src/features/player/mediaSession.ts';

const createSession = (unsupported: MediaSessionAction[] = []) => {
  const actions = new Map<MediaSessionAction, MediaSessionActionHandler | null>();
  const positions: (MediaPositionState | undefined)[] = [];
  const session = {
    metadata: null as MediaMetadata | null,
    playbackState: 'none' as MediaSessionPlaybackState,
    setActionHandler(action: MediaSessionAction, handler: MediaSessionActionHandler | null) {
      if (unsupported.includes(action)) throw new Error('Unsupported action');
      actions.set(action, handler);
    },
    setPositionState(state?: MediaPositionState) {
      positions.push(state);
    },
  } as MediaSession;
  return { session, actions, positions };
};

test('chapter duration overrides partial browser estimates and Infinity', () => {
  assert.equal(resolvePlaybackDuration(660, 120, true), 660);
  assert.equal(resolvePlaybackDuration(660, Infinity, true), 660);
  assert.equal(resolvePlaybackDuration(660, 650, false), 660);
});

test('transcodes never use or persist an estimated duration', () => {
  for (const source of [undefined, 0, -1, NaN, Infinity]) {
    for (const estimate of [120, Infinity, NaN, 0]) {
      assert.equal(resolvePlaybackDuration(source, estimate, true), 0);
    }
  }
});

test('ordinary audio can use finite browser metadata', () => {
  assert.equal(resolvePlaybackDuration(undefined, 660, false), 660);
  for (const estimate of [Infinity, NaN, 0, -1]) {
    assert.equal(resolvePlaybackDuration(undefined, estimate, false), 0);
  }
});

test('notification controls route actions and use the latest chapter position', () => {
  const { session, actions } = createSession();
  const calls: (string | number)[] = [];
  let position = 100;
  const cleanup = registerMediaSessionControls(session, {
    play: () => calls.push('play'),
    pause: () => calls.push('pause'),
    previous: () => calls.push('previous'),
    next: () => calls.push('next'),
    seek: (target) => calls.push(target),
    getPosition: () => position,
  });
  const dispatch = (action: MediaSessionAction, details = {}) =>
    actions.get(action)?.({ action, ...details });

  dispatch('play');
  dispatch('pause');
  dispatch('previoustrack');
  dispatch('nexttrack');
  dispatch('seekbackward');
  position = 200;
  dispatch('seekforward');
  dispatch('seekforward', { seekOffset: 10 });
  dispatch('seekto', { seekTime: 400 });
  dispatch('seekto');
  dispatch('seekto', { seekTime: NaN });
  assert.deepEqual(calls, ['play', 'pause', 'previous', 'next', 85, 230, 210, 400]);

  cleanup();
  assert.equal(actions.size, 7);
  assert.ok([...actions.values()].every((handler) => handler === null));
});

test('unsupported media actions do not prevent registration or cleanup', () => {
  const { session, actions } = createSession(['seekto', 'nexttrack']);
  const cleanup = registerMediaSessionControls(session, {
    play() {}, pause() {}, previous() {}, next() {}, seek() {},
    getPosition: () => 0,
  });
  assert.equal(typeof actions.get('play'), 'function');
  assert.equal(actions.has('seekto'), false);
  assert.doesNotThrow(cleanup);
});

test('system progress uses full chapter duration and playback speed', () => {
  const { session, positions } = createSession();
  updateMediaSessionPlayback(session, true, 660, 180, 1.5);
  assert.equal(session.playbackState, 'playing');
  assert.deepEqual(positions.at(-1), { duration: 660, position: 180, playbackRate: 1.5 });
  updateMediaSessionPlayback(session, false, 660, 180, 1.5);
  assert.equal(session.playbackState, 'paused');
});

test('system progress clears stale duration when the new source is unknown', () => {
  const { session, positions } = createSession();
  updateMediaSessionPlayback(session, true, 660, 180, 1);
  for (const duration of [0, NaN, Infinity, -1]) {
    updateMediaSessionPlayback(session, true, duration, 180, 1);
    assert.equal(positions.at(-1), undefined);
  }
});

test('system progress clamps positions and rejects invalid rates', () => {
  const { session, positions } = createSession();
  for (const [position, expected] of [[-10, 0], [800, 660], [NaN, 0]]) {
    updateMediaSessionPlayback(session, true, 660, position, 0);
    assert.deepEqual(positions.at(-1), { duration: 660, position: expected, playbackRate: 1 });
  }
});

test('browsers without system position support can still play and pause', () => {
  const session = { playbackState: 'none' } as MediaSession;
  assert.doesNotThrow(() => updateMediaSessionPlayback(session, true, 660, 10, 1));
  assert.equal(session.playbackState, 'playing');
});

test('metadata contains chapter, book, narrator and authenticated cover URL', () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'MediaMetadata');
  Object.defineProperty(globalThis, 'MediaMetadata', {
    configurable: true,
    value: class {
      constructor(metadata: MediaMetadataInit) {
        Object.assign(this, metadata);
      }
    },
  });
  try {
    const { session } = createSession();
    const metadata = {
      title: 'Chapter 1',
      artist: 'Narrator',
      album: 'Audiobook',
      artwork: [{ src: 'https://reader.test/ting/api/proxy/cover?book_id=1&token=test' }],
    };
    updateMediaSessionMetadata(session, metadata);
    assert.equal(session.metadata?.title, metadata.title);
    assert.equal(session.metadata?.artist, metadata.artist);
    assert.equal(session.metadata?.album, metadata.album);
    assert.deepEqual(session.metadata?.artwork, metadata.artwork);
    updateMediaSessionMetadata(session, { ...metadata, title: 'Chapter 2' });
    assert.equal(session.metadata?.title, 'Chapter 2');
    updateMediaSessionMetadata(session, null);
    assert.equal(session.metadata, null);
  } finally {
    if (original) Object.defineProperty(globalThis, 'MediaMetadata', original);
    else Reflect.deleteProperty(globalThis, 'MediaMetadata');
  }
});

test('browsers without MediaMetadata do not throw', () => {
  const { session } = createSession();
  assert.doesNotThrow(() => updateMediaSessionMetadata(session, { title: 'Chapter' }));
});
