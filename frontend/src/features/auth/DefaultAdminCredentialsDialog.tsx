import { useEffect, useId, useRef } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import { ShieldAlert } from 'lucide-react';

interface Props {
  onLater: () => void;
  onChangeCredentials: () => void;
}

export default function DefaultAdminCredentialsDialog({ onLater, onChangeCredentials }: Props) {
  const { t } = useTranslation();
  const titleId = useId();
  const messageId = useId();
  const dialog = useRef<HTMLDivElement>(null);
  const confirmButton = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const previousFocus = document.activeElement;
    confirmButton.current?.focus();
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        onLater();
      } else if (event.key === 'Tab') {
        const buttons = dialog.current?.querySelectorAll<HTMLButtonElement>('button');
        if (!buttons?.length) return;
        const first = buttons[0];
        const last = buttons[buttons.length - 1];
        if (event.shiftKey && document.activeElement === first) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault();
          first.focus();
        }
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected)
        previousFocus.focus();
    };
  }, [onLater]);

  return createPortal(
    <div className="fixed inset-0 z-[2000] flex items-center justify-center bg-black/50 backdrop-blur-sm p-4">
      <div ref={dialog} role="dialog" aria-modal="true" aria-labelledby={titleId} aria-describedby={messageId}
        className="w-full max-w-md rounded-3xl border border-slate-100 bg-white p-6 shadow-2xl dark:border-slate-800 dark:bg-slate-900">
        <div className="mb-4 inline-flex rounded-2xl bg-amber-50 p-3 text-amber-600 dark:bg-amber-900/20 dark:text-amber-400">
          <ShieldAlert size={26} aria-hidden="true" />
        </div>
        <h2 id={titleId} className="text-xl font-bold text-slate-900 dark:text-white">{t('auth.defaultCredentialsTitle')}</h2>
        <p id={messageId} className="mt-3 text-sm leading-6 text-slate-600 dark:text-slate-400">{t('auth.defaultCredentialsMessage')}</p>
        <div className="mt-6 flex justify-end gap-3">
          <button type="button" onClick={onLater}
            className="rounded-xl px-4 py-2.5 text-sm font-medium text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800">
            {t('auth.defaultCredentialsLater')}
          </button>
          <button ref={confirmButton} type="button" onClick={onChangeCredentials}
            className="rounded-xl bg-primary-600 px-4 py-2.5 text-sm font-semibold text-white hover:bg-primary-700">
            {t('auth.defaultCredentialsChange')}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
