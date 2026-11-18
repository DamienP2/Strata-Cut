// Numbers, sizes, durations and dates in the interface language.
import { locale, t } from '../i18n';
import { useApp } from '../store';

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];

/** A size in bytes: « 12 Mo », « 1,5 Go » (base 1024). */
export function formatBytes(n: number | null | undefined): string {
  let v = Math.max(0, n ?? 0);
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  const num = v.toLocaleString(locale(), { maximumFractionDigits: i > 0 && v < 10 ? 1 : 0 });
  return `${num} ${t(UNITS[i])}`;
}

/** A number in the interface language (« 1 234,5 »). */
export function formatNumber(n: number, digits = 0): string {
  return n.toLocaleString(locale(), { maximumFractionDigits: digits, minimumFractionDigits: digits });
}

/** How long: `2d 3h`, `3h 12m`, `5m 10s`, `8s` (French: « 2 j 3 h »…). */
export function formatDuration(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d) return t('{d}d {h}h', { d, h });
  if (h) return t('{h}h {m}m', { h, m });
  if (m) return t('{m}m {s}s', { m, s: s % 60 });
  return t('{s}s', { s });
}

/** A timestamp (ms or ISO text) as a Date. */
export function toDate(at: number | string | null | undefined): Date | null {
  if (at === null || at === undefined || at === '') return null;
  const d = new Date(at);
  return Number.isNaN(d.getTime()) ? null : d;
}

/** « 18 nov. 2026, 14:05 » in the server's time zone. */
export function formatDateTime(at: number | string | null | undefined): string {
  const d = toDate(at);
  if (!d) return '';
  try {
    return d.toLocaleString(locale(), { dateStyle: 'medium', timeStyle: 'short', timeZone: useApp.getState().timezone });
  } catch {
    return d.toLocaleString(locale(), { dateStyle: 'medium', timeStyle: 'short' });
  }
}

/** « 18 nov. 2026 » in the server's time zone. */
export function formatDate(at: number | string | null | undefined): string {
  const d = toDate(at);
  if (!d) return '';
  try {
    return d.toLocaleDateString(locale(), { dateStyle: 'medium', timeZone: useApp.getState().timezone });
  } catch {
    return d.toLocaleDateString(locale(), { dateStyle: 'medium' });
  }
}

/** « il y a 5 min ». */
export function timeAgo(at: number | string | null | undefined): string {
  const d = toDate(at);
  if (!d) return '';
  const s = Math.max(0, (Date.now() - d.getTime()) / 1000);
  if (s < 60) return t('just now');
  if (s < 3600) return t('{n} min ago', { n: Math.floor(s / 60) });
  if (s < 86400) return t('{n} h ago', { n: Math.floor(s / 3600) });
  return t('{n} d ago', { n: Math.floor(s / 86400) });
}

/** The tone of a load: ok < 60 %, warn < 85 %, bad above. */
export function tone(pct: number | null | undefined): 'tone-ok' | 'tone-warn' | 'tone-bad' | 'tone-idle' {
  if (pct === null || pct === undefined || Number.isNaN(pct)) return 'tone-idle';
  return pct < 60 ? 'tone-ok' : pct < 85 ? 'tone-warn' : 'tone-bad';
}
