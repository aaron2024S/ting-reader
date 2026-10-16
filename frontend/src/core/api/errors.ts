import axios from 'axios';

export interface ApiErrorDetails {
  reason?: string;
  operation?: string;
  resource?: { kind?: string; id?: string } | null;
  context?: string[];
  retryable?: boolean;
  [key: string]: unknown;
}

export interface ApiErrorBody {
  error?: string;
  message?: string;
  details?: ApiErrorDetails | null;
  trace_id?: string;
  status?: number;
}

export class ApiRequestError extends Error {
  readonly body: ApiErrorBody;

  constructor(body: ApiErrorBody) {
    super(body.message || body.error || 'Request failed');
    this.name = 'ApiRequestError';
    this.body = body;
  }
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function text(value: unknown): string | undefined {
  return typeof value === 'string' ? value.trim() || undefined : undefined;
}

export function readApiError(error: unknown): ApiErrorBody | null {
  if (error instanceof ApiRequestError) return error.body;
  const response = axios.isAxiosError(error)
    ? error.response
    : record(record(error)?.response);
  const data = record(response?.data) || record(record(error)?.body) || record(error);
  if (!data) return null;
  const code = text(data.error);
  const message = text(data.message);
  if (!code && !message) return null;
  const rawDetails = record(data.details);
  const rawResource = record(rawDetails?.resource);
  const details: ApiErrorDetails | null = rawDetails ? {
    ...rawDetails,
    reason: text(rawDetails.reason),
    operation: text(rawDetails.operation),
    resource: rawResource ? {
      kind: text(rawResource.kind),
      id: text(rawResource.id),
    } : null,
    context: Array.isArray(rawDetails.context)
      ? rawDetails.context.filter((entry): entry is string => typeof entry === 'string')
      : undefined,
    retryable: typeof rawDetails.retryable === 'boolean' ? rawDetails.retryable : undefined,
  } : null;
  const status = response?.status ?? data.status;
  return {
    error: code, message, details, trace_id: text(data.trace_id),
    status: typeof status === 'number' && Number.isInteger(status) ? status : undefined,
  };
}

/** Serializable error information only; never serialize Axios request/config. */
export function apiErrorPayload(error: unknown, fallback: string): ApiErrorBody {
  const body = readApiError(error);
  return {
    error: body?.error || 'InternalError',
    message: body?.message || apiErrorMessage(error, fallback),
    details: body?.details || { retryable: false },
    ...(body?.trace_id ? { trace_id: body.trace_id } : {}),
    ...(body?.status ? { status: body.status } : {}),
  };
}

export function apiErrorMessage(error: unknown, fallback: string): string {
  const body = readApiError(error);
  const message = body?.message?.trim();
  if (message) return message;
  const code = body?.error?.trim();
  if (code) return code;
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  if (typeof error === 'string' && error.trim()) return error.trim();
  return fallback;
}

export function apiErrorCode(error: unknown): string | undefined {
  return readApiError(error)?.error?.trim() || undefined;
}
