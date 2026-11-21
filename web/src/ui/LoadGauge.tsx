/*
 * The top bar's gauge: the server's highest load among the processor, the memory, the disks
 * (loops and clips) and the bandwidth going out (against its limit, when one is set). The
 * detail of each in its tooltip; for an admin, a click opens Système › Santé.
 */
import { useEffect } from 'react';
import { Gauge } from 'lucide-react';
import { useShallow } from 'zustand/react/shallow';
import { locale, t } from '../i18n';
import { navigate } from '../router';
import { can, useApp } from '../store';
import { useMetrics, watchMetrics } from './metrics';

const pct = (v: number) => `${Math.round(v).toLocaleString(locale())} %`;

export function LoadGauge() {
  const { connected, user } = useApp(useShallow((s) => ({ connected: s.connected, user: s.auth?.user })));
  const load = useMetrics((s) => s.metrics?.load);
  useEffect(() => watchMetrics(), []);
  if (!connected || !load) return null;

  const parts: [string, number, string?][] = [
    [t('Processor'), load.cpu],
    [t('Memory'), load.memory],
    [t('Disk'), load.disk],
  ];
  if (load.upload != null) parts.push([t('Bandwidth out'), load.upload, `${load.outMbps.toLocaleString(locale(), { maximumFractionDigits: 1 })} / ${load.uploadLimit.toLocaleString(locale())} Mb/s`]);
  const top = parts.reduce((a, p) => (p[1] > a[1] ? p : a));
  const v = Math.min(100, Math.max(0, top[1]));
  const tone = v >= 90 ? 'bad' : v >= 75 ? 'warn' : '';
  const tip = [
    t('Highest load: {what} {n}', { what: top[0].toLowerCase(), n: pct(top[1]) }),
    '',
    ...parts.map(([what, n, extra]) => `${what} : ${pct(n)}${extra ? ` (${extra})` : ''}`),
    ...(load.upload == null ? [`${t('Bandwidth out')} : ${load.outMbps.toLocaleString(locale(), { maximumFractionDigits: 1 })} Mb/s`] : []),
  ].join('\n');
  const body = (
    <>
      <Gauge size={14} />
      <span className="load-bar">
        <i style={{ width: `${v}%` }} />
      </span>
      <b>{pct(v)}</b>
    </>
  );
  const cls = `load-gauge ${tone}`;
  return can.admin(user) ? (
    <button type="button" className={cls} title={tip} aria-label={tip} onClick={() => navigate('/system/health')}>
      {body}
    </button>
  ) : (
    <span className={cls} title={tip}>
      {body}
    </span>
  );
}
