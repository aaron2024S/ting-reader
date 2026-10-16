import type { ApiErrorBody } from '../../core/api/errors';

export type PluginBridgeRequest = {
  type: "ting-plugin:request";
  bridge_token: string;
  id: string;
  method: "capability.invoke" | "host.invoke";
  params?: unknown;
};

export type PluginBridgeResponse = {
  type: "ting-plugin:response";
  bridge_token: string;
  id: string;
  ok: boolean;
  result?: unknown;
  error?: ApiErrorBody;
};

export type PluginExternalUrlRequest = {
  type: "ting-plugin:external-url";
  bridge_token: string;
  url: string;
};

const MAX_BRIDGE_MESSAGE_BYTES = 256 * 1024;
export const MAX_BRIDGE_REQUESTS_PER_WINDOW = 100;
export const BRIDGE_RATE_WINDOW_MS = 10_000;
const MAX_BRIDGE_JSON_NODES = 20_000;

export const createBridgeToken = () => {
  const bytes = new Uint8Array(24);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(
    "",
  );
};

const isPlainBridgeValue = (
  value: unknown,
  depth: number,
  state: { nodes: number; seen: WeakSet<object> },
): boolean => {
  if (value === null) return true;
  if (typeof value === "string") {
    return value.length <= MAX_BRIDGE_MESSAGE_BYTES;
  }
  if (typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (typeof value !== "object" || depth > 64) return false;

  state.nodes += 1;
  if (state.nodes > MAX_BRIDGE_JSON_NODES || state.seen.has(value)) {
    return false;
  }
  state.seen.add(value);

  let valid: boolean;
  if (Array.isArray(value)) {
    valid = value.every((entry) =>
      isPlainBridgeValue(entry, depth + 1, state),
    );
  } else {
    const prototype = Object.getPrototypeOf(value);
    valid =
      (prototype === Object.prototype || prototype === null) &&
      Object.entries(value as Record<string, unknown>).every(
        ([key, entry]) =>
          key.length <= MAX_BRIDGE_MESSAGE_BYTES &&
          isPlainBridgeValue(entry, depth + 1, state),
      );
  }

  state.seen.delete(value);
  return valid;
};

export const isBridgeMessageWithinLimit = (value: unknown) => {
  try {
    if (
      !isPlainBridgeValue(value, 0, {
        nodes: 0,
        seen: new WeakSet<object>(),
      })
    ) {
      return false;
    }
    const serialized = JSON.stringify(value);
    if (typeof serialized !== "string") return false;
    const byteLength =
      typeof TextEncoder === "undefined"
        ? new Blob([serialized]).size
        : new TextEncoder().encode(serialized).byteLength;
    return byteLength <= MAX_BRIDGE_MESSAGE_BYTES;
  } catch {
    return false;
  }
};

export const isBridgeRequest = (
  value: unknown,
  expectedToken: string,
): value is PluginBridgeRequest => {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<PluginBridgeRequest>;
  return (
    candidate.type === "ting-plugin:request" &&
    candidate.bridge_token === expectedToken &&
    typeof candidate.id === "string" &&
    candidate.id.length > 0 &&
    candidate.id.length <= 128 &&
    (candidate.method === "capability.invoke" ||
      candidate.method === "host.invoke")
  );
};

export const isDocumentReady = (value: unknown, expectedToken: string) => {
  if (!value || typeof value !== "object") return false;
  const candidate = value as {
    type?: unknown;
    bridge_token?: unknown;
  };
  return (
    candidate.type === "ting-plugin:document-ready" &&
    candidate.bridge_token === expectedToken
  );
};

export const isDocumentUnloading = (value: unknown, expectedToken: string) => {
  if (!value || typeof value !== "object") return false;
  const candidate = value as {
    type?: unknown;
    bridge_token?: unknown;
  };
  return (
    candidate.type === "ting-plugin:document-unloading" &&
    candidate.bridge_token === expectedToken
  );
};

export const externalUrlRequest = (
  value: unknown,
  expectedToken: string,
): PluginExternalUrlRequest | undefined => {
  if (!value || typeof value !== "object") return undefined;
  const candidate = value as Partial<PluginExternalUrlRequest>;
  if (
    candidate.type !== "ting-plugin:external-url" ||
    candidate.bridge_token !== expectedToken ||
    typeof candidate.url !== "string" ||
    candidate.url.length === 0 ||
    candidate.url.length > 2048
  ) {
    return undefined;
  }
  try {
    const parsed = new URL(candidate.url);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return undefined;
    }
    return {
      type: "ting-plugin:external-url",
      bridge_token: expectedToken,
      url: parsed.href,
    };
  } catch {
    return undefined;
  }
};

export const sanitizeStringArray = (value: unknown, maxLength: number) => {
  if (!Array.isArray(value)) return [];
  return value
    .filter((item): item is string => typeof item === "string")
    .map((item) => item.trim())
    .filter((item) => item.length > 0 && item.length <= maxLength);
};

export const responseFor = (
  request: PluginBridgeRequest,
  payload: Omit<PluginBridgeResponse, "type" | "bridge_token" | "id">,
): PluginBridgeResponse => ({
  type: "ting-plugin:response",
  bridge_token: request.bridge_token,
  id: request.id,
  ...payload,
});
