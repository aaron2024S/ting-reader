import React, { useCallback, useEffect, useState } from 'react';
import { Link } from 'react-router';
import apiClient from '../../core/api/client';
import type { ActivityBook, ReadingPage as Page } from '../../core/api/reading';
import type { Progress } from '../../core/types';
import { usePlayerStore } from '../../core/stores/playerStore';
import BackButton from '../../shared/widgets/BackButton';
import { CheckSquare, Clock, History, Play, Square, Trash2, X } from 'lucide-react';
import { useBookshelfCoverShape, type CoverShape } from '../../core/hooks/useBookshelfCoverShape';
import LoadingSpinner from '../../shared/ui/LoadingSpinner';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { parseBackendDate } from '../../core/utils/date';
import { getApplicationDayDifference, getApplicationTimeZone, useApplicationTimeZone } from '../../core/utils/timeZone';
import ActivityBookCard, { ActivityProgress } from './ActivityBookCard';
import { chapterProgressPercent } from '../../core/utils/playbackPreferences';

const PAGE_SIZE = 40;
const CHAPTER_PAGE_SIZE = 50;
const progressKey = (item: Progress) => item.id || `${item.book_id}:${item.chapter_id}`;

const HistoryPage: React.FC = () => {
  const { t } = useTranslation();
  useApplicationTimeZone();
  const currentChapter = usePlayerStore((state) => state.currentChapter);
  const coverShape = useBookshelfCoverShape();
  const [books, setBooks] = useState<ActivityBook[]>([]);
  const [page, setPage] = useState(1);
  const [total, setTotal] = useState(0);
  const [chapters, setChapters] = useState<Record<string, Progress[]>>({});
  const [chapterPages, setChapterPages] = useState<Record<string, number>>({});
  const [chapterTotals, setChapterTotals] = useState<Record<string, number>>({});
  const [loadingChapters, setLoadingChapters] = useState<Set<string>>(new Set());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [selectionMode, setSelectionMode] = useState(false);
  const [selectedBooks, setSelectedBooks] = useState<Set<string>>(new Set());
  const [selectedProgress, setSelectedProgress] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [deleting, setDeleting] = useState(false);
  const [confirmClear, setConfirmClear] = useState(false);
  const [clearAll, setClearAll] = useState(false);
  const [clearProgress, setClearProgress] = useState(false);

  const fetchBooks = useCallback(async (targetPage: number) => {
    try {
      const response = await apiClient.get<Page<ActivityBook>>('/api/history/books', {
        params: { page: targetPage, page_size: PAGE_SIZE },
      });
      setBooks(response.data.items || []);
      setTotal(response.data.total || 0);
      setPage(targetPage);
      setChapters({});
      setChapterPages({});
      setChapterTotals({});
      setExpanded(new Set());
    } catch (error) {
      console.error('获取我的历史失败', error);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    const timer = window.setTimeout(() => void fetchBooks(1), 0);
    return () => window.clearTimeout(timer);
  }, [fetchBooks]);
  useEffect(() => {
    const onFocus = () => void fetchBooks(page);
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  }, [fetchBooks, page]);

  const loadChapters = async (bookId: string, targetPage = 1) => {
    setLoadingChapters((current) => new Set(current).add(bookId));
    try {
      const response = await apiClient.get<Page<Progress>>(`/api/history/books/${bookId}/chapters`, {
        params: { page: targetPage, page_size: CHAPTER_PAGE_SIZE },
      });
      setChapters((current) => ({
        ...current,
        [bookId]: response.data.items,
      }));
      setChapterPages((current) => ({ ...current, [bookId]: targetPage }));
      setChapterTotals((current) => ({ ...current, [bookId]: response.data.total }));
    } catch (error) {
      console.error('获取历史章节失败', error);
      alert(t('historyPage.loadFailed', '加载章节失败'));
    } finally {
      setLoadingChapters((current) => { const next = new Set(current); next.delete(bookId); return next; });
    }
  };

  const toggleExpanded = (bookId: string) => {
    const isOpen = expanded.has(bookId);
    setExpanded((current) => {
      const next = new Set(current);
      if (isOpen) next.delete(bookId);
      else next.add(bookId);
      return next;
    });
    if (!isOpen && !chapters[bookId]) void loadChapters(bookId);
  };

  const beginDelete = (all: boolean) => {
    setClearAll(all);
    setClearProgress(false);
    setConfirmClear(true);
  };

  const deleteHistory = async () => {
    if (deleting) return;
    setDeleting(true);
    try {
      const bookIds = [...selectedBooks];
      const progressIds = [...selectedProgress];
      const count = clearAll ? 1 : Math.max(bookIds.length, progressIds.length);
      for (let offset = 0; offset < count; offset += 250) {
        await apiClient.post('/api/progress/recent/delete', {
          all: clearAll,
          book_ids: clearAll ? [] : bookIds.slice(offset, offset + 250),
          progress_ids: clearAll ? [] : progressIds.slice(offset, offset + 250),
          clear_progress: clearProgress,
        });
      }
      setConfirmClear(false);
      setSelectedBooks(new Set());
      setSelectedProgress(new Set());
      setSelectionMode(false);
      setExpanded(new Set());
      setChapters({});
      await fetchBooks(1);
    } catch (error) {
      console.error('清除播放历史失败', error);
      alert(t('historyPage.deleteFailed'));
    } finally {
      setDeleting(false);
    }
  };

  const toggleBookSelection = (bookId: string) => {
    setSelectedBooks((current) => {
      const next = new Set(current);
      if (next.has(bookId)) next.delete(bookId);
      else next.add(bookId);
      return next;
    });
  };
  const toggleProgressSelection = (item: Progress) => {
    const id = progressKey(item);
    setSelectedProgress((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };
  const selectCurrentPage = () => {
    const allSelected = books.length > 0 && books.every((book) => selectedBooks.has(book.book_id));
    setSelectedBooks((current) => {
      const next = new Set(current);
      for (const book of books) {
        if (allSelected) next.delete(book.book_id);
        else next.add(book.book_id);
      }
      return next;
    });
  };

  if (loading) return <LoadingSpinner />;

  return (
    <div className="flex-1 min-h-full flex flex-col p-4 sm:p-6 md:p-8">
      <div className="flex-1 space-y-6">
        <BackButton fallback="/mine" />
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h1 className="flex items-center gap-3 text-2xl font-bold text-slate-900 dark:text-white"><History className="text-primary-600" />{t('historyPage.title')}</h1>
            <p className="mt-1 text-sm text-slate-500 dark:text-slate-400">{t('historyPage.subtitle', { books: total })}</p>
          </div>
          {selectionMode ? (
            <div className="flex flex-wrap items-center gap-2">
              <button onClick={selectCurrentPage} className="inline-flex items-center gap-2 rounded-lg bg-slate-100 px-3 py-2 text-sm dark:bg-slate-800"><CheckSquare size={17} />{t('historyPage.selectAll')}</button>
              <button onClick={() => beginDelete(false)} disabled={!selectedBooks.size && !selectedProgress.size || deleting} className="inline-flex items-center gap-2 rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700 disabled:opacity-50 dark:bg-red-950/30 dark:text-red-300"><Trash2 size={17} />{t('historyPage.deleteSelected', { count: selectedBooks.size + selectedProgress.size })}</button>
              <button onClick={() => beginDelete(true)} disabled={!total || deleting} className="inline-flex items-center gap-2 rounded-lg border border-red-200 px-3 py-2 text-sm text-red-700 disabled:opacity-50 dark:border-red-900 dark:text-red-300"><Trash2 size={17} />{t('historyPage.clearAll', '清空全部')}</button>
              <button onClick={() => { setSelectionMode(false); setSelectedBooks(new Set()); setSelectedProgress(new Set()); }} className="rounded-lg border border-slate-200 p-2 dark:border-slate-700" title={t('historyPage.cancel')}><X size={18} /></button>
            </div>
          ) : (
            <button onClick={() => setSelectionMode(true)} disabled={!total} className="inline-flex items-center gap-2 self-start rounded-lg border border-slate-200 px-3 py-2 text-sm text-slate-700 disabled:opacity-50 dark:border-slate-700 dark:text-slate-200"><CheckSquare size={17} />{t('historyPage.select')}</button>
          )}
        </div>

        {books.length ? <div className="space-y-3">
          {books.map((book) => <HistoryBookSection
            key={book.book_id}
            book={book}
            chapters={chapters[book.book_id] || []}
            chapterTotal={chapterTotals[book.book_id] || 0}
            chapterPage={chapterPages[book.book_id] || 1}
            loadingChapters={loadingChapters.has(book.book_id)}
            coverShape={coverShape}
            expanded={expanded.has(book.book_id)}
            selectionMode={selectionMode}
            bookSelected={selectedBooks.has(book.book_id)}
            selectedProgress={selectedProgress}
            onExpand={() => toggleExpanded(book.book_id)}
            onToggleBook={() => toggleBookSelection(book.book_id)}
            onToggleProgress={toggleProgressSelection}
            onPage={(targetPage) => void loadChapters(book.book_id, targetPage)}
          />)}
          {total > PAGE_SIZE && <div className="flex items-center justify-center gap-4 py-3">
            <button disabled={page <= 1} onClick={() => void fetchBooks(page - 1)} className="rounded border border-slate-200 px-3 py-2 text-sm disabled:opacity-40 dark:border-slate-700">{t('common.previous', '上一页')}</button>
            <span className="text-sm tabular-nums text-slate-500">{page} / {Math.max(1, Math.ceil(total / PAGE_SIZE))}</span>
            <button disabled={page * PAGE_SIZE >= total} onClick={() => void fetchBooks(page + 1)} className="rounded border border-slate-200 px-3 py-2 text-sm disabled:opacity-40 dark:border-slate-700">{t('common.next', '下一页')}</button>
          </div>}
        </div> : <div className="rounded-lg border border-dashed border-slate-300 p-10 text-center dark:border-slate-700"><Play className="mx-auto mb-3 text-slate-400" /><p className="text-slate-500">{t('historyPage.empty')}</p><Link to="/bookshelf" className="mt-4 inline-flex rounded bg-primary-600 px-4 py-2 text-sm font-semibold text-white">{t('historyPage.goBookshelf')}</Link></div>}
      </div>
      <div className="shrink-0" style={{ height: currentChapter ? 'var(--safe-bottom-with-player)' : 'var(--safe-bottom-base)' }} />

      {confirmClear && <div className="fixed inset-0 z-[400] flex items-center justify-center bg-black/50 p-4" role="presentation">
        <div role="dialog" aria-modal="true" aria-labelledby="history-clear-title" className="w-full max-w-md rounded-lg bg-white p-5 shadow-xl dark:bg-slate-900">
          <h2 id="history-clear-title" className="text-lg font-semibold text-slate-900 dark:text-white">{t('historyPage.confirmTitle', '确认清除历史')}</h2>
          <p className="mt-2 text-sm text-slate-600 dark:text-slate-300">{clearAll ? t('historyPage.confirmAll', '将清除全部播放历史。') : t('historyPage.confirmSelected', '将清除选中的播放历史。')}</p>
          <label className="mt-4 flex items-center gap-2 text-sm text-slate-700 dark:text-slate-200"><input type="checkbox" checked={clearProgress} onChange={(event) => setClearProgress(event.target.checked)} />{t('historyPage.clearProgress', '同步清除进度')}</label>
          <div className="mt-6 flex justify-end gap-2"><button onClick={() => setConfirmClear(false)} disabled={deleting} className="rounded border border-slate-300 px-3 py-2 text-sm dark:border-slate-700">{t('common.cancel')}</button><button onClick={() => void deleteHistory()} disabled={deleting} className="rounded bg-red-600 px-3 py-2 text-sm font-semibold text-white disabled:opacity-50">{deleting ? t('historyPage.deleting') : t('common.confirm', '确认')}</button></div>
        </div>
      </div>}
    </div>
  );
};

const HistoryBookSection = ({ book, chapters, chapterTotal, chapterPage, loadingChapters, coverShape, expanded, selectionMode, bookSelected, selectedProgress, onExpand, onToggleBook, onToggleProgress, onPage }: {
  book: ActivityBook; chapters: Progress[]; chapterTotal: number; chapterPage: number; loadingChapters: boolean; coverShape: CoverShape; expanded: boolean; selectionMode: boolean; bookSelected: boolean; selectedProgress: Set<string>; onExpand: () => void; onToggleBook: () => void; onToggleProgress: (item: Progress) => void; onPage: (page: number) => void;
}) => {
  const { t, i18n } = useTranslation();
  return <ActivityBookCard
    book={{ ...book, book_title: book.book_title || t('historyPage.unknownBook') }}
    coverShape={coverShape}
    countLabel={t('historyPage.chapterUnit', { count: book.chapter_count })}
    preview={book.latest_chapter_title}
    meta={<><Clock size={13} className="shrink-0" />{t('historyPage.lastListened', { time: formatLastListenedTime(book.updated_at, t, i18n.language) })}</>}
    percent={chapterProgressPercent(book.latest_position, book.latest_duration)}
    expanded={expanded}
    onExpand={onExpand}
    selection={selectionMode && <button onClick={onToggleBook} className="shrink-0 text-primary-600" aria-label={t('historyPage.chooseBook', { title: book.book_title })}>{bookSelected ? <CheckSquare size={22} /> : <Square size={22} />}</button>}
  >
      {chapters.map((chapter) => <HistoryChapterRow key={progressKey(chapter)} progress={chapter} selectionMode={selectionMode} selected={selectedProgress.has(progressKey(chapter))} onToggle={() => onToggleProgress(chapter)} />)}
      {loadingChapters && <div className="p-4"><LoadingSpinner /></div>}
      {chapterTotal > CHAPTER_PAGE_SIZE && <div className="flex items-center justify-center gap-4 border-t border-slate-100 p-4 text-sm dark:border-slate-800">
        <button disabled={loadingChapters || chapterPage <= 1} onClick={() => onPage(chapterPage - 1)} className="disabled:opacity-40">{t('common.previous', '上一页')}</button>
        <span>{chapterPage} / {Math.ceil(chapterTotal / CHAPTER_PAGE_SIZE)}</span>
        <button disabled={loadingChapters || chapterPage * CHAPTER_PAGE_SIZE >= chapterTotal} onClick={() => onPage(chapterPage + 1)} className="disabled:opacity-40">{t('common.next', '下一页')}</button>
      </div>}
      {!loadingChapters && chapters.length === 0 && <p className="p-4 text-sm text-slate-500">{t('historyPage.empty')}</p>}
  </ActivityBookCard>;
};

const HistoryChapterRow = ({ progress, selectionMode, selected, onToggle }: { progress: Progress; selectionMode: boolean; selected: boolean; onToggle: () => void }) => {
  const { t, i18n } = useTranslation();
  const duration = progress.chapter_duration || progress.duration || 0;
  const percent = chapterProgressPercent(progress.position, duration);
  const content = <>
    {selectionMode && (selected ? <CheckSquare size={20} className="shrink-0 text-primary-600" /> : <Square size={20} className="shrink-0 text-primary-600" />)}
    <div className="min-w-0 flex-1">
      <p className="truncate text-sm font-bold text-slate-800 dark:text-slate-100">{progress.chapter_title || t('historyPage.unknownChapter')}</p>
      <p className="mt-1.5 flex items-center gap-1.5 text-xs text-slate-400"><Clock size={13} className="shrink-0" />{formatLastListenedTime(progress.updated_at, t, i18n.language)}</p>
      <ActivityProgress percent={percent} />
    </div>
  </>;
  const rowClass = "flex w-full items-center gap-3 border-t border-slate-100 px-4 py-4 text-left first:border-t-0 md:px-5 dark:border-slate-800";
  if (selectionMode) return <button onClick={onToggle} className={rowClass}>{content}</button>;
  return <Link to={`/book/${progress.book_id}?chapter_id=${encodeURIComponent(progress.chapter_id)}`} className={`${rowClass} transition-colors hover:bg-white dark:hover:bg-slate-900`}>{content}</Link>;
};

const formatLastListenedTime = (value: string | undefined, t: TFunction, language: string) => {
  if (!value) return t('historyPage.unknownTime');
  const date = parseBackendDate(value);
  if (!date) return t('historyPage.unknownTime');
  const dayDiff = getApplicationDayDifference(date);
  const locale = language?.startsWith('en') ? 'en-US' : 'zh-CN';
  const time = date.toLocaleTimeString(locale, { hour: '2-digit', minute: '2-digit', timeZone: getApplicationTimeZone() });
  if (dayDiff === 0) return t('historyPage.today', { time });
  if (dayDiff === 1) return t('historyPage.yesterday', { time });
  if (dayDiff > 1 && dayDiff < 7) return t('historyPage.daysAgo', { count: dayDiff, time });
  return date.toLocaleString(locale, { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', timeZone: getApplicationTimeZone() });
};

export default HistoryPage;
