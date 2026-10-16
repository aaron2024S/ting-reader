import { apiErrorMessage } from '../../core/api/errors';
import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate, useParams } from 'react-router';
import {
  ArrowLeft,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  Download,
  Puzzle,
  RefreshCw,
} from 'lucide-react';
import apiClient from '../../core/api/client';
import type { Plugin } from '../../core/types';
import { formatDate } from '../../core/utils/date';
import { useApplicationTimeZone } from '../../core/utils/timeZone';

interface PluginLogEntry {
  timestamp: string;
  level: string;
  module: string;
  message: string;
  raw_message?: string;
  fields?: Record<string, unknown>;
}

interface PluginLogsResponse {
  logs: PluginLogEntry[];
  total: number;
  page: number;
  page_size: number;
}

const LEVEL_OPTIONS = ['', 'DEBUG', 'INFO', 'WARN', 'ERROR'];
const SOURCE_OPTIONS = ['', 'code', 'lifecycle', 'runtime', 'gateway', 'security'];
const PAGE_SIZE = 100;

const stringField = (fields: Record<string, unknown> | undefined, key: string) => {
  const value = fields?.[key];
  return typeof value === 'string' && value ? value : undefined;
};

const PluginLogsPage: React.FC = () => {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { pluginId = '' } = useParams();
  useApplicationTimeZone();

  const [plugin, setPlugin] = useState<Plugin | null>(null);
  const [logs, setLogs] = useState<PluginLogEntry[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [levelFilter, setLevelFilter] = useState('');
  const [sourceFilter, setSourceFilter] = useState('');
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [expandedKeys, setExpandedKeys] = useState<Set<string>>(new Set());

  const encodedPluginId = useMemo(() => encodeURIComponent(pluginId), [pluginId]);
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));

  useEffect(() => {
    if (!pluginId) return;
    void apiClient
      .get<Plugin>(`/api/v1/plugins/${encodedPluginId}`)
      .then((response) => setPlugin(response.data))
      .catch((requestError) => {
        console.error('Failed to load plugin details', requestError);
      });
  }, [encodedPluginId, pluginId]);

  const fetchLogs = useCallback(async () => {
    if (!pluginId) return;
    setLoading(true);
    try {
      const response = await apiClient.get<PluginLogsResponse>(
        `/api/v1/plugins/${encodedPluginId}/logs`,
        {
          params: {
            level: levelFilter,
            source: sourceFilter,
            page,
            page_size: PAGE_SIZE,
          },
        },
      );
      setLogs(response.data.logs || []);
      setTotal(response.data.total || 0);
      setError('');
    } catch (requestError) {
      console.error('Failed to fetch plugin logs', requestError);
      setError(apiErrorMessage(requestError, t('pluginLogs.loadFailed')));
    } finally {
      setLoading(false);
    }
  }, [encodedPluginId, levelFilter, page, pluginId, sourceFilter, t]);

  useEffect(() => {
    const timer = window.setTimeout(() => void fetchLogs(), 0);
    return () => window.clearTimeout(timer);
  }, [fetchLogs]);

  useEffect(() => {
    if (!autoRefresh) return;
    const interval = window.setInterval(() => void fetchLogs(), 3000);
    return () => window.clearInterval(interval);
  }, [autoRefresh, fetchLogs]);

  const handleExport = async () => {
    try {
      const response = await apiClient.get(
        `/api/v1/plugins/${encodedPluginId}/logs/export`,
        {
          params: { level: levelFilter, source: sourceFilter },
          responseType: 'blob',
        },
      );
      const url = URL.createObjectURL(response.data);
      const link = document.createElement('a');
      link.href = url;
      link.download = `plugin_${pluginId.split('@')[0]}_logs.txt`;
      document.body.appendChild(link);
      link.click();
      link.remove();
      URL.revokeObjectURL(url);
    } catch (requestError) {
      console.error('Failed to export plugin logs', requestError);
    }
  };

  const toggleExpanded = (key: string) => {
    setExpandedKeys((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  return (
    <div className="flex h-full min-h-0 flex-1 flex-col overflow-hidden p-4 sm:p-6 md:p-8">
      <header className="mb-6 flex shrink-0 flex-col gap-4 lg:flex-row lg:items-center lg:justify-between">
        <div className="flex min-w-0 items-center gap-3">
          <button
            type="button"
            onClick={() => navigate('/admin/plugins')}
            className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-lg text-slate-500 transition-colors hover:bg-slate-100 hover:text-slate-900 dark:text-slate-400 dark:hover:bg-slate-800 dark:hover:text-white"
            title={t('pluginLogs.back')}
            aria-label={t('pluginLogs.back')}
          >
            <ArrowLeft size={18} />
          </button>
          <div className="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg border border-cyan-100 bg-cyan-50 text-cyan-700 dark:border-cyan-900/50 dark:bg-cyan-950/40 dark:text-cyan-300">
            <Puzzle size={21} />
          </div>
          <div className="min-w-0">
            <h1 className="truncate text-xl font-bold text-slate-950 dark:text-white">
              {t('pluginLogs.title', { name: plugin?.name || pluginId })}
            </h1>
            <p className="mt-1 truncate text-sm text-slate-500 dark:text-slate-400">
              {plugin ? `${plugin.id} · v${plugin.version}` : pluginId}
            </p>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={() => void fetchLogs()}
            className="inline-flex h-9 w-9 items-center justify-center rounded-lg border border-slate-200 bg-white text-slate-600 transition-colors hover:bg-slate-50 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-300 dark:hover:bg-slate-800"
            title={t('pluginLogs.refresh')}
            aria-label={t('pluginLogs.refresh')}
          >
            <RefreshCw size={17} className={loading ? 'animate-spin' : ''} />
          </button>
          <button
            type="button"
            onClick={() => void handleExport()}
            className="inline-flex h-9 w-9 items-center justify-center rounded-lg border border-slate-200 bg-white text-slate-600 transition-colors hover:bg-slate-50 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-300 dark:hover:bg-slate-800"
            title={t('pluginLogs.export')}
            aria-label={t('pluginLogs.export')}
          >
            <Download size={17} />
          </button>
          <label className="inline-flex h-9 items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 text-sm text-slate-600 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-300">
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(event) => setAutoRefresh(event.target.checked)}
              className="h-4 w-4 accent-primary-600"
            />
            {t('pluginLogs.autoRefresh')}
          </label>
        </div>
      </header>

      <div className="mb-4 flex shrink-0 flex-wrap items-center gap-3 border-y border-slate-200 py-4 dark:border-slate-800">
        <label className="flex h-10 min-w-40 items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 text-sm dark:border-slate-700 dark:bg-slate-900">
          <span className="text-slate-500">{t('pluginLogs.level')}</span>
          <select
            value={levelFilter}
            onChange={(event) => {
              setLevelFilter(event.target.value);
              setPage(1);
            }}
            className="min-w-0 flex-1 bg-transparent text-slate-700 outline-none dark:text-slate-200"
          >
            {LEVEL_OPTIONS.map((level) => (
              <option key={level || 'all'} value={level}>
                {level || t('pluginLogs.allLevels')}
              </option>
            ))}
          </select>
        </label>

        <label className="flex h-10 min-w-52 items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 text-sm dark:border-slate-700 dark:bg-slate-900">
          <span className="text-slate-500">{t('pluginLogs.source')}</span>
          <select
            value={sourceFilter}
            onChange={(event) => {
              setSourceFilter(event.target.value);
              setPage(1);
            }}
            className="min-w-0 flex-1 bg-transparent text-slate-700 outline-none dark:text-slate-200"
          >
            {SOURCE_OPTIONS.map((source) => (
              <option key={source || 'all'} value={source}>
                {source ? t(`pluginLogs.sources.${source}`) : t('pluginLogs.allSources')}
              </option>
            ))}
          </select>
        </label>

        <span className="ml-auto text-sm text-slate-500 dark:text-slate-400">
          {t('pluginLogs.total', { count: total })}
        </span>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain rounded-lg border border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900">
        {loading && logs.length === 0 ? (
          <div className="flex min-h-64 items-center justify-center">
            <RefreshCw size={24} className="animate-spin text-primary-600" />
          </div>
        ) : error ? (
          <div className="flex min-h-64 items-center justify-center px-6 text-sm text-red-600 dark:text-red-300">
            {error}
          </div>
        ) : logs.length === 0 ? (
          <div className="flex min-h-64 items-center justify-center px-6 text-sm text-slate-500 dark:text-slate-400">
            {t('pluginLogs.empty')}
          </div>
        ) : (
          <div className="divide-y divide-slate-100 dark:divide-slate-800">
            {logs.map((log, index) => {
              const key = `${log.timestamp}-${index}`;
              const fields = log.fields || {};
              const source = stringField(fields, 'source');
              const runtime = stringField(fields, 'runtime');
              const operation = stringField(fields, 'op');
              const instanceId = stringField(fields, 'plugin_instance_id');
              const eventId = stringField(fields, 'event_id');
              const expanded = expandedKeys.has(key);
              const fieldEntries = Object.entries(fields);

              return (
                <article key={key} className="p-4 transition-colors hover:bg-slate-50/70 dark:hover:bg-slate-800/30 sm:p-5">
                  <div className="flex items-start gap-3">
                    <div className={`mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-lg ${
                      log.level === 'ERROR'
                        ? 'bg-red-50 text-red-600 dark:bg-red-950/40 dark:text-red-300'
                        : log.level === 'WARN'
                          ? 'bg-amber-50 text-amber-600 dark:bg-amber-950/40 dark:text-amber-300'
                          : 'bg-slate-100 text-slate-600 dark:bg-slate-800 dark:text-slate-300'
                    }`}>
                      <Puzzle size={17} />
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="rounded-md bg-slate-100 px-2 py-0.5 text-[10px] font-bold text-slate-600 dark:bg-slate-800 dark:text-slate-300">
                          {log.level}
                        </span>
                        {instanceId ? (
                          <span className="max-w-64 truncate rounded-md bg-cyan-50 px-2 py-0.5 text-[11px] font-semibold text-cyan-700 dark:bg-cyan-950/40 dark:text-cyan-300" title={instanceId}>
                            {instanceId}
                          </span>
                        ) : null}
                        {runtime ? (
                          <span className="rounded-md bg-slate-100 px-2 py-0.5 text-[10px] font-semibold uppercase text-slate-600 dark:bg-slate-800 dark:text-slate-300">
                            {runtime}
                          </span>
                        ) : null}
                        {source ? (
                          <span className="rounded-md border border-slate-200 px-2 py-0.5 text-[10px] text-slate-500 dark:border-slate-700 dark:text-slate-400">
                            {t(`pluginLogs.sources.${source}`, { defaultValue: source })}
                          </span>
                        ) : null}
                        {operation ? (
                          <span className="max-w-64 truncate rounded-md bg-emerald-50 px-2 py-0.5 font-mono text-[10px] text-emerald-700 dark:bg-emerald-950/30 dark:text-emerald-300" title={operation}>
                            {operation}
                          </span>
                        ) : null}
                        <time className="ml-auto text-xs text-slate-400">{formatDate(log.timestamp)}</time>
                      </div>
                      <p className="mt-2 break-words text-sm leading-6 text-slate-700 dark:text-slate-200">
                        {log.raw_message || log.message}
                      </p>
                      {eventId ? (
                        <p className="mt-1 truncate font-mono text-[10px] text-slate-400" title={eventId}>
                          event_id: {eventId}
                        </p>
                      ) : null}

                      {fieldEntries.length > 0 ? (
                        <>
                          <button
                            type="button"
                            onClick={() => toggleExpanded(key)}
                            className="mt-2 inline-flex items-center gap-1 text-xs font-medium text-slate-500 hover:text-primary-600"
                          >
                            {expanded ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
                            {t('pluginLogs.details')}
                          </button>
                          {expanded ? (
                            <dl className="mt-2 grid grid-cols-1 gap-2 rounded-lg bg-slate-50 p-3 text-xs dark:bg-slate-950/40 sm:grid-cols-2 xl:grid-cols-3">
                              {fieldEntries.map(([name, value]) => (
                                <div key={name} className="min-w-0">
                                  <dt className="font-medium text-slate-400">{name}</dt>
                                  <dd className="mt-0.5 break-words font-mono text-slate-700 dark:text-slate-300">
                                    {typeof value === 'string' ? value : JSON.stringify(value)}
                                  </dd>
                                </div>
                              ))}
                            </dl>
                          ) : null}
                        </>
                      ) : null}
                    </div>
                  </div>
                </article>
              );
            })}
          </div>
        )}
      </div>

      {pageCount > 1 ? (
        <footer className="mt-4 flex shrink-0 items-center justify-end gap-2">
          <button
            type="button"
            onClick={() => setPage((current) => Math.max(1, current - 1))}
            disabled={page <= 1}
            className="inline-flex h-9 w-9 items-center justify-center rounded-lg border border-slate-200 bg-white text-slate-600 disabled:opacity-40 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-300"
            title={t('pluginLogs.previous')}
            aria-label={t('pluginLogs.previous')}
          >
            <ChevronLeft size={17} />
          </button>
          <span className="min-w-20 text-center text-sm text-slate-500 dark:text-slate-400">
            {t('pluginLogs.page', { page, total: pageCount })}
          </span>
          <button
            type="button"
            onClick={() => setPage((current) => Math.min(pageCount, current + 1))}
            disabled={page >= pageCount}
            className="inline-flex h-9 w-9 items-center justify-center rounded-lg border border-slate-200 bg-white text-slate-600 disabled:opacity-40 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-300"
            title={t('pluginLogs.next')}
            aria-label={t('pluginLogs.next')}
          >
            <ChevronRight size={17} />
          </button>
        </footer>
      ) : null}
    </div>
  );
};

export default PluginLogsPage;
