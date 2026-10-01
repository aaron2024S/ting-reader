import React from 'react';
import type { Book } from '../../core/types';
import { Check, Play } from 'lucide-react';
import { Link } from 'react-router';

import { getCoverUrl } from '../../core/utils/image';
import { toSolidColor, isLight, isTooLight } from '../../core/utils/color';
import ExpandableTitle from '../widgets/ExpandableTitle';
import { useTranslation } from 'react-i18next';
import { useUiPreferencesStore } from '../../core/stores/uiPreferencesStore';
import { coverProgressPercent } from '../../core/utils/playbackPreferences';

interface BookCardProps {
  book: Book;
  onClick?: (e: React.MouseEvent) => void;
  disableLink?: boolean;
  coverShape?: 'rect' | 'square';
}

const BookCard: React.FC<BookCardProps> = ({ book, onClick, disableLink, coverShape = 'square' }) => {
  const { t } = useTranslation();
  const showProgress = useUiPreferencesStore((state) => state.bookshelfProgressEnabled);
  const progress = coverProgressPercent(book.progress_percent);
  const effectiveThemeColor = book.theme_color && !isTooLight(book.theme_color) ? book.theme_color : undefined;

  const content = (
    <>
      <div className={`relative ${coverShape === 'square' ? 'aspect-square' : 'aspect-[3/4]'} overflow-hidden rounded-md shadow-md bg-white dark:bg-slate-800`}>
        <img
          src={getCoverUrl(book.cover_url, book.library_id, book.id)}
          alt={book.title}
          loading="lazy"
          referrerPolicy="no-referrer"
          className="w-full h-full object-cover transition-transform duration-300 group-hover:scale-105"
          onError={(e) => {
            (e.target as HTMLImageElement).src = 'https://placehold.co/300x400?text=No+Cover';
          }}
        />
        {showProgress && (
          <div className="pointer-events-none absolute inset-x-0 bottom-0 z-10 pt-8" style={{ background: 'linear-gradient(to top, rgba(15,23,42,0.64), rgba(15,23,42,0.18) 55%, transparent)' }}>
            <span className="flex items-center gap-1 px-2.5 pb-2 text-[10px] font-medium leading-none tabular-nums tracking-wide text-white/95 [text-shadow:0_1px_3px_rgba(0,0,0,0.35)]">
              {progress === 100 && <Check size={11} strokeWidth={2} />}
              {progress === 100 ? t('bookshelf.read') : progress === 0 ? t('bookshelf.unread') : `${progress}%`}
            </span>
            {progress > 0 && <div className="h-0.5 bg-white/15"><div className={`h-full ${progress === 100 ? 'bg-emerald-400/85' : 'bg-primary-400/90'}`} style={{ width: `${progress}%` }} /></div>}
          </div>
        )}
        <div className="absolute inset-0 bg-black/40 opacity-0 group-hover:opacity-100 transition-opacity flex items-center justify-center">
          <div 
            className={`w-10 h-10 rounded-full text-white flex items-center justify-center shadow-lg transform translate-y-4 group-hover:translate-y-0 transition-transform ${!effectiveThemeColor ? 'bg-primary-600' : ''}`}
            style={effectiveThemeColor ? { 
              backgroundColor: toSolidColor(effectiveThemeColor),
              color: isLight(effectiveThemeColor) ? '#475569' : '#ffffff'
            } : {}}
          >
            <Play size={20} fill="currentColor" />
          </div>
        </div>
      </div>
      <div className="mt-2 min-w-0">
        <ExpandableTitle 
          title={book.title} 
          className="font-bold text-sm text-slate-900 dark:text-white group-hover:text-primary-600 transition-colors leading-tight" 
          maxLines={1}
        />
        <div className="mt-1 flex flex-col gap-0.5">
          <div className="flex items-center gap-1.5 text-xs text-slate-500 dark:text-slate-400">
            <span className="line-clamp-1">{book.author || t('shared.unknownAuthor')}</span>
          </div>
        </div>
      </div>
    </>
  );

  if (disableLink) {
    return (
      <div 
        className="group flex flex-col relative cursor-pointer"
        onClick={onClick}
      >
        {content}
      </div>
    );
  }

  return (
    <Link 
      to={`/book/${book.id}`}
      className="group flex flex-col relative"
      onClick={onClick}
    >
      {content}
    </Link>
  );
};

export default BookCard;
