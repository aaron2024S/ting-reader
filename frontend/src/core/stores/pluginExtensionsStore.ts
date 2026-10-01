import { create } from "zustand";
import { listPluginCapabilities } from "../api/pluginCapabilities";
import { useAuthStore } from "./authStore";

type PluginCapabilityRegistrations = Awaited<
  ReturnType<typeof listPluginCapabilities>
>;

type PluginExtensionsStore = {
  registrations: PluginCapabilityRegistrations;
  loading: boolean;
  loaded: boolean;
  loadedForToken?: string;
  error?: string;
  ensureLoaded: () => Promise<void>;
  refresh: () => Promise<void>;
  reset: () => void;
};

let pendingRefresh: Promise<void> | undefined;
let pendingToken: string | undefined;
let loadGeneration = 0;
let scheduledRefresh: ReturnType<typeof globalThis.setTimeout> | undefined;

const CLIENT_GRANT_REFRESH_INTERVAL_MS = 60 * 60 * 1000;
const CLIENT_GRANT_RETRY_INTERVAL_MS = 5 * 60 * 1000;

const clearScheduledRefresh = () => {
  if (scheduledRefresh === undefined) return;
  globalThis.clearTimeout(scheduledRefresh);
  scheduledRefresh = undefined;
};

const scheduleRefresh = (delay: number) => {
  clearScheduledRefresh();
  scheduledRefresh = globalThis.setTimeout(() => {
    scheduledRefresh = undefined;
    void usePluginExtensionsStore.getState().refresh();
  }, delay);
};

export const usePluginExtensionsStore = create<PluginExtensionsStore>(
  (set, get) => ({
    registrations: [],
    loading: false,
    loaded: false,
    loadedForToken: undefined,
    error: undefined,
    ensureLoaded: async () => {
      const token = useAuthStore.getState().token;
      if (!token || (get().loaded && get().loadedForToken === token)) return;
      await get().refresh();
    },
    refresh: async () => {
      const token = useAuthStore.getState().token;
      if (!token) {
        get().reset();
        return;
      }
      if (pendingRefresh) {
        if (pendingToken === token) {
          await pendingRefresh;
          return;
        }
        loadGeneration += 1;
        pendingRefresh = undefined;
        pendingToken = undefined;
      }
      clearScheduledRefresh();

      const current = get();
      const preserveExisting =
        current.loadedForToken === token && current.registrations.length > 0;
      const generation = loadGeneration;
      set({
        registrations: preserveExisting ? current.registrations : [],
        loading: !preserveExisting,
        loaded: preserveExisting,
        loadedForToken: preserveExisting ? token : undefined,
        error: undefined,
      });
      const refreshPromise = (async () => {
        try {
          const uiExtensions = await listPluginCapabilities("ui_extension");
          if (generation !== loadGeneration) return;
          set({
            registrations: uiExtensions,
            loading: false,
            loaded: true,
            loadedForToken: token,
            error: undefined,
          });
          scheduleRefresh(CLIENT_GRANT_REFRESH_INTERVAL_MS);
        } catch (error) {
          if (generation !== loadGeneration) return;
          set({
            registrations: preserveExisting ? current.registrations : [],
            loading: false,
            loaded: true,
            loadedForToken: token,
            error: error instanceof Error ? error.message : String(error),
          });
          scheduleRefresh(CLIENT_GRANT_RETRY_INTERVAL_MS);
        }
      })();
      pendingRefresh = refreshPromise;
      pendingToken = token;

      try {
        await refreshPromise;
      } finally {
        if (pendingRefresh === refreshPromise) {
          pendingRefresh = undefined;
          pendingToken = undefined;
        }
      }
    },
    reset: () => {
      loadGeneration += 1;
      clearScheduledRefresh();
      pendingRefresh = undefined;
      pendingToken = undefined;
      set({
        registrations: [],
        loading: false,
        loaded: false,
        loadedForToken: undefined,
        error: undefined,
      });
    },
  }),
);

export const refreshClientExtensions = () =>
  usePluginExtensionsStore.getState().refresh();
