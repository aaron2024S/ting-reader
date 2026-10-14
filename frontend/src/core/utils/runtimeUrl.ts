const gatewayPathPattern = /^\/app\/[^/]+(?=\/|$)/;

export const getGatewayBasePath = () => {
  if (typeof window === 'undefined') return '';
  return window.location.pathname.match(gatewayPathPattern)?.[0] || '';
};

export const getRuntimeBaseUrl = (activeUrl?: string | null) => {
  const gatewayBasePath = getGatewayBasePath();
  if (gatewayBasePath && typeof window !== 'undefined') {
    return `${window.location.origin}${gatewayBasePath}`;
  }

  if (activeUrl) return activeUrl.replace(/\/$/, '');

  return import.meta.env?.VITE_API_BASE_URL || (import.meta.env?.PROD ? '' : 'http://localhost:3000');
};

export const getRuntimeUrl = (path: string, activeUrl?: string | null) => {
  if (/^https?:\/\//i.test(path)) return path;
  const baseUrl = getRuntimeBaseUrl(activeUrl);
  const normalizedPath = path.startsWith('/') ? path : `/${path}`;
  return `${baseUrl}${normalizedPath}`;
};

export const getRuntimeAssetUrl = (path: string) => {
  if (/^(?:https?:|data:|blob:)/i.test(path)) return path;

  const normalizedPath = path.startsWith('/') ? path : `/${path}`;
  const gatewayBasePath = getGatewayBasePath();
  if (gatewayBasePath) return `${gatewayBasePath}${normalizedPath}`;

  const configuredBase = import.meta.env?.BASE_URL || '/';
  if (/^https?:\/\//i.test(configuredBase)) {
    return new URL(normalizedPath.slice(1), configuredBase).toString();
  }
  if (configuredBase.startsWith('/')) {
    return `${configuredBase.replace(/\/$/, '')}${normalizedPath}`;
  }

  return normalizedPath;
};

export const getRuntimePath = (path: string) => {
  const gatewayBasePath = getGatewayBasePath();
  const normalizedPath = path.startsWith('/') ? path : `/${path}`;
  return `${gatewayBasePath}${normalizedPath}`;
};

export const getRuntimePathname = () => {
  if (typeof window === 'undefined') return '/';
  const gatewayBasePath = getGatewayBasePath();
  if (gatewayBasePath && window.location.pathname.startsWith(gatewayBasePath)) {
    return window.location.pathname.slice(gatewayBasePath.length) || '/';
  }
  return window.location.pathname;
};
