// Machine load (GET /api/system/metrics every 3 s), for the status pill and the Performance page.
import { create } from 'zustand';
import { api } from '../store';

export interface Machine {
  /** Percent, 0..100. */
  cpu: number;
  cores: number;
  cpuName: string;
  memUsed: number;
  memTotal: number;
  hostname: string;
}

export interface Metrics {
  machine: Machine;
  [k: string]: unknown;
}

export const useMetrics = create<{ metrics: Metrics | null; at: number }>(() => ({ metrics: null, at: 0 }));

let users = 0;
let timer = 0;

async function poll() {
  try {
    const metrics = await api<Metrics>('GET', '/api/system/metrics');
    useMetrics.setState({ metrics, at: Date.now() });
  } catch {
    /* the pill shows the connection state anyway */
  }
}

/** Polls while at least one component wants the metrics; returns the stop function. */
export function watchMetrics(): () => void {
  if (users++ === 0) {
    void poll();
    timer = window.setInterval(() => {
      if (document.visibilityState === 'visible') void poll();
    }, 3000);
  }
  return () => {
    if (--users === 0) clearInterval(timer);
  };
}
