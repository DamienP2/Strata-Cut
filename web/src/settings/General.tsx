/* Système › Serveur › Général: name and time zone, where the web interface listens, the clock. */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Clock, Gauge, Network, Server } from 'lucide-react';
import { locale, t } from '../i18n';
import { api } from '../store';
import { PageHead } from '../pages/SidePage';
import { Dropdown, type DropOption } from '../ui/Dropdown';
import { NumberField } from '../ui/fields';
import { InfoTip } from '../ui/InfoTip';
import { Card, Field, SaveBar, Switch } from './ui';
import { useMetrics, watchMetrics } from '../ui/metrics';
import { useServerConfig } from './serverConfig';

/** Every IANA time zone the browser knows, grouped by region. */
function zones(current: string): DropOption[] {
  let list: string[] = [];
  try {
    list = Intl.supportedValuesOf('timeZone');
  } catch {
    /* old browser */
  }
  if (!list.includes('UTC')) list = ['UTC', ...list];
  if (current && !list.includes(current)) list = [current, ...list];
  return list.map((z) => ({ value: z, label: z.replace(/_/g, ' '), group: z.includes('/') ? z.split('/')[0] : undefined }));
}

/** "0.0.0.0:8080" → ["0.0.0.0", 8080]; "[::]:8080" → ["::", 8080]. */
function splitBind(bind: string): [string, number] {
  const i = bind.lastIndexOf(':');
  return [bind.slice(0, i).replace(/^\[|\]$/g, ''), Number(bind.slice(i + 1)) || 8080];
}
const joinBind = (ip: string, port: number) => (ip.includes(':') ? `[${ip}]:${port}` : `${ip}:${port}`);

interface TimeStatus {
  synchronized: boolean | null;
  service: string;
  server: string | null;
  offsetMs: number | null;
  managed: boolean;
}

export function General() {
  const { info, cfg, setCfg, dirty, saving, save, discard } = useServerConfig();
  const options = useMemo(() => zones(cfg?.timezone ?? ''), [cfg?.timezone]);
  const [time, setTime] = useState<TimeStatus | null>(null);
  const loadTime = useCallback(async () => {
    try {
      setTime(await api<TimeStatus>('GET', '/api/system/time'));
    } catch {
      /* the next try */
    }
  }, []);
  useEffect(() => {
    void loadTime();
    const i = setInterval(() => void loadTime(), 10_000);
    return () => clearInterval(i);
  }, [loadTime]);

  const bind = (cfg?.bind as string | null | undefined) || info?.bind || '0.0.0.0:8080';
  const [ip, port] = splitBind(bind);
  const cards: DropOption[] = [{ value: '0.0.0.0', label: t('All network cards') }, ...(info?.interfaces ?? []).filter((i) => !i.ip.includes(':')).map((i) => ({ value: i.ip, label: i.name, detail: i.ip }))];
  if (!cards.some((c) => c.value === ip)) cards.push({ value: ip, label: ip });
  const ntp = Array.isArray(cfg?.ntp) ? (cfg.ntp as string[]) : [];
  const offset = time?.offsetMs;

  return (
    <>
      <PageHead title={t('General')} />
      {!cfg ? (
        <div className="loading">{t('Loading…')}</div>
      ) : (
        <>
          <Card icon={<Server size={16} />} title={t('General')}>
            <div className="set-grid two">
              <Field label={t('Server name')}>
                <input value={cfg.name} aria-invalid={!cfg.name.trim()} onChange={(e) => setCfg((c) => ({ ...c, name: e.target.value }))} />
              </Field>
              <Field label={t('Time zone')}>
                <Dropdown value={cfg.timezone} options={options} onChange={(timezone) => setCfg((c) => ({ ...c, timezone }))} />
              </Field>
            </div>
          </Card>
          <Card icon={<Network size={16} />} title={t('Web interface')}>
            <div className="set-grid two">
              <Field label={t('Network card')} group>
                <Dropdown value={ip} options={cards} onChange={(v) => setCfg((c) => ({ ...c, bind: joinBind(v, port) }))} />
              </Field>
              <Field
                label={
                  <>
                    {t('Port')}
                    <InfoTip text={t('Open this port in the firewall: sudo ufw allow {port}/tcp', { port })} />
                  </>
                }
                group
              >
                <NumberField value={port} min={1} max={65535} onChange={(v) => setCfg((c) => ({ ...c, bind: joinBind(ip, v) }))} />
              </Field>
            </div>
          </Card>
          <Card icon={<Clock size={16} />} title={t('Clock')}>
            <div className="set-grid two">
              <Field
                label={
                  <>
                    {t('NTP servers')}
                    {time && !time.managed && <InfoTip text={t('Set when Strata Cut is installed with the install script.')} />}
                  </>
                }
              >
                <input value={ntp.join(' ')} placeholder={t('System default')} spellCheck={false} disabled={!!time && !time.managed} onChange={(e) => setCfg((c) => ({ ...c, ntp: e.target.value.split(/[\s,;]+/) }))} />
              </Field>
              <Field label={t('State')} group>
                <div className="sc-time">
                  <span className={`nav-dot ${time?.synchronized ? 'ok' : time?.synchronized === false ? 'warn' : ''}`} />
                  <span>{time?.synchronized ? t('Synchronised') : time?.synchronized === false ? t('Not synchronised') : '—'}</span>
                  {time?.server && <span className="muted">{time.server}</span>}
                  {offset !== null && offset !== undefined && (
                    <span className="muted">
                      {t('offset {ms} ms', { ms: offset.toLocaleString(locale(), { maximumFractionDigits: Math.abs(offset) < 10 ? 2 : 0, signDisplay: 'always' }) })}
                    </span>
                  )}
                </div>
              </Field>
            </div>
          </Card>
          <LimitsCard limits={{ ...LIMITS, ...((cfg.limits as Partial<Limits> | undefined) ?? {}) }} set={(p) => setCfg((c) => ({ ...c, limits: { ...LIMITS, ...((c.limits as Partial<Limits> | undefined) ?? {}), ...p } }))} />
        </>
      )}
      <SaveBar dirty={dirty} saving={saving} onSave={() => void save()} onDiscard={discard} />
    </>
  );
}

interface Limits {
  enabled: boolean;
  maxCpu: number;
  maxMemory: number;
  uploadMbps: number;
}
const LIMITS: Limits = { enabled: true, maxCpu: 85, maxMemory: 90, uploadMbps: 0 };

/** The resources' guard: a new recording or stream refused past these limits (with a window saying why). */
function LimitsCard({ limits, set }: { limits: Limits; set: (p: Partial<Limits>) => void }) {
  const m = useMetrics((s) => s.metrics?.machine);
  useEffect(() => watchMetrics(), []);
  const mem = m && m.memTotal ? Math.round((m.memUsed / m.memTotal) * 100) : null;
  return (
    <Card icon={<Gauge size={16} />} title={t('Resources’ guard')} actions={<Switch checked={limits.enabled} onChange={(enabled) => set({ enabled })} label={limits.enabled ? t('On') : t('Off')} />}>
      <p className="muted set-note">{t('A new recording or stream started by hand is refused when it would take the server past these limits; a window says why. What already runs goes on, and the programmed slots start all the same.')}</p>
      <div className="set-grid three">
        <Field label={t('Processor')} hint={m ? t('Now: {n} %', { n: Math.round(m.cpu) }) : undefined}>
          <NumberField value={limits.maxCpu} min={30} max={100} step={5} suffix="%" disabled={!limits.enabled} onChange={(v) => set({ maxCpu: v })} />
        </Field>
        <Field label={t('Memory')} hint={mem !== null ? t('Now: {n} %', { n: mem }) : undefined}>
          <NumberField value={limits.maxMemory} min={30} max={100} step={5} suffix="%" disabled={!limits.enabled} onChange={(v) => set({ maxMemory: v })} />
        </Field>
        <Field label={t('Bandwidth of the streams')} hint={t('0: no limit. What the server’s link can send out.')}>
          <NumberField value={limits.uploadMbps} min={0} max={100000} step={10} suffix="Mb/s" disabled={!limits.enabled} onChange={(v) => set({ uploadMbps: v })} />
        </Field>
      </div>
    </Card>
  );
}
