import React, { useCallback, useState, useEffect, useRef } from 'react';
import apiClient from '../../core/api/client';
import { getBookshelfGridClass, useBookshelfDisplaySettings } from '../../core/hooks/useBookshelfCoverShape';
import type { Book, Library, Series } from '../../core/types';
import BookCard from '../../shared/cards/BookCard';
import { Search as SearchIcon, Loader2, BookX, ChevronLeft, ChevronRight, SlidersHorizontal, ChevronUp, ChevronDown } from 'lucide-react';
import { usePlayerStore } from '../../core/stores/playerStore';
import BackButton from '../../shared/widgets/BackButton';
import { useTranslation } from 'react-i18next';

type FilterOption = string | { id: string; name: string };

interface FilterRowProps {
  label: string;
  allLabel: string;
  items: FilterOption[];
  selected: string;
  onSelect: (value: string) => void;
  scrollRef: React.RefObject<HTMLDivElement | null>;
}

const scrollFilterRow = (
  ref: React.RefObject<HTMLDivElement | null>,
  direction: 'left' | 'right',
) => {
  ref.current?.scrollBy({
    left: direction === 'left' ? -300 : 300,
    behavior: 'smooth',
  });
};

const FilterRow: React.FC<FilterRowProps> = ({
  label,
  allLabel,
  items,
  selected,
  onSelect,
  scrollRef,
}) => {
  const [canScrollLeft, setCanScrollLeft] = useState(false);
  const [canScrollRight, setCanScrollRight] = useState(false);

  const checkScroll = useCallback(() => {
    const element = scrollRef.current;
    if (!element) return;
    const { scrollLeft, scrollWidth, clientWidth } = element;
    setCanScrollLeft(scrollLeft > 0);
    setCanScrollRight(scrollLeft < scrollWidth - clientWidth - 1);
  }, [scrollRef]);

  useEffect(() => {
    const frame = window.requestAnimationFrame(checkScroll);
    window.addEventListener('resize', checkScroll);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener('resize', checkScroll);
    };
  }, [checkScroll, items]);

  if (items.length === 0) return null;

  return (
    <div className="flex flex-row items-center gap-3 sm:gap-6 py-1">
      <div className="text-sm font-bold text-slate-400 shrink-0 min-w-[60px] sm:min-w-[70px] text-left">
        {label}
      </div>
      <div className="relative flex-1 group/row min-w-0">
        {canScrollLeft && (
          <button
            onClick={() => scrollFilterRow(scrollRef, 'left')}
            className="absolute -left-3 top-1/2 -translate-y-1/2 z-10 p-1 bg-white/90 dark:bg-slate-800/90 rounded-full shadow-md border border-slate-100 dark:border-slate-700 text-slate-400 hover:text-primary-500 hidden sm:flex items-center justify-center backdrop-blur-sm transition-all animate-in fade-in zoom-in duration-200"
          >
            <ChevronLeft size={16} />
          </button>
        )}

        <div
          ref={scrollRef}
          onScroll={checkScroll}
          className="flex items-center gap-2 overflow-x-auto no-scrollbar scroll-smooth pr-4 sm:pr-0 -mr-4 sm:mr-0 mask-linear-fade"
        >
          <button
            onClick={() => onSelect('')}
            className={`shrink-0 px-3 py-1.5 rounded-lg text-sm transition-all whitespace-nowrap ${
              selected === ''
                ? 'bg-primary-500 text-white font-medium shadow-md shadow-primary-500/20'
                : 'text-slate-600 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800'
            }`}
          >
            {allLabel}
          </button>
          {items.map((item) => {
            const value = typeof item === 'string' ? item : item.id;
            const itemLabel = typeof item === 'string' ? item : item.name;
            return (
              <button
                key={value}
                onClick={() => onSelect(selected === value ? '' : value)}
                className={`shrink-0 px-3 py-1.5 rounded-lg text-sm transition-all whitespace-nowrap ${
                  selected === value
                    ? 'bg-primary-500 text-white font-medium shadow-md shadow-primary-500/20'
                    : 'text-slate-600 dark:text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800'
                }`}
              >
                {itemLabel}
              </button>
            );
          })}
        </div>

        {canScrollRight && (
          <button
            onClick={() => scrollFilterRow(scrollRef, 'right')}
            className="absolute -right-3 top-1/2 -translate-y-1/2 z-10 p-1 bg-white/90 dark:bg-slate-800/90 rounded-full shadow-md border border-slate-100 dark:border-slate-700 text-slate-400 hover:text-primary-500 hidden sm:flex items-center justify-center backdrop-blur-sm transition-all animate-in fade-in zoom-in duration-200"
          >
            <ChevronRight size={16} />
          </button>
        )}
      </div>
    </div>
  );
};

const SearchPage: React.FC = () => {
  const { t } = useTranslation();
  const [query, setQuery] = useState('');
  
  // Filter states
  const [selectedLibraryId, setSelectedLibraryId] = useState<string>('');
  const [selectedSeries, setSelectedSeries] = useState<string>('');
  const [selectedTag, setSelectedTag] = useState<string>('');
  const [selectedGenre, setSelectedGenre] = useState<string>('');
  const [selectedYear, setSelectedYear] = useState<string>('');
  const [selectedAuthor, setSelectedAuthor] = useState<string>('');
  const [selectedNarrator, setSelectedNarrator] = useState<string>('');
  const [showFilters, setShowFilters] = useState(false);
  
  // Data options
  const [libraries, setLibraries] = useState<Library[]>([]);
  const [allSeries, setAllSeries] = useState<Series[]>([]);
  const [allTags, setAllTags] = useState<string[]>([]);
  const [allGenres, setAllGenres] = useState<string[]>([]);
  const [allYears, setAllYears] = useState<string[]>([]);
  const [allAuthors, setAllAuthors] = useState<string[]>([]);
  const [allNarrators, setAllNarrators] = useState<string[]>([]);
  
  const [results, setResults] = useState<Book[]>([]);
  const [loading, setLoading] = useState(false);
  const [debouncedQuery, setDebouncedQuery] = useState('');
  const currentChapter = usePlayerStore((state) => state.currentChapter);
  
  // Cover shape and icon size settings from bookshelf
  const { coverShape, iconSize } = useBookshelfDisplaySettings();

  // Scroll refs for filter rows
  const librariesScrollRef = useRef<HTMLDivElement>(null);
  const seriesScrollRef = useRef<HTMLDivElement>(null);
  const tagsScrollRef = useRef<HTMLDivElement>(null);
  const genresScrollRef = useRef<HTMLDivElement>(null);
  const yearsScrollRef = useRef<HTMLDivElement>(null);
  const authorsScrollRef = useRef<HTMLDivElement>(null);
  const narratorsScrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const timer = setTimeout(() => {
      setDebouncedQuery(query);
    }, 500);
    return () => clearTimeout(timer);
  }, [query]);

  // Load bookshelf settings on mount


  // Fetch all metadata options on mount
  useEffect(() => {
    const fetchMetadata = async () => {
      try {
        const [tagsRes, booksRes, libsRes, seriesRes] = await Promise.all([
          apiClient.get('/api/tags'),
          apiClient.get('/api/books'),
          apiClient.get('/api/libraries'),
          apiClient.get('/api/v1/series')
        ]);
        
        setAllTags(tagsRes.data);
        setLibraries(libsRes.data);
        setAllSeries(seriesRes.data);
        
        // Extract unique authors, narrators, genres, and years
        const books = booksRes.data as Book[];
        const authors = new Set<string>();
        const narrators = new Set<string>();
        const genres = new Set<string>();
        const years = new Set<string>();
        
        books.forEach(book => {
          if (book.author) authors.add(book.author);
          if (book.narrator) narrators.add(book.narrator);
          if (book.genre) {
            book.genre.split(',').forEach(g => {
              const trimmed = g.trim();
              if (trimmed) genres.add(trimmed);
            });
          }
          if (book.year) years.add(book.year.toString());
        });
        
        setAllAuthors(Array.from(authors).sort());
        setAllNarrators(Array.from(narrators).sort());
        setAllGenres(Array.from(genres).sort());
        setAllYears(Array.from(years).sort((a, b) => parseInt(b) - parseInt(a))); // 降序排列
        
      } catch (err) {
        console.error('获取元数据失败', err);
      }
    };
    fetchMetadata();
  }, []);

  useEffect(() => {
    const searchBooks = async () => {
      // If no filters are active, clear results
      if (!debouncedQuery.trim() && !selectedTag && !selectedGenre && !selectedYear && !selectedAuthor && !selectedNarrator && !selectedLibraryId && !selectedSeries) {
        setResults([]);
        return;
      }

      setLoading(true);
      try {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const params: Record<string, any> = {};
        if (debouncedQuery.trim()) params.search = debouncedQuery;
        if (selectedTag) params.tag = selectedTag;
        if (selectedLibraryId) params.library_id = selectedLibraryId;
        
        const response = await apiClient.get('/api/books', { params });
        let filtered = response.data as Book[];

        // Apply additional client-side filters
        if (selectedAuthor) {
          filtered = filtered.filter(b => b.author === selectedAuthor);
        }
        
        if (selectedNarrator) {
          filtered = filtered.filter(b => b.narrator === selectedNarrator);
        }

        if (selectedGenre) {
          filtered = filtered.filter(b => b.genre && b.genre.split(',').map(g => g.trim()).includes(selectedGenre));
        }

        if (selectedYear) {
          filtered = filtered.filter(b => b.year && b.year.toString() === selectedYear);
        }

        if (selectedSeries) {
           const series = allSeries.find(s => s.id === selectedSeries);
           if (series?.books) {
               const seriesBookIds = new Set(series.books.map(b => b.id));
               filtered = filtered.filter(b => seriesBookIds.has(b.id));
           } else {
               filtered = [];
           }
        }
        
        setResults(filtered);
      } catch (err) {
        console.error('搜索失败', err);
      } finally {
        setLoading(false);
      }
    };

    searchBooks();
  }, [debouncedQuery, selectedTag, selectedGenre, selectedYear, selectedAuthor, selectedNarrator, selectedLibraryId, selectedSeries, allSeries]);

  const hasActiveFilters = selectedLibraryId || selectedTag || selectedGenre || selectedYear || selectedAuthor || selectedNarrator || selectedSeries;

  return (
    <div className="w-full max-w-screen-2xl mx-auto p-4 sm:p-6 md:p-8 lg:p-10 space-y-6">
      <BackButton fallback="/bookshelf" />

      <div className="text-center space-y-4">
        <h1 className="text-3xl md:text-4xl font-bold dark:text-white">{t('searchPage.title')}</h1>
        <p className="text-sm md:text-base text-slate-500">{t('searchPage.subtitle')}</p>
        
        {/* Main Search Input */}
        <div className="w-full max-w-md sm:max-w-xl md:max-w-3xl lg:max-w-5xl xl:max-w-6xl 2xl:max-w-7xl mx-auto relative mt-8">
          <SearchIcon className="absolute left-4 top-1/2 -translate-y-1/2 text-slate-400" size={20} />
          <input 
            type="text"
            placeholder={t('searchPage.placeholder')}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="w-full pl-12 pr-12 py-3 md:py-4 bg-white dark:bg-slate-900 border border-slate-200 dark:border-slate-800 rounded-2xl shadow-lg focus:ring-2 focus:ring-primary-500 outline-none text-base md:text-lg transition-all dark:text-white"
            autoFocus
          />
          {loading ? (
            <div className="absolute right-4 top-1/2 -translate-y-1/2">
              <Loader2 className="animate-spin text-primary-600" size={24} />
            </div>
          ) : (
            query && (
              <button 
                onClick={() => setQuery('')}
                className="absolute right-4 top-1/2 -translate-y-1/2 text-slate-400 hover:text-slate-600 dark:hover:text-slate-200"
              >
                <BookX size={20} />
              </button>
            )
          )}
        </div>

        {/* Filters Toggle Button */}
        <div className="flex justify-center mt-4">
          <button
            onClick={() => setShowFilters(!showFilters)}
            className={`flex items-center gap-2 px-4 py-2 rounded-full text-sm font-medium transition-colors ${
              showFilters || hasActiveFilters
                ? 'bg-primary-50 dark:bg-primary-900/20 text-primary-600'
                : 'text-slate-500 hover:bg-slate-100 dark:hover:bg-slate-800'
            }`}
          >
            <SlidersHorizontal size={16} />
            {t(showFilters ? 'searchPage.hideFilters' : 'searchPage.showFilters')}
            {hasActiveFilters && !showFilters && <div className="w-2 h-2 rounded-full bg-primary-500 ml-1" />}
            {showFilters ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
          </button>
        </div>

        {/* Advanced Filters Panel */}
        {showFilters && (
          <div className="w-full max-w-7xl mx-auto mt-6 p-4 sm:p-6 bg-white dark:bg-slate-900 rounded-2xl border border-slate-100 dark:border-slate-800 shadow-sm animate-in slide-in-from-top-4 duration-300">
            <div className="space-y-4 divide-y divide-slate-50 dark:divide-slate-800/50">
              <FilterRow
                label={t('searchPage.library')}
                allLabel={t('searchPage.all')}
                items={libraries}
                selected={selectedLibraryId}
                onSelect={setSelectedLibraryId}
                scrollRef={librariesScrollRef}
              />
              <FilterRow
                label={t('searchPage.series')}
                allLabel={t('searchPage.all')}
                items={allSeries.map(s => ({ id: s.id, name: s.title }))}
                selected={selectedSeries}
                onSelect={setSelectedSeries}
                scrollRef={seriesScrollRef}
              />
              <FilterRow
                label={t('searchPage.tags')}
                allLabel={t('searchPage.all')}
                items={allTags}
                selected={selectedTag}
                onSelect={setSelectedTag}
                scrollRef={tagsScrollRef}
              />
              <FilterRow
                label={t('searchPage.genre')}
                allLabel={t('searchPage.all')}
                items={allGenres}
                selected={selectedGenre}
                onSelect={setSelectedGenre}
                scrollRef={genresScrollRef}
              />
              <FilterRow
                label={t('searchPage.year')}
                allLabel={t('searchPage.all')}
                items={allYears}
                selected={selectedYear}
                onSelect={setSelectedYear}
                scrollRef={yearsScrollRef}
              />
              <FilterRow
                label={t('searchPage.author')}
                allLabel={t('searchPage.all')}
                items={allAuthors}
                selected={selectedAuthor}
                onSelect={setSelectedAuthor}
                scrollRef={authorsScrollRef}
              />
              <FilterRow
                label={t('searchPage.narrator')}
                allLabel={t('searchPage.all')}
                items={allNarrators}
                selected={selectedNarrator}
                onSelect={setSelectedNarrator} 
                scrollRef={narratorsScrollRef}
              />
            </div>
          </div>
        )}
      </div>

      {results.length > 0 ? (
        <div className={`grid ${getBookshelfGridClass(iconSize)} pt-4`}>
          {results.map((book) => (
            <BookCard key={book.id} book={book} coverShape={coverShape} />
          ))}
        </div>
      ) : (debouncedQuery || hasActiveFilters) && !loading ? (
        <div className="py-20 text-center">
          <div className="inline-flex items-center justify-center w-20 h-20 rounded-full bg-slate-100 dark:bg-slate-900 text-slate-400 mb-4">
            <BookX size={40} />
          </div>
          <h3 className="text-xl font-medium dark:text-white">{t('searchPage.noResults')}</h3>
          <p className="text-slate-500 mt-2">{t('searchPage.noResultsHint')}</p>
        </div>
      ) : !debouncedQuery && !hasActiveFilters && (
        <div className="py-20 text-center text-slate-400">
          <p>{t('searchPage.emptyHint')}</p>
        </div>
      )}

      {/* Dynamic Safe Bottom Spacer */}
      <div 
        className="shrink-0 transition-all duration-300" 
        style={{ height: currentChapter ? 'var(--safe-bottom-with-player)' : 'var(--safe-bottom-base)' }} 
      />
    </div>
  );
};

export default SearchPage;
