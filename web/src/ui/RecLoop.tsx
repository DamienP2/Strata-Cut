/*
 * REC (Sources, Régie): the loop's length chosen as the recording starts — 3, 6, 12 or 24 hours
 * (the source's own if it is another), each with the room it takes on the disk.
 */
import { createElement } from 'react';
import { Check } from 'lucide-react';
import { api } from '../store';
import { lang, locale, t } from '../i18n';
import { openContextMenu, type MenuEntry } from './ContextMenu';
import { explainGuard } from './guard';
import { showError } from './serverError';
import type { Recorder } from '../types';

export const LOOPS = [3, 6, 12, 24];
/** Mb/s when nothing is known of the source's (as the server). */
const DEFAULT_MBPS = 8;

/** Bytes a full loop takes (as the server counts it: the preview and 5 % included). */
export const loopBytes = (mbps: number, hours: number) => ((mbps > 0 ? mbps : DEFAULT_MBPS) + 1.5) * 1e6 / 8 * 3600 * hours * 1.05;

/** What the disk gives the loops, and what each recorder reserves. */
export interface Space {
  capacity: number | null;
  recorders: { id: string; enabled: boolean; need: number | null }[];
}

export const gb = (b: number) => `${(b / 1e9).toLocaleString(locale(), { maximumFractionDigits: b < 1e10 ? 1 : 0 })} ${lang() === 'fr' ? 'Go' : 'GB'}`;

/** Records a source all the time, with this loop. */
async function record(rec: Recorder, hours: number, done?: () => void) {
  try {
    await api('POST', `/api/recorders/${rec.id}/recording`, { mode: 'continuous', loopHours: hours });
    done?.();
  } catch (e) {
    if (!explainGuard(e)) showError(e);
  }
}

/** REC pressed: the loops offered under the button, with the room each takes; one chosen, the source records. */
export function recWithLoop(e: React.MouseEvent, rec: Recorder, mbps: number, done?: () => void) {
  e.preventDefault();
  e.stopPropagation();
  const r = e.currentTarget.getBoundingClientRect();
  const at = { clientX: r.left, clientY: r.bottom + 4 };
  const hours = LOOPS.includes(rec.loopHours) ? LOOPS : [...LOOPS, rec.loopHours].sort((a, b) => a - b);
  const open = (space: Space | null) => {
    const others = space ? space.recorders.filter((x) => x.enabled && x.id !== rec.id).reduce((a, x) => a + (x.need ?? 0), 0) : 0;
    const left = space?.capacity != null ? Math.max(0, space.capacity - others) : null;
    const items: MenuEntry[] = [
      { note: t('Record with a loop of') },
      ...hours.map((h) => {
        const need = loopBytes(mbps || rec.bitrate, h);
        return {
          label: t('{h} h', { h }),
          icon: h === rec.loopHours ? createElement(Check, { size: 14 }) : createElement('span', { style: { width: 14, display: 'inline-block' } }),
          side: `≈ ${gb(need)}`,
          warn: left !== null && need > left,
          onClick: () => void record(rec, h, done),
        };
      }),
    ];
    if (left !== null) items.push('sep', { note: t('{left} free for the loops', { left: gb(left) }) });
    openContextMenu(at, items);
  };
  api<Space>('GET', '/api/storage').then(open, () => open(null));
}
