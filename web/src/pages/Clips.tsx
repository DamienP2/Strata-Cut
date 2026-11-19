/*
 * Extraits: the clips of the one signed in (and those shared with them; an admin may see all),
 * by recorder then by day, the newest first. Each says until when it is kept and with whom it
 * is shared; it is played, downloaded, shared, deleted, one or several at once.
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useDisplayPref } from '../displayPrefs';
import { useShallow } from 'zustand/react/shallow';
import { AudioLines, CalendarClock, Camera, Clapperboard, Film, Download, Globe2, Lock, Play, Search, Share2, Trash2, UsersRound, X } from 'lucide-react';
import { api, can, useApp, useChanged } from '../store';
import { lang, locale, t, tn } from '../i18n';
import { confirmDialog } from '../ui/dialogs';
import { RecLogo } from '../ui/RecLogo';
import { clock, day, duration, fpsOf, size, timecode } from '../tc';
import type { Clip } from '../types';
import { ClipPlayer } from './ClipPlayer';
import { loadGroups, useGroups, type Group } from './Groups';

/** "il y a 2 min", "il y a 3 h", else the date. */
function ago(ms: number, now: number, tz: string): string {
  const s = Math.max(0, (now - ms) / 1000);
  const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: 'auto' });
  if (s < 60) return rtf.format(0, 'second');
  if (s < 3600) return rtf.format(-Math.floor(s / 60), 'minute');
  if (s < 86400) return rtf.format(-Math.floor(s / 3600), 'hour');
  return day(ms, tz, locale());
}

/** "dans 3 h", "dans 2 jours". */
function within(ms: number, now: number): string {
  const s = Math.max(0, (ms - now) / 1000);
  const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: 'auto' });
  if (s < 3600) return rtf.format(Math.max(1, Math.round(s / 60)), 'minute');
  if (s < 86400 * 2) return rtf.format(Math.round(s / 3600), 'hour');
  return rtf.format(Math.round(s / 86400), 'day');
}

type Tab = 'mine' | 'shared' | 'all';

/** With whom a clip is shared, in words. */
function shareText(c: Clip, groups: Group[]): string {
  if (c.share?.all) return t('Everyone');
  const names = (c.share?.groups ?? []).map((g) => groups.find((x) => x.id === g)?.name).filter(Boolean) as string[];
  return names.length ? names.join(', ') : t('Private');
}

export function Clips() {
  const { user, timezone, toast, recorders } = useApp(useShallow((s) => ({ user: s.auth?.user, timezone: s.timezone, toast: s.toast, recorders: s.recorders })));
  const groups = useGroups((s) => s.groups) ?? [];
  const [clips, setClips] = useState<Clip[] | null>(null);
  const [q, setQ] = useDisplayPref('clips.q', '');
  const [source, setSource] = useDisplayPref<string | null>('clips.source', null);
  const [tab, setTab] = useDisplayPref<Tab>('clips.tab', 'mine');
  const [open, setOpen] = useState<Clip | null>(null);
  const [sharing, setSharing] = useState<Clip[] | null>(null);
  /** The clips ticked, for actions on several at once. */
  const [picked, setPicked] = useState<Set<string>>(() => new Set());
  const admin = can.admin(user);
  const load = useCallback(async () => {
    try {
      setClips(await api<Clip[]>('GET', '/api/clips'));
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  }, [toast]);
  useEffect(() => {
    void load();
    void loadGroups();
  }, [load]);
  useChanged('clips', load);
  useChanged('groups', () => void loadGroups());
  // A clip gone (deleted here or elsewhere), or no longer on this tab, is no longer ticked.
  const all = clips ?? [];
  const inTab = useMemo(() => all.filter((c) => (tab === 'all' && admin) || (tab === 'shared' ? c.sharedWithMe : c.mine)), [all, tab, admin]);
  useEffect(() => {
    setPicked((p) => {
      const ids = new Set(inTab.map((c) => c.id));
      const next = new Set([...p].filter((id) => ids.has(id)));
      return next.size === p.size ? p : next;
    });
  }, [inTab]);
  // Nothing of mine but shared clips: open on them.
  useEffect(() => {
    if (clips && tab === 'mine' && !clips.some((c) => c.mine) && clips.some((c) => c.sharedWithMe)) setTab('shared');
  }, [clips]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!clips) return <div className="loading">{t('Loading…')}</div>;
  const fr = lang() === 'fr';
  const now = Date.now();
  const count = { mine: all.filter((c) => c.mine).length, shared: all.filter((c) => c.sharedWithMe).length, all: all.length };
  const sources = [...new Map(inTab.map((c) => [c.recorder, c.recorderName])).entries()];
  const shown = inTab.filter((c) => (!source || c.recorder === source) && (!q || `${c.name} ${c.recorderName} ${c.createdBy}`.toLowerCase().includes(q.toLowerCase())));
  const total = inTab.reduce((a, c) => a + c.size, 0);
  const recOf = (id: string) => recorders.find((r) => r.id === id);
  const mayChange = (c: Clip) => admin || (!!c.mine && can.extract(user));

  // By recorder (the one with the newest clip first), then by day, the newest first.
  const byRecorder = [...new Set(shown.map((c) => c.recorder))]
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
  const pickedClips = inTab.filter((c) => picked.has(c.id));
  const removeMany = async () => {
    const list = pickedClips.filter(mayChange);
    const n = list.length;
    if (!n || !(await confirmDialog({ title: tn(n, 'Delete {n} clip?', 'Delete {n} clips?'), confirm: t('Delete'), danger: true }))) return;
    let failed = 0;
    for (const c of list) {
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

  const tabs: { id: Tab; label: string; n: number }[] = [
    { id: 'mine', label: t('My clips'), n: count.mine },
    { id: 'shared', label: t('Shared with me'), n: count.shared },
    ...(admin ? [{ id: 'all' as Tab, label: t('All clips'), n: count.all }] : []),
  ];

  return (
    <div className="library">
      <div className="library-inner wide sc-clips">
        <div className="settings-head">
          <div>
            <h1>{t('Clips')}</h1>
            <p className="muted">
              {tn(inTab.length, '{n} clip', '{n} clips')} · {size(total, locale(), fr)}
            </p>
          </div>
        </div>
        <div className="sc-clips-tabs" role="tablist">
          {tabs.map((x) => (
            <button key={x.id} type="button" role="tab" aria-selected={tab === x.id} className={`sc-clips-tab ${tab === x.id ? 'on' : ''}`} onClick={() => setTab(x.id)}>
              {x.id === 'mine' ? <Clapperboard size={14} /> : x.id === 'shared' ? <UsersRound size={14} /> : <Globe2 size={14} />}
              {x.label}
              <span className="asr-chip-n">{x.n}</span>
            </button>
          ))}
        </div>
        {inTab.length > 0 && (
          <div className="media-toolbar">
            <label className="search">
              <Search size={14} />
              <input value={q} placeholder={t('Search clips')} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
            </label>
            <div className="asr-chips">
              <button className={`asr-chip ${source === null ? 'on' : ''}`} onClick={() => setSource(null)}>
                {t('All recorders')} <span className="asr-chip-n">{inTab.length}</span>
              </button>
              {sources.map(([id, name]) => (
                <button key={id} className={`asr-chip ${source === id ? 'on' : ''}`} onClick={() => setSource(id)}>
                  <RecLogo rec={recOf(id)} size="sm" />
                  {name} <span className="asr-chip-n">{inTab.filter((c) => c.recorder === id).length}</span>
                </button>
              ))}
            </div>
          </div>
        )}
        {shown.length === 0 ? (
          <div className="empty big">
            {q || source ? <Search size={22} /> : <Clapperboard size={22} />}
            <p>{q || source ? t('No match') : tab === 'shared' ? t('No clip is shared with you') : t('No clip')}</p>
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
                {pickedClips.some(mayChange) && (
                  <button type="button" className="btn small" onClick={() => setSharing(pickedClips.filter(mayChange))}>
                    <Share2 size={13} /> {t('Share')}
                  </button>
                )}
                {pickedClips.some(mayChange) && (
                  <button type="button" className="btn small danger" onClick={() => void removeMany()}>
                    <Trash2 size={13} /> {t('Delete')}
                  </button>
                )}
                <button type="button" className="icon-btn ghost" data-tip={t('Clear the selection')} onClick={() => setPicked(new Set())}>
                  <X size={14} />
                </button>
              </div>
            )}
            {byRecorder.map((g) => {
              const allPicked = g.list.every((c) => picked.has(c.id));
              let lastDay = '';
              return (
                <section key={g.id} className="sc-clip-group">
                  <header>
                    <label className="sc-pick-all" data-tip={t('Select the clips of this recorder')}>
                      <input type="checkbox" checked={allPicked} ref={(el) => { if (el) el.indeterminate = !allPicked && g.list.some((c) => picked.has(c.id)); }} onChange={(e) => toggle(g.list.map((c) => c.id), e.target.checked)} />
                    </label>
                    <RecLogo rec={recOf(g.id)} />
                    <h2>{g.name}</h2>
                    <span className="muted">
                      {tn(g.list.length, '{n} clip', '{n} clips')} · {size(g.size, locale(), fr)}
                    </span>
                  </header>
                  <div className="sc-clip-table-wrap">
                    <table className="user-table sc-clip-table">
                      {/* The same widths for every recorder: the columns line up from one to the next. */}
                      <colgroup>
                        <col className="c-pick" />
                        <col className="c-clip" />
                        <col className="c-in" />
                        <col className="c-dur" />
                        <col className="c-size" />
                        <col className="c-made" />
                        <col className="c-until" />
                        <col className="c-share" />
                        <col className="c-act" />
                      </colgroup>
                      <thead>
                        <tr>
                          <th />
                          <th>{t('Clip')}</th>
                          <th>{t('In')}</th>
                          <th>{t('Duration')}</th>
                          <th>{t('Size')}</th>
                          <th>{t('Created')}</th>
                          <th>{t('Valid until')}</th>
                          <th>{t('Shared with')}</th>
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
                                <td colSpan={9}>{d}</td>
                              </tr>,
                            );
                          }
                          const soon = c.expiresAt != null && c.expiresAt - now < 86_400_000;
                          rows.push(
                            <tr key={c.id} className={picked.has(c.id) ? 'picked' : ''}>
                              <td>
                                <input type="checkbox" checked={picked.has(c.id)} aria-label={c.name} onChange={(e) => toggle([c.id], e.target.checked)} />
                              </td>
                              <td>
                                <div className="sc-clip-name sc-clip-open" onClick={() => setOpen(c)}>
                                  <div className={`sc-mini ${c.kind ?? 'video'}`}>
                                    {/* A sound: its icon, not a picture. */}
                                    {c.kind === 'audio' ? (
                                      <span className="sc-mini-sound">
                                        <AudioLines size={22} />
                                      </span>
                                    ) : (
                                      <img src={`/api/clips/${c.id}/thumb.jpg`} alt="" onError={(e) => (e.currentTarget.style.visibility = 'hidden')} />
                                    )}
                                    <span className="sc-mini-play">
                                      <Play size={14} />
                                    </span>
                                  </div>
                                  <span className="sc-clip-t">
                                    <strong title={c.name}>{c.name}</strong>
                                    <span className="sc-clip-sub">
                                      <span className={`sc-kind-tag ${c.kind ?? 'video'}`}>
                                        {c.kind === 'audio' ? <AudioLines size={11} /> : c.kind === 'image' ? <Camera size={11} /> : <Film size={11} />}
                                        {c.kind === 'audio' ? t('Sound') : c.kind === 'image' ? t('Image') : t('Video')}
                                      </span>
                                      <span className="muted">{c.mine ? c.format : t('By {name}', { name: c.createdBy })}</span>
                                    </span>
                                  </span>
                                </div>
                              </td>
                              <td className="num" title={d}>
                                {timecode(c.start, fps, timezone)}
                              </td>
                              <td className="num">{c.kind === 'image' ? <span className="muted">{t('Image')}</span> : duration(c.frames, fps)}</td>
                              <td className="num">{size(c.size, locale(), fr)}</td>
                              <td className="muted" title={c.createdBy}>
                                {ago(c.createdAt, now, timezone)}
                              </td>
                              <td className={soon ? 'sc-until soon' : 'sc-until'}>
                                {c.expiresAt != null ? (
                                  <>
                                    <span>
                                      {day(c.expiresAt, timezone, locale())} {clock(c.expiresAt, timezone).slice(0, 5)}
                                    </span>
                                    <span className="muted">{within(c.expiresAt, now)}</span>
                                  </>
                                ) : (
                                  <span className="muted">{t('No limit')}</span>
                                )}
                              </td>
                              <td>
                                {mayChange(c) ? (
                                  <button type="button" className={`sc-share-chip ${c.share?.all ? 'all' : c.share?.groups.length ? 'groups' : ''}`} title={t('Share')} onClick={() => setSharing([c])}>
                                    {c.share?.all ? <Globe2 size={12} /> : c.share?.groups.length ? <UsersRound size={12} /> : <Lock size={12} />}
                                    <span>{shareText(c, groups)}</span>
                                  </button>
                                ) : (
                                  <span className="sc-share-chip ro">
                                    <UsersRound size={12} />
                                    <span>{shareText(c, groups)}</span>
                                  </span>
                                )}
                              </td>
                              <td className="sc-clip-acts">
                                <button className="icon-btn ghost" title={t('Play')} onClick={() => setOpen(c)}>
                                  <Play size={14} />
                                </button>
                                <a className="icon-btn ghost" title={t('Download')} href={`/api/clips/${c.id}/download`}>
                                  <Download size={14} />
                                </a>
                                {mayChange(c) && (
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
                  </div>
                </section>
              );
            })}
          </>
        )}
        {inTab.some((c) => c.expiresAt != null) && (
          <p className="muted sc-clips-note">
            <CalendarClock size={13} /> {t('The clips are deleted by themselves at the end of their validity (Système › Stockage).')}
          </p>
        )}
      </div>
      {open && <ClipPlayer clip={open} onClose={() => setOpen(null)} />}
      {sharing && <ShareModal clips={sharing} groups={groups} onClose={() => setSharing(null)} onDone={() => void load()} />}
    </div>
  );
}

/** Shares clips: with nobody, with every user, or with groups of users. */
function ShareModal({ clips, groups, onClose, onDone }: { clips: Clip[]; groups: Group[]; onClose: () => void; onDone: () => void }) {
  const toast = useApp((s) => s.toast);
  const first = clips[0]?.share;
  const [mode, setMode] = useState<'none' | 'all' | 'groups'>(first?.all ? 'all' : first?.groups.length ? 'groups' : 'none');
  const [chosen, setChosen] = useState<Set<string>>(() => new Set(first?.groups ?? []));
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true);
    const body = { all: mode === 'all', groups: mode === 'groups' ? [...chosen] : [] };
    let failed = 0;
    for (const c of clips) {
      try {
        await api('PUT', `/api/clips/${c.id}/share`, body);
      } catch {
        failed++;
      }
    }
    setBusy(false);
    if (failed) toast(tn(failed, '{n} clip could not be shared', '{n} clips could not be shared'), 'error');
    else toast(mode === 'none' ? t('No longer shared') : t('Shared'));
    onDone();
    onClose();
  };
  const option = (id: 'none' | 'all' | 'groups', icon: React.ReactNode, title: string, sub: string, disabled = false) => (
    <label className={`sc-share-opt ${mode === id ? 'on' : ''} ${disabled ? 'off' : ''}`}>
      <input type="radio" name="share" checked={mode === id} disabled={disabled} onChange={() => setMode(id)} />
      <span className="sc-share-ic">{icon}</span>
      <span className="sc-share-txt">
        <strong>{title}</strong>
        <span className="muted">{sub}</span>
      </span>
    </label>
  );
  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form className="modal" onPointerDown={(e) => e.stopPropagation()} onSubmit={(e) => { e.preventDefault(); void save(); }} onKeyDown={(e) => e.key !== 'Escape' && e.stopPropagation()}>
        <header>
          <h2>{clips.length === 1 ? t('Share “{name}”', { name: clips[0].name }) : tn(clips.length, 'Share {n} clip', 'Share {n} clips')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="sc-share-opts">
          {option('none', <Lock size={15} />, t('Private'), t('Only you (and the admins) see it.'))}
          {option('all', <Globe2 size={15} />, t('Every user'), t('Everyone signed in finds it in “Shared with me”.'))}
          {option('groups', <UsersRound size={15} />, t('Groups of users'), groups.length ? t('Their members find it in “Shared with me”.') : t('No group yet: an admin makes them in Système › Groupes.'), groups.length === 0)}
        </div>
        {mode === 'groups' && groups.length > 0 && (
          <div className="sc-members-list">
            {groups.map((g) => (
              <label key={g.id} className={`sc-member-row ${chosen.has(g.id) ? 'on' : ''}`}>
                <input
                  type="checkbox"
                  checked={chosen.has(g.id)}
                  onChange={() =>
                    setChosen((s) => {
                      const n = new Set(s);
                      if (n.has(g.id)) n.delete(g.id);
                      else n.add(g.id);
                      return n;
                    })
                  }
                />
                <span className="user-avatar small sc-group-avatar">
                  <UsersRound size={11} />
                </span>
                <span className="sc-member-name">
                  <strong>{g.name}</strong>
                </span>
              </label>
            ))}
          </div>
        )}
        <footer>
          <button type="button" className="btn" onClick={onClose}>
            {t('Cancel')}
          </button>
          <button type="submit" className="btn primary" disabled={busy || (mode === 'groups' && chosen.size === 0)}>
            <Share2 size={14} /> {t('Save')}
          </button>
        </footer>
      </form>
    </div>
  );
}
