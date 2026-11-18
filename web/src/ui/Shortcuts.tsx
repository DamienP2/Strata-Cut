/*
 * The keyboard shortcuts overlay: the `?` key or « Raccourcis clavier » in the
 * user menu. Label on the left, keys on the right; Escape closes.
 */
import { Fragment, useEffect } from 'react';
import { create } from 'zustand';
import { Keyboard, X } from 'lucide-react';
import { t, tc } from '../i18n';

export interface Shortcut {
  /** English label, translated where shown. */
  label: string;
  /** Each key: a letter, or Shift, Space, Left, Right, ?. */
  keys: string[];
}

export interface ShortcutGroup {
  title: string;
  items: Shortcut[];
}

/** The app's shortcuts (English texts, translated where shown). */
export const SHORTCUTS: ShortcutGroup[] = [
  {
    title: 'Extraction',
    items: [
      { label: 'Play / pause', keys: ['Space'] },
      { label: 'Play backwards', keys: ['J'] },
      { label: 'Pause', keys: ['K'] },
      { label: 'Play forwards', keys: ['L'] },
      { label: 'Previous frame', keys: ['Left'] },
      { label: 'Next frame', keys: ['Right'] },
      { label: 'Set the in point', keys: ['I'] },
      { label: 'Set the out point', keys: ['O'] },
      { label: 'Go to the in point', keys: ['Shift', 'I'] },
      { label: 'Go to the out point', keys: ['Shift', 'O'] },
    ],
  },
  {
    title: 'General',
    items: [{ label: 'Keyboard shortcuts', keys: ['?'] }],
  },
];

/** A key as written on the keyboard (« Maj », « Espace », ←). */
export function keyName(k: string): string {
  switch (k) {
    case 'Left':
      return '←';
    case 'Right':
      return '→';
    case 'Up':
      return '↑';
    case 'Down':
      return '↓';
    case 'Shift':
      return tc('key', 'Shift');
    case 'Space':
      return tc('key', 'Space');
    default:
      return k;
  }
}

/** Keys with `+` between them. */
export function Keys({ keys }: { keys: string[] }) {
  return (
    <span className="shortcut-keys">
      {keys.map((k, i) => (
        <Fragment key={i}>
          {i > 0 && <span className="shortcut-plus">+</span>}
          <kbd>{keyName(k)}</kbd>
        </Fragment>
      ))}
    </span>
  );
}

/** The groups of shortcuts, as on the overlay and the help page. */
export function ShortcutList() {
  return (
    <div className="shortcut-list">
      {SHORTCUTS.map((g) => (
        <section key={g.title} className="shortcut-group">
          <div className="shortcut-title">{t(g.title)}</div>
          {g.items.map((s) => (
            <div key={s.label} className="shortcut-row">
              <span>{t(s.label)}</span>
              <Keys keys={s.keys} />
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}

export const useShortcuts = create<{ open: boolean }>(() => ({ open: false }));
export const openShortcuts = () => useShortcuts.setState({ open: true });
const closeShortcuts = () => useShortcuts.setState({ open: false });

const typing = (el: EventTarget | null) => el instanceof HTMLElement && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName));

/** Mounted once in the shell: the `?` key and the overlay. */
export function Shortcuts() {
  const open = useShortcuts((s) => s.open);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key !== '?' || e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented || typing(e.target)) return;
      if (!useShortcuts.getState().open && document.querySelector('.modal-back')) return;
      e.preventDefault();
      useShortcuts.setState((s) => ({ open: !s.open }));
    };
    addEventListener('keydown', key);
    return () => removeEventListener('keydown', key);
  }, []);
  if (!open) return null;
  return (
    <div className="modal-back" onPointerDown={closeShortcuts}>
      <div ref={(el) => el?.focus()} tabIndex={-1} className="modal shortcuts" role="dialog" aria-labelledby="shortcuts-title" onPointerDown={(e) => e.stopPropagation()}>
        <header>
          <span className="dialog-icon">
            <Keyboard size={16} />
          </span>
          <h2 id="shortcuts-title">{t('Keyboard shortcuts')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={closeShortcuts}>
            <X size={16} />
          </button>
        </header>
        <ShortcutList />
      </div>
    </div>
  );
}
