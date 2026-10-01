import type { ReactNode } from 'react';
import { ChevronDown } from 'lucide-react';
import type { ActivityBook } from '../../core/api/reading';
import { getCoverUrl } from '../../core/utils/image';
import { getCoverAspectClass, type CoverShape } from '../../core/hooks/useBookshelfCoverShape';

export const ActivityProgress = ({ percent }: { percent: number }) => (
  <div className="mt-3 flex items-center gap-3">
    <div className="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-slate-100 dark:bg-slate-800">
      <div className={`h-full rounded-full ${percent === 100 ? 'bg-emerald-500' : 'bg-primary-500'}`} style={{ width: `${percent}%` }} />
    </div>
    <span className="min-w-8 text-right text-[10px] tabular-nums text-slate-400">{percent}%</span>
  </div>
);

const ActivityBookCard = ({ book, coverShape, countLabel, preview, meta, percent, expanded, onExpand, selection, children }: {
  book: ActivityBook;
  coverShape: CoverShape;
  countLabel: string;
  preview?: ReactNode;
  meta?: ReactNode;
  percent?: number;
  expanded: boolean;
  onExpand: () => void;
  selection?: ReactNode;
  children?: ReactNode;
}) => (
  <section className="overflow-hidden rounded-3xl border border-slate-100 bg-white shadow-sm dark:border-slate-800 dark:bg-slate-900">
    <div className="flex items-center gap-3 p-4 md:gap-4 md:p-5">
      {selection}
      <button onClick={onExpand} aria-expanded={expanded} className="group flex min-w-0 flex-1 items-center gap-3 text-left md:gap-4">
        <div className={`w-16 shrink-0 overflow-hidden rounded-xl shadow-sm md:w-20 ${getCoverAspectClass(coverShape)}`}>
          <img src={getCoverUrl(book.cover_url, book.library_id, book.book_id)} alt={book.book_title || ''} referrerPolicy="no-referrer" loading="lazy" className="h-full w-full object-cover transition-transform duration-300 group-hover:scale-105" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-bold text-slate-900 transition-colors group-hover:text-primary-600 md:text-base dark:text-white">{book.book_title}</span>
            <span className="shrink-0 rounded-full bg-slate-100 px-2 py-0.5 text-[10px] text-slate-500 dark:bg-slate-800 dark:text-slate-400">{countLabel}</span>
          </div>
          {preview && <div className="mt-1 truncate text-xs text-slate-500 dark:text-slate-400">{preview}</div>}
          {meta && <div className="mt-1.5 flex items-center gap-1.5 truncate text-xs text-slate-400">{meta}</div>}
          {percent !== undefined && <ActivityProgress percent={percent} />}
        </div>
        <ChevronDown size={18} className={`shrink-0 text-slate-300 transition-transform duration-200 dark:text-slate-500 ${expanded ? '' : '-rotate-90'}`} />
      </button>
    </div>
    {expanded && <div className="border-t border-slate-100 bg-slate-50/50 dark:border-slate-800 dark:bg-slate-950/30">{children}</div>}
  </section>
);

export default ActivityBookCard;
