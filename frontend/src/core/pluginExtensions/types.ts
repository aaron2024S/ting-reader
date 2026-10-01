import type { PluginCapability, PluginCapabilityRegistration } from "../types";

export type ClientExtensionSlot =
  | "app.sidebar_page"
  | "global.floating_action"
  | "global.panel"
  | "book.detail_action";

export type ClientExtensionRenderMode =
  "web_container" | "action";

export type ClientExtensionIcon =
  | string
  | {
      type?: "lucide" | "emoji" | "image" | "url";
      name?: string;
      value?: string;
      src?: string;
      alt?: string;
    };

export type ClientExtensionDescriptor = {
  id: string;
  pluginId: string;
  pluginName: string;
  clientGrant?: string;
  slot: ClientExtensionSlot;
  renderMode: ClientExtensionRenderMode;
  render?: UiExtensionRenderConfig;
  title?: string;
  icon?: ClientExtensionIcon;
  capability: PluginCapability;
  priority: number;
  contexts: string[];
};

export type ClientExtensionRegistrySnapshot = {
  extensions: ClientExtensionDescriptor[];
  bySlot: Partial<Record<ClientExtensionSlot, ClientExtensionDescriptor[]>>;
};

export type UiExtensionCapabilityExtra = Extract<PluginCapability, { kind: "ui_extension" }>;

export type UiExtensionRenderConfig = UiExtensionCapabilityExtra["render"];

export type CapabilityRegistrationLike = Pick<
  PluginCapabilityRegistration,
  "plugin_id" | "plugin_name" | "client_grant" | "capability"
>;
