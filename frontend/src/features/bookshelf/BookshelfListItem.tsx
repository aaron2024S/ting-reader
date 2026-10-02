import { Check, ChevronRight, Layers } from 'lucide-react';
import { Link } from 'react-router';
import { useTranslation } from 'react-i18next';
import type { Book, Series } from '../../core/types';
import { useUiPreferencesStore } from '../../core/stores/uiPreferencesStore';
import { getCoverUrl } from '../../core/utils/image';
import { coverProgressPercent } from '../../core/utils/playbackPreferences';
import { getRuntimeAssetUrl } from '../../core/utils/runtimeUrl';

interface Props {
  item: Book | Series;
  kind: 'book' | 'series';
  coverShape: 'rect' | 'square';
  iconSize: 'small' | 'medium' | 'large';
  selected: boolean;
  onSelect?: () => void;
}

export default function BookshelfListItem({ item, kind, coverShape, iconSize, selected, onSelect }: Props) {
  const { t } = useTranslation();
  const showProgress = useUiPreferencesStore(state => state.bookshelfProgressEnabled);
  const isSeries = kind === 'series';
  const progress = coverProgressPercent((item as Book).progress_percent);
  const coverWidth = iconSize === 'small' ? 'w-12' : iconSize === 'large' ? 'w-20' : 'w-16';
  const className = `group flex w-full items-center gap-3 px-3 py-3 text-left transition-colors sm:gap-4 sm:px-4 ${
    selected && onSelect
      ? 'bg-primary-50/70 dark:bg-primary-900/20'
      : 'hover:bg-slate-50 dark:hover:bg-slate-800/60'
  }`;
  const content = (
    <>
      <div className={`${coverWidth} ${coverShape === 'square' ? 'aspect-square' : 'aspect-[3/4]'} shrink-0 overflow-hidden rounded-lg bg-slate-100 shadow-sm dark:bg-slate-800`}>
        <img
          src={getCoverUrl(item.cover_url, item.library_id, isSeries ? undefined : item.id)}
          alt=""
          loading="lazy"
          referrerPolicy="no-referrer"
          className="h-full w-full object-cover"
          onError={({ currentTarget }) => {
            const fallback = getRuntimeAssetUrl('/placeholder-cover.png');
            if (currentTarget.getAttribute('src') !== fallback) currentTarget.src = fallback;
          }}
        />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-3 sm:gap-4">
          <div className="min-w-0 flex-1">
            {isSeries && (
              <span className="mb-1 flex items-center gap-1 text-[11px] font-medium text-primary-600 dark:text-primary-400">
                <Layers size={12} />{t('shared.series')}
              </span>
            )}
            <h3 title={item.title} className="truncate text-sm font-semibold leading-snug text-slate-900 transition-colors group-hover:text-primary-600 dark:text-white sm:text-base">
              {item.title}
            </h3>
            <p className="mt-1 truncate text-xs text-slate-500 dark:text-slate-400">
              {[item.author || t('shared.unknownAuthor'), item.narrator].filter(Boolean).join(' · ')}
            </p>
          </div>
          {isSeries ? (
            <span className="shrink-0 text-xs text-slate-500 dark:text-slate-400">
              {t('shared.seriesBookCount', { count: (item as Series).books?.length || 0 })}
            </span>
          ) : showProgress && (
            <span className={`inline-flex shrink-0 items-center gap-1 text-xs tabular-nums ${progress === 100 ? 'text-emerald-600 dark:text-emerald-400' : 'text-slate-500 dark:text-slate-400'}`}>
              {progress === 100 && <Check size={12} />}
              {progress === 100 ? t('bookshelf.read') : progress === 0 ? t('bookshelf.unread') : `${progress}%`}
            </span>
          )}
          {onSelect ? (
            <span className={`flex h-5 w-5 shrink-0 items-center justify-center rounded-md border ${selected ? 'border-primary-600 bg-primary-600 text-white' : 'border-slate-300 dark:border-slate-600'}`}>
              {selected && <Check size={14} />}
            </span>
          ) : <ChevronRight size={16} className="hidden shrink-0 text-slate-300 dark:text-slate-600 sm:block" />}
        </div>
        {!isSeries && showProgress && progress > 0 && (
          <div
            role="progressbar"
            aria-label={item.title}
            aria-valuenow={progress}
            aria-valuemin={0}
            aria-valuemax={100}
            className="mt-2 h-1 w-full overflow-hidden rounded-full bg-slate-100 dark:bg-slate-800"
          >
            <div className={`h-full rounded-full ${progress === 100 ? 'bg-emerald-500' : 'bg-primary-500'}`} style={{ width: `${progress}%` }} />
          </div>
        )}
      </div>
    </>
  );

  return onSelect ? (
    <button type="button" className={className} aria-pressed={selected} onClick={onSelect}>{content}</button>
  ) : (
    <Link to={`/${kind}/${item.id}`} className={className}>{content}</Link>
  );
}
