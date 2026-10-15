import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Volume2, VolumeX, Clock } from 'lucide-react';

// ─── VolumeSliderPanel ─────────────────────────────────────────────────────
// 通用音量滑块弹出面板：纵向 range + 数值 + 静音按钮。
// 在 mini-player desktop 控件区和 expanded view 底排控件中各使用一次，
// 外观差异（阴影、margin）通过 className 透传。

interface VolumeSliderPanelProps {
  volume: number;
  isMuted: boolean;
  className?: string;
  onChangeVolume: (volume: number) => void;
  onToggleMuted: () => void;
}

export const VolumeSliderPanel: React.FC<VolumeSliderPanelProps> = ({
  volume,
  isMuted,
  className = '',
  onChangeVolume,
  onToggleMuted,
}) => {
  const { t } = useTranslation();

  return (
    <div
      className={`bg-white dark:bg-slate-800 rounded-full py-4 border border-slate-100 dark:border-slate-700 w-12 flex flex-col items-center gap-3 cursor-default ${className}`}
      onClick={(e) => e.stopPropagation()}
    >
      <span className="text-[10px] font-bold text-slate-500 min-w-[24px] text-center select-none">
        {Math.round(volume * 100)}
      </span>

      <div className="h-24 w-full flex items-center justify-center relative">
        <input
          type="range"
          min="0"
          max="1"
          step="0.01"
          value={volume}
          onChange={(e) => {
            const next = parseFloat(e.target.value);
            onChangeVolume(next);
            if (isMuted && next > 0) onToggleMuted();
          }}
          className="absolute w-24 h-1.5 bg-slate-200 dark:bg-slate-700 rounded-lg appearance-none cursor-pointer accent-primary-600 -rotate-90 hover:accent-primary-500"
        />
      </div>

      <button
        onClick={onToggleMuted}
        className={`p-2 rounded-full transition-colors ${
          isMuted
            ? 'bg-primary-100 text-primary-600 dark:bg-primary-900/30'
            : 'text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-700'
        }`}
        title={isMuted ? t('player.unmute') : t('player.mute')}
      >
        {isMuted ? <VolumeX size={16} /> : <Volume2 size={16} />}
      </button>
    </div>
  );
};

// ─── SleepTimerPopover ─────────────────────────────────────────────────────
// 睡眠定时弹出层：按钮 + 弹出面板（预设 4 档 + 自定义输入 + 取消按钮）。
// 倒计时逻辑在 useSleepTimer 里。

interface SleepTimerPopoverProps {
  sleepTimer: number | null;
  show: boolean;
  customMinutes: string;
  onToggleShow: () => void;
  onSetCustomMinutes: (value: string) => void;
  onStart: (durationSeconds: number) => void;
  onCancel: () => void;
  onClose: () => void;
  // 按集数定时（1 = 播完本集即停）
  sleepEpisodes: number | null;
  customEpisodes: string;
  onSetCustomEpisodes: (value: string) => void;
  onStartEpisodes: (episodes: number) => void;
  // 把 menuRef 直接挂在最外层 div 上，让外部 outside-click 逻辑能识别。
  menuRef: React.RefObject<HTMLDivElement | null>;
}

const SLEEP_TIMER_PRESET_MINUTES = [15, 30, 45, 60];
const SLEEP_TIMER_PRESET_EPISODES = [1, 2, 3, 4];

const formatSleepTimerRemaining = (seconds: number) =>
  `${Math.floor(seconds / 60)}:${(seconds % 60).toString().padStart(2, '0')}`;

export const SleepTimerPopover: React.FC<SleepTimerPopoverProps> = ({
  sleepTimer,
  show,
  customMinutes,
  onToggleShow,
  onSetCustomMinutes,
  onStart,
  onCancel,
  onClose,
  sleepEpisodes,
  customEpisodes,
  onSetCustomEpisodes,
  onStartEpisodes,
  menuRef,
}) => {
  const { t } = useTranslation();

  return (
    <div className="relative" ref={menuRef}>
      <button
        onClick={onToggleShow}
        className="w-full flex flex-col items-center gap-1.5 transition-all active:scale-95 group"
        title={t('player.sleepTimer')}
      >
        <div className="w-10 h-10 rounded-2xl bg-white/50 dark:bg-slate-800/60 flex items-center justify-center group-hover:bg-white/70 dark:group-hover:bg-slate-800 transition-colors">
          <Clock size={18} className={sleepTimer || sleepEpisodes ? 'text-primary-600' : ''} />
        </div>
        <span className="text-[10px] sm:text-xs font-bold leading-none whitespace-nowrap">
          {sleepTimer
            ? formatSleepTimerRemaining(sleepTimer)
            : sleepEpisodes
              ? t('player.episodes', { count: sleepEpisodes })
              : t('player.timer')}
        </span>
      </button>

      {show && (
        <SleepTimerPanel
          sleepTimer={sleepTimer}
          sleepEpisodes={sleepEpisodes}
          customMinutes={customMinutes}
          customEpisodes={customEpisodes}
          onSetCustomMinutes={onSetCustomMinutes}
          onSetCustomEpisodes={onSetCustomEpisodes}
          onStart={onStart}
          onStartEpisodes={onStartEpisodes}
          onCancel={onCancel}
          onClose={onClose}
        />
      )}
    </div>
  );
};

// ─── SleepTimerPanel ───────────────────────────────────────────────────────
// 睡眠定时面板本体，按分钟 / 按集数两个 tab：上方各 4 个预设，下方自定义输入。
// 独立组件是为了让 tab 状态随面板关闭重置（有集数定时在跑时默认落在集数 tab）。

interface SleepTimerPanelProps {
  sleepTimer: number | null;
  sleepEpisodes: number | null;
  customMinutes: string;
  customEpisodes: string;
  onSetCustomMinutes: (value: string) => void;
  onSetCustomEpisodes: (value: string) => void;
  onStart: (durationSeconds: number) => void;
  onStartEpisodes: (episodes: number) => void;
  onCancel: () => void;
  onClose: () => void;
}

const SleepTimerPanel: React.FC<SleepTimerPanelProps> = ({
  sleepTimer,
  sleepEpisodes,
  customMinutes,
  customEpisodes,
  onSetCustomMinutes,
  onSetCustomEpisodes,
  onStart,
  onStartEpisodes,
  onCancel,
  onClose,
}) => {
  const { t } = useTranslation();
  const [tab, setTab] = useState<'minutes' | 'episodes'>(
    sleepEpisodes != null ? 'episodes' : 'minutes',
  );

  // 预设按钮样式对齐 Flutter 端：描边、小字号、中等字重、单行。
  const presetButtonClass =
    'h-8 px-2 text-[11px] font-medium whitespace-nowrap rounded-xl border border-slate-200 dark:border-slate-600 text-slate-600 dark:text-slate-400 hover:bg-slate-50 dark:hover:bg-slate-700 transition-colors';

  return (
    <div className="absolute bottom-full mb-4 right-0 bg-white dark:bg-slate-800 shadow-2xl rounded-2xl p-3 border border-slate-100 dark:border-slate-700 min-w-[216px] flex flex-col gap-2 z-[220] animate-in zoom-in-95 duration-200">
      <div className="px-2 py-1 text-[11px] font-bold text-slate-500 dark:text-slate-400 border-b border-slate-100 dark:border-slate-700 mb-1 text-center">
        {t('player.sleepTimer')}
      </div>

      <div className="flex bg-slate-100 dark:bg-slate-900/60 p-0.5 rounded-xl">
        {(['minutes', 'episodes'] as const).map((mode) => (
          <button
            key={mode}
            onClick={() => setTab(mode)}
            className={`flex-1 px-2 py-1 rounded-lg text-xs font-medium transition-all ${
              tab === mode
                ? 'bg-white dark:bg-slate-700 text-primary-600 shadow-sm'
                : 'text-slate-500 hover:text-slate-700 dark:hover:text-slate-300'
            }`}
          >
            {mode === 'minutes' ? t('player.byMinutes') : t('player.byEpisodes')}
          </button>
        ))}
      </div>

      {tab === 'minutes' ? (
        <>
          <div className="grid grid-cols-2 gap-2">
            {SLEEP_TIMER_PRESET_MINUTES.map((mins) => (
              <button
                key={mins}
                onClick={() => {
                  onStart(mins * 60);
                  onClose();
                }}
                className={presetButtonClass}
              >
                {t('player.minutes', { count: mins })}
              </button>
            ))}
          </div>

          <div className="mt-1 flex items-center gap-1 p-1 bg-slate-50 dark:bg-slate-900/50 rounded-xl border border-slate-100 dark:border-slate-700 focus-within:border-primary-500/50 transition-colors">
            <input
              type="number"
              min="1"
              value={customMinutes}
              onChange={(e) => {
                const val = e.target.value;
                if (val === '' || parseInt(val) >= 0) {
                  onSetCustomMinutes(val);
                }
              }}
              placeholder={t('player.customMinutes')}
              className="flex-1 bg-transparent border-none outline-none px-2 py-1.5 text-[11px] dark:text-white placeholder:text-slate-400 w-0"
            />
            <button
              onClick={() => {
                const mins = parseInt(customMinutes);
                if (mins > 0) {
                  onStart(mins * 60);
                  onClose();
                  onSetCustomMinutes('');
                }
              }}
              className="px-3 py-1.5 text-xs font-bold rounded-lg bg-primary-600 text-white hover:bg-primary-700 transition-colors shrink-0"
            >
              {t('player.start')}
            </button>
          </div>
        </>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-2">
            {SLEEP_TIMER_PRESET_EPISODES.map((episodes) => (
              <button
                key={episodes}
                onClick={() => {
                  onStartEpisodes(episodes);
                  onClose();
                }}
                className={presetButtonClass}
              >
                {episodes === 1
                  ? t('player.finishCurrentEpisode')
                  : t('player.finishNEpisodes', { count: episodes })}
              </button>
            ))}
          </div>

          <div className="mt-1 flex items-center gap-1 p-1 bg-slate-50 dark:bg-slate-900/50 rounded-xl border border-slate-100 dark:border-slate-700 focus-within:border-primary-500/50 transition-colors">
            <input
              type="number"
              min="1"
              value={customEpisodes}
              onChange={(e) => {
                const val = e.target.value;
                if (val === '' || parseInt(val) >= 0) {
                  onSetCustomEpisodes(val);
                }
              }}
              placeholder={t('player.customEpisodes')}
              className="flex-1 bg-transparent border-none outline-none px-2 py-1.5 text-[11px] dark:text-white placeholder:text-slate-400 w-0"
            />
            <button
              onClick={() => {
                const episodes = parseInt(customEpisodes);
                if (episodes > 0) {
                  onStartEpisodes(episodes);
                  onClose();
                  onSetCustomEpisodes('');
                }
              }}
              className="px-3 py-1.5 text-xs font-bold rounded-lg bg-primary-600 text-white hover:bg-primary-700 transition-colors shrink-0"
            >
              {t('player.start')}
            </button>
          </div>
        </>
      )}

      {sleepEpisodes !== null && (
        <div className="px-2 text-center text-[10px] text-slate-400">
          {t('player.episodesRemaining', { count: sleepEpisodes })}
        </div>
      )}

      <button
        onClick={() => {
          onCancel();
          onClose();
        }}
        disabled={sleepTimer === null && sleepEpisodes === null}
        className="mt-2 px-4 py-2 text-xs sm:text-sm font-bold rounded-xl bg-red-50 dark:bg-red-900/20 text-red-500 transition-colors disabled:opacity-40"
      >
        {t('player.cancelTimer')}
      </button>
    </div>
  );
};
