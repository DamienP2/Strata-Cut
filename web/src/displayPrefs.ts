/*
 * Display preferences kept for the user on the server (panel sizes, folded
 * sections, display choices): the same view in every browser of the user.
 * Loaded before the pages are drawn; changes are sent together 600 ms later,
 * and with `keepalive` when the page goes away.
 */
import { useCallback } from 'react';
import { create } from 'zustand';
import { api } from './store';

interface DisplayPrefs {
  /** The user the preferences belong to (null: not loaded). */
  user: string | null;
  prefs: Record<string, unknown>;
}

export const useDisplayPrefs = create<DisplayPrefs>(() => ({ user: null, prefs: {} }));

let pending: Record<string, unknown> = {};
let timer = 0;

export async function loadDisplayPrefs(userId: string) {
  if (useDisplayPrefs.getState().user === userId) return;
  let prefs: Record<string, unknown> = {};
  try {
    prefs = (await api<Record<string, unknown>>('GET', '/api/me/display')) ?? {};
  } catch {
    /* drawn with the defaults */
  }
  pending = {};
  useDisplayPrefs.setState({ user: userId, prefs });
}

function flush(keepalive = false) {
  clearTimeout(timer);
  timer = 0;
  if (!Object.keys(pending).length || !useDisplayPrefs.getState().user) return;
  const body = JSON.stringify(pending);
  pending = {};
  if (keepalive) {
    void fetch('/api/me/display', { method: 'PATCH', keepalive: true, credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body }).catch(() => {});
  } else {
    api('PATCH', '/api/me/display', JSON.parse(body)).catch(() => {});
  }
}

addEventListener('pagehide', () => flush(true));

/** Saves one preference (`null` or `undefined` removes it). */
export function setDisplayPref(key: string, value: unknown) {
  const prefs = { ...useDisplayPrefs.getState().prefs };
  if (value === null || value === undefined) delete prefs[key];
  else prefs[key] = value;
  useDisplayPrefs.setState({ prefs });
  pending[key] = value ?? null;
  clearTimeout(timer);
  timer = window.setTimeout(() => flush(), 600);
}

export function getDisplayPref<T>(key: string, fallback: T): T {
  const v = useDisplayPrefs.getState().prefs[key];
  return v === undefined ? fallback : (v as T);
}

/** Like useState, kept for the user on the server. */
export function useDisplayPref<T>(key: string, fallback: T): [T, (v: T | ((prev: T) => T)) => void] {
  const stored = useDisplayPrefs((s) => s.prefs[key]);
  const value = stored === undefined ? fallback : (stored as T);
  const set = useCallback(
    (v: T | ((prev: T) => T)) => {
      const prev = getDisplayPref(key, fallback);
      const next = typeof v === 'function' ? (v as (p: T) => T)(prev) : v;
      setDisplayPref(key, next);
    },
    // The fallback is a default value: only the key matters.
    [key],
  );
  return [value, set];
}
