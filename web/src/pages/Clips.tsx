import { useCallback, useEffect, useState } from 'react';
import { useDisplayPref } from '../displayPrefs';
import { useShallow } from 'zustand/react/shallow';
import { Clapperboard, Download, Play, Search, Trash2 } from 'lucide-react';
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
  const load = useCallback(async () => {
    try {
      setClips(await api<Clip[]>('GET', '/api/clips'));
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  }, [toast]);
  useEffect(() => void load(), [load]);
  useChanged('clips', load);

  if (!clips) return <div className="loading">{t('Loading…')}</div>;
  const total = clips.reduce((a, c) => a + c.size, 0);
  const sources = [...new Map(clips.map((c) => [c.recorder, c.recorderName])).entries()];
  const shown = clips.filter((c) => (!source || c.recorder === source) && (!q || `${c.name} ${c.recorderName}`.toLowerCase().includes(q.toLowerCase())));
  const now = Date.now();
  const fr = lang() === 'fr';

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
          <table className="user-table">
            <thead>
              <tr>
                <th>{t('Clip')}</th>
                <th>{t('Source')}</th>
                <th>{t('In')}</th>
                <th>{t('Duration')}</th>
                <th>{t('Format')}</th>
                <th>{t('Size')}</th>
                <th>{t('Created')}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {shown.map((c) => {
                const fps = fpsOf(c.fpsNum, c.fpsDen);
                return (
                  <tr key={c.id}>
                    <td>
                      <div className="sc-clip-name sc-clip-open" title={t('Play')} onClick={() => setOpen(c)}>
                        <div className="sc-mini">
                          <img src={`/api/clips/${c.id}/thumb.jpg`} alt="" onError={(e) => (e.currentTarget.style.visibility = 'hidden')} />
                        </div>
                        <strong title={c.name}>{c.name}</strong>
                      </div>
                    </td>
                    <td>{c.recorderName}</td>
                    <td className="num" title={day(c.start, timezone, locale())}>
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
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
      {open && <ClipPlayer clip={open} onClose={() => setOpen(null)} />}
    </div>
  );
}
