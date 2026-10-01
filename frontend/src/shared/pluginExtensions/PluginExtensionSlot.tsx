import { Loader2, MoreHorizontal, X } from "lucide-react";
import type { ReactNode } from "react";
import { useState } from "react";
import { invokePluginCapability } from "../../core/api/pluginCapabilities";
import { useClientExtensions } from "../../core/hooks/useClientExtensions";
import type {
  ClientExtensionDescriptor,
  ClientExtensionSlot,
} from "../../core/pluginExtensions";
import PluginExtensionIcon from "./PluginExtensionIcon";
import PluginWebContainer from "./PluginWebContainer";

type PluginExtensionSlotProps = {
  slot: ClientExtensionSlot;
  context?: Record<string, unknown>;
  className?: string;
  buttonClassName?: string;
  showLabel?: boolean;
  menuLabel?: string;
  menuLabelClassName?: string;
  menuClassName?: string;
  limit?: number;
  empty?: ReactNode;
};

const extensionLabel = (extension: ClientExtensionDescriptor) =>
  extension.title || extension.pluginName || extension.capability.id;

const defaultButtonClassName =
  "inline-flex h-9 w-9 items-center justify-center rounded-md text-slate-500 transition-colors hover:bg-slate-100 hover:text-slate-900 dark:text-slate-300 dark:hover:bg-slate-800 dark:hover:text-white";

const PluginExtensionSlot = ({
  slot,
  context,
  className = "flex items-center gap-1",
  buttonClassName = defaultButtonClassName,
  showLabel = false,
  menuLabel,
  menuLabelClassName = "truncate",
  menuClassName = "absolute right-0 top-full z-30 mt-2 min-w-44 overflow-hidden rounded-lg border border-slate-200 bg-white p-1 shadow-xl shadow-slate-900/10 dark:border-slate-700 dark:bg-slate-900 dark:shadow-slate-950/30",
  limit,
  empty = null,
}: PluginExtensionSlotProps) => {
  const { registry } = useClientExtensions();
  const extensions = registry.bySlot[slot] || [];
  const visibleExtensions =
    typeof limit === "number" ? extensions.slice(0, limit) : extensions;
  const [activeExtension, setActiveExtension] =
    useState<ClientExtensionDescriptor | null>(null);
  const [actionState, setActionState] = useState<
    "idle" | "running" | "success" | "error"
  >("idle");
  const [actionMessage, setActionMessage] = useState<string>();
  const [menuOpen, setMenuOpen] = useState(false);
  const visibleActiveExtension = activeExtension
    ? visibleExtensions.find((extension) => extension.id === activeExtension.id) ||
      null
    : null;

  if (visibleExtensions.length === 0) {
    return <>{empty}</>;
  }

  const closePanel = () => {
    setActiveExtension(null);
    setActionState("idle");
    setActionMessage(undefined);
  };

  const invokeAction = async (extension: ClientExtensionDescriptor) => {
    setActiveExtension(extension);
    setActionState("running");
    setActionMessage(undefined);
    try {
      const result = await invokePluginCapability(
        extension.pluginId,
        extension.capability.id,
        {
          slot: extension.slot,
          contexts: extension.contexts,
          context: context || {},
        },
        extension.capability.id,
        extension.clientGrant,
      );
      setActionState("success");
      setActionMessage(
        typeof result === "string"
          ? result
          : JSON.stringify(result ?? { ok: true }, null, 2),
      );
    } catch (err) {
      setActionState("error");
      setActionMessage(err instanceof Error ? err.message : String(err));
    }
  };

  const openExtension = (extension: ClientExtensionDescriptor) => {
    setMenuOpen(false);
    if (
      extension.renderMode === "web_container"
    ) {
      setActiveExtension(extension);
      setActionState("idle");
      setActionMessage(undefined);
      return;
    }

    void invokeAction(extension);
  };

  return (
    <>
      <div className={className}>
        {menuLabel ? (
          <>
            <button
              type="button"
              onClick={() => setMenuOpen((open) => !open)}
              className={buttonClassName}
              title={menuLabel}
              aria-label={menuLabel}
              aria-haspopup="menu"
              aria-expanded={menuOpen}
            >
              <MoreHorizontal size={18} />
              <span className={menuLabelClassName}>{menuLabel}</span>
            </button>
            {menuOpen ? (
              <div className={menuClassName} role="menu">
                {visibleExtensions.map((extension) => (
                  <button
                    key={extension.id}
                    type="button"
                    onClick={() => openExtension(extension)}
                    className="flex w-full items-center gap-2 rounded-md px-3 py-2 text-left text-sm font-semibold text-slate-600 transition-colors hover:bg-slate-100 hover:text-primary-700 dark:text-slate-200 dark:hover:bg-slate-800 dark:hover:text-primary-300"
                    title={extensionLabel(extension)}
                    role="menuitem"
                  >
                    <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-slate-100 text-slate-500 dark:bg-slate-800 dark:text-slate-300">
                      <PluginExtensionIcon extension={extension} size={16} />
                    </span>
                    <span className="min-w-0 flex-1 truncate">
                      {extensionLabel(extension)}
                    </span>
                  </button>
                ))}
              </div>
            ) : null}
          </>
        ) : visibleExtensions.map((extension) => (
          <button
            key={extension.id}
            type="button"
            onClick={() => openExtension(extension)}
            className={buttonClassName}
            title={extensionLabel(extension)}
          >
            <PluginExtensionIcon extension={extension} size={17} />
            {showLabel ? (
              <span className="truncate">{extensionLabel(extension)}</span>
            ) : null}
          </button>
        ))}
      </div>

      {visibleActiveExtension ? (
        <div className="fixed inset-0 z-[125] flex items-end justify-end bg-slate-950/30 p-3 backdrop-blur-sm sm:p-6">
          <section className="flex h-[min(38rem,86vh)] w-full max-w-md flex-col overflow-hidden rounded-lg border border-slate-200 bg-white shadow-2xl dark:border-slate-700 dark:bg-slate-900">
            <header className="flex h-14 shrink-0 items-center gap-3 border-b border-slate-200 px-4 dark:border-slate-800">
              <div className="flex h-8 w-8 items-center justify-center rounded-md bg-primary-50 text-primary-700 dark:bg-primary-950/40 dark:text-primary-300">
                <PluginExtensionIcon extension={visibleActiveExtension} size={17} />
              </div>
              <div className="min-w-0 flex-1">
                <h2 className="truncate text-sm font-semibold text-slate-950 dark:text-white">
                  {extensionLabel(visibleActiveExtension)}
                </h2>
                <p className="truncate text-xs text-slate-500 dark:text-slate-400">
                  {visibleActiveExtension.pluginName}
                </p>
              </div>
              <button
                type="button"
                onClick={closePanel}
                className="flex h-9 w-9 items-center justify-center rounded-md text-slate-500 transition-colors hover:bg-slate-100 hover:text-slate-900 dark:hover:bg-slate-800 dark:hover:text-white"
                title="Close"
              >
                <X size={18} />
              </button>
            </header>
            <div
              className={`flex flex-1 flex-col text-sm leading-6 text-slate-500 dark:text-slate-400 ${
                visibleActiveExtension.renderMode === "web_container"
                  ? "min-h-0"
                  : "justify-center gap-4 px-6"
              }`}
            >
              {visibleActiveExtension.renderMode === "web_container" ? (
                <PluginWebContainer
                  extension={visibleActiveExtension}
                  context={context}
                />
              ) : actionState === "running" ? (
                <div className="flex items-center justify-center gap-2 text-slate-500 dark:text-slate-400">
                  <Loader2 size={16} className="animate-spin" />
                  <span>Running...</span>
                </div>
              ) : actionMessage ? (
                <pre
                  className={`mx-6 max-h-72 overflow-auto rounded-md border px-3 py-2 text-left text-xs ${
                    actionState === "error"
                      ? "border-red-200 bg-red-50 text-red-700 dark:border-red-900/40 dark:bg-red-950/30 dark:text-red-300"
                      : "border-slate-200 bg-slate-50 text-slate-700 dark:border-slate-800 dark:bg-slate-950 dark:text-slate-300"
                  }`}
                >
                  {actionMessage}
                </pre>
              ) : null}
            </div>
          </section>
        </div>
      ) : null}
    </>
  );
};

const PluginActionView = ({
  extension,
  context,
}: {
  extension: ClientExtensionDescriptor;
  context?: Record<string, unknown>;
}) => {
  const [running, setRunning] = useState(false);
  const [message, setMessage] = useState<string>();
  const [failed, setFailed] = useState(false);

  const run = async () => {
    setRunning(true);
    setMessage(undefined);
    setFailed(false);
    try {
      const result = await invokePluginCapability(
        extension.pluginId,
        extension.capability.id,
        {
          slot: extension.slot,
          contexts: extension.contexts,
          context: context || {},
        },
        extension.capability.id,
        extension.clientGrant,
      );
      setMessage(
        typeof result === "string"
          ? result
          : JSON.stringify(result ?? { ok: true }, null, 2),
      );
    } catch (error) {
      setFailed(true);
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setRunning(false);
    }
  };

  return (
    <div className="flex flex-1 flex-col gap-4 p-5">
      <button
        type="button"
        onClick={() => void run()}
        disabled={running}
        className="inline-flex h-10 items-center justify-center gap-2 rounded-md bg-primary-600 px-4 text-sm font-semibold text-white transition-colors hover:bg-primary-700 disabled:cursor-not-allowed disabled:opacity-60"
      >
        {running ? <Loader2 size={16} className="animate-spin" /> : null}
        {running ? "Running..." : "Run"}
      </button>
      {message ? (
        <pre
          className={`min-h-0 flex-1 overflow-auto rounded-md border px-3 py-2 text-xs ${
            failed
              ? "border-red-200 bg-red-50 text-red-700 dark:border-red-900/40 dark:bg-red-950/30 dark:text-red-300"
              : "border-slate-200 bg-slate-50 text-slate-700 dark:border-slate-800 dark:bg-slate-950 dark:text-slate-300"
          }`}
        >
          {message}
        </pre>
      ) : null}
    </div>
  );
};

export const PluginExtensionContent = ({
  extension,
  context,
}: {
  extension: ClientExtensionDescriptor;
  context?: Record<string, unknown>;
}) => {
  if (extension.renderMode === "web_container") {
    return <PluginWebContainer extension={extension} context={context} />;
  }
  return <PluginActionView extension={extension} context={context} />;
};

export default PluginExtensionSlot;
