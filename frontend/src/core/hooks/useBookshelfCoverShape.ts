import { useEffect, useState } from 'react';
import apiClient from '../api/client';

export type CoverShape = 'rect' | 'square';
export type BookshelfIconSize = 'small' | 'medium' | 'large';

export const getBookshelfGridClass = (size: BookshelfIconSize) => {
  switch (size) {
    case 'small':
      return 'grid-cols-4 sm:grid-cols-6 md:grid-cols-7 lg:grid-cols-8 xl:grid-cols-8 2xl:grid-cols-10 gap-x-3 gap-y-7';
    case 'large':
      return 'grid-cols-2 sm:grid-cols-4 md:grid-cols-4 lg:grid-cols-4 xl:grid-cols-4 2xl:grid-cols-6 gap-x-6 gap-y-10';
    default:
      return 'grid-cols-3 sm:grid-cols-5 md:grid-cols-5 lg:grid-cols-6 xl:grid-cols-6 2xl:grid-cols-7 gap-x-5 gap-y-9';
  }
};

const COVER_SHAPE_EVENT = 'ting-reader-bookshelf-cover-shape-change';
let cachedCoverShape: CoverShape = 'square';

export const normalizeCoverShape = (value: unknown): CoverShape => (
  value === 'rect' ? 'rect' : 'square'
);

export const getCoverAspectClass = (coverShape: CoverShape) => (
  coverShape === 'square' ? 'aspect-square' : 'aspect-[3/4]'
);

export const publishBookshelfCoverShape = (coverShape: CoverShape) => {
  cachedCoverShape = coverShape;
  window.dispatchEvent(new CustomEvent(COVER_SHAPE_EVENT, { detail: coverShape }));
};

export const useBookshelfDisplaySettings = (enabled = true) => {
  const [coverShape, setCoverShape] = useState<CoverShape>(cachedCoverShape);
  const [iconSize, setIconSize] = useState<BookshelfIconSize>('medium');

  useEffect(() => {
    const request = new AbortController();
    if (!enabled) return;

    apiClient.get('/api/settings', { signal: request.signal })
      .then(res => {
        const nextCoverShape = normalizeCoverShape(res.data.settings_json?.bookshelf_cover_shape);
        if (request.signal.aborted) return;
        cachedCoverShape = nextCoverShape;
        setCoverShape(nextCoverShape);
        const size = res.data.settings_json?.bookshelf_icon_size;
        setIconSize(size === 'small' || size === 'large' ? size : 'medium');
      })
      .catch(err => {
        if (!request.signal.aborted) console.error('加载书架展示设置失败', err);
      });

    const handleCoverShapeChange = (event: Event) => {
      const nextCoverShape = normalizeCoverShape((event as CustomEvent).detail);
      cachedCoverShape = nextCoverShape;
      setCoverShape(nextCoverShape);
    };

    window.addEventListener(COVER_SHAPE_EVENT, handleCoverShapeChange);
    return () => {
      request.abort();
      window.removeEventListener(COVER_SHAPE_EVENT, handleCoverShapeChange);
    };
  }, [enabled]);

  return { coverShape, iconSize };
};

export const useBookshelfCoverShape = (enabled = true) =>
  useBookshelfDisplaySettings(enabled).coverShape;
