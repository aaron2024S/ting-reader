import type {
  PluginCapability,
  PluginCapabilityRegistration,
  ToolProviderRegistration,
} from "../types";
import apiClient from "./client";

type JsonRecord = Record<string, unknown>;

const isJsonRecord = (value: unknown): value is JsonRecord =>
  value !== null && typeof value === "object" && !Array.isArray(value);

const normalizeRegistrationList = (
  payload: unknown,
): PluginCapabilityRegistration[] => {
  if (!Array.isArray(payload)) throw new Error("Invalid capability registration list");
  return payload.map((value): PluginCapabilityRegistration => {
    if (!isJsonRecord(value) || !isJsonRecord(value.capability) ||
        typeof value.plugin_id !== "string" || typeof value.plugin_name !== "string" ||
        typeof value.capability.id !== "string" || typeof value.capability.kind !== "string") {
      throw new Error("Invalid capability registration");
    }
    return {
      plugin_id: value.plugin_id,
      plugin_name: value.plugin_name,
      admin_only: value.admin_only === true,
      client_grant: typeof value.client_grant === "string" ? value.client_grant : undefined,
      capability: value.capability as PluginCapability,
    };
  });
};

export type PluginCapabilityInvokeResult<T = unknown> = {
  result: T;
};

export type SignPluginRouteRequest = {
  method: string;
  path: string;
  expires_in_seconds?: number;
  bind_current_user?: boolean;
};

export type SignPluginRouteResponse = {
  path: string;
  expires: number;
  signature: string;
  user_id?: string | null;
  signed_url: string;
};

export type InvokePluginHostRequest = {
  plugin_id: string;
  ui_capability_id: string;
  ui_grant: string;
  method: string;
  params?: unknown;
};

export type InvokePluginHostResponse<T = unknown> = {
  result: T;
};

export const listPluginCapabilities = async (kind?: string) => {
  const response = await apiClient.get<unknown>(
    "/api/v1/plugin-capabilities",
    {
      params: kind ? { kind } : undefined,
    },
  );
  return normalizeRegistrationList(response.data);
};

export const findContentProcessors = async (
  extension: string,
  operation?: string,
) => {
  const response = await apiClient.get<PluginCapabilityRegistration[]>(
    "/api/v1/plugin-capabilities/content-processors",
    {
      params: { extension, operation },
    },
  );
  return response.data;
};

export const findToolProviders = async (name?: string) => {
  const response = await apiClient.get<ToolProviderRegistration[]>(
    "/api/v1/plugin-capabilities/tools",
    {
      params: name ? { name } : undefined,
    },
  );
  return response.data;
};

export const findTaskHandlers = async (taskType?: string) => {
  const response = await apiClient.get<PluginCapabilityRegistration[]>(
    "/api/v1/plugin-capabilities/task-handlers",
    {
      params: taskType ? { task_type: taskType } : undefined,
    },
  );
  return response.data;
};

export const findEventHandlers = async (event?: string) => {
  const response = await apiClient.get<PluginCapabilityRegistration[]>(
    "/api/v1/plugin-capabilities/event-handlers",
    {
      params: event ? { event } : undefined,
    },
  );
  return response.data;
};

export const invokePluginCapability = async <T = unknown>(
  pluginId: string,
  capabilityId: string,
  params: unknown = {},
  uiCapabilityId?: string,
  uiGrant?: string,
) => {
  const response = await apiClient.post<PluginCapabilityInvokeResult<T>>(
    `/api/v1/plugins/${encodeURIComponent(pluginId)}/capabilities/${encodeURIComponent(capabilityId)}/invoke`,
    { params, ui_capability_id: uiCapabilityId, ui_grant: uiGrant },
  );
  return response.data.result;
};

export const signPluginRoute = async (request: SignPluginRouteRequest) => {
  const response = await apiClient.post<SignPluginRouteResponse>(
    "/api/v1/plugin-route-signatures",
    request,
  );
  return response.data;
};

export const invokePluginHost = async <T = unknown>(
  request: InvokePluginHostRequest,
) => {
  const response = await apiClient.post<InvokePluginHostResponse<T>>(
    "/api/v1/plugin-host/invoke",
    request,
  );
  return response.data.result;
};
