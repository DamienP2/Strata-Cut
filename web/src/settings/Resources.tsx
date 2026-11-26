/*
 * Système › Ressources: how loaded the server is against the guard's limit, what still fits, and
 * what each source, Streamer, clip… costs (measured every 2 seconds, the last 10 minutes drawn).
 * Something still working for a stopped source can be let go.
 */
import { useCallback, useEffect, useState } from 'react';
import { Cpu, HardDriveDownload, MemoryStick, Network, Radio, Scissors, Server, TriangleAlert } from 'lucide-react';
import { lang, locale, t, tc, tn } from '../i18n';
import { api, useApp } from '../store';
import { PageHead } from '../pages/SidePage';
import { SOURCE_KINDS } from '../ui/sourceKinds';
import { RecLogo } from '../ui/RecLogo';
import { showError } from '../ui/serverError';
import { confirmDialog } from '../ui/dialogs';
import { size } from '../tc';
import { Card } from './ui';

interface Flow {
  key: string;
  name: string;
  kind: 'source' | 'streamer' | 'clip' | 'server' | 'other';
  id?: string;
  source?: string;
  protocol?: string;
  roles: string[];
  cores: number;
  memory: number;
  writes: number;
  history: number[];
  running: boolean;
  inMbps?: number;
  lost?: number;
  damaged?: number;
  previewLag?: number;
  previewDropped?: boolean;
  state?: string;
  leftover?: boolean;
  lives?: string[];
}
interface Resources {
  cores: number;
  cpuName: string;
  cpu: { total: number; busiestCore: number; softirq: number; steal: number; limit: number; guard: boolean; overs: number; history: number[]; usedCores: number; margin: number };
  memory: { used: number; total: number; limit: number };
  nics: { name: string; mbps: number; speed: number }[];
  writes: number;
  lost: number;
  room: { what: 'network' | 'sdi' | 'page' | 'copy'; cores: number; count: number }[];
  flows: Flow[];
}

const num = (v: number, digits = 1) => v.toLocaleString(locale(), { minimumFractionDigits: digits, maximumFractionDigits: digits });
const pct = (v: number) => `${Math.round(v)} %`;

/** Each source kind's icon. */
const kindIcon = (k?: string) => {
  const key = k === 'sdi' ? 'card' : k === 'network' ? 'net' : k === 'online' ? 'web' : k;
  return SOURCE_KINDS.find((x) => x.key === key)?.icon ?? <Radio size={14} />;
};

/** The last 10 minutes, a line. */
function Spark({ values, max, tone }: { values: number[]; max: number; tone: string }) {
  const w = 96;
  const h = 24;
  if (values.length < 2) return <svg className="rs-spark" width={w} height={h} />;
  const top = Math.max(max, ...values, 0.01);
  const pts = values.map((v, i) => `${((i / (values.length - 1)) * w).toFixed(1)},${(h - 1 - (v / top) * (h - 2)).toFixed(1)}`).join(' ');
  return (
    <svg className={`rs-spark ${tone}`} width={w} height={h} viewBox={`0 0 ${w} ${h}`} aria-hidden="true">
      <polyline points={pts} fill="none" strokeWidth="1.5" />
    </svg>
  );
}

/** How a flow goes, said plainly, and its tone. */
function stateOf(f: Flow): [string, 'ok' | 'warn' | 'bad' | 'info'] {
  if (f.leftover) return [t('Stopped, but still working'), 'bad'];
  if (f.kind === 'clip') return [t('Being cut'), 'info'];
  if (f.kind === 'streamer') return f.lives?.length ? [`→ ${f.lives.join(', ')}`, 'info'] : [t('Normal'), 'ok'];
  if (f.kind !== 'source') return [t('Normal'), 'ok'];
  if (f.lost) return [tn(f.lost, '{n} packet lost · 10 min', '{n} packets lost · 10 min'), 'bad'];
  if (f.damaged) return [tn(f.damaged, 'Damaged {n} time · 10 min', 'Damaged {n} times · 10 min'), 'warn'];
  if (f.state === 'nosignal') return [t('No signal'), 'warn'];
  if ((f.previewLag ?? 0) >= 2) return [t('Preview {s} s late', { s: Math.round(f.previewLag ?? 0) }), 'warn'];
  if (f.previewDropped) return [t('Preview jerky'), 'warn'];
  return [t('Normal'), 'ok'];
}

export function Resources() {
  const recorders = useApp((s) => s.recorders);
  const toast = useApp((s) => s.toast);
  const [r, setR] = useState<Resources | null>(null);
  const load = useCallback(async () => {
    try {
      setR(await api<Resources>('GET', '/api/system/resources'));
    } catch {
      /* the next try */
    }
  }, []);
  useEffect(() => {
    void load();
    const i = setInterval(() => void load(), 2000);
    return () => clearInterval(i);
  }, [load]);
  const fr = lang() === 'fr';
  const sz = (b: number) => size(b, locale(), fr);
  const free = async (f: Flow) => {
    if (!(await confirmDialog({ title: t('Let go of “{name}”?', { name: f.name }), message: t('What still works for it is stopped. Its recordings stay.'), confirm: t('Let go') }))) return;
    try {
      await api('POST', '/api/system/resources/free', { key: f.key });
      toast(t('Let go'));
      void load();
    } catch (e) {
      showError(e);
    }
  };
  if (!r) return <PageHead title={t('Resources')} />;
  const c = r.cpu;
  const cores = r.cores || 1;
  const over = c.total > c.limit;
  const memPct = r.memory.total ? (r.memory.used / r.memory.total) * 100 : 0;
  const nic = [...r.nics].sort((a, b) => b.mbps - a.mbps)[0];
  const top = r.flows.find((f) => f.kind !== 'server');
  // A single core saturated while the whole stays low: the network's interrupts, on one core.
  const oneCore = c.busiestCore >= 90 && c.total < c.limit;
  const roomLabel = (w: Resources['room'][number]['what']) => ({ network: t('network source (UDP, SRT…)'), sdi: t('SDI / NDI source'), page: t('web page'), copy: t('live as a copy') })[w];
  return (
    <>
      <PageHead title={t('Resources')} sub={t('Measured every 2 seconds · {n} cores · {cpu}', { n: r.cores, cpu: r.cpuName })} />
      {c.overs > 0 && (
        <div className="rs-alert bad">
          <TriangleAlert size={16} />
          <div>
            <b>{tn(c.overs, 'The processor went over its limit once in the last 10 minutes.', 'The processor went over its limit {n} times in the last 10 minutes.')}</b>
            <span>{t('When it is saturated, the UDP sources lose packets and their picture breaks up. Stop a source, or record it at another time.')}{top ? ` ${t('The most: {name} ({c} cores).', { name: top.name, c: num(top.cores) })}` : ''}</span>
          </div>
        </div>
      )}
      {oneCore && (
        <div className="rs-alert warn">
          <TriangleAlert size={16} />
          <div>
            <b>{t('One core is saturated ({p}) while the whole is at {q}.', { p: pct(c.busiestCore), q: pct(c.total) })}</b>
            <span>{c.softirq >= 3 ? t('The network card’s interrupts are on one core: a virtual machine’s card without several queues (virtio “multiqueue”) receives everything on one core. Give it as many queues as cores.') : t('A source’s receiving or encoding cannot share its work: the other cores do not help it.')}</span>
          </div>
        </div>
      )}
      {c.steal >= 5 && (
        <div className="rs-alert warn">
          <TriangleAlert size={16} />
          <div>
            <b>{t('The hypervisor takes {p} of the processor from this machine.', { p: pct(c.steal) })}</b>
            <span>{t('Other virtual machines of the host use its cores: give this one dedicated cores, or fewer machines on the host.')}</span>
          </div>
        </div>
      )}
      <Card icon={<Cpu size={16} />} title={t('Server capacity')}>
        <div className="rs-gauges">
          <div className={`rs-g ${over ? 'bad' : c.total > c.limit * 0.85 ? 'warn' : ''}`}>
            <span className="rs-g-l">
              <Cpu size={13} /> {t('Processor')}
            </span>
            <b className="rs-g-v">{pct(c.total)}</b>
            <span className="rs-g-bar">
              <i style={{ width: `${Math.min(100, c.total)}%` }} />
              <em style={{ left: `${c.limit}%` }} />
              <small style={{ left: `${c.limit}%` }}>{t('limit {p}', { p: pct(c.limit) })}</small>
            </span>
            <span className="rs-g-s">
              {t('{used} cores of {n}', { used: num(c.usedCores), n: cores })} · {c.margin >= 0 ? t('{m} core free before the limit', { m: num(c.margin) }) : t('{m} core over the limit', { m: num(-c.margin) })}
            </span>
            <span className="rs-g-s rs-g-more">
              {t('Busiest core {p}', { p: pct(c.busiestCore) })} · {t('network interrupts {p}', { p: pct(c.softirq) })}
              {c.steal >= 1 ? ` · ${t('taken by the hypervisor {p}', { p: pct(c.steal) })}` : ''}
            </span>
          </div>
          <div className={`rs-g ${memPct > r.memory.limit ? 'bad' : ''}`}>
            <span className="rs-g-l">
              <MemoryStick size={13} /> {t('Memory')}
            </span>
            <b className="rs-g-v">{pct(memPct)}</b>
            <span className="rs-g-bar">
              <i style={{ width: `${Math.min(100, memPct)}%` }} />
              <em style={{ left: `${r.memory.limit}%` }} />
              <small style={{ left: `${r.memory.limit}%` }}>{t('limit {p}', { p: pct(r.memory.limit) })}</small>
            </span>
            <span className="rs-g-s">{t('{used} of {total}', { used: sz(r.memory.used), total: sz(r.memory.total) })}</span>
          </div>
          <div className="rs-g">
            <span className="rs-g-l">
              <Network size={13} /> {t('Network in')}
              {nic ? ` · ${nic.name}` : ''}
            </span>
            <b className="rs-g-v">{nic ? `${num(nic.mbps, 0)} Mb/s` : '—'}</b>
            <span className="rs-g-bar">
              <i style={{ width: `${nic?.speed ? Math.min(100, (nic.mbps / nic.speed) * 100) : 0}%` }} />
            </span>
            <span className="rs-g-s">
              {nic?.speed ? t('of {s} Mb/s', { s: nic.speed }) : t('speed not said by the card')} · {r.lost ? tn(r.lost, '{n} packet lost · 10 min', '{n} packets lost · 10 min') : t('no packet lost')}
            </span>
          </div>
          <div className="rs-g">
            <span className="rs-g-l">
              <HardDriveDownload size={13} /> {t('Disk writes')}
            </span>
            <b className="rs-g-v">{num(r.writes / 1e6)} Mo/s</b>
            <span className="rs-g-s">{t('recordings, previews and clips')}</span>
          </div>
        </div>
        <div className="rs-room">
          <span className="rs-room-k">{t('Still possible before the limit')}</span>
          {r.room.map((x) => (
            <span key={x.what} className={`rs-room-chip ${x.count === 0 ? 'none' : x.count >= 2 ? 'ok' : ''}`} data-tip={t('≈ {c} core each', { c: num(x.cores) })}>
              {x.count === 0 ? `${t('No more')} ${roomLabel(x.what)}` : `≈ ${x.count} × ${roomLabel(x.what)}`}
            </span>
          ))}
        </div>
      </Card>
      <Card icon={<Server size={16} />} title={t('By flow')} actions={<span className="muted rs-sort">{t('The most demanding first')}</span>}>
        <table className="user-table rs-table">
          <thead>
            <tr>
              <th>{t('Flow')}</th>
              <th>{t('What runs')}</th>
              <th>{t('Processor')}</th>
              <th>{t('10 min')}</th>
              <th className="num">{t('Memory')}</th>
              <th className="num">{t('Received')}</th>
              <th>{t('State')}</th>
            </tr>
          </thead>
          <tbody>
            {r.flows.map((f) => {
              const [state, tone] = stateOf(f);
              const share = (f.cores / cores) * 100;
              const rec = f.id ? recorders.find((x) => x.id === f.id) : undefined;
              return (
                <tr key={f.key} className={tone === 'bad' ? 'rs-hot' : ''}>
                  <td>
                    <span className="rs-flow">
                      <span className="rs-kind">{f.kind === 'source' ? kindIcon(f.source) : f.kind === 'streamer' ? <Radio size={14} /> : f.kind === 'clip' ? <Scissors size={14} /> : <Server size={14} />}</span>
                      <span className="rs-flow-t">
                        <b>
                          {rec && <RecLogo rec={rec} size="sm" />} {f.name}
                        </b>
                        <small>{f.kind === 'source' ? (f.protocol ?? '').toUpperCase() : f.kind === 'streamer' ? t('Streamer') : f.kind === 'clip' ? t('Clip') : t('Server')}</small>
                      </span>
                    </span>
                  </td>
                  <td>
                    <span className="rs-parts">
                      {f.roles.map((x) => (
                        <span key={x} className="rs-part">
                          {tc('role', x)}
                        </span>
                      ))}
                    </span>
                  </td>
                  <td className="rs-cpu">
                    <span className="rs-cpu-v">
                      <b>{num(f.cores)}</b> {t('core')}
                    </span>
                    <span className="rs-bar">
                      <i className={tone} style={{ width: `${Math.min(100, share)}%` }} />
                    </span>
                    <small>{t('{p} of the server', { p: pct(share) })}</small>
                  </td>
                  <td>
                    <Spark values={f.history} max={1} tone={tone} />
                  </td>
                  <td className="num">{sz(f.memory)}</td>
                  <td className="num">{f.inMbps ? `${num(f.inMbps)} Mb/s` : '—'}</td>
                  <td>
                    <span className="rs-state-cell">
                      <span className={`rs-st ${tone}`}>{state}</span>
                      {f.leftover && (
                        <button type="button" className="btn small" onClick={() => void free(f)}>
                          {t('Let go')}
                        </button>
                      )}
                    </span>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </Card>
    </>
  );
}
