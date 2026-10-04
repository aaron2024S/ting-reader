import React, { useState } from 'react';
import { ChevronDown, Filter } from 'lucide-react';
import { useTranslation } from 'react-i18next';

export type DisplaySettingsOption = {
  value: string;
  label: string;
};

export type DisplaySettingsSection = {
  title: string;
  value: string;
  options: DisplaySettingsOption[];
  onChange: (value: string) => void;
};

type Props = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  sections: DisplaySettingsSection[];
  className?: string;
  buttonLabel?: string;
  sheetLabel?: string;
};

const DisplaySettingsMenu: React.FC<Props> = ({
  open,
  onOpenChange,
  sections,
  className = '',
  buttonLabel,
  sheetLabel,
}) => {
  const { t } = useTranslation();
  const resolvedButtonLabel = buttonLabel ?? t('displaySettings.buttonLabel');
  const resolvedSheetLabel = sheetLabel ?? t('displaySettings.sheetLabel');
  const [expandedSection, setExpandedSection] = useState<string | null>(null);
  const [previousOpen, setPreviousOpen] = useState(open);
  if (previousOpen !== open) {
    setPreviousOpen(open);
    setExpandedSection(null);
  }

  const handleSelect = (section: DisplaySettingsSection, value: string) => {
    section.onChange(value);
    onOpenChange(false);
  };

  const renderMenuContent = () => (
    <>
      {sections.map((section, index) => (
        <div key={section.title}>
          <button
            type="button"
            aria-expanded={expandedSection === section.title}
            onClick={() => setExpandedSection(current => current === section.title ? null : section.title)}
            className={`flex w-full items-center justify-between gap-3 px-4 py-3 text-left text-sm font-bold text-slate-700 dark:text-slate-200 ${index > 0 ? 'border-t border-slate-100 dark:border-slate-800' : ''}`}
          >
            <span>{section.title}</span>
            <ChevronDown size={16} className={`shrink-0 transition-transform ${expandedSection === section.title ? 'rotate-180' : ''}`} />
          </button>
          {expandedSection === section.title && section.options.map(option => {
            const selected = section.value === option.value;
            return (
              <button
                key={option.value}
                type="button"
                onClick={() => handleSelect(section, option.value)}
                className={`w-full px-4 py-2.5 text-left text-sm flex items-center justify-between ${selected ? 'text-primary-600 font-bold bg-primary-50/50 dark:bg-primary-900/20' : 'text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800'}`}
              >
                <span>{option.label}</span>
                {selected && (
                  <span className="w-1.5 h-1.5 rounded-full bg-primary-600" />
                )}
              </button>
            );
          })}
        </div>
      ))}
    </>
  );

  return (
    <div className={`relative min-w-0 ${className}`}>
      <button
        type="button"
        aria-label={resolvedButtonLabel}
        onClick={() => onOpenChange(!open)}
        className={`p-2.5 bg-white dark:bg-slate-900 border border-slate-200 dark:border-slate-800 rounded-xl text-slate-600 dark:text-slate-400 hover:bg-slate-50 dark:hover:bg-slate-800 transition-colors ${open ? 'ring-2 ring-primary-500' : ''}`}
      >
        <Filter size={20} />
      </button>

      {open && (
        <div
          className="absolute right-0 top-full mt-2 w-56 max-w-[calc(100vw-2rem)] max-h-[65dvh] overflow-y-auto bg-white dark:bg-slate-900 border border-slate-100 dark:border-slate-800 rounded-2xl shadow-xl z-50 py-2 animate-in zoom-in-95 duration-200"
          aria-label={resolvedSheetLabel}
        >
          {renderMenuContent()}
        </div>
      )}
    </div>
  );
};

export default DisplaySettingsMenu;
