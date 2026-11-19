import { useCallback, useEffect, useState } from 'react';
import { useDisplayPref } from '../displayPrefs';
import { useShallow } from 'zustand/react/shallow';
import { Clapperboard, Download, Play, Search, Trash2, X } from 'lucide-react';
import { api, can, useApp, useChanged } from '../store';
import { lang, locale, t, tn } from '../i18n';
import { confirmDialog } from '../ui/dialogs';
import { day, duration, fpsOf, size, timecode } from '../tc';
import type { Clip } from '../types';
import { ClipPlayer } from './ClipPlayer';

/** "il y a 2 min", "il y a 3 h", else the date. */
function ago(ms: number, now: number, tz: string): string {
  const s = Math.max(0, (now - ms) / 1000);
  const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: 'auto' });
  if (s < 60) return rtf.format(0, 'second');
  if (s < 3600) return rtf.format(-Math.floor(s / 60), 'minute');
  if (s < 86400) return rtf.format(-Math.floor(s / 3600), 'hour');
  return day(ms, tz, locale());
}

export function Clips() {
  const { user, timezone, toast } = useApp(useShallow((s) => ({ user: s.auth?.user, timezone: s.timezone, toast: s.toast })));
  const [clips, setClips] = useState<Clip[] | null>(null);
  const [q, setQ] = useDisplayPref('clips.q', '');
  const [source, setSource] = useDisplayPref<string | null>('clips.source', null);
  const [open, setOpen] = useState<Clip | null>(null);
  /** The clips ticked, for actions on several at once. */
  const [picked, setPicked] = useState<Set<string>>(() => new Set());
  const load = useCallback(async () => {
    try {
      setClips(await api<Clip[]>('GET', '/api/clips'));
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  }, [toast]);
  useEffect(() => void load(), [load]);
  useChanged('clips', load);
  // A clip gone (deleted here or elsewhere) is no longer ticked.
  useEffect(() => {
    if (!clips) return;
    setPicked((p) => {
      const ids = new Set(clips.map((c) => c.id));
      const next = new Set([...p].filter((id) => ids.has(id)));
      return next.size === p.size ? p : next;
    });
  }, [clips]);

  if (!clips) return <div className="loading">{t('Loading…')}</div>;
  const total = clips.reduce((a, c) => a + c.size, 0);
  const sources = [...new Map(clips.map((c) => [c.recorder, c.recorderName])).entries()];
  const shown = clips.filter((c) => (!source || c.recorder === source) && (!q || `${c.name} ${c.recorderName}`.toLowerCase().includes(q.toLowerCase())));
  const now = Date.now();
  const fr = lang() === 'fr';

  // By recorder (the one with the newest clip first), then by day, the newest first.
  const groups = [...new Set(shown.map((c) => c.recorder))]
    .map((id) => {
      const list = shown.filter((c) => c.recorder === id).sort((a, b) => b.start - a.start);
      return { id, name: list[0].recorderName, list, size: list.reduce((a, c) => a + c.size, 0) };
    })
    .sort((a, b) => b.list[0].start - a.list[0].start);
  const toggle = (ids: string[], on: boolean) =>
    setPicked((p) => {
      const next = new Set(p);
      for (const id of ids) on ? next.add(id) : next.delete(id);
      return next;
    });
  const pickedClips = clips.filter((c) => picked.has(c.id));
  const removeMany = async () => {
    const n = pickedClips.length;
    if (!n || !(await confirmDialog({ title: tn(n, 'Delete {n} clip?', 'Delete {n} clips?'), confirm: t('Delete'), danger: true }))) return;
    let failed = 0;
    for (const c of pickedClips) {
      try {
        await api('DELETE', `/api/clips/${c.id}`);
      } catch {
        failed++;
      }
    }
    if (failed) toast(tn(failed, '{n} clip could not be deleted', '{n} clips could not be deleted'), 'error');
    setPicked(new Set());
    void load();
  };
  const downloadMany = () => {
    // One download after another (a browser lets a page start several).
    pickedClips.forEach((c, i) =>
      setTimeout(() => {
        const a = document.createElement('a');
        a.href = `/api/clips/${c.id}/download`;
        a.click();
      }, i * 400),
    );
  };

  const remove = async (c: Clip) => {
    if (!(await confirmDialog({ title: t('Delete the clip “{name}”?', { name: c.name }), confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/clips/${c.id}`);
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };

  return (
    <div className="library">
      <div className="library-inner wide">
        <div className="settings-head">
          <div>
            <h1>{t('Clips')}</h1>
            <p className="muted">
              {tn(clips.length, '{n} clip', '{n} clips')} · {size(total, locale(), fr)}
            </p>
          </div>
        </div>
        {clips.length > 0 && (
          <div className="media-toolbar">
            <label className="search">
              <Search size={14} />
              <input value={q} placeholder={t('Search clips')} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
            </label>
            <div className="asr-chips">
              <button className={`asr-chip ${source === null ? 'on' : ''}`} onClick={() => setSource(null)}>
                {t('All clips')} <span className="asr-chip-n">{clips.length}</span>
              </button>
              {sources.map(([id, name]) => (
                <button key={id} className={`asr-chip ${source === id ? 'on' : ''}`} onClick={() => setSource(id)}>
                  {name} <span className="asr-chip-n">{clips.filter((c) => c.recorder === id).length}</span>
                </button>
              ))}
            </div>
          </div>
        )}
        {shown.length === 0 ? (
          <div className="empty big">
            {q || source ? <Search size={22} /> : <Clapperboard size={22} />}
            <p>{q || source ? t('No match') : t('No clip')}</p>
          </div>
        ) : (
          <>
            {picked.size > 0 && (
              <div className="sc-selbar">
                <strong>{tn(picked.size, '{n} clip selected', '{n} clips selected')}</strong>
                <span className="muted">{size(pickedClips.reduce((a, c) => a + c.size, 0), locale(), fr)}</span>
                <div className="spacer" />
                <button type="button" className="btn small" onClick={downloadMany}>
                  <Download size={13} /> {t('Download')}
                </button>
                {can.extract(user) && (
                  <button type="button" className="btn small danger" onClick={() => void removeMany()}>
                    <Trash2 size={13} /> {t('Delete')}
                  </button>
                )}
                <button type="button" className="icon-btn ghost" data-tip={t('Clear the selection')} onClick={() => setPicked(new Set())}>
                  <X size={14} />
                </button>
              </div>
            )}
            {groups.map((g) => {
              const all = g.list.every((c) => picked.has(c.id));
              let lastDay = '';
              return (
                <section key={g.id} className="sc-clip-group">
                  <header>
                    <label className="check" data-tip={t('Select the clips of this recorder')}>
                      <input type="checkbox" checked={all} ref={(el) => { if (el) el.indeterminate = !all && g.list.some((c) => picked.has(c.id)); }} onChange={(e) => toggle(g.list.map((c) => c.id), e.target.checked)} />
                    </label>
                    <h2>{g.name}</h2>
                    <span className="muted">
                      {tn(g.list.length, '{n} clip', '{n} clips')} · {size(g.size, locale(), fr)}
                    </span>
                  </header>
                  <table className="user-table">
                    <thead>
                      <tr>
                        <th className="sc-pick" />
                        <th>{t('Clip')}</th>
                        <th>{t('In')}</th>
                        <th>{t('Duration')}</th>
                        <th>{t('Format')}</th>
                        <th>{t('Size')}</th>
                        <th>{t('Created')}</th>
                        <th />
                      </tr>
                    </thead>
                    <tbody>
                      {g.list.flatMap((c) => {
                        const fps = fpsOf(c.fpsNum, c.fpsDen);
                        const d = day(c.start, timezone, locale());
                        const rows = [];
                        if (d !== lastDay) {
                          lastDay = d;
                          rows.push(
                            <tr key={`day-${d}`} className="sc-day-row">
                              <td colSpan={8}>{d}</td>
                            </tr>,
                          );
                        }
                        rows.push(
                          <tr key={c.id} className={picked.has(c.id) ? 'picked' : ''}>
                            <td className="sc-pick">
                              <input type="checkbox" checked={picked.has(c.id)} aria-label={c.name} onChange={(e) => toggle([c.id], e.target.checked)} />
                            </td>
                            <td>
                              <div className="sc-clip-name sc-clip-open" title={t('Play')} onClick={() => setOpen(c)}>
                                <div className="sc-mini">
                                  <img src={`/api/clips/${c.id}/thumb.jpg`} alt="" onError={(e) => (e.currentTarget.style.visibility = 'hidden')} />
                                </div>
                                <strong title={c.name}>{c.name}</strong>
                              </div>
                            </td>
                            <td className="num" title={d}>
                              {timecode(c.start, fps, timezone)}
                            </td>
                            <td className="num">{duration(c.frames, fps)}</td>
                            <td>{c.format}</td>
                            <td className="num">{size(c.size, locale(), fr)}</td>
                            <td className="muted" title={c.createdBy}>
                              {ago(c.createdAt, now, timezone)}
                            </td>
                            <td style={{ textAlign: 'right', whiteSpace: 'nowrap' }}>
                              <button className="icon-btn ghost" title={t('Play')} onClick={() => setOpen(c)}>
                                <Play size={14} />
                              </button>{' '}
                              <a className="icon-btn ghost" title={t('Download')} href={`/api/clips/${c.id}/download`}>
                                <Download size={14} />
                              </a>{' '}
                              {can.extract(user) && (
                                <button className="icon-btn ghost" title={t('Delete')} onClick={() => void remove(c)}>
                                  <Trash2 size={14} />
                                </button>
                              )}
                            </td>
                          </tr>,
                        );
                        return rows;
                      })}
                    </tbody>
                  </table>
                </section>
              );
            })}
          </>
        )}
      </div>
      {open && <ClipPlayer clip={open} onClose={() => setOpen(null)} />}
    </div>
  );
}
