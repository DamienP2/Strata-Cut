/*
 * Système › Serveur › Stockage: the real size of each disk, what the system and other
 * files keep, what is left for Strata Cut, and how it is shared between loops and clips.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { Expand, HardDrive, PieChart, TriangleAlert } from 'lucide-react';
import { lang, locale, t, tn } from '../i18n';
import { api, useChanged } from '../store';
import { PageHead } from '../pages/SidePage';
import { NumberField } from '../ui/fields';
import { Dropdown, type DropOption } from '../ui/Dropdown';
import { InfoTip } from '../ui/InfoTip';
import { confirmDialog } from '../ui/dialogs';
import { showError } from '../ui/serverError';
import { bitrate, size } from '../tc';
import { Card, Field, SaveBar } from './ui';
import { useServerConfig } from './serverConfig';

interface Physical {
  disk: string;
  size: number;
  lvmFree: number;
  unpartitioned: number;
  /** What extending the volume gives: free space of its LVM group + end of the disk outside any partition. */
  growable: number;
  lv: string | null;
}
/** One disk as Strata Cut sees it: `available` = total − other files − the system's reserve. */
interface Usage {
  mount: string;
  total: number;
  free: number;
  others: number;
  reserve: number;
  available: number;
  loops: number;
  clips: number;
  physical: Physical | null;
}
interface StorageInfo {
  recordings: { dir: string; bytes: number };
  clips: { dir: string; bytes: number; sameDisk: boolean };
  capacity: number | null;
  reserved: number;
  disks: { mount: string; device: string; fs: string; total: number; free: number }[];
  prepares: boolean;
  loopsDisk: Usage | null;
  clipsDisk: Usage | null;
  recorders: { id: string; name: string; enabled: boolean; loopHours: number; expected: number; bitrate: number; bytes: number; need: number | null; spaceLimited: boolean }[];
}

const GB = 1e9;
/** Down to 0.1 GB, so that a value given is never more than what is there. */
const floorGb = (b: number) => Math.max(0, Math.floor(b / 1e8) / 10);
const ceilGb = (b: number) => Math.ceil(b / 1e8) / 10;
/** Worth offering: more than 1 GB. */
const WORTH = GB;

export function Storage() {
  const { cfg, setCfg, dirty, saving, save, discard } = useServerConfig();
  const [info, setInfo] = useState<StorageInfo | null>(null);
  const [extending, setExtending] = useState<string | null>(null);
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
  const unit = fr ? 'Go' : 'GB';
  const str = (k: string) => (typeof cfg?.[k] === 'string' ? (cfg[k] as string) : '');
  const num = (k: string) => (typeof cfg?.[k] === 'number' ? (cfg[k] as number) : 0);

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
  // A disk changed in the form but not saved yet: its figures are not known before saving.
  const moved = !!info && (diskOf(loopsFolder) !== diskOf(info.recordings.dir) || diskOf(clipsFolder) !== diskOf(info.clips.dir));

  const ld = info?.loopsDisk ?? null;
  const cd = info?.clipsDisk ?? null;
  const same = !!info?.clips.sameDisk;
  const reserved = info?.reserved ?? 0;
  // What the recorders already reserve: the loops cannot be given less.
  const loopsAvail = ld ? floorGb(ld.available) : 0;
  const loopsMin = Math.min(ceilGb(reserved), loopsAvail);
  const clipsAvail = same ? loopsAvail : cd ? floorGb(cd.available) : 0;
  const loopsGb = num('loopsMaxGb');
  const clipsGb = num('clipsMaxGb');

  // Nothing set yet (0 = no limit): propose a split of the available space, to save.
  const proposed = useRef(false);
  useEffect(() => {
    if (!cfg || !ld || proposed.current || moved) return;
    proposed.current = true;
    if (num('loopsMaxGb') > 0 && num('clipsMaxGb') > 0) return;
    if (same) {
      const clips = Math.min(Math.max(floorGb(ld.available * 0.1), 0.1), 200);
      const loops = Math.max(loopsMin, floorGb(ld.available) - clips);
      setCfg((c) => ({ ...c, loopsMaxGb: loops, clipsMaxGb: Math.max(0, Math.round((floorGb(ld.available) - loops) * 10) / 10) }));
    } else {
      setCfg((c) => ({ ...c, loopsMaxGb: num('loopsMaxGb') || floorGb(ld.available), clipsMaxGb: num('clipsMaxGb') || (cd ? floorGb(cd.available) : 0) }));
    }
  }, [cfg, ld, cd, same, moved]);

  const round = (v: number) => Math.round(v * 10) / 10;
  /** On one disk the two uses share what is available: moving one gives the rest to the other. */
  const setLoops = (v: number) => {
    const l = round(Math.min(Math.max(v, loopsMin), loopsAvail));
    setCfg((c) => ({ ...c, loopsMaxGb: l, ...(same ? { clipsMaxGb: round(Math.max(0, loopsAvail - l)) } : {}) }));
  };
  const setClips = (v: number) => {
    const k = round(Math.min(Math.max(v, 0), same ? loopsAvail - loopsMin : clipsAvail));
    setCfg((c) => ({ ...c, clipsMaxGb: k, ...(same ? { loopsMaxGb: round(Math.max(loopsMin, loopsAvail - k)) } : {}) }));
  };
  const over = same ? loopsGb + clipsGb > loopsAvail * 1.01 + 0.1 : loopsGb > loopsAvail * 1.01 + 0.1 || clipsGb > clipsAvail * 1.01 + 0.1;

  // With every loop sharing the loops' space, how long they can be.
  const perHour = (info?.recorders ?? []).filter((r) => r.enabled && r.need).reduce((a, r) => a + (r.need ?? 0) / r.loopHours, 0);
  const hours = (gb: number) => (perHour ? (gb * GB) / perHour : null);
  const dur = (h: number) => (h >= 1 ? `${Math.floor(h)} h ${String(Math.floor((h % 1) * 60)).padStart(2, '0')}` : `${Math.max(1, Math.round(h * 60))} min`);
  const loopsHours = hours(loopsGb);
  const ratio = loopsGb > 0 ? reserved / (loopsGb * GB) : 0;
  const tone = ratio >= 1 ? 'tone-bad' : ratio >= 0.85 ? 'tone-warn' : 'tone-ok';

  const extend = async (u: Usage) => {
    const p = u.physical;
    if (!p) return;
    const ok = await confirmDialog({
      title: t('Use the whole disk?'),
      message: t('The volume {mount} grows from {from} to about {to}, all the free space of the disk, in one go. The files stay; the service keeps running. A volume cannot be shrunk afterwards.', { mount: u.mount, from: sz(u.total), to: sz(u.total + p.growable) }),
      confirm: t('Use the whole disk'),
    });
    if (!ok) return;
    setExtending(u.mount);
    try {
      await api('POST', '/api/system/storage/extend', { mount: u.mount });
      proposed.current = false;
      await load();
    } catch (e) {
      showError(e);
    } finally {
      setExtending(null);
    }
  };

  const diskView = (u: Usage, title: string) => {
    const p = u.physical;
    const loopsAlloc = loopsGb * GB;
    const clipsAlloc = clipsGb * GB;
    const isLoops = u === ld;
    const isClips = same || u === cd;
    const pct = (b: number) => `${Math.max(0, Math.min(100, (b / u.total) * 100))}%`;
    const given = (isLoops ? loopsAlloc : 0) + (isClips ? clipsAlloc : 0);
    return (
      <div className="sc-disk" key={u.mount}>
        <div className="sc-disk-head">
          <b>{title}</b>
          <span className="muted">
            {u.mount === '/' ? t('System disk') : u.mount}
            {p && ` · ${p.disk} ${sz(p.size)}`}
          </span>
        </div>
        {p && p.lv && p.growable > WORTH && (
          <div className="sc-alert">
            <TriangleAlert size={14} />
            <span>{t('The volume uses only {total} of the {size} disk: {left} are not given to it yet.', { total: sz(u.total), size: sz(p.size), left: sz(p.growable) })}</span>
            <button className="btn primary small" disabled={extending !== null} onClick={() => void extend(u)}>
              <Expand size={14} /> {extending === u.mount ? t('Extending…') : t('Use the whole disk ({to})', { to: sz(u.total + p.growable) })}
            </button>
          </div>
        )}
        {p && !p.lv && p.unpartitioned > WORTH && (
          <div className="sc-alert">
            <TriangleAlert size={14} />
            <span>{t('{left} of the disk are in no partition: they must be partitioned by hand.', { left: sz(p.unpartitioned) })}</span>
          </div>
        )}
        <div className="sc-disk-bar">
          <div style={{ width: pct(u.others), background: 'var(--line-2)' }} />
          <div className="sc-hatch" style={{ width: pct(u.reserve) }} />
          {isLoops && <div style={{ width: pct(loopsAlloc), background: 'var(--accent)' }} />}
          {isClips && <div style={{ width: pct(clipsAlloc), background: 'var(--key)' }} />}
        </div>
        <dl className="sc-figures">
          <div>
            <dt>{t('Volume size')}</dt>
            <dd>{sz(u.total)}</dd>
          </div>
          <div>
            <dt>
              <i style={{ background: 'var(--line-2)' }} />
              {t('System and other files')}
            </dt>
            <dd>{sz(u.others)}</dd>
          </div>
          <div>
            <dt>
              <i className="sc-hatch" />
              {t('Kept for the system')}
              <InfoTip text={t('Never used by Strata Cut, so that the system keeps working.')} />
            </dt>
            <dd>{sz(u.reserve)}</dd>
          </div>
          <div className="strong">
            <dt>{t('Available for Strata Cut')}</dt>
            <dd>{sz(u.available)}</dd>
          </div>
          {u.available - given > WORTH && (
            <div>
              <dt>
                <i style={{ background: 'var(--panel-3)' }} />
                {t('Not given')}
              </dt>
              <dd>{sz(u.available - given)}</dd>
            </div>
          )}
        </dl>
      </div>
    );
  };

  const slider = (value: number, min: number, max: number, onChange: (v: number) => void, color: string) => (
    <div className="sc-slider">
      <input type="range" min={0} max={Math.max(max, 0.1)} step={0.1} value={Math.min(value, max)} disabled={max <= 0} onChange={(e) => onChange(Number(e.target.value))} style={{ accentColor: color }} />
      <NumberField value={value} min={min} max={Math.max(max, min)} step={1} precision={1} suffix={unit} onChange={onChange} />
    </div>
  );

  return (
    <>
      <PageHead title={t('Storage')} sub={ld ? t('{avail} available for Strata Cut', { avail: sz(ld.available + (!same && cd ? cd.available : 0)) }) : undefined} />
      <Card icon={<HardDrive size={16} />} title={t('Disks')}>
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
        {moved && (
          <span className="sc-warnline">
            <TriangleAlert size={12} /> {t('Save to see the space of the new disk.')}
          </span>
        )}
        {!moved && ld && diskView(ld, same ? t('Loops and clips') : t('Loops'))}
        {!moved && !same && cd && diskView(cd, t('Clips'))}
      </Card>

      {!moved && ld && cfg && (
        <Card icon={<PieChart size={16} />} title={t('Space for each use')}>
          <div className="sc-uses">
            <div className="sc-use">
              <div className="sc-use-head">
                <span>
                  <i style={{ background: 'var(--accent)' }} />
                  {t('Loops')}
                </span>
                <span className="muted">{loopsHours !== null ? t('about {d} of loop for each recorder', { d: dur(loopsHours) }) : t('of {avail} available', { avail: sz(loopsAvail * GB) })}</span>
              </div>
              {slider(loopsGb, loopsMin, loopsAvail, setLoops, 'var(--accent)')}
              <div className="sc-reserve">
                <div className="sc-loop-head">
                  <span>
                    {t('Reserved by the loops')} <b>{sz(reserved)}</b> {t('of {cap} possible', { cap: sz(loopsGb * GB) })}
                  </span>
                  <span>{ratio > 9.99 ? '> 999 %' : `${Math.round(ratio * 100)} %`}</span>
                </div>
                <span className={`perf-bar ${tone}`}>
                  <span className="perf-bar-fill" style={{ transform: `translateX(${Math.min(100, ratio * 100) - 100}%)` }} />
                </span>
              </div>
            </div>
            <div className="sc-use">
              <div className="sc-use-head">
                <span>
                  <i style={{ background: 'var(--key)' }} />
                  {t('Clips')}
                </span>
                <span className="muted">{t('{used} used', { used: sz(info?.clips.bytes ?? 0) })}</span>
              </div>
              {slider(clipsGb, 0, same ? Math.max(0, loopsAvail - loopsMin) : clipsAvail, setClips, 'var(--key)')}
              <Field label={<>{t('Keep the clips')}<InfoTip text={t('The clips older than this (since they were made) are deleted by themselves, checked every half hour. “Always”: they stay until deleted by hand.')} /></>}>
                <Dropdown
                  value={String(num('clipsKeepDays'))}
                  options={[0, 1, 7, 15, 30, 60, 90, 180, 365].map((d) => ({ value: String(d), label: d === 0 ? t('Always') : tn(d, '{n} day', '{n} days') }))}
                  onChange={(v) => setCfg((c) => ({ ...c, clipsKeepDays: Number(v) }))}
                />
              </Field>
            </div>
          </div>
          <span className="set-hint">{same ? t('The two share what the disk leaves available: moving one gives the rest to the other.') : t('Each use takes at most what its disk leaves available.')}</span>
          {over && (
            <span className="sc-warnline">
              <TriangleAlert size={12} /> {t('More than the disk leaves available now: lower it.')}
            </span>
          )}
        </Card>
      )}

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
