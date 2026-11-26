/*
 * Système › Serveur › Stockage: how full the space of Strata LiveCut is: the recordings (the loops)
 * and the clips, then each recorder; and the disks they are written on. The loops and the clips
 * share their disk: when it is full, the oldest recordings go first.
 */
import { useCallback, useEffect, useState } from 'react';
import { Expand, HardDrive, PieChart, Radio, TriangleAlert } from 'lucide-react';
import { lang, locale, t, tn } from '../i18n';
import { api, useApp, useChanged } from '../store';
import { PageHead } from '../pages/SidePage';
import { Dropdown, type DropOption } from '../ui/Dropdown';
import { InfoTip } from '../ui/InfoTip';
import { RecLogo } from '../ui/RecLogo';
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
/** One disk as Strata LiveCut sees it: `available` = total − other files − the system's reserve. */
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
interface RecorderUse {
  id: string;
  name: string;
  enabled: boolean;
  loopHours: number;
  expected: number;
  bitrate: number;
  bytes: number;
  loopBytes?: number;
  need: number | null;
  spaceLimited: boolean;
  clips: number;
  clipBytes: number;
}
interface StorageInfo {
  recordings: { dir: string; bytes: number };
  /** What programmed slots recorded, kept longer than the loops (counted in the recordings' bytes too). */
  programmed: { bytes: number; count: number };
  clips: { dir: string; bytes: number; sameDisk: boolean; count: number; orphans: number; orphanBytes: number };
  capacity: number | null;
  reserved: number;
  disks: { mount: string; device: string; fs: string; total: number; free: number }[];
  prepares: boolean;
  loopsDisk: Usage | null;
  clipsDisk: Usage | null;
  recorders: RecorderUse[];
}

/** Worth offering: more than 1 GB. */
const WORTH = 1e9;
const pctOf = (part: number, whole: number) => (whole > 0 ? Math.max(0, Math.min(100, (part / whole) * 100)) : 0);
const pctText = (p: number) => (p > 0 && p < 1 ? '< 1 %' : `${Math.round(p)} %`);
const toneOf = (p: number) => (p >= 95 ? 'tone-bad' : p >= 85 ? 'tone-warn' : 'tone-ok');

export function Storage() {
  const { cfg, setCfg, dirty, saving, save, discard } = useServerConfig();
  const recorders = useApp((s) => s.recorders);
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
  const str = (k: string) => (typeof cfg?.[k] === 'string' ? (cfg[k] as string) : '');
  const num = (k: string) => (typeof cfg?.[k] === 'number' ? (cfg[k] as number) : 0);

  // The disks: a folder is on the disk with the longest mount point that starts it.
  const diskOf = (folder: string) => (info?.disks ?? []).filter((d) => folder === d.mount || folder.startsWith(d.mount === '/' ? '/' : `${d.mount}/`)).sort((a, b) => b.mount.length - a.mount.length)[0]?.mount ?? '/';
  const diskOptions: DropOption[] = (info?.disks ?? []).map((d) => ({
    value: d.mount,
    label: d.mount === '/' ? t('System disk') : d.mount,
    detail: t('{free} free of {total}', { free: sz(d.free), total: sz(d.total) }),
  }));
  /** The folder Strata LiveCut uses on a disk: its default one on the system disk, else <disk>/strata-cut/<kind>. */
  const folderOn = (mount: string, kind: 'loops' | 'clips') => (mount === '/' ? '' : `${mount}/strata-cut/${kind}`);
  const loopsFolder = str('recordingsDir') || info?.recordings.dir || '';
  const clipsFolder = str('clipsDir') || info?.clips.dir || '';
  const moved = !!info && (diskOf(loopsFolder) !== diskOf(info.recordings.dir) || diskOf(clipsFolder) !== diskOf(info.clips.dir));

  const ld = info?.loopsDisk ?? null;
  const cd = info?.clipsDisk ?? null;
  const same = !!info?.clips.sameDisk;
  // The loops, and apart what programmed slots recorded (kept longer).
  const programmed = Math.min(info?.programmed?.bytes ?? 0, info?.recordings.bytes ?? 0);
  const loops = (info?.recordings.bytes ?? 0) - programmed;
  const clips = info?.clips.bytes ?? 0;
  const reserved = info?.reserved ?? 0;
  // The space of Strata LiveCut: what the disk(s) leave available.
  const loopsSpace = ld?.available ?? 0;
  const clipsSpace = same ? loopsSpace : (cd?.available ?? 0);
  const total = same ? loopsSpace : loopsSpace + clipsSpace;
  const used = loops + programmed + clips;
  const loopsPct = pctOf(loops, loopsSpace);
  const programmedPct = pctOf(programmed, loopsSpace);
  const clipsPct = pctOf(clips, clipsSpace);
  // The loops once full: what they will take (the recorders' reserve).
  const fullPct = pctOf(reserved, loopsSpace - (same ? clips : 0));

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
      await load();
    } catch (e) {
      showError(e);
    } finally {
      setExtending(null);
    }
  };

  /** The space of a disk for Strata LiveCut: the recordings, the clips, what is free. */
  const spaceBar = (u: Usage, withLoops: boolean, withClips: boolean) => {
    const a = u.available;
    const l = withLoops ? u.loops : 0;
    const c = withClips ? u.clips : 0;
    const p = withLoops ? Math.min(programmed, l) : 0;
    return (
      <div className="sc-st-bar" role="img" aria-label={t('{used} used of {total}', { used: sz(l + c), total: sz(a) })}>
        {withLoops && <span className="loops" style={{ width: `${pctOf(l - p, a)}%` }} />}
        {p > 0 && <span className="prog" style={{ width: `${pctOf(p, a)}%` }} />}
        {withClips && <span className="clips" style={{ width: `${pctOf(c, a)}%` }} />}
        {/* Where the loops will be once full. */}
        {withLoops && reserved > 0 && <i className="sc-st-mark" style={{ left: `${pctOf(reserved + (withClips ? c : 0), a)}%` }} title={t('The loops once full: {s}', { s: sz(reserved) })} />}
      </div>
    );
  };

  const diskView = (u: Usage, title: string) => {
    const p = u.physical;
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
          <div style={{ width: `${pctOf(u.others, u.total)}%`, background: 'var(--line-2)' }} />
          <div className="sc-hatch" style={{ width: `${pctOf(u.reserve, u.total)}%` }} />
          <div style={{ width: `${pctOf(u.loops, u.total)}%`, background: 'var(--accent)' }} />
          <div style={{ width: `${pctOf(u.clips, u.total)}%`, background: 'var(--key)' }} />
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
              <InfoTip text={t('Never used by Strata LiveCut, so that the system keeps working.')} />
            </dt>
            <dd>{sz(u.reserve)}</dd>
          </div>
          <div className="strong">
            <dt>{t('Available for Strata LiveCut')}</dt>
            <dd>{sz(u.available)}</dd>
          </div>
        </dl>
      </div>
    );
  };

  const recs = info?.recorders ?? [];
  const logoOf = (id: string) => recorders.find((r) => r.id === id);

  return (
    <>
      <PageHead title={t('Storage')} sub={info && total > 0 ? t('{used} used of {total} available for Strata LiveCut ({p})', { used: sz(used), total: sz(total), p: pctText(pctOf(used, total)) }) : undefined} />

      {info && ld && !moved && (
        <Card icon={<PieChart size={16} />} title={t('Occupancy')}>
          {same ? spaceBar(ld, true, true) : (
            <>
              {spaceBar(ld, true, false)}
              {cd && spaceBar(cd, false, true)}
            </>
          )}
          <div className="sc-st-legend">
            <span><i className="loops" /> {t('Recordings')}</span>
            <span><i className="prog" /> {t('Programmed recordings')}</span>
            <span><i className="clips" /> {t('Clips')}</span>
            <span><i className="free" /> {t('Free')}</span>
            {reserved > 0 && <span><i className="mark" /> {t('The loops once full')}</span>}
          </div>
          <div className="sc-st-tiles">
            <div className="sc-st-tile">
              <span className="sc-st-k"><i className="loops" /> {t('Recordings')}</span>
              <b className="sc-st-v">{pctText(loopsPct)}</b>
              <span className="sc-st-sub">{t('{used} of {total}', { used: sz(loops), total: sz(loopsSpace) })}</span>
              <span className={`perf-bar ${toneOf(fullPct)}`}>
                <span className="perf-bar-fill" style={{ transform: `translateX(${Math.min(100, fullPct) - 100}%)` }} />
              </span>
              <span className={`sc-st-sub ${fullPct >= 100 ? 'bad' : ''}`}>
                {reserved > 0 ? t('Full, the loops will take {s} ({p} of the space left to them)', { s: sz(reserved), p: pctText(fullPct) }) : t('No recorder records')}
                <InfoTip text={t('Each loop takes, once full, its length at its bitrate. When the disk is full the oldest recordings go first, and a loop is shorter than asked.')} />
              </span>
            </div>
            <div className="sc-st-tile">
              <span className="sc-st-k"><i className="prog" /> {t('Programmed recordings')}</span>
              <b className="sc-st-v">{pctText(programmedPct)}</b>
              <span className="sc-st-sub">{t('{used} of {total}', { used: sz(programmed), total: sz(loopsSpace) })} · {tn(info.programmed?.count ?? 0, '{n} programmed recording', '{n} programmed recordings')}</span>
              {cfg && (
                <Field label={<>{t('Keep the programmed recordings')}<InfoTip text={t('What a programmed recording recorded stays in the Cut’s timeline this long after it, even once the loop has gone past it. “Always”: until the loop is emptied by hand.')} /></>}>
                  <Dropdown
                    value={String(num('programmedKeepDays'))}
                    options={[0, 1, 7, 15, 30, 60, 90, 180, 365].map((d) => ({ value: String(d), label: d === 0 ? t('Always') : tn(d, '{n} day', '{n} days') }))}
                    onChange={(v) => setCfg((c) => ({ ...c, programmedKeepDays: Number(v) }))}
                  />
                </Field>
              )}
            </div>
            <div className="sc-st-tile">
              <span className="sc-st-k"><i className="clips" /> {t('Clips')}</span>
              <b className="sc-st-v">{pctText(clipsPct)}</b>
              <span className="sc-st-sub">{t('{used} of {total}', { used: sz(clips), total: sz(clipsSpace) })} · {tn(info.clips.count, '{n} clip', '{n} clips')}</span>
              {cfg && (
                <Field label={<>{t('Keep the clips')}<InfoTip text={t('The clips older than this (since they were made) are deleted by themselves, checked every half hour. “Always”: they stay until deleted by hand.')} /></>}>
                  <Dropdown
                    value={String(num('clipsKeepDays'))}
                    options={[0, 1, 7, 15, 30, 60, 90, 180, 365].map((d) => ({ value: String(d), label: d === 0 ? t('Always') : tn(d, '{n} day', '{n} days') }))}
                    onChange={(v) => setCfg((c) => ({ ...c, clipsKeepDays: Number(v) }))}
                  />
                </Field>
              )}
            </div>
          </div>
          <span className="set-hint">{same ? t('The recordings and the clips share the disk: when it is full, the oldest recordings go first.') : t('The recordings and the clips are on two disks: when one is full, its oldest files go first.')}</span>
        </Card>
      )}

      {info && recs.length > 0 && (
        <Card icon={<Radio size={16} />} title={t('By recorder')}>
          <table className="user-table sc-rec-table sc-st-table">
            <thead>
              <tr>
                <th>{t('Recorder')}</th>
                <th>{t('Loop')}</th>
                <th className="num">{t('Recordings')}</th>
                <th className="num">{t('Clips')}</th>
                <th>{t('Share of the space')}</th>
              </tr>
            </thead>
            <tbody>
              {recs.map((r) => {
                // The loop alone (what programmed slots recorded before it is kept apart).
                const fill = r.need ? pctOf(r.loopBytes ?? r.bytes, r.need) : 0;
                const share = pctOf(r.bytes + r.clipBytes, total);
                return (
                  <tr key={r.id} className={r.enabled ? '' : 'dim'}>
                    <td>
                      <span className="sc-st-name">
                        <RecLogo rec={logoOf(r.id)} size="sm" />
                        <b>{r.name}</b>
                        {r.spaceLimited && (
                          <span className="sc-warnline" title={t('Loop shortened for lack of disk space')}>
                            <TriangleAlert size={12} />
                          </span>
                        )}
                      </span>
                      <span className="muted sc-st-rate">{r.bitrate ? bitrate(r.bitrate, locale()) : r.enabled ? '—' : t('Stopped')}</span>
                    </td>
                    <td className="sc-st-loop">
                      <span className="sc-st-row">
                        <span>{t('{p} of {h} h', { p: pctText(fill), h: r.loopHours })}</span>
                        <span className="muted">{r.need && r.enabled ? t('{s} once full', { s: sz(r.need) }) : ''}</span>
                      </span>
                      <span className={`perf-bar ${r.spaceLimited ? 'tone-warn' : 'tone-accent'}`}>
                        <span className="perf-bar-fill" style={{ transform: `translateX(${fill - 100}%)` }} />
                      </span>
                    </td>
                    <td className="num">{sz(r.bytes)}</td>
                    <td className="num">
                      {r.clips ? (
                        <>
                          {sz(r.clipBytes)} <span className="muted">· {r.clips}</span>
                        </>
                      ) : (
                        '—'
                      )}
                    </td>
                    <td className="sc-st-share">
                      <span className="sc-st-row">
                        <span>{pctText(share)}</span>
                      </span>
                      <span className="sc-st-minibar">
                        <span className="loops" style={{ width: `${pctOf(r.bytes, total)}%` }} />
                        <span className="clips" style={{ width: `${pctOf(r.clipBytes, total)}%` }} />
                      </span>
                    </td>
                  </tr>
                );
              })}
              {info.clips.orphans > 0 && (
                <tr className="dim">
                  <td>{t('Recorders deleted')}</td>
                  <td />
                  <td className="num">—</td>
                  <td className="num">
                    {sz(info.clips.orphanBytes)} <span className="muted">· {info.clips.orphans}</span>
                  </td>
                  <td className="sc-st-share">{pctText(pctOf(info.clips.orphanBytes, total))}</td>
                </tr>
              )}
            </tbody>
          </table>
        </Card>
      )}

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
      <SaveBar dirty={dirty} saving={saving} onSave={() => void save()} onDiscard={discard} />
    </>
  );
}
