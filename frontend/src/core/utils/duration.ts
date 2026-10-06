export type MinuteMetric = {
  value: number;
  unit: 'minutes' | 'hours';
};

export const formatMinuteMetric = (minutes: number): MinuteMetric => {
  const safeMinutes = Number.isFinite(minutes)
    ? Math.max(0, Math.round(minutes))
    : 0;
  if (safeMinutes > 60) {
    return {
      value: Math.round(safeMinutes / 60),
      unit: 'hours',
    };
  }
  return { value: safeMinutes, unit: 'minutes' };
};

export const resolvePlaybackDuration = (
  chapterDuration: number | undefined,
  browserDuration: number,
  isTranscoded: boolean,
): number => {
  if (chapterDuration !== undefined && Number.isFinite(chapterDuration) && chapterDuration > 0) {
    return chapterDuration;
  }
  // A chunked transcode's finite duration can still be just a partial estimate.
  if (isTranscoded) return 0;
  return Number.isFinite(browserDuration) && browserDuration > 0 ? browserDuration : 0;
};
