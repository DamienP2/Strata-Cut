/* Système › Serveur › Stockage: where the loops and the clips go, and how full the disks are. */
import { useCallback, useEffect, useState } from 'react';
import { HardDrive } from 'lucide-react';
import { lang, locale, t } from '../i18n';
import { api } from '../store';
import { PageHead } from '../pages/SidePage';
import { size } from '../tc';
import { Card, Field, SaveBar } from './ui';
import { useServerConfig } from './serverConfig';

interface Disk {
  total: number;
  free: number;
}
interface StorageInfo {
  recordings: { dir: string; disk: Disk | null };
  clips: { dir: string; disk: Disk | null; bytes: number };
  recorders: { id: string; name: string; bytes: number; loopHours: number; full: number }[];
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

  const fr = lang() === 'fr';
  const sz = (b: number) => size(b, locale(), fr);
  const disk = info?.recordings.disk;
  const loops = info?.recorders.reduce((a, r) => a + r.bytes, 0) ?? 0;
  const clips = info?.clips.bytes ?? 0;
  const sameDisk = !!disk && !!info?.clips.disk && info.clips.disk.total === disk.total && info.clips.disk.free === disk.free;
  const pct = (b: number) => (disk && disk.total ? (b / disk.total) * 100 : 0);
  const other = disk ? Math.max(0, disk.total - disk.free - loops - (sameDisk ? clips : 0)) : 0;
  const str = (k: string) => (typeof cfg?.[k] === 'string' ? (cfg[k] as string) : '');

  return (
    <>
      <PageHead title={t('Storage')} sub={disk ? t('Free {free} of {total}', { free: sz(disk.free), total: sz(disk.total) }) : undefined} />
      <Card icon={<HardDrive size={16} />} title={t('Storage')}>
        {cfg && (
          <div className="set-grid two">
            <Field label={t('Recordings folder')}>
              <input value={str('recordingsDir')} placeholder={info?.recordings.dir} spellCheck={false} onChange={(e) => setCfg((c) => ({ ...c, recordingsDir: e.target.value }))} />
            </Field>
            <Field label={t('Clips folder')}>
              <input value={str('clipsDir')} placeholder={info?.clips.dir} spellCheck={false} onChange={(e) => setCfg((c) => ({ ...c, clipsDir: e.target.value }))} />
            </Field>
          </div>
        )}
        {disk && (
          <div className="sc-disk">
            <div className="sc-disk-bar">
              <div style={{ width: `${pct(loops)}%`, background: 'var(--accent)' }} />
              {sameDisk && <div style={{ width: `${pct(clips)}%`, background: 'var(--key)' }} />}
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
            </div>
          </div>
        )}
      </Card>
      {info && info.recorders.length > 0 && (
        <table className="user-table sc-rec-table">
          <thead>
            <tr>
              <th>{t('Recorders')}</th>
              <th>{t('Loop length')}</th>
              <th>{t('Used now')}</th>
              <th title={t('At the current bit rate')}>{t('Full loop')}</th>
            </tr>
          </thead>
          <tbody>
            {info.recorders.map((r) => (
              <tr key={r.id}>
                <td>{r.name}</td>
                <td className="num">{r.loopHours} h</td>
                <td className="num">{sz(r.bytes)}</td>
                <td className="num">{r.full ? sz(r.full) : '—'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <SaveBar dirty={dirty} saving={saving} onSave={() => void save()} onDiscard={discard} />
    </>
  );
}
