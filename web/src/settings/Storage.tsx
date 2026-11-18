/* Système › Serveur › Stockage: where the loops and clips go, how much they may take, what each loop reserves. */
import { useCallback, useEffect, useState } from 'react';
import { HardDrive, PieChart, TriangleAlert } from 'lucide-react';
import { lang, locale, t } from '../i18n';
import { api, useChanged } from '../store';
import { PageHead } from '../pages/SidePage';
import { NumberField } from '../ui/fields';
import { Dropdown, type DropOption } from '../ui/Dropdown';
import { InfoTip } from '../ui/InfoTip';
import { bitrate, size } from '../tc';
import { Card, Field, SaveBar } from './ui';
import { useServerConfig } from './serverConfig';

interface Disk {
  total: number;
  free: number;
}
interface StorageInfo {
  recordings: { dir: string; disk: Disk | null; bytes: number };
  clips: { dir: string; disk: Disk | null; bytes: number; sameDisk: boolean };
  capacity: number | null;
  reserved: number;
  keepFree: number | null;
  disks: { mount: string; device: string; fs: string; total: number; free: number }[];
  prepares: boolean;
  recorders: { id: string; name: string; enabled: boolean; loopHours: number; expected: number; bitrate: number; bytes: number; need: number | null; spaceLimited: boolean }[];
}

export function Storage() {
  const { cfg, setCfg, dirty, saving, save, discard } = useServerConfig();
  const [info, setInfo] = useState<StorageInfo | null>(null);
  const load = useCallback(async () => {
    try {
      setInfo(await api<StorageInfo>('GET', '/api/storage'));
    } catch {
      /* the next try */
    }
  }, []);
  useEffect(() => {
    void load();
    const i = setInterval(() => void load(), 10_000);
    return () => clearInterval(i);
  }, [load]);
  useChanged('config', load);

  const fr = lang() === 'fr';
  const sz = (b: number) => size(b, locale(), fr);
  const disk = info?.recordings.disk;
  const loops = info?.recordings.bytes ?? 0;
  const clips = info?.clips.bytes ?? 0;
  const same = !!info?.clips.sameDisk;
  const pct = (b: number) => (disk && disk.total ? Math.max(0, (b / disk.total) * 100) : 0);
  const other = disk ? Math.max(0, disk.total - disk.free - loops - (same ? clips : 0)) : 0;
  const str = (k: string) => (typeof cfg?.[k] === 'string' ? (cfg[k] as string) : '');
  const num = (k: string, d: number) => (typeof cfg?.[k] === 'number' ? (cfg[k] as number) : d);
  // The disks: a folder is on the disk with the longest mount point that starts it.
  const diskOf = (folder: string) => (info?.disks ?? []).filter((d) => folder === d.mount || folder.startsWith(d.mount === '/' ? '/' : `${d.mount}/`)).sort((a, b) => b.mount.length - a.mount.length)[0]?.mount ?? '/';
  const diskOptions: DropOption[] = (info?.disks ?? []).map((d) => ({
    value: d.mount,
    label: d.mount === '/' ? t('System disk') : d.mount,
    detail: t('{free} free of {total}', { free: sz(d.free), total: sz(d.total) }),
  }));
  /** The folder Strata Cut uses on a disk: its default one on the system disk, else <disk>/strata-cut/<kind>. */
  const folderOn = (mount: string, kind: 'loops' | 'clips') => (mount === '/' ? '' : `${mount}/strata-cut/${kind}`);
  const loopsFolder = str('recordingsDir') || info?.recordings.dir || '';
  const clipsFolder = str('clipsDir') || info?.clips.dir || '';
  const capacity = info?.capacity ?? null;
  const reserved = info?.reserved ?? 0;
  const ratio = capacity ? reserved / capacity : 0;
  const tone = ratio >= 1 ? 'tone-bad' : ratio >= 0.85 ? 'tone-warn' : 'tone-ok';
  // With every loop sharing the space, how long they can be.
  const perHour = (info?.recorders ?? []).filter((r) => r.enabled && r.need).reduce((a, r) => a + (r.need ?? 0) / r.loopHours, 0);
  const possibleHours = capacity && perHour ? capacity / perHour : null;

  return (
    <>
      <PageHead title={t('Storage')} sub={disk ? t('Free {free} of {total}', { free: sz(disk.free), total: sz(disk.total) }) : undefined} />
      <Card icon={<HardDrive size={16} />} title={t('Storage')}>
        {cfg && (
          <div className="set-grid two">
            <Field
              label={
                <>
                  {t('Disk of the loops')}
                  {info && !info.prepares && <InfoTip text={t('Another disk needs the install script (it prepares the folder).')} />}
                </>
              }
              group
              hint={<code>{loopsFolder}</code>}
            >
              <Dropdown value={diskOf(loopsFolder)} options={diskOptions} onChange={(m) => setCfg((c) => ({ ...c, recordingsDir: folderOn(m, 'loops') }))} />
            </Field>
            <Field label={t('Disk of the clips')} group hint={<code>{clipsFolder}</code>}>
              <Dropdown value={diskOf(clipsFolder)} options={diskOptions} onChange={(m) => setCfg((c) => ({ ...c, clipsDir: folderOn(m, 'clips') }))} />
            </Field>
          </div>
        )}
        {disk && (
          <div className="sc-disk">
            <div className="sc-disk-bar">
              <div style={{ width: `${pct(loops)}%`, background: 'var(--accent)' }} />
              {same && <div style={{ width: `${pct(clips)}%`, background: 'var(--key)' }} />}
              <div style={{ width: `${pct(other)}%`, background: 'var(--line-2)' }} />
            </div>
            <div className="sc-legend">
              <span>
                <i style={{ background: 'var(--accent)' }} />
                {t('Loops')} {sz(loops)}
              </span>
              <span>
                <i style={{ background: 'var(--key)' }} />
                {t('Clips')} {sz(clips)}
              </span>
              <span>
                <i style={{ background: 'var(--line-2)' }} />
                {t('Other')} {sz(other)}
              </span>
              <span>
                <i style={{ background: 'var(--panel-3)' }} />
                {t('Free')} {sz(disk.free)}
              </span>
            </div>
          </div>
        )}
      </Card>

      <Card icon={<PieChart size={16} />} title={t('Space for each use')}>
        {cfg && (
          <div className="set-grid">
            <Field
              label={
                <>
                  {t('Loops at most')}
                  <InfoTip text={t('0 = the whole disk')} />
                </>
              }
              group
            >
              <NumberField value={num('loopsMaxGb', 0)} min={0} max={1_000_000} step={10} precision={1} suffix={fr ? 'Go' : 'GB'} onChange={(v) => setCfg((c) => ({ ...c, loopsMaxGb: v }))} />
            </Field>
            <Field
              label={
                <>
                  {t('Clips at most')}
                  <InfoTip text={t('0 = the whole disk')} />
                </>
              }
              group
            >
              <NumberField value={num('clipsMaxGb', 0)} min={0} max={1_000_000} step={10} precision={1} suffix={fr ? 'Go' : 'GB'} onChange={(v) => setCfg((c) => ({ ...c, clipsMaxGb: v }))} />
            </Field>
            <Field
              label={
                <>
                  {t('Always free')}
                  <InfoTip text={t('When the disk gets this full, the oldest recordings are removed first.')} />
                </>
              }
              group
            >
              <NumberField value={num('keepFreePercent', 5)} min={1} max={50} suffix="%" onChange={(v) => setCfg((c) => ({ ...c, keepFreePercent: v }))} />
            </Field>
          </div>
        )}
        {capacity !== null && (
          <div className="sc-reserve">
            <div className="sc-loop-head">
              <span>
                {t('Reserved by the loops')} <b>{sz(reserved)}</b> {t('of {cap} possible', { cap: sz(capacity) })}
              </span>
              <span>{ratio > 9.99 ? '> 999 %' : `${Math.round(ratio * 100)} %`}</span>
            </div>
            <span className={`perf-bar ${tone}`}>
              <span className="perf-bar-fill" style={{ transform: `translateX(${Math.min(100, ratio * 100) - 100}%)` }} />
            </span>
            {ratio > 1 && possibleHours !== null && (
              <span className="sc-warnline">
                <TriangleAlert size={12} /> {t('Not enough space: the loops will last about {d}.', { d: possibleHours >= 1 ? `${Math.floor(possibleHours)} h ${String(Math.floor((possibleHours % 1) * 60)).padStart(2, '0')}` : `${Math.max(1, Math.round(possibleHours * 60))} min` })}
              </span>
            )}
          </div>
        )}
      </Card>

      {info && info.recorders.length > 0 && (
        <table className="user-table sc-rec-table">
          <thead>
            <tr>
              <th>{t('Recorders')}</th>
              <th>{t('Bitrate')}</th>
              <th>{t('Loop length')}</th>
              <th>{t('Reserved')}</th>
              <th>{t('Used now')}</th>
            </tr>
          </thead>
          <tbody>
            {info.recorders.map((r) => (
              <tr key={r.id} className={r.enabled ? '' : 'dim'}>
                <td>
                  {r.name}
                  {r.spaceLimited && (
                    <span className="sc-warnline" style={{ marginLeft: 8 }} title={t('Loop shortened for lack of disk space')}>
                      <TriangleAlert size={12} />
                    </span>
                  )}
                </td>
                <td className="num">
                  {r.bitrate ? bitrate(r.bitrate, locale()) : '—'}
                  {r.expected > 0 && <span className="muted"> · {t('expected {e}', { e: bitrate(r.expected * 1e6, locale()) })}</span>}
                </td>
                <td className="num">{r.loopHours} h</td>
                <td className="num">{r.enabled && r.need ? sz(r.need) : '—'}</td>
                <td className="num">{sz(r.bytes)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <SaveBar dirty={dirty} saving={saving} onSave={() => void save()} onDiscard={discard} />
    </>
  );
}
