import { apiErrorMessage, apiErrorPayload, ApiRequestError } from '../../core/api/errors';
import { ExternalLink, Loader2, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  invokePluginCapability,
  invokePluginHost,
} from "../../core/api/pluginCapabilities";
import apiClient from "../../core/api/client";
import type { ClientExtensionDescriptor } from "../../core/pluginExtensions";
import { useAuthStore } from "../../core/stores/authStore";
import { getRuntimeAssetUrl } from "../../core/utils/runtimeUrl";
import {
  MAX_BRIDGE_REQUESTS_PER_WINDOW,
  BRIDGE_RATE_WINDOW_MS,
  createBridgeToken,
  isBridgeMessageWithinLimit,
  isBridgeRequest,
  isDocumentReady,
  isDocumentUnloading,
  externalUrlRequest,
  sanitizeStringArray,
  responseFor,
} from "./pluginBridge";
import {
  pluginThemeFromDocument,
  pluginAssetPath,
  absoluteAssetUrl,
  finalResponseUrl,
  withPluginDocumentPolicy,
  responseContentType,
  isExpectedTextAsset,
} from "./pluginDocument";

type PluginDocument = {
  html: string;
  token: string;
};

type PluginDocumentGeneration = {
  token: string;
  frame?: Window;
  port?: MessagePort;
  loadCount: number;
  initialized: boolean;
  invalidated: boolean;
  requestTimestamps: number[];
};

type PluginWebContainerProps = {
  extension: ClientExtensionDescriptor;
  context?: Record<string, unknown>;
};

const PluginWebContainer = ({
  extension,
  context,
}: PluginWebContainerProps) => {
  const { t } = useTranslation();
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const generationRef = useRef<PluginDocumentGeneration | undefined>(
    undefined,
  );
  const activeUrl = useAuthStore((state) => state.activeUrl);
  const [loadError, setLoadError] = useState<string>();
  const [pluginDocument, setPluginDocument] = useState<PluginDocument>();
  const [pendingExternalUrl, setPendingExternalUrl] = useState<string>();
  const src = useMemo(() => pluginAssetPath(extension), [extension]);
  const srcBaseUrl = useMemo(
    () => (src ? absoluteAssetUrl(src, activeUrl) : undefined),
    [activeUrl, src],
  );
  const bootstrapUrl = useMemo(() => {
    if (typeof window === "undefined") return undefined;
    return new URL(
      getRuntimeAssetUrl("/plugin-container-bootstrap.js"),
      window.location.href,
    ).toString();
  }, []);
  const allowedHostMethods = useMemo(
    () =>
      new Set(
        sanitizeStringArray(extension.render?.bridge?.host_methods, 128),
      ),
    [extension.render?.bridge?.host_methods],
  );
  const allowedCapabilityIds = useMemo(() => {
    const configured = sanitizeStringArray(
      extension.render?.bridge?.capabilities,
      128,
    );
    const currentCapabilityId =
      typeof extension.capability.id === "string"
        ? extension.capability.id.trim()
        : "";
    return new Set(
      currentCapabilityId && currentCapabilityId.length <= 128
        ? [currentCapabilityId, ...configured]
        : configured,
    );
  }, [extension.capability.id, extension.render?.bridge?.capabilities]);
  const bridgeGenerationKey = JSON.stringify({
    pluginId: extension.pluginId,
    pluginName: extension.pluginName,
    capabilityId: extension.capability.id,
    slot: extension.slot,
    contexts: extension.contexts,
    bridge: extension.render?.bridge || {},
    context: context || {},
  });

  const invalidateGeneration = (generation?: PluginDocumentGeneration) => {
    if (!generation) return;
    generation.invalidated = true;
    generation.port?.close();
    generation.port = undefined;
    generation.frame = undefined;
    generation.requestTimestamps = [];
  };

  const blockPluginNavigation = (generation: PluginDocumentGeneration) => {
    if (generationRef.current !== generation || generation.invalidated) return;
    invalidateGeneration(generation);
    setPendingExternalUrl(undefined);
    setPluginDocument(undefined);
    setLoadError(t("pluginExtensions.navigationBlocked"));
  };

  const postToGeneration = (
    generation: PluginDocumentGeneration,
    message: unknown,
  ) => {
    if (
      generationRef.current !== generation ||
      generation.invalidated ||
      !generation.initialized ||
      !generation.port
    ) {
      return;
    }
    generation.port.postMessage(message);
  };

  const handleLoad = () => {
    const generation = generationRef.current;
    const frame = iframeRef.current?.contentWindow;
    if (!generation || !frame || generation.invalidated) return;
    generation.loadCount += 1;
    if (generation.loadCount > 1) {
      blockPluginNavigation(generation);
    }
  };

  useEffect(() => {
    let cancelled = false;
    const timer = window.setTimeout(() => {
      invalidateGeneration(generationRef.current);
      generationRef.current = undefined;
      setPendingExternalUrl(undefined);
      if (!src || !srcBaseUrl || !bootstrapUrl) {
        setPluginDocument(undefined);
        return;
      }

      setLoadError(undefined);
      setPluginDocument(undefined);
      void apiClient
        .get<string>(src, { responseType: "text" })
        .then(async (response) => {
          if (cancelled) return;
          const contentType = responseContentType(response.headers);
          if (!isExpectedTextAsset(contentType, "document")) {
            throw new Error(
              `Plugin document returned an invalid content type (${contentType || "missing"}).`,
            );
          }
          const html =
            typeof response.data === "string"
              ? response.data
              : String(response.data ?? "");
          const token = createBridgeToken();
          const assetUrl = finalResponseUrl(response.request) || srcBaseUrl;
          const renderedHtml = await withPluginDocumentPolicy(
            html,
            assetUrl,
            bootstrapUrl,
            token,
            pluginThemeFromDocument(),
          );
          if (cancelled) return;
          const generation: PluginDocumentGeneration = {
            token,
            loadCount: 0,
            initialized: false,
            invalidated: false,
            requestTimestamps: [],
          };
          generationRef.current = generation;
          setPluginDocument({
            html: renderedHtml,
            token,
          });
        })
        .catch((error) => {
          if (cancelled) return;
          setLoadError(
            apiErrorMessage(error, "Plugin UI failed to load."),
          );
        });
    }, 0);

    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      invalidateGeneration(generationRef.current);
    };
  }, [bootstrapUrl, bridgeGenerationKey, src, srcBaseUrl]);

  const handleBridgeMessage = async (
    generation: PluginDocumentGeneration,
    data: unknown,
  ) => {
    if (
      generationRef.current !== generation ||
      generation.invalidated ||
      !generation.initialized ||
      !isBridgeMessageWithinLimit(data)
    ) {
      return;
    }
    if (isDocumentUnloading(data, generation.token)) {
      blockPluginNavigation(generation);
      return;
    }

    const externalRequest = externalUrlRequest(data, generation.token);
    if (externalRequest) {
      setPendingExternalUrl(externalRequest.url);
      return;
    }
    if (!isBridgeRequest(data, generation.token)) return;

    const now = Date.now();
    generation.requestTimestamps = generation.requestTimestamps.filter(
      (timestamp) => now - timestamp < BRIDGE_RATE_WINDOW_MS,
    );
    if (
      generation.requestTimestamps.length >= MAX_BRIDGE_REQUESTS_PER_WINDOW
    ) {
      postToGeneration(
        generation,
        responseFor(data, {
          ok: false,
          error: {
            error: 'ResourceLimitExceeded',
            message: 'Plugin bridge rate limit exceeded',
            status: 429,
            details: { reason: 'rate_limit', operation: data.method, retryable: true },
          },
        }),
      );
      return;
    }
    generation.requestTimestamps.push(now);

    const request = data;
    try {
      if (request.method === "capability.invoke") {
        const params =
          request.params && typeof request.params === "object"
            ? (request.params as {
                capabilityId?: string;
                params?: unknown;
              })
            : {};
        const requestedCapabilityId =
          typeof params.capabilityId === "string" &&
          params.capabilityId.trim()
            ? params.capabilityId.trim()
            : extension.capability.id;
        if (
          requestedCapabilityId.length > 128 ||
          !allowedCapabilityIds.has(requestedCapabilityId)
        ) {
          throw new ApiRequestError({
            error: 'PermissionDenied',
            message: 'Capability is not allowed for this view',
            status: 403,
            details: { reason: 'capability_denied', operation: request.method, retryable: false },
          });
        }
        const result = await invokePluginCapability(
          extension.pluginId,
          requestedCapabilityId,
          params.params ?? {},
          extension.capability.id,
          extension.clientGrant,
        );
        postToGeneration(
          generation,
          responseFor(request, { ok: true, result }),
        );
        return;
      }

      const params =
        request.params && typeof request.params === "object"
          ? (request.params as { method?: string; params?: unknown })
          : {};
      const hostMethod =
        typeof params.method === "string" ? params.method.trim() : "";
      if (!hostMethod) {
        throw new ApiRequestError({
          error: 'InvalidRequest',
          message: 'Missing host method',
          status: 400,
          details: { reason: 'missing_field', operation: request.method, retryable: false },
        });
      }
      if (hostMethod.length > 128 || !allowedHostMethods.has(hostMethod)) {
        throw new ApiRequestError({
          error: 'PermissionDenied',
          message: 'Host method is not allowed for this view',
          status: 403,
          details: { reason: 'capability_denied', operation: request.method, retryable: false },
        });
      }

      const result = await invokePluginHost({
        plugin_id: extension.pluginId,
        ui_capability_id: extension.capability.id,
        ui_grant: extension.clientGrant || "",
        method: hostMethod,
        params: params.params ?? {},
      });
      postToGeneration(
        generation,
        responseFor(request, { ok: true, result }),
      );
    } catch (err) {
      postToGeneration(
        generation,
        responseFor(request, {
          ok: false,
          error: apiErrorPayload(err, 'Plugin operation failed'),
        }),
      );
    }
  };

  const handleMessage = (event: MessageEvent) => {
    const generation = generationRef.current;
    const frame = iframeRef.current?.contentWindow;
    if (
      !generation ||
      generation.invalidated ||
      generation.initialized ||
      !frame ||
      event.source !== frame ||
      event.ports.length !== 1 ||
      !isBridgeMessageWithinLimit(event.data) ||
      !isDocumentReady(event.data, generation.token)
    ) {
      return;
    }

    const port = event.ports[0];
    generation.initialized = true;
    generation.frame = frame;
    generation.port = port;
    port.onmessage = (portEvent) => {
      void handleBridgeMessage(generation, portEvent.data);
    };
    port.onmessageerror = () => invalidateGeneration(generation);
    port.start();
    setLoadError(undefined);
    postToGeneration(generation, {
      type: "ting-plugin:init",
      bridge_token: generation.token,
      bridgeToken: generation.token,
      pluginId: extension.pluginId,
      pluginName: extension.pluginName,
      capabilityId: extension.capability.id,
      slot: extension.slot,
      contexts: extension.contexts,
      context: context || {},
      theme: pluginThemeFromDocument(),
    });
  };

  useEffect(() => {
    window.addEventListener("message", handleMessage);
    return () => window.removeEventListener("message", handleMessage);
  });

  useEffect(() => {
    const root = document.documentElement;
    const observer = new MutationObserver(() => {
      const generation = generationRef.current;
      if (
        !generation ||
        generation.invalidated ||
        !generation.initialized ||
        !generation.port
      ) {
        return;
      }
      generation.port.postMessage({
        type: "ting-plugin:theme",
        bridge_token: generation.token,
        theme: pluginThemeFromDocument(),
      });
    });
    observer.observe(root, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);

  if (!src) {
    return (
      <div className="flex h-full items-center justify-center px-6 text-center text-sm text-slate-500 dark:text-slate-400">
        {t("pluginExtensions.missingEntry")}
      </div>
    );
  }

  return (
    <div className="relative h-full w-full">
      {loadError ? (
        <div className="absolute inset-x-4 top-4 z-10 rounded-md border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700 dark:border-red-900/40 dark:bg-red-950/40 dark:text-red-300">
          {loadError}
        </div>
      ) : null}
      {pendingExternalUrl ? (
        <div className="absolute inset-x-4 bottom-4 z-20 flex min-w-0 items-center gap-2 rounded-md border border-slate-200 bg-white px-3 py-2 shadow-lg dark:border-slate-700 dark:bg-slate-900">
          <ExternalLink size={16} className="shrink-0 text-slate-500" />
          <span className="min-w-0 flex-1 truncate text-sm text-slate-700 dark:text-slate-200" title={pendingExternalUrl}>
            {pendingExternalUrl}
          </span>
          <a
            href={pendingExternalUrl}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex size-8 shrink-0 items-center justify-center rounded-md text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800"
            title={t("pluginExtensions.openExternalLink")}
            aria-label={t("pluginExtensions.openExternalLink")}
            onClick={() => setPendingExternalUrl(undefined)}
          >
            <ExternalLink size={17} />
          </a>
          <button
            type="button"
            className="inline-flex size-8 shrink-0 items-center justify-center rounded-md text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800"
            title={t("common.cancel")}
            aria-label={t("common.cancel")}
            onClick={() => setPendingExternalUrl(undefined)}
          >
            <X size={17} />
          </button>
        </div>
      ) : null}
      {!pluginDocument && !loadError ? (
        <div className="flex h-full items-center justify-center gap-2 text-sm text-slate-500 dark:text-slate-400">
          <Loader2 size={16} className="animate-spin" />
          <span>{t("pluginExtensions.loadingUi")}</span>
        </div>
      ) : null}
      {pluginDocument ? (
        <iframe
          key={pluginDocument.token}
          ref={iframeRef}
          srcDoc={pluginDocument.html}
          title={extension.title || extension.capability.id}
          sandbox="allow-scripts allow-forms"
          referrerPolicy="no-referrer"
          className="h-full w-full border-0 bg-white dark:bg-slate-950"
          onLoad={handleLoad}
        />
      ) : null}
    </div>
  );
};

export default PluginWebContainer;
