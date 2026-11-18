/* Système › Supervision › Performances: the machine's load. */
import { useEffect } from 'react';
import { Cpu } from 'lucide-react';
import { t } from '../i18n';
import { PageHead } from '../pages/SidePage';
import { formatBytes, tone } from '../ui/format';
import { useMetrics, watchMetrics } from '../ui/metrics';
import { Card } from './ui';

/** A load bar, `pct` 0..100, coloured by its tone. */
export function PerfBar({ pct }: { pct: number | null }) {
  const p = Math.max(0, Math.min(100, pct ?? 0));
  return (
    <span className={`perf-bar ${tone(pct)}`} role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(p)}>
      <span className="perf-bar-track">
        <span className="perf-bar-fill" style={{ transform: `translateX(${p - 100}%)` }} />
      </span>
    </span>
  );
}

function Row({ label, pct, value, title }: { label: string; pct: number | null; value: string; title?: string }) {
  return (
    <>
      <span className={`perf-label ${tone(pct)}`} title={title}>
        <span className="perf-dot" />
        {label}
      </span>
      <PerfBar pct={pct} />
      <span className="perf-value">{value}</span>
    </>
  );
}

export function Performance() {
  const metrics = useMetrics((s) => s.metrics);
  useEffect(() => watchMetrics(), []);
  const m = metrics?.machine;
  const mem = m && m.memTotal ? (m.memUsed / m.memTotal) * 100 : null;
  return (
    <>
      <PageHead title={t('Performance')} sub={m?.hostname} />
      {!m ? (
        <div className="loading">{t('Loading…')}</div>
      ) : (
        <Card icon={<Cpu size={16} />} title={t('Machine')} actions={<span className="muted">{m.cpuName}</span>}>
          <div className="perf-rows">
            <Row label={t('CPU')} pct={m.cpu} value={`${t('{n}%', { n: Math.round(m.cpu) })} ·${t('{n} cores', { n: m.cores })}`} title={m.cpuName} />
            <Row label={t('Memory')} pct={mem} value={t('{used} of {total}', { used: formatBytes(m.memUsed), total: formatBytes(m.memTotal) })} />
          </div>
        </Card>
      )}
    </>
  );
}
