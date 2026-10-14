import type { AxiosAdapter, InternalAxiosRequestConfig, AxiosResponse } from 'axios';

const stableSerialize = (value: unknown): string => {
  if (value === null || value === undefined) return '';
  if (Array.isArray(value)) return `[${value.map(stableSerialize).join(',')}]`;
  if (typeof value === 'object') {
    return `{${Object.entries(value as Record<string, unknown>)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, entry]) => `${JSON.stringify(key)}:${stableSerialize(entry)}`)
      .join(',')}}`;
  }
  return JSON.stringify(value);
};

const fingerprint = (config: InternalAxiosRequestConfig, scope: string) => [
  scope,
  config.headers.get('Authorization'),
  config.method?.toLowerCase(),
  config.baseURL,
  config.url,
  stableSerialize(config.params),
  stableSerialize(config.data),
].join('|');

const createIdempotencyKey = () =>
  typeof crypto !== 'undefined' && crypto.randomUUID
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(36).slice(2)}`;

/// Deduplicate concurrent mutations only within the same authenticated session.
/// Every completion, including a synchronous adapter error, releases its entry.
export const createSingleFlightAdapter = (
  adapter: AxiosAdapter,
  sessionScope: () => string,
): AxiosAdapter => {
  const pending = new Map<string, Promise<AxiosResponse>>();
  return (config) => {
    if (!config.headers.has('Idempotency-Key')) {
      config.headers.set('Idempotency-Key', createIdempotencyKey());
    }
    // Cancellation belongs to one caller. Binary bodies cannot be compared by
    // filenames and sizes: two different uploads can have identical metadata.
    const binary = typeof FormData !== 'undefined' && config.data instanceof FormData
      || typeof Blob !== 'undefined' && config.data instanceof Blob
      || config.data instanceof ArrayBuffer || ArrayBuffer.isView(config.data);
    if (config.signal || config.cancelToken || binary) {
      return Promise.resolve().then(() => adapter(config));
    }
    const key = fingerprint(config, sessionScope());
    const existing = pending.get(key);
    if (existing) return existing;
    const request = Promise.resolve().then(() => adapter(config)).finally(() => {
      pending.delete(key);
    });
    pending.set(key, request);
    return request;
  };
};
