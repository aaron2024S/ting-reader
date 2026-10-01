import type {
  CapabilityRegistrationLike,
  ClientExtensionDescriptor,
  ClientExtensionRegistrySnapshot,
  ClientExtensionSlot,
  UiExtensionCapabilityExtra,
  UiExtensionRenderConfig,
} from "./types";

const isJsonRecord = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);

const isCapabilityRegistration = (
  value: unknown,
): value is CapabilityRegistrationLike => {
  if (!isJsonRecord(value)) return false;
  if (typeof value.plugin_id !== "string" || typeof value.plugin_name !== "string") return false;
  const capability = value.capability;
  return isJsonRecord(capability) && typeof capability.id === "string" && capability.kind === "ui_extension";
};

const isClientExtensionSlot = (value: unknown): value is ClientExtensionSlot =>
  typeof value === "string" &&
  ["app.sidebar_page", "global.floating_action", "global.panel", "book.detail_action"].includes(value);

const capabilityExtra = (registration: CapabilityRegistrationLike): UiExtensionCapabilityExtra =>
  registration.capability as UiExtensionCapabilityExtra;

const renderConfig = (extra: UiExtensionCapabilityExtra): UiExtensionRenderConfig | undefined => extra.render;

const normalizeSlots = (extra: UiExtensionCapabilityExtra) => extra.slots.filter(isClientExtensionSlot);
const normalizeContexts = (extra: UiExtensionCapabilityExtra) => extra.contexts;

const localizedText = (
  value: unknown,
  locale?: string,
): string | undefined => {
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed || undefined;
  }
  if (!value || typeof value !== "object") return undefined;

  const record = value as Record<string, unknown>;
  const normalizedLocale = locale?.replace("_", "-");
  const language = normalizedLocale?.split("-")[0];
  const candidates = [
    normalizedLocale ? record[normalizedLocale] : undefined,
    language ? record[language] : undefined,
    record["zh-CN"],
    record.zh,
    record["en-US"],
    record.en,
    ...Object.values(record),
  ];
  for (const candidate of candidates) {
    if (typeof candidate === "string" && candidate.trim()) {
      return candidate.trim();
    }
  }
  return undefined;
};

export const createClientExtensionDescriptor = (
  registration: CapabilityRegistrationLike,
  slot: ClientExtensionSlot,
  locale?: string,
): ClientExtensionDescriptor => {
  const extra = capabilityExtra(registration);
  const render = renderConfig(extra);
  const renderMode = render?.mode ?? "action";

  return {
    id: `${registration.plugin_id}:${registration.capability.id}:${slot}`,
    pluginId: registration.plugin_id,
    pluginName: registration.plugin_name,
    clientGrant: registration.client_grant,
    slot,
    renderMode,
    render,
    title:
      localizedText(extra.title, locale),
    icon: extra.icon,
    capability: registration.capability,
    priority: typeof extra.priority === "number" ? extra.priority : 100,
    contexts: normalizeContexts(extra),
  };
};

export const buildClientExtensionRegistry = (
  registrations: CapabilityRegistrationLike[],
  locale?: string,
): ClientExtensionRegistrySnapshot => {
  const extensions = registrations
    .filter(isCapabilityRegistration)
    .filter(
      (registration) =>
        registration.capability.kind === "ui_extension",
    )
    .flatMap((registration) =>
      normalizeSlots(capabilityExtra(registration)).map((slot) =>
        createClientExtensionDescriptor(registration, slot, locale),
      ),
    )
    .sort(
      (left, right) =>
        left.priority - right.priority || left.id.localeCompare(right.id),
    );

  const bySlot: ClientExtensionRegistrySnapshot["bySlot"] = {};
  for (const extension of extensions) {
    bySlot[extension.slot] = [...(bySlot[extension.slot] || []), extension];
  }

  return { extensions, bySlot };
};
