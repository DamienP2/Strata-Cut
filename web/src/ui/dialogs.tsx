/*
 * Drawn dialogs instead of confirm() / prompt() / alert(): a queue, one dialog
 * at a time, shown by the single <DialogHost /> mounted in App.
 *
 *   if (!(await confirmDialog({ title, message, confirm, danger: true }))) return;
 *   const name = await promptDialog({ title, label, value });   // null if cancelled or empty
 *   await alertDialog({ title, message });
 */
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { create } from 'zustand';
import { AlertTriangle, Info, X } from 'lucide-react';
import { t } from '../i18n';

export interface ConfirmOptions {
  title: string;
  message?: ReactNode;
  confirm?: string;
  cancel?: string;
  danger?: boolean;
}

export interface PromptOptions {
  title: string;
  message?: ReactNode;
  label?: string;
  value?: string;
  placeholder?: string;
  confirm?: string;
  password?: boolean;
}

export interface AlertOptions {
  title: string;
  message?: ReactNode;
}

type Request =
  | { id: number; kind: 'confirm'; opts: ConfirmOptions; resolve: (v: boolean) => void }
  | { id: number; kind: 'prompt'; opts: PromptOptions; resolve: (v: string | null) => void }
  | { id: number; kind: 'alert'; opts: AlertOptions; resolve: (v: void) => void };

const useDialogs = create<{ queue: Request[] }>(() => ({ queue: [] }));
let nextId = 0;

function push(r: Request) {
  useDialogs.setState((s) => ({ queue: [...s.queue, r] }));
}

export function confirmDialog(opts: ConfirmOptions): Promise<boolean> {
  return new Promise((resolve) => push({ id: ++nextId, kind: 'confirm', opts, resolve }));
}

export function promptDialog(opts: PromptOptions): Promise<string | null> {
  return new Promise((resolve) => push({ id: ++nextId, kind: 'prompt', opts, resolve }));
}

export function alertDialog(opts: AlertOptions): Promise<void> {
  return new Promise((resolve) => push({ id: ++nextId, kind: 'alert', opts, resolve }));
}

export function DialogHost() {
  const current = useDialogs((s) => s.queue[0]);
  if (!current) return null;
  return <Dialog key={current.id} req={current} />;
}

function Dialog({ req }: { req: Request }) {
  const [value, setValue] = useState(req.kind === 'prompt' ? (req.opts.value ?? '') : '');
  const done = useRef(false);
  const input = useRef<HTMLInputElement>(null);
  const ok = useRef<HTMLButtonElement>(null);
  const danger = req.kind === 'confirm' && !!req.opts.danger;

  useEffect(() => {
    if (input.current) {
      input.current.focus();
      input.current.select();
    } else ok.current?.focus();
  }, []);

  const close = (yes: boolean) => {
    if (done.current) return;
    done.current = true;
    useDialogs.setState((s) => ({ queue: s.queue.filter((r) => r.id !== req.id) }));
    if (req.kind === 'confirm') req.resolve(yes);
    else if (req.kind === 'prompt') req.resolve(yes && value.trim() ? value.trim() : null);
    else req.resolve();
  };

  const { title, message } = req.opts;
  return (
    <div className="modal-back dialog-back" onPointerDown={() => close(false)}>
      <form
        className={`modal dialog ${danger ? 'danger' : ''}`}
        role="dialog"
        aria-labelledby="dialog-title"
        onPointerDown={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault();
          close(true);
        }}
      >
        <header>
          <span className="dialog-icon">{danger ? <AlertTriangle size={16} /> : <Info size={16} />}</span>
          <h2 id="dialog-title">{title}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={() => close(false)}>
            <X size={16} />
          </button>
        </header>
        {message && <div className="dialog-message">{message}</div>}
        {req.kind === 'prompt' && (
          <label className="set-field">
            {req.opts.label && <span className="set-label">{req.opts.label}</span>}
            <input
              ref={input}
              type={req.opts.password ? 'password' : 'text'}
              value={value}
              placeholder={req.opts.placeholder}
              onChange={(e) => setValue(e.target.value)}
              onKeyDown={(e) => e.key !== 'Escape' && e.stopPropagation()}
            />
          </label>
        )}
        <footer>
          {req.kind !== 'alert' && (
            <button type="button" className="btn" onClick={() => close(false)}>
              {(req.kind === 'confirm' && req.opts.cancel) || t('Cancel')}
            </button>
          )}
          <button ref={ok} type="submit" className={`btn ${danger ? 'danger-solid' : 'primary'}`}>
            {req.kind === 'alert' ? t('OK') : req.opts.confirm || t('Confirm')}
          </button>
        </footer>
      </form>
    </div>
  );
}
