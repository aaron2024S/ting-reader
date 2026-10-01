import apiClient from './client';
import type { Book, Chapter } from '../types';
import { usePlayerStore } from '../stores/playerStore';

export interface ReadingPage<T> { items: T[]; total: number; page: number; page_size: number }
export interface ActivityBook {
  book_id: string;
  book_title?: string;
  cover_url?: string;
  library_id: string;
  chapter_count: number;
  updated_at: string;
  latest_chapter_id?: string;
  latest_chapter_title?: string;
  latest_position: number;
  latest_duration: number;
  latest_note?: string;
}
export interface Bookmark {
  id: string;
  book_id: string;
  chapter_id: string;
  chapter_title?: string;
  chapter_duration: number;
  position: number;
  note: string;
  created_at: string;
  updated_at: string;
}
export interface HistorySummary { books: number; chapters: number; position_seconds: number }
export const BOOKMARK_SEEK_REQUESTED = 'ting-reader:bookmark-seek-requested';

export const playBookmark = async (bookmark: Bookmark) => {
  const [book, chapters] = await Promise.all([
    apiClient.get<Book>(`/api/books/${bookmark.book_id}`),
    apiClient.get<Chapter[]>(`/api/books/${bookmark.book_id}/chapters`),
  ]);
  const chapter = chapters.data.find((item) => item.id === bookmark.chapter_id);
  if (!chapter) throw new Error('Bookmark chapter unavailable');
  const player = usePlayerStore.getState();
  if (player.currentChapter?.id === bookmark.chapter_id) {
    window.dispatchEvent(new CustomEvent(BOOKMARK_SEEK_REQUESTED, {
      detail: { chapterId: bookmark.chapter_id, position: bookmark.position },
    }));
  } else {
    player.playChapter(book.data, chapters.data, chapter, bookmark.position);
  }
};

export const formatBookmarkPosition = (seconds: number) => {
  const safe = Math.max(0, Math.floor(seconds));
  return `${Math.floor(safe / 3600).toString().padStart(2, '0')}:${Math.floor(safe % 3600 / 60).toString().padStart(2, '0')}:${(safe % 60).toString().padStart(2, '0')}`;
};
