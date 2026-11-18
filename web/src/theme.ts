import { create } from 'zustand';

export type Theme = 'dark' | 'light' | 'system';
export const THEMES: Theme[] = ['dark', 'light', 'system'];
const KEY = 'stratacut.theme';
const isTheme = (t: unknown): t is Theme => t === 'dark' || t === 'light' || t === 'system';
const media = typeof matchMedia === 'function' ? matchMedia('(prefers-color-scheme: light)') : null;

function first(): Theme {
  try {
    const t = localStorage.getItem(KEY);
    if (isTheme(t)) return t;
  } catch {
    /* private browsing */
  }
  return 'dark';
}

function apply(t: Theme) {
  const light = t === 'light' || (t === 'system' && !!media?.matches);
  if (light) document.documentElement.dataset.theme = 'light';
  else delete document.documentElement.dataset.theme;
}

/** The chosen theme; the user menu redraws when it changes. */
export const useTheme = create<{ theme: Theme }>(() => ({ theme: first() }));
apply(useTheme.getState().theme);
media?.addEventListener('change', () => apply(useTheme.getState().theme));

/** Ignores unknown values, saves, applies. */
export function setTheme(t: string | null | undefined) {
  if (!isTheme(t)) return;
  try {
    localStorage.setItem(KEY, t);
  } catch {
    /* */
  }
  apply(t);
  useTheme.setState({ theme: t });
}
