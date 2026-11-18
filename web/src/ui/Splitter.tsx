/*
 * A resize handle: 7 px, a 1 px accent line on hover, double-click = default
 * size. Sizes are kept for the user with useLayout (display preferences).
 */
import { useCallback, useRef } from 'react';
import { getDisplayPref, setDisplayPref, useDisplayPrefs } from '../displayPrefs';

export interface SplitterProps {
  /** x: drags left/right (a width); y: drags up/down (a height). */
  axis?: 'x' | 'y';
  /** Pinned to this edge of its (relative) parent, for `x`. */
  edge?: 'left' | 'right';
  /** The size it changes, px. */
  value: number;
  onChange: (v: number) => void;
  /** Double-click. */
  onReset?: () => void;
  min?: number;
  max?: number;
  /** The size grows when dragging left (x) or up (y): a panel on the right or at the bottom. */
  invert?: boolean;
  title?: string;
  className?: string;
}

export function Splitter({ axis = 'x', edge, value, onChange, onReset, min = 0, max = Infinity, invert = false, title, className }: SplitterProps) {
  const start = useRef<{ at: number; v: number; id: number } | null>(null);
  return (
    <div
      className={`splitter ${axis} ${edge ? `edge-${edge}` : ''} ${className ?? ''}`}
      role="separator"
      aria-orientation={axis === 'x' ? 'vertical' : 'horizontal'}
      title={title}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        start.current = { at: axis === 'x' ? e.clientX : e.clientY, v: value, id: e.pointerId };
        document.body.classList.add(`resizing-${axis}`);
      }}
      onPointerMove={(e) => {
        const s = start.current;
        if (!s || s.id !== e.pointerId) return;
        const d = (axis === 'x' ? e.clientX : e.clientY) - s.at;
        onChange(Math.round(Math.min(max, Math.max(min, s.v + (invert ? -d : d)))));
      }}
      onPointerUp={() => {
        start.current = null;
        document.body.classList.remove(`resizing-${axis}`);
      }}
      onPointerCancel={() => {
        start.current = null;
        document.body.classList.remove(`resizing-${axis}`);
      }}
      onDoubleClick={onReset}
    />
  );
}

/**
 * Panel sizes kept for the user: `const [layout, setLayout, resetLayout] = useLayout('extract', { left: 260, bottom: 220 })`.
 * `setLayout({ left: 300 })` merges; `resetLayout('left')` goes back to the default (all when no key).
 */
export function useLayout<T extends Record<string, number>>(key: string, defaults: T): [T, (patch: Partial<T>) => void, (k?: keyof T) => void] {
  const pref = `layout.${key}`;
  const stored = useDisplayPrefs((s) => s.prefs[pref]) as Partial<T> | undefined;
  const layout = { ...defaults, ...(stored ?? {}) } as T;
  const set = useCallback(
    (patch: Partial<T>) => {
      setDisplayPref(pref, { ...getDisplayPref<Partial<T>>(pref, {}), ...patch });
    },
    [pref],
  );
  const reset = useCallback(
    (k?: keyof T) => {
      if (k === undefined) return setDisplayPref(pref, null);
      const cur = { ...getDisplayPref<Partial<T>>(pref, {}) };
      delete cur[k];
      setDisplayPref(pref, Object.keys(cur).length ? cur : null);
    },
    [pref],
  );
  return [layout, set, reset];
}
