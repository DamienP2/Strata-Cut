import { useEffect, useMemo, useState } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { CircleDot, Copy, Pencil, Plus, RadioTower, Scissors, Search, Square, Trash2, TriangleAlert, WifiOff, X } from 'lucide-react';
import { api, can, serverNow, useApp } from '../store';
import { t, tn, locale } from '../i18n';
import { navigate } from '../router';
import { bitrate, scanFormat } from '../tc';
import { confirmDialog } from '../ui/dialogs';
import { Dropdown } from '../ui/Dropdown';
import { NumberField, Segmented } from '../ui/fields';
import { Field } from '../settings/ui';
import type { Protocol, Recorder, RecorderStatus } from '../types';

const PROTOCOLS: { value: Protocol; label: string; detail: string }[] = [
  { value: 'udp', label: 'UDP', detail: 'udp://' },
  { value: 'rtp', label: 'RTP', detail: 'rtp://' },
  { value: 'rtmp', label: 'RTMP', detail: 'rtmp://' },
  { value: 'srt', label: 'SRT', detail: 'srt://' },
];

const CODECS: Record<string, string> = { h264: 'H.264', hevc: 'HEVC', mpeg2video: 'MPEG-2', mpeg4: 'MPEG-4', prores: 'ProRes', dnxhd: 'DNxHD' };

/** Where the source sends or what we pull, as shown to people. */
export function sourceAddress(r: Recorder): string {
  const host = location.hostname;
  if (r.protocol === 'rtmp' && r.mode === 'listener') return `rtmp://${host}:${r.port}/${r.path}`;
  if (r.protocol === 'srt' && r.mode === 'listener') return `srt://${host}:${r.port}`;
  return r.url + (r.iface && (r.protocol === 'udp' || r.protocol === 'rtp') ? ` · ${r.iface}` : '');
}

/** "23 h 41" */
function hours(ms: number): string {
  const m = Math.floor(ms / 60_000);
  return `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, '0')}`;
}

/** "00:02:14" */
function hms(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const two = (n: number) => String(n).padStart(2, '0');
  return `${two(Math.floor(s / 3600))}:${two(Math.floor(s / 60) % 60)}:${two(s % 60)}`;
}

/** Changes every `ms`: for images that refresh. */
function useTick(ms: number) {
  const [n, setN] = useState(0);
  useEffect(() => {
    const i = setInterval(() => setN((x) => x + 1), ms);
    return () => clearInterval(i);
  }, [ms]);
  return n;
}

export function Recorders() {
  const { recorders, status, user } = useApp(useShallow((s) => ({ recorders: s.recorders, status: s.status, user: s.auth?.user })));
  const admin = can.admin(user);
  const [q, setQ] = useState('');
  const [chip, setChip] = useState<'all' | 'recording' | 'nosignal'>('all');
  const [edit, setEdit] = useState<Recorder | 'new' | null>(null);
  const counts = {
    all: recorders.length,
    recording: recorders.filter((r) => status[r.id]?.state === 'recording').length,
    nosignal: recorders.filter((r) => r.enabled && ['nosignal', 'error'].includes(status[r.id]?.state ?? '')).length,
  };
  const shown = recorders.filter((r) => {
    if (q && !`${r.name} ${sourceAddress(r)} ${r.protocol}`.toLowerCase().includes(q.toLowerCase())) return false;
    if (chip === 'recording') return status[r.id]?.state === 'recording';
    if (chip === 'nosignal') return r.enabled && ['nosignal', 'error'].includes(status[r.id]?.state ?? '');
    return true;
  });
  return (
    <div className="library">
      <div className="library-inner wide">
        <div className="settings-head">
          <div>
            <h1>{t('Recorders')}</h1>
            <p className="muted">
              {tn(counts.all, '{n} recorder', '{n} recorders')} · {tn(counts.recording, '{n} recording', '{n} recording')}
            </p>
          </div>
          <div className="spacer" />
          {admin && (
            <button className="btn primary" onClick={() => setEdit('new')}>
              <Plus size={14} /> {t('New recorder')}
            </button>
          )}
        </div>
        {recorders.length > 0 && (
          <div className="media-toolbar">
            <label className="search">
              <Search size={14} />
              <input value={q} placeholder={t('Search recorders')} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
            </label>
            <div className="asr-chips">
              <button className={`asr-chip ${chip === 'all' ? 'on' : ''}`} onClick={() => setChip('all')}>
                {t('All recorders')} <span className="asr-chip-n">{counts.all}</span>
              </button>
              <button className={`asr-chip ${chip === 'recording' ? 'on' : ''}`} onClick={() => setChip('recording')}>
                {t('Recording')} <span className="asr-chip-n">{counts.recording}</span>
              </button>
              {counts.nosignal > 0 && (
                <button className={`asr-chip ${chip === 'nosignal' ? 'on' : ''}`} onClick={() => setChip('nosignal')}>
                  {t('No signal')} <span className="asr-chip-n">{counts.nosignal}</span>
                </button>
              )}
            </div>
          </div>
        )}
        <div className="project-grid">
          {shown.map((r) => (
            <RecorderCard key={r.id} rec={r} st={status[r.id]} admin={admin} onEdit={() => setEdit(r)} />
          ))}
          {shown.length === 0 && (q || chip !== 'all') && (
            <div className="empty big">
              <Search size={22} />
              <p>{t('No match')}</p>
            </div>
          )}
          {shown.length === 0 && !admin && !q && chip === 'all' && (
            <div className="empty big">
              <RadioTower size={22} />
              <p>{t('No recorder')}</p>
            </div>
          )}
          {admin && !q && chip === 'all' && (
            <button className="project-new" onClick={() => setEdit('new')}>
              <Plus size={22} />
              <span>{t('New recorder')}</span>
            </button>
          )}
        </div>
      </div>
      {edit && <RecorderModal rec={edit === 'new' ? null : edit} onClose={() => setEdit(null)} />}
    </div>
  );
}

function RecorderCard({ rec, st, admin, onEdit }: { rec: Recorder; st?: RecorderStatus; admin: boolean; onEdit: () => void }) {
  const toast = useApp((s) => s.toast);
  const tick = useTick(2000);
  const [img, setImg] = useState(true);
  const state = st?.state ?? 'off';
  const recording = state === 'recording';
  useEffect(() => setImg(true), [tick]);
  const now = serverNow();
  const have = st?.oldest && st.newest ? st.newest - st.oldest : 0;
  const fill = Math.min(100, (have / (rec.loopHours * 3_600_000)) * 100);
  const m = st?.media;
  const fmt = [scanFormat(m), m ? CODECS[m.codec] ?? m.codec.toUpperCase() : '', recording && st?.bitrate ? bitrate(st.bitrate, locale()) : ''].filter(Boolean).join(' · ');

  const toggle = async () => {
    try {
      await api('POST', `/api/recorders/${rec.id}/enabled`, { enabled: !rec.enabled });
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  const remove = async () => {
    if (!(await confirmDialog({ title: t('Delete the recorder “{name}”?', { name: rec.name }), message: t('Its loop is deleted too. The clips stay.'), confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/recorders/${rec.id}`);
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };

  return (
    <article className="project-card">
      <div className="sc-thumb">
        {img && state !== 'off' ? <img src={`/api/recorders/${rec.id}/thumb.jpg?v=${recording ? tick : 0}`} alt="" onError={() => setImg(false)} /> : <div className="sc-pic lost" />}
        {recording && (
          <span className="sc-onpic tl">
            <span className="sc-rec" />
            REC
          </span>
        )}
        {state === 'nosignal' && (
          <span className="sc-onpic lostmsg">
            <WifiOff size={14} /> {t('No signal')}
          </span>
        )}
        {state === 'starting' && <span className="sc-onpic mid">{rec.protocol !== 'udp' && rec.protocol !== 'rtp' && rec.mode === 'listener' ? t('Waiting for the source') : t('Connecting…')}</span>}
        {state === 'off' && <span className="sc-onpic mid">{t('Stopped')}</span>}
        <span className="sc-onpic tr mono">{rec.protocol.toUpperCase()}</span>
      </div>
      <div className="project-body">
        <div className="sc-card-title">
          <span className={`nav-dot ${recording ? 'ok' : state === 'off' ? '' : state === 'error' ? 'bad' : 'warn'}`} />
          <strong>{rec.name}</strong>
        </div>
        <div className="sc-addr" title={sourceAddress(rec)}>
          {sourceAddress(rec)}
          {rec.mode === 'listener' && (rec.protocol === 'rtmp' || rec.protocol === 'srt') ? ` · ${t('listening')}` : ''}
        </div>
        {fmt && <div className="project-meta"><span>{fmt}</span></div>}
        {state === 'nosignal' && st && (
          <span className="sc-warnline">
            <TriangleAlert size={12} /> {t('No signal for {d}', { d: hms(now - st.since) })}
          </span>
        )}
        {state === 'nosignal' && st?.message && (
          <span className="sc-warnline sc-reason" title={st.message}>
            {t(st.message)}
          </span>
        )}
        {state === 'error' && st?.message && (
          <span className="sc-errline" title={st.message}>
            <TriangleAlert size={12} /> {t(st.message)}
          </span>
        )}
        <div className="sc-loop">
          <div className="sc-loop-head">
            <span>{t('Loop')} <b>{hours(have)}</b> {t('of {h} h', { h: rec.loopHours })}</span>
            <span>{fill >= 99.5 ? t('full') : `${Math.round(fill)} %`}</span>
          </div>
          <span className="perf-bar tone-accent">
            <span className="perf-bar-fill" style={{ transform: `translateX(${fill - 100}%)` }} />
          </span>
        </div>
      </div>
      <div className="sc-card-foot">
        <button className="btn small" onClick={() => void navigate(`/extract/${rec.id}`)} disabled={!st?.oldest}>
          <Scissors size={13} /> {t('Extract')}
        </button>
        <div className="spacer" />
        {admin && (
          <>
            <button className="icon-btn ghost" title={rec.enabled ? t('Stop recording') : t('Start recording')} onClick={() => void toggle()}>
              {rec.enabled ? <Square size={14} /> : <CircleDot size={14} />}
            </button>
            <button className="icon-btn ghost" title={t('Edit')} onClick={onEdit}>
              <Pencil size={14} />
            </button>
            <button className="icon-btn ghost" title={t('Delete')} onClick={() => void remove()}>
              <Trash2 size={14} />
            </button>
          </>
        )}
      </div>
    </article>
  );
}

const blank: Recorder = { id: '', name: '', protocol: 'srt', url: '', mode: 'listener', port: 9000, path: 'live/stream', latency: 120, passphrase: '', iface: '', loopHours: 24, enabled: true };

function RecorderModal({ rec, onClose }: { rec: Recorder | null; onClose: () => void }) {
  const { recorders, toast } = useApp(useShallow((s) => ({ recorders: s.recorders, toast: s.toast })));
  const firstPort = useMemo(() => {
    const used = new Set(recorders.map((r) => r.port));
    let p = 9000;
    while (used.has(p)) p++;
    return p;
  }, [recorders]);
  const [c, setC] = useState<Recorder>(() => rec ?? { ...blank, port: firstPort });
  const [busy, setBusy] = useState(false);
  const set = (patch: Partial<Recorder>) => setC((x) => ({ ...x, ...patch }));
  const listens = (c.protocol === 'rtmp' || c.protocol === 'srt') && c.mode === 'listener';
  const placeholder = { udp: 'udp://239.1.1.1:5000', rtp: 'rtp://239.1.1.1:5004', rtmp: 'rtmp://server/live/key', srt: 'srt://server:9000' }[c.protocol];
  const pushUrl = c.protocol === 'rtmp' ? `rtmp://${location.hostname}:${c.port}/${c.path}` : `srt://${location.hostname}:${c.port}`;

  const save = async () => {
    setBusy(true);
    try {
      if (rec) await api('PUT', `/api/recorders/${rec.id}`, c);
      else await api('POST', '/api/recorders', c);
      onClose();
    } catch (e) {
      toast(t((e as Error).message), 'error');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form className="modal wide" onPointerDown={(e) => e.stopPropagation()} onSubmit={(e) => { e.preventDefault(); void save(); }}>
        <header>
          <h2>{rec ? t('Edit {name}', { name: rec.name }) : t('New recorder')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="set-grid two">
          <Field label={t('Name')}>
            <input value={c.name} autoFocus onChange={(e) => set({ name: e.target.value })} />
          </Field>
          <Field label={t('Source')} group>
            <Dropdown value={c.protocol} options={PROTOCOLS.map((p) => ({ value: p.value, label: p.label, detail: p.detail }))} onChange={(v) => set({ protocol: v as Protocol })} />
          </Field>
          {(c.protocol === 'rtmp' || c.protocol === 'srt') && (
            <Field label={t('Mode')} group>
              <Segmented value={c.mode} options={[{ value: 'listener', label: t('Listen'), title: t('The source connects to this server') }, { value: 'caller', label: t('Call'), title: t('This server connects to the source') }]} onChange={(v) => set({ mode: v as Recorder['mode'] })} />
            </Field>
          )}
          {listens ? (
            <>
              <Field label={t('Port')}>
                <NumberField value={c.port} min={1} max={65535} onChange={(v) => set({ port: v })} />
              </Field>
              {c.protocol === 'rtmp' && (
                <Field label={t('Path')}>
                  <input value={c.path} placeholder="live/stream" onChange={(e) => set({ path: e.target.value })} />
                </Field>
              )}
              <Field label={t('Address for the source')} group>
                <div className="sc-url">
                  <code>{pushUrl}</code>
                  <button type="button" className="icon-btn" title={t('Copy')} onClick={() => void navigator.clipboard?.writeText(pushUrl)}>
                    <Copy size={14} />
                  </button>
                </div>
              </Field>
            </>
          ) : (
            <Field label={t('Address')}>
              <input value={c.url} placeholder={placeholder} spellCheck={false} onChange={(e) => set({ url: e.target.value })} />
            </Field>
          )}
          {(c.protocol === 'udp' || c.protocol === 'rtp') && (
            <Field label={t('Network interface')}>
              <input value={c.iface} placeholder={t('Optional')} spellCheck={false} onChange={(e) => set({ iface: e.target.value })} title={t('IP address of the card that receives the multicast')} />
            </Field>
          )}
          {c.protocol === 'srt' && (
            <>
              <Field label={t('Latency')}>
                <NumberField value={c.latency} min={20} max={8000} step={10} suffix="ms" onChange={(v) => set({ latency: v })} />
              </Field>
              <Field label={t('Passphrase')}>
                <input type="password" value={c.passphrase} placeholder={t('Optional')} autoComplete="new-password" onChange={(e) => set({ passphrase: e.target.value })} />
              </Field>
            </>
          )}
          <Field label={t('Loop length')}>
            <NumberField value={c.loopHours} min={1} max={168} suffix="h" onChange={(v) => set({ loopHours: v })} />
          </Field>
          <Field label={t('State')} group>
            <label className="check">
              <input type="checkbox" checked={c.enabled} onChange={(e) => set({ enabled: e.target.checked })} /> {t('Record')}
            </label>
          </Field>
        </div>
        <footer>
          <button type="button" className="btn" onClick={onClose}>
            {t('Cancel')}
          </button>
          <button type="submit" className="btn primary" disabled={busy || !c.name.trim()}>
            {rec ? (
              t('Save')
            ) : (
              <>
                <Plus size={14} /> {t('Create the recorder')}
              </>
            )}
          </button>
        </footer>
      </form>
    </div>
  );
}
