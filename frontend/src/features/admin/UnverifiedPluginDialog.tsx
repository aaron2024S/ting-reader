import { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import type { PluginPermission, UnverifiedPluginInstallConfirmation } from '../../core/types';
import { capabilityLabels, getPluginCapabilityKinds, getPluginPermissionLabels } from './PluginCard';

interface Props {
  confirmation: UnverifiedPluginInstallConfirmation;
  onDecision: (accepted: boolean) => void;
}

function isSensitive(permission: PluginPermission): boolean {
  return (permission.type === 'network_access' && permission.domain === '*')
    || ['file_write', 'books_write', 'chapters_write', 'libraries_write',
      'metadata_write', 'task_manage', 'capability_invoke'].includes(permission.type);
}

export default function UnverifiedPluginDialog({ confirmation, onDecision }: Props) {
  const { t } = useTranslation();
  const dialog = useRef<HTMLDivElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const permissions = confirmation.permissions;
  const labels = getPluginPermissionLabels(permissions || [], t);
  const kinds = getPluginCapabilityKinds(confirmation.capabilities);

  useEffect(() => {
    const previousFocus = document.activeElement as HTMLElement | null;
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    cancel.current?.focus();
    return () => {
      document.body.style.overflow = previousOverflow;
      previousFocus?.focus();
    };
  }, []);

  return (
    <div className="fixed inset-0 z-[500] flex items-center justify-center bg-black/60 p-4"
      onMouseDown={(event) => { if (event.target === event.currentTarget) onDecision(false); }}>
      <div ref={dialog} role="dialog" aria-modal="true" aria-labelledby="unverified-plugin-title"
        aria-describedby="unverified-plugin-warning"
        className="flex max-h-[85dvh] w-full max-w-xl flex-col rounded-2xl bg-white shadow-xl dark:bg-slate-900"
        onKeyDown={(event) => {
          if (event.key === 'Escape') onDecision(false);
          if (event.key !== 'Tab') return;
          const buttons = dialog.current?.querySelectorAll<HTMLButtonElement>('button');
          if (!buttons?.length) return;
          const first = buttons[0];
          const last = buttons[buttons.length - 1];
          if (event.shiftKey && document.activeElement === first) {
            event.preventDefault(); last.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault(); first.focus();
          }
        }}>
        <div className="border-b border-slate-200 p-5 dark:border-slate-700">
          <h2 id="unverified-plugin-title" className="text-lg font-semibold">{t('adminPlugins.unverifiedTitle')}</h2>
          <p className="mt-1 break-words text-sm">{confirmation.plugin_name} · {confirmation.plugin_version}</p>
          <p className="mt-1 text-sm text-slate-500">
            {t('adminPlugins.unknownPublisher')} · {confirmation.runtime || t('adminPlugins.unknownType')}
          </p>
        </div>
        <div className="min-h-0 overflow-y-auto p-5 text-sm">
          <p id="unverified-plugin-warning" className="leading-relaxed">{t('adminPlugins.unverifiedWarning')}</p>
          {confirmation.package_changed && (
            <p role="alert" className="mt-3 rounded-lg bg-amber-50 p-3 text-amber-800 dark:bg-amber-950 dark:text-amber-200">
              {t('adminPlugins.packageChanged')}
            </p>
          )}
          {confirmation.runtime === 'native' && (
            <p className="mt-3 rounded-lg bg-red-50 p-3 text-red-700 dark:bg-red-950 dark:text-red-200">
              {t('adminPlugins.nativeWarning')}
            </p>
          )}
          <h3 className="mb-2 mt-5 font-semibold">{t('adminPlugins.requestedPermissions')}</h3>
          {Array.isArray(permissions) ? permissions.length === 0 ? (
            <p className="text-slate-500">{t('adminPlugins.noRequestedPermissions')}</p>
          ) : (
            <ul className="space-y-2">
              {permissions.map((permission, index) => (
                <li key={index} className={`break-words rounded-lg border p-3 ${isSensitive(permission)
                  ? 'border-amber-300 bg-amber-50 text-amber-900 dark:border-amber-800 dark:bg-amber-950 dark:text-amber-100'
                  : 'border-slate-200 dark:border-slate-700'}`}>
                  {labels[index]}
                  {isSensitive(permission) && <span className="ml-2 text-xs font-semibold">{t('adminPlugins.sensitivePermission')}</span>}
                </li>
              ))}
            </ul>
          ) : <p className="text-amber-700">{t('adminPlugins.permissionsUnavailable')}</p>}
          {kinds.length > 0 && (
            <>
              <h3 className="mb-2 mt-5 font-semibold">{t('adminPlugins.declaredCapabilities')}</h3>
              <p>{kinds.map((kind) => t(`adminPlugins.capabilityLabels.${capabilityLabels[kind]}`)).join(' · ')}</p>
            </>
          )}
        </div>
        <div className="flex shrink-0 justify-end gap-3 border-t border-slate-200 p-5 dark:border-slate-700">
          <button ref={cancel} type="button" onClick={() => onDecision(false)}
            className="rounded-lg border border-slate-300 px-4 py-2 dark:border-slate-600">{t('common.cancel')}</button>
          <button type="button" onClick={() => onDecision(true)}
            className="rounded-lg bg-primary-600 px-4 py-2 font-semibold text-white">{t('adminPlugins.agreeInstall')}</button>
        </div>
      </div>
    </div>
  );
}
