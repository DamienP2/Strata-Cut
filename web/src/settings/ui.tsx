/* Settings cards, fields, and the save bar. */
import { useEffect, type ReactNode } from 'react';
import { Save } from 'lucide-react';
import { t } from '../i18n';
import { setLeaveGuard } from '../router';
import { confirmDialog } from '../ui/dialogs';

export function Card({ icon, title, children, actions, className }: { icon?: ReactNode; title: ReactNode; children?: ReactNode; actions?: ReactNode; className?: string }) {
  return (
    <section className={`set-card ${className ?? ''}`}>
      <header>
        {icon /* size 16 */}
        <h2>{title}</h2>
        <div className="spacer" />
        {actions}
      </header>
      {children ? <div className="set-body">{children}</div> : null}
    </section>
  );
}

/**
 * A labelled field. `group`: a <div> instead of a <label> (several buttons
 * inside, such as a Segmented, or a switch). `hint`: a line under it.
 */
export function Field({ label, children, hint, group, className }: { label: ReactNode; children: ReactNode; hint?: ReactNode; group?: boolean; className?: string }) {
  const body = (
    <>
      <span className="set-label">{label}</span>
      {children}
      {hint && <span className="set-hint">{hint}</span>}
    </>
  );
  return group ? <div className={`set-field ${className ?? ''}`}>{body}</div> : <label className={`set-field ${className ?? ''}`}>{body}</label>;
}

/** An on / off switch (`.check`). */
export function Switch({ checked, onChange, label, disabled, title }: { checked: boolean; onChange: (v: boolean) => void; label?: ReactNode; disabled?: boolean; title?: string }) {
  return (
    <label className="check" title={title}>
      <input type="checkbox" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
      {label}
    </label>
  );
}

/** Asks before leaving the page while `dirty`. */
export function useLeaveGuard(dirty: boolean) {
  useEffect(() => {
    if (!dirty) return;
    setLeaveGuard(
      () => true,
      () => confirmDialog({ title: t('Discard the changes?'), confirm: t('Discard'), danger: true }),
    );
    return () => setLeaveGuard(null);
  }, [dirty]);
}

/** Rises from the bottom while the local copy differs from the saved settings. */
export function SaveBar({ dirty, saving, onSave, onDiscard }: { dirty: boolean; saving: boolean; onSave: () => void; onDiscard: () => void }) {
  useLeaveGuard(dirty);
  return (
    <div className={`save-bar ${dirty ? 'show' : ''}`} aria-hidden={!dirty}>
      <span>{t('Some changes are not saved.')}</span>
      <div className="spacer" />
      <button type="button" className="btn ghost" disabled={saving} onClick={onDiscard} tabIndex={dirty ? 0 : -1}>
        {t('Discard')}
      </button>
      <button type="button" className="btn primary" disabled={saving} onClick={onSave} tabIndex={dirty ? 0 : -1}>
        <Save size={14} /> {saving ? t('Applying…') : t('Save and apply')}
      </button>
    </div>
  );
}
