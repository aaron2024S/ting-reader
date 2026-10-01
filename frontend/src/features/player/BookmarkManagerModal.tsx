import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Bookmark, BookmarkPlus, Pencil, Trash2, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import apiClient from '../../core/api/client';
import { formatBookmarkPosition as formatPosition, type Bookmark as BookmarkedPosition, type ReadingPage as Page } from '../../core/api/reading';
import { ActivityProgress } from '../mine/ActivityBookCard';
import { chapterProgressPercent } from '../../core/utils/playbackPreferences';


interface Props {
  bookId: string;
  bookTitle: string;
  chapterId: string;
  position: number;
  onClose: () => void;
  onJump: (chapterId: string, position: number) => void;
}

const BookmarkManagerModal: React.FC<Props> = ({ bookId, bookTitle, chapterId, position, onClose, onJump }) => {
  const { t } = useTranslation();
  const [items, setItems] = useState<BookmarkedPosition[]>([]);
  const [page, setPage] = useState(1);
  const [total, setTotal] = useState(0);
  const [note, setNote] = useState('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const pendingLoad = useRef<AbortController | null>(null);

  const load = useCallback(async (targetPage = 1) => {
    pendingLoad.current?.abort();
    const controller = new AbortController();
    pendingLoad.current = controller;
    setLoading(true);
    try {
      const response = await apiClient.get<Page<BookmarkedPosition>>(`/api/bookmarks/books/${bookId}`, { params: { page: targetPage, page_size: 40 }, signal: controller.signal });
      if (controller.signal.aborted) return;
      setItems(response.data.items.filter((item) => item.book_id === bookId));
      setTotal(response.data.total);
      setPage(targetPage);
    } catch (error) {
      if (controller.signal.aborted) return;
      console.error('获取书签失败', error);
      alert(t('bookmarks.loadFailed', '加载书签失败'));
    } finally {
      if (!controller.signal.aborted) setLoading(false);
    }
  }, [bookId, t]);
  useEffect(() => {
    const timer = window.setTimeout(() => void load(), 0);
    return () => {
      window.clearTimeout(timer);
      pendingLoad.current?.abort();
    };
  }, [load]);

  const add = async () => {
    if (saving) return;
    setSaving(true);
    try {
      await apiClient.post('/api/bookmarks', { book_id: bookId, chapter_id: chapterId, position: Math.max(0, position), note });
      setNote('');
      await load();
    } catch (error) {
      console.error('添加书签失败', error);
      alert(t('bookmarks.saveFailed', '添加书签失败'));
    } finally {
      setSaving(false);
    }
  };

  const updateNote = async (item: BookmarkedPosition) => {
    const nextNote = window.prompt(t('bookmarks.editNote', '编辑书签备注'), item.note);
    if (nextNote === null) return;
    try {
      await apiClient.put(`/api/bookmarks/${item.id}`, { note: nextNote });
      await load(1);
    } catch (error) {
      console.error('更新书签失败', error);
      alert(t('common.saveFailed'));
    }
  };

  const remove = async (item: BookmarkedPosition) => {
    try {
      await apiClient.delete(`/api/bookmarks/${item.id}`);
      await load(items.length === 1 && page > 1 ? page - 1 : page);
    } catch (error) {
      console.error('删除书签失败', error);
      alert(t('common.deleteFailed', '删除失败'));
    }
  };

  return <div className="fixed inset-0 z-[350] flex items-center justify-center bg-black/55 p-4" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
    <section role="dialog" aria-modal="true" aria-labelledby="bookmark-dialog-title" className="flex max-h-[min(80vh,720px)] w-full max-w-lg flex-col rounded-lg bg-white shadow-2xl dark:bg-slate-900">
      <header className="flex items-start justify-between gap-3 border-b border-slate-200 p-4 dark:border-slate-800"><div className="min-w-0"><h2 id="bookmark-dialog-title" className="flex items-center gap-2 text-lg font-semibold text-slate-900 dark:text-white"><Bookmark size={19} />{t('bookmarks.currentBook', '本书书签')}</h2><p className="mt-1 truncate text-sm text-slate-500">{bookTitle}</p></div><button onClick={onClose} className="rounded p-2 hover:bg-slate-100 dark:hover:bg-slate-800" aria-label={t('common.close', '关闭')}><X size={18} /></button></header>
      <div className="border-b border-slate-200 p-4 dark:border-slate-800"><label className="mb-2 block text-sm font-medium text-slate-700 dark:text-slate-200">{t('bookmarks.note', '备注')}</label><textarea value={note} onChange={(event) => setNote(event.target.value)} maxLength={2000} rows={2} className="w-full resize-y rounded border border-slate-300 bg-transparent px-3 py-2 text-sm outline-none focus:border-primary-500 dark:border-slate-700" placeholder={t('bookmarks.notePlaceholder', '为当前位置添加备注')} /><div className="mt-2 flex items-center justify-between"><span className="text-xs tabular-nums text-slate-500">{formatPosition(position)}</span><button onClick={() => void add()} disabled={saving} className="inline-flex items-center gap-2 rounded bg-primary-600 px-3 py-2 text-sm font-semibold text-white disabled:opacity-50"><BookmarkPlus size={16} />{t('bookmarks.add', '添加书签')}</button></div></div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {items.filter((item) => item.book_id === bookId).map((item) => <div key={item.id} className="flex items-center gap-3 border-b border-slate-100 px-4 py-3 last:border-0 dark:border-slate-800"><button onClick={() => onJump(item.chapter_id, item.position)} className="min-w-0 flex-1 text-left"><span className="block truncate text-sm font-medium text-slate-800 dark:text-slate-100">{item.chapter_title || t('historyPage.unknownChapter')} · {formatPosition(item.position)}</span><span className="mt-1 block truncate text-xs text-slate-500">{item.note || t('bookmarks.noNote', '无备注')}</span><ActivityProgress percent={chapterProgressPercent(item.position, item.chapter_duration)} /></button><button onClick={() => void updateNote(item)} title={t('bookmarks.editNote', '编辑备注')} className="rounded p-2 text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800"><Pencil size={16} /></button><button onClick={() => void remove(item)} title={t('common.delete')} className="rounded p-2 text-red-600 hover:bg-red-50 dark:hover:bg-red-950/30"><Trash2 size={16} /></button></div>)}
        {loading && <p className="p-5 text-center text-sm text-slate-500">{t('common.loading')}</p>}
        {!loading && items.length === 0 && <p className="p-5 text-center text-sm text-slate-500">{t('bookmarks.empty', '还没有书签')}</p>}
        {total > 40 && <div className="flex items-center justify-center gap-4 p-3 text-sm">
          <button disabled={loading || page <= 1} onClick={() => void load(page - 1)} className="disabled:opacity-40">{t('common.previous', '上一页')}</button>
          <span>{page} / {Math.ceil(total / 40)}</span>
          <button disabled={loading || page * 40 >= total} onClick={() => void load(page + 1)} className="disabled:opacity-40">{t('common.next', '下一页')}</button>
        </div>}
      </div>
    </section>
  </div>;
};

export default BookmarkManagerModal;
