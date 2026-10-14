import apiClient from "../../core/api/client";
import type { ClientExtensionDescriptor } from "../../core/pluginExtensions";
import { getRuntimeUrl } from "../../core/utils/runtimeUrl";

type PluginTheme = {
  colorScheme: "light" | "dark";
  brightness: "light" | "dark";
  cssVariables: Record<string, string>;
};

const MAX_PLUGIN_DOCUMENT_BYTES = 2 * 1024 * 1024;
const MAX_PLUGIN_TEXT_ASSET_BYTES = 2 * 1024 * 1024;
const MAX_PLUGIN_BUNDLED_BYTES = 8 * 1024 * 1024;

export const pluginThemeFromDocument = (): PluginTheme => {
  const colorScheme = document.documentElement.classList.contains("dark")
    ? "dark"
    : "light";
  const cssVariables =
    colorScheme === "dark"
      ? {
          "--bg": "#020617",
          "--panel": "#0f172a",
          "--text": "#f8fafc",
          "--muted": "#cbd5e1",
          "--line": "#1e293b",
          "--accent": "#7dd3fc",
          "--soft": "#082f49",
          "--danger": "#fca5a5",
        }
      : {
          "--bg": "#f8fafc",
          "--panel": "#ffffff",
          "--text": "#0f172a",
          "--muted": "#475569",
          "--line": "#e2e8f0",
          "--accent": "#0284c7",
          "--soft": "#f0f9ff",
          "--danger": "#dc2626",
        };
  return { colorScheme, brightness: colorScheme, cssVariables };
};

export const pluginAssetPath = (extension: ClientExtensionDescriptor) => {
  const entry = extension.render?.entry?.replace(/^\/+/, "");
  if (!entry) return undefined;
  if (entry.includes("\\") || entry.includes("\0")) return undefined;
  const segments = entry.split("/");
  if (
    segments.some(
      (segment) => !segment || segment === "." || segment === "..",
    )
  ) {
    return undefined;
  }
  const encodedEntry = segments
    .map((segment) => encodeURIComponent(segment))
    .join("/");
  if (!extension.clientGrant) return undefined;
  return `/api/v1/plugin-assets/${encodeURIComponent(extension.clientGrant)}/${encodeURIComponent(extension.pluginId)}/${encodedEntry}`;
};

export const absoluteAssetUrl = (path: string, activeUrl?: string) => {
  try {
    const runtimeUrl = getRuntimeUrl(path, activeUrl);
    if (/^https?:\/\//i.test(runtimeUrl)) return runtimeUrl;
    if (typeof window === "undefined") return undefined;
    return new URL(runtimeUrl, window.location.href).toString();
  } catch {
    return undefined;
  }
};

export const finalResponseUrl = (request: unknown) => {
  if (!request || typeof request !== "object") return undefined;
  const responseUrl = (request as { responseURL?: unknown }).responseURL;
  return typeof responseUrl === "string" && /^https?:\/\//i.test(responseUrl)
    ? responseUrl
    : undefined;
};

const textByteLength = (value: string) =>
  typeof TextEncoder === "undefined"
    ? new Blob([value]).size
    : new TextEncoder().encode(value).byteLength;

const documentScriptNonce = () => {
  const nonce = document
    .querySelector<HTMLMetaElement>('meta[name="ting-csp-nonce"]')
    ?.content.trim();
  if (nonce && /^[A-Za-z0-9_-]{16,128}$/.test(nonce)) return nonce;
  return undefined;
};

const pluginAssetRoot = (href: string) => {
  const assetUrl = new URL(href);
  const markers = ["/api/v1/plugin-assets/", "/api/plugin-assets/"];
  const marker = markers.find((candidate) =>
    assetUrl.pathname.includes(candidate),
  );
  if (!marker) return undefined;
  const markerIndex = assetUrl.pathname.indexOf(marker);
  const assetStart = markerIndex + marker.length;
  const segments = assetUrl.pathname.slice(assetStart).split("/");
  if (segments.length < 3 || !segments[0] || !segments[1]) return undefined;

  const root = new URL(assetUrl.href);
  root.pathname = `${assetUrl.pathname.slice(0, assetStart)}${segments[0]}/${segments[1]}/`;
  root.search = "";
  root.hash = "";
  return root;
};

const isWithinPluginAssetRoot = (candidate: URL, root: URL) =>
  candidate.origin === root.origin &&
  candidate.username === "" &&
  candidate.password === "" &&
  candidate.pathname.startsWith(root.pathname);

const isSameAsset = (candidate: URL, expected: URL) =>
  candidate.origin === expected.origin &&
  candidate.pathname === expected.pathname &&
  candidate.search === expected.search;

export const responseContentType = (headers: unknown) => {
  if (!headers || typeof headers !== "object") return "";
  const value = (headers as Record<string, unknown>)["content-type"];
  return typeof value === "string" ? value.toLowerCase() : "";
};

export const isExpectedTextAsset = (
  contentType: string,
  kind: "document" | "script" | "stylesheet",
) => {
  if (kind === "document") return contentType.includes("text/html");
  if (kind === "stylesheet") return contentType.includes("text/css");
  return /(?:java|ecma)script/.test(contentType);
};

const fetchPluginTextAsset = async (
  url: URL,
  kind: "script" | "stylesheet",
  isAllowed: (candidate: URL) => boolean,
) => {
  if (!isAllowed(url)) {
    throw new Error(`Plugin ${kind} is outside the allowed asset directory.`);
  }
  const response = await apiClient.get<string>(url.href, {
    responseType: "text",
  });
  const responseUrl = new URL(finalResponseUrl(response.request) || url.href);
  if (!isAllowed(responseUrl)) {
    throw new Error(`Plugin ${kind} redirected outside the allowed asset directory.`);
  }
  const contentType = responseContentType(response.headers);
  if (!isExpectedTextAsset(contentType, kind)) {
    throw new Error(
      `Plugin ${kind} returned an invalid content type (${contentType || "missing"}).`,
    );
  }
  const source =
    typeof response.data === "string"
      ? response.data
      : String(response.data ?? "");
  const bytes = textByteLength(source);
  if (bytes > MAX_PLUGIN_TEXT_ASSET_BYTES) {
    throw new Error(`Plugin ${kind} exceeds the per-file size limit.`);
  }
  return { source, bytes };
};

const escapeInlineScript = (source: string) =>
  source.replace(/<\/script/gi, "<\\/script");

const containsExternalCssResource = (source: string) =>
  /@import\b|url\s*\(/i.test(source);

export const withPluginDocumentPolicy = async (
  html: string,
  href: string,
  bootstrapUrl: string,
  token: string,
  theme: PluginTheme,
) => {
  const document = new DOMParser().parseFromString(html, "text/html");
  const origin = new URL(href).origin;
  const root = pluginAssetRoot(href);
  const nonce = documentScriptNonce();
  if (!root) throw new Error("Plugin asset directory could not be resolved.");

  let bundledBytes = textByteLength(html);
  if (bundledBytes > MAX_PLUGIN_DOCUMENT_BYTES) {
    throw new Error("Plugin document exceeds the size limit.");
  }
  const reserveBytes = (bytes: number) => {
    bundledBytes += bytes;
    if (bundledBytes > MAX_PLUGIN_BUNDLED_BYTES) {
      throw new Error("Plugin UI exceeds the bundled asset size limit.");
    }
  };

  document.querySelectorAll("base").forEach((base) => base.remove());
  document.querySelectorAll('meta[http-equiv]').forEach((meta) => {
    if (
      meta.getAttribute("http-equiv")?.toLowerCase() ===
      "content-security-policy"
    ) {
      meta.remove();
    }
  });

  if (!nonce) {
    const bootstrapAssetUrl = new URL(bootstrapUrl);
    const bootstrapOrigin = bootstrapAssetUrl.origin;
    const scripts = Array.from(
      document.querySelectorAll<HTMLScriptElement>("script[src]"),
    );
    for (const script of scripts) {
      if (script.type.trim().toLowerCase() === "module") {
        throw new Error("Plugin module scripts are not supported in the sandbox.");
      }
      const assetUrl = new URL(script.getAttribute("src") || "", href);
      if (!isWithinPluginAssetRoot(assetUrl, root)) {
        throw new Error("Plugin script is outside the allowed asset directory.");
      }
      script.src = assetUrl.href;
      script.removeAttribute("nonce");
      script.removeAttribute("crossorigin");
      script.removeAttribute("integrity");
    }

    const inlineScripts = document.querySelectorAll("script:not([src])");
    if (inlineScripts.length > 0) {
      throw new Error(
        "Inline plugin scripts require an application CSP nonce.",
      );
    }

    const stylesheets = Array.from(
      document.querySelectorAll<HTMLLinkElement>('link[rel~="stylesheet"][href]'),
    );
    for (const stylesheet of stylesheets) {
      const assetUrl = new URL(stylesheet.getAttribute("href") || "", href);
      if (!isWithinPluginAssetRoot(assetUrl, root)) {
        throw new Error("Plugin stylesheet is outside the allowed asset directory.");
      }
      stylesheet.href = assetUrl.href;
      stylesheet.removeAttribute("crossorigin");
      stylesheet.removeAttribute("integrity");
    }

    const scriptSources = Array.from(
      new Set([origin, bootstrapOrigin]),
    ).join(" ");
    const policy = [
      "default-src 'none'",
      `script-src ${scriptSources}`,
      `style-src 'unsafe-inline' ${origin}`,
      `font-src data: ${origin}`,
      `img-src data: blob: ${origin}`,
      `media-src data: blob: ${origin}`,
      "connect-src 'none'",
      "object-src 'none'",
      "frame-src 'none'",
      "worker-src 'none'",
      "form-action 'none'",
      `base-uri ${origin}`,
    ].join("; ");

    const base = document.createElement("base");
    base.href = new URL(".", href).toString();
    const csp = document.createElement("meta");
    csp.httpEquiv = "Content-Security-Policy";
    csp.content = policy;
    const bootstrapData = document.createElement("meta");
    bootstrapData.name = "ting-plugin-bootstrap";
    bootstrapData.content = encodeURIComponent(
      JSON.stringify({ bridgeToken: token, theme, assetUrl: href }),
    );
    const bootstrap = document.createElement("script");
    bootstrap.src = bootstrapAssetUrl.href;
    document.head.prepend(bootstrap);
    document.head.prepend(bootstrapData);
    document.head.prepend(base);
    document.head.prepend(csp);
    return `<!doctype html>\n${document.documentElement.outerHTML}`;
  }

  const stylesheets = Array.from(
    document.querySelectorAll<HTMLLinkElement>('link[rel~="stylesheet"][href]'),
  );
  for (const stylesheet of stylesheets) {
    const assetUrl = new URL(stylesheet.getAttribute("href") || "", href);
    const asset = await fetchPluginTextAsset(
      assetUrl,
      "stylesheet",
      (candidate) => isWithinPluginAssetRoot(candidate, root),
    );
    if (containsExternalCssResource(asset.source)) {
      throw new Error(
        "Plugin stylesheet contains an external resource that cannot be sandboxed.",
      );
    }
    reserveBytes(asset.bytes);
    const style = document.createElement("style");
    style.dataset.tingBundledAsset = assetUrl.href;
    style.textContent = asset.source;
    stylesheet.replaceWith(style);
  }

  document.querySelectorAll("style").forEach((style) => {
    if (containsExternalCssResource(style.textContent || "")) {
      throw new Error(
        "Plugin inline styles contain an external resource that cannot be sandboxed.",
      );
    }
  });

  const scripts = Array.from(
    document.querySelectorAll<HTMLScriptElement>("script[src]"),
  );
  for (const script of scripts) {
    if (script.type.trim().toLowerCase() === "module") {
      throw new Error("Plugin module scripts are not supported in the sandbox.");
    }
    const assetUrl = new URL(script.getAttribute("src") || "", href);
    const asset = await fetchPluginTextAsset(
      assetUrl,
      "script",
      (candidate) => isWithinPluginAssetRoot(candidate, root),
    );
    reserveBytes(asset.bytes);
    const replacement = document.createElement("script");
    for (const attribute of Array.from(script.attributes)) {
      if (
        !["src", "crossorigin", "integrity", "nonce"].includes(
          attribute.name.toLowerCase(),
        )
      ) {
        replacement.setAttribute(attribute.name, attribute.value);
      }
    }
    replacement.dataset.tingCspNonce = token;
    replacement.dataset.tingBundledAsset = assetUrl.href;
    replacement.textContent = escapeInlineScript(asset.source);
    script.replaceWith(replacement);
  }

  const bootstrapAssetUrl = new URL(bootstrapUrl);
  const bootstrapAsset = await fetchPluginTextAsset(
    bootstrapAssetUrl,
    "script",
    (candidate) => isSameAsset(candidate, bootstrapAssetUrl),
  );
  reserveBytes(bootstrapAsset.bytes);

  const policy = [
    "default-src 'none'",
    `script-src 'nonce-${nonce}'`,
    "style-src 'unsafe-inline'",
    "font-src data:",
    `img-src data: blob: ${origin}`,
    `media-src data: blob: ${origin}`,
    "connect-src 'none'",
    "object-src 'none'",
    "frame-src 'none'",
    "worker-src 'none'",
    "form-action 'none'",
    `base-uri ${origin}`,
  ].join("; ");

  const base = document.createElement("base");
  base.href = new URL(".", href).toString();
  const csp = document.createElement("meta");
  csp.httpEquiv = "Content-Security-Policy";
  csp.content = policy;
  const bootstrapData = document.createElement("meta");
  bootstrapData.name = "ting-plugin-bootstrap";
  bootstrapData.content = encodeURIComponent(
    JSON.stringify({ bridgeToken: token, theme, assetUrl: href }),
  );
  const bootstrap = document.createElement("script");
  bootstrap.dataset.tingCspNonce = token;
  bootstrap.textContent = escapeInlineScript(bootstrapAsset.source);
  document.querySelectorAll<HTMLScriptElement>("script:not([src])").forEach(
    (script) => {
      script.removeAttribute("nonce");
      script.dataset.tingCspNonce = token;
    },
  );
  document.head.prepend(bootstrap);
  document.head.prepend(bootstrapData);
  document.head.prepend(base);
  document.head.prepend(csp);
  const nonceMarker = ` data-ting-csp-nonce="${token}"`;
  const serialized = document.documentElement.outerHTML
    .split(nonceMarker)
    .join(` nonce="${nonce}"`);
  return `<!doctype html>\n${serialized}`;
};
