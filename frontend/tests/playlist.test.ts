import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Book } from '../src/core/types/book.ts';
import type { Playlist } from '../src/core/types/playlist.ts';
import {
  collectPlaylistCovers,
  getPlaylistBookCount,
} from '../src/core/utils/playlist.ts';

const books = ['one', 'two', 'three'].map(id => ({ id, title: id }) as Book);
const playlist: Playlist = {
  id: 'list',
  user_id: 'user',
  title: 'Playlist',
  created_at: '',
  updated_at: '',
  books,
  book_ids: books.map(book => book.id),
};

test('playlist stacks use up to three distinct books and preserve home single-cover behavior', () => {
  const stack = collectPlaylistCovers(playlist, 42, 3);
  assert.equal(stack.length, 3);
  assert.equal(new Set(stack.map(cover => cover.book_id)).size, 3);
  assert.deepEqual(collectPlaylistCovers(playlist, 42), stack.slice(0, 1));
  assert.deepEqual(collectPlaylistCovers(playlist, 42, 3), stack);
  assert.equal(collectPlaylistCovers({ ...playlist, books: books.slice(0, 1) }, 42, 3).length, 1);
  assert.deepEqual(collectPlaylistCovers({ ...playlist, books: [] }, 42, 3), []);
});

test('series and explicit books share cover candidates without duplicating stack layers', () => {
  const mixed: Playlist = {
    ...playlist,
    items: [
      { item_type: 'book', item_id: 'one', order: 1, book: books[0] },
      {
        item_type: 'series',
        item_id: 'series',
        order: 2,
        series: { id: 'series', title: 'Series', books } as NonNullable<NonNullable<Playlist['items']>[number]['series']>,
      },
    ],
  };
  assert.equal(getPlaylistBookCount(mixed), 4);
  assert.equal(collectPlaylistCovers(mixed, 42, 3).length, 3);
  assert.equal(new Set(collectPlaylistCovers(mixed, 42, 3).map(cover => cover.book_id)).size, 3);
});

test('legacy playlists retain their covers and count when items are empty or unresolved', () => {
  assert.equal(getPlaylistBookCount({ ...playlist, items: [] }), 3);
  assert.equal(getPlaylistBookCount({ ...playlist, items: [], books: [] }), 3);
  assert.equal(collectPlaylistCovers({
    ...playlist,
    items: [{ item_type: 'book', item_id: 'missing', order: 1 }],
  }, 0, 3).length, 3);
});
