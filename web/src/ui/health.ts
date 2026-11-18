// The server's health (GET /api/system/health, every minute for admins): Health page, dots, crash banner.
import { create } from 'zustand';
import { api } from '../store';

export interface Incident {
  kind: string;
  level: 'CRASH' | 'ERROR' | 'WARN';
  cat: string;
  title: string;
  detail: string;
  first: number | string;
  last: number | string;
  count: number;
  crash?: string;
}

export interface Health {
  version: string;
  startedAt: number | string;
  uptimeSecs: number;
  crashes: number;
  lastCrash: number | string | null;
  unseenCrash: boolean;
  errors: number;
  warnings: number;
  lastHourErrors: number;
  disk: { total: number; free: number };
  incidents: Incident[];
}

export const useHealth = create<{ health: Health | null }>(() => ({ health: null }));

/** An unseen crash, or errors in the last hour. */
export const needsLook = (h: Health | null | undefined) => !!h && (h.unseenCrash || h.lastHourErrors > 0);

export async function loadHealth() {
  try {
    useHealth.setState({ health: await api<Health>('GET', '/api/system/health') });
  } catch {
    /* kept as it was */
  }
}

/** Marks the last crash as seen. */
export async function healthSeen() {
  const h = useHealth.getState().health;
  if (h) useHealth.setState({ health: { ...h, unseenCrash: false } });
  try {
    await api('POST', '/api/system/health/seen');
  } catch {
    /* */
  }
}

/** Reloads every minute; returns the stop function. */
export function watchHealth(): () => void {
  void loadHealth();
  const id = window.setInterval(() => void loadHealth(), 60_000);
  return () => clearInterval(id);
}
