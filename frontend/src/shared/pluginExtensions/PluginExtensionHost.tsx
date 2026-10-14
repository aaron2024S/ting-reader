import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useClientExtensions } from "../../core/hooks/useClientExtensions";
import type { ClientExtensionDescriptor } from "../../core/pluginExtensions";
import { usePlayerStore } from "../../core/stores/playerStore";
import { useUiPreferencesStore } from "../../core/stores/uiPreferencesStore";
import PluginExtensionIcon from "./PluginExtensionIcon";
import { PluginExtensionContent, PluginExtensionPanel } from "./PluginExtensionSlot";

const extensionLabel = (extension: ClientExtensionDescriptor) =>
  extension.title || extension.pluginName || extension.capability.id;

const PluginLauncherIcon = () => (
  <span
    aria-hidden="true"
    className="grid h-6 w-6 grid-cols-2 gap-0.5"
  >
    <span className="rounded-[1px] bg-[#54cde3]" />
    <span className="rounded-[1px] bg-[#48bfdd]" />
    <span className="rounded-[1px] bg-[#32b4d4]" />
    <span className="rounded-[1px] bg-[#249ec8]" />
  </span>
);

const PluginExtensionHost = () => {
  const { t } = useTranslation();
  const { registry } = useClientExtensions();
  const enabled = useUiPreferencesStore(
    (state) => state.pluginToolMenuEnabled,
  );
  const hasCurrentChapter = usePlayerStore((state) => !!state.currentChapter);
  const primaryActions = useMemo(
    () => {
      const floatingActions = registry.bySlot["global.floating_action"] || [];
      const panels = registry.bySlot["global.panel"] || [];
      return Array.from(
        new Map(
          [...floatingActions, ...panels].map((extension) => [
            `${extension.pluginId}:${extension.capability.id}`,
            extension,
          ]),
        ).values(),
      ).sort(
        (left, right) =>
          left.priority - right.priority || left.id.localeCompare(right.id),
      );
    },
    [registry],
  );
  const [activeExtension, setActiveExtension] =
    useState<ClientExtensionDescriptor | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const visibleActiveExtension =
    activeExtension &&
    primaryActions.find(
      (extension) =>
        extension.pluginId === activeExtension.pluginId &&
        extension.capability.id === activeExtension.capability.id,
    )
      || null;

  if (!enabled || primaryActions.length === 0) {
    return null;
  }

  const openExtension = (extension: ClientExtensionDescriptor) => {
    setMenuOpen(false);
    setActiveExtension(extension);
  };

  return (
    <>
      <div
        className="fixed right-4 z-[90] flex flex-col items-center gap-2"
        style={{
          bottom: hasCurrentChapter
            ? "var(--safe-bottom-with-player)"
            : "var(--safe-bottom-base)",
        }}
      >
        {menuOpen ? (
          <div className="mb-1 flex max-h-[min(60vh,24rem)] flex-col items-center gap-2 overflow-y-auto">
            {primaryActions.map((extension) => (
              <button
                key={extension.id}
                type="button"
                onClick={() => openExtension(extension)}
                className="inline-flex h-10 w-10 shrink-0 items-center justify-center rounded-lg border border-slate-200/80 bg-white/95 text-slate-700 shadow-md shadow-slate-900/10 transition-colors hover:border-primary-200 hover:bg-primary-50 hover:text-primary-700 focus:outline-none focus:ring-2 focus:ring-cyan-300 focus:ring-offset-2 focus:ring-offset-white dark:border-slate-700/80 dark:bg-slate-900/95 dark:text-slate-100 dark:hover:border-primary-700 dark:hover:bg-primary-950/40 dark:hover:text-primary-300 dark:focus:ring-offset-slate-950"
                title={extensionLabel(extension)}
                aria-label={extensionLabel(extension)}
              >
                <PluginExtensionIcon extension={extension} size={18} />
              </button>
            ))}
          </div>
        ) : null}
        <button
          type="button"
          onClick={() => setMenuOpen((open) => !open)}
          className="inline-flex h-12 w-12 items-center justify-center rounded-xl border border-slate-200/80 bg-white/95 text-primary-600 shadow-lg shadow-slate-900/10 transition-colors hover:border-primary-200 hover:bg-primary-50 focus:outline-none focus:ring-2 focus:ring-cyan-300 focus:ring-offset-2 focus:ring-offset-white dark:border-slate-700/80 dark:bg-slate-900/95 dark:text-primary-300 dark:shadow-slate-950/25 dark:hover:bg-slate-800 dark:focus:ring-offset-slate-950"
          title={t("pluginExtensions.toolMenu")}
          aria-label={t("pluginExtensions.toolMenu")}
          aria-expanded={menuOpen}
        >
          <PluginLauncherIcon />
        </button>
      </div>

      {visibleActiveExtension ? (
        <PluginExtensionPanel extension={visibleActiveExtension} onClose={() => setActiveExtension(null)}>
          <PluginExtensionContent extension={visibleActiveExtension} />
        </PluginExtensionPanel>
      ) : null}
    </>
  );
};

export default PluginExtensionHost;
