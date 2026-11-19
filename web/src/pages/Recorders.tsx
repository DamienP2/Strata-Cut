import { useEffect, useMemo, useState } from 'react';
import { useDisplayPref } from '../displayPrefs';
import { useShallow } from 'zustand/react/shallow';
import { CalendarClock, Copy, Eraser, MoreHorizontal, Pencil, Plus, RadioTower, Scissors, Search, Square, Trash2, TriangleAlert, Tv, WifiOff, X } from 'lucide-react';
import { api, can, serverNow, useApp } from '../store';
import { lang, locale, t, tn } from '../i18n';
import { navigate } from '../router';
import { bitrate, clock, day, scanFormat } from '../tc';
import { confirmDialog } from '../ui/dialogs';
import { Dropdown } from '../ui/Dropdown';
import { NumberField, Segmented } from '../ui/fields';
import { Field } from '../settings/ui';
import { InfoTip } from '../ui/InfoTip';
import { openContextMenu } from '../ui/ContextMenu';
import type { Protocol, Recorder, RecorderStatus } from '../types';

const PROTOCOLS: { value: Protocol; label: string; detail: string }[] = [
  { value: 'udp', label: 'UDP', detail: 'udp://' },
  { value: 'rtp', label: 'RTP', detail: 'rtp://' },
  { value: 'rtmp', label: 'RTMP', detail: 'rtmp://' },
  { value: 'srt', label: 'SRT', detail: 'srt://' },
  { value: 'web', label: 'Web', detail: 'https://' },
  { value: 'decklink', label: 'DeckLink', detail: 'SDI · HDMI' },
];

/** DeckLink: the card's inputs, the formats and the audio inputs, as the server knows them. */
const DL_CONNECTIONS = ['auto', 'sdi', 'hdmi', 'optical-sdi', 'component', 'composite', 'svideo'];
const DL_MODES = ['auto', '1080i50', '1080i5994', '1080i60', '1080p25', '1080p2997', '1080p30', '1080p50', '1080p5994', '1080p60', '1080p24', '1080p2398', '720p50', '720p5994', '720p60', 'pal', 'ntsc', '2160p25', '2160p2997', '2160p50', '2160p5994', '2160p60'];
const DL_AUDIO_INPUTS = ['auto', 'embedded', 'aes', 'analog'];
const dlConnection = (v: string) => ({ auto: t('Automatic'), sdi: 'SDI', hdmi: 'HDMI', 'optical-sdi': t('Optical SDI'), component: t('Component'), composite: t('Composite'), svideo: 'S-Video' })[v] ?? v;
/** "1080i50" → "1080i 50", "1080p5994" → "1080p 59.94" (French: 59,94). */
const dlMode = (v: string) => {
  if (v === 'auto') return t('Automatic (detected by the card)');
  if (v === 'pal') return 'PAL (576i 50)';
  if (v === 'ntsc') return 'NTSC (480i 59.94)'.replace('.', lang() === 'fr' ? ',' : '.');
  const m = /^(\d+[ip])(\d+)$/.exec(v);
  if (!m) return v;
  const rate = m[2].length > 2 ? `${m[2].slice(0, 2)}${lang() === 'fr' ? ',' : '.'}${m[2].slice(2)}` : m[2];
  return `${m[1]} ${rate}`;
};
const dlAudioInput = (v: string) => ({ auto: t('Automatic'), embedded: t('Embedded in the signal (SDI / HDMI)'), aes: 'AES/EBU', analog: t('Analogue') })[v] ?? v;

/** The platform of a web page, from its address (the server says the same). */
export function platformOf(url: string): string {
  const host = (url.split('://')[1] ?? url).split(/[/?#]/)[0].toLowerCase();
  const is = (d: string) => host === d || host.endsWith(`.${d}`);
  if (is('youtube.com') || is('youtu.be')) return 'youtube';
  if (is('facebook.com') || is('fb.watch') || is('fb.com')) return 'facebook';
  if (is('instagram.com')) return 'instagram';
  if (is('x.com') || is('twitter.com')) return 'x';
  if (is('linkedin.com')) return 'linkedin';
  return 'web';
}
const PLATFORM_NAME: Record<string, string> = { youtube: 'YouTube', facebook: 'Facebook', instagram: 'Instagram', x: 'X', linkedin: 'LinkedIn', web: 'Web' };
/** The badge on a recorder's picture: the protocol, or the platform of a web page. */
const badge = (r: Recorder) => (r.protocol === 'web' ? PLATFORM_NAME[platformOf(r.url)] : r.protocol === 'decklink' ? 'DeckLink' : r.protocol.toUpperCase());
const MASKED = '••••••••';

const CODECS: Record<string, string> = { h264: 'H.264', hevc: 'HEVC', mpeg2video: 'MPEG-2', mpeg4: 'MPEG-4', prores: 'ProRes', dnxhd: 'DNxHD' };

/** Where the source sends or what we pull, as shown to people. */
export function sourceAddress(r: Recorder): string {
  const host = location.hostname;
  if (r.protocol === 'rtmp' && r.mode === 'listener') return `rtmp://${host}:${r.port}/${r.path}`;
  if (r.protocol === 'srt' && r.mode === 'listener') return `srt://${host}:${r.port}`;
  if (r.protocol === 'decklink') return `DeckLink ${(r.device ?? 0) + 1} · ${dlConnection(r.connection ?? 'auto')} · ${dlMode(r.videoMode ?? 'auto')}`;
  return r.url + (r.iface && (r.protocol === 'udp' || r.protocol === 'rtp') ? ` · ${r.iface}` : '');
}

/** The list again, after an action (the live event may have been missed). */
async function reloadRecorders() {
  try {
    useApp.setState({ recorders: await api<Recorder[]>('GET', '/api/recorders') });
  } catch {
    /* the live events will tell */
  }
}

/** Bytes a full loop takes (as the server counts it: proxies and 5 % included). */
/** Bitrate assumed for a source not measured yet, Mb/s (as the server). */
const DEFAULT_MBPS = 8;
export const loopNeed = (mbps: number, hours: number) => ((mbps > 0 ? mbps : DEFAULT_MBPS) + 1.5) * 1e6 / 8 * 3600 * hours * 1.05;
/** Loop lengths on the slider (hours), and the ones a button gives at once. */
const LOOP_STEPS = [1, 2, 3, 4, 5, 6, 8, 10, 12, 16, 18, 20, 24, 30, 36, 48, 60, 72, 96, 120, 144, 168];
const LOOP_PRESETS = [6, 12, 24];

const stepOf = (h: number) => LOOP_STEPS.reduce((best, v, i) => (Math.abs(v - h) < Math.abs(LOOP_STEPS[best] - h) ? i : best), 0);

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
  // The search and the filter stay as the user left them.
  const [q, setQ] = useDisplayPref('recorders.q', '');
  const [chip, setChip] = useDisplayPref<'all' | 'recording' | 'nosignal'>('recorders.filter', 'all');
  /** The modal: a new recorder (from = the one duplicated), or one to edit. */
  const [edit, setEdit] = useState<{ rec: Recorder | null; from?: Recorder } | null>(null);
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
            <button className="btn primary" onClick={() => setEdit({ rec: null })}>
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
            <RecorderCard key={r.id} rec={r} st={status[r.id]} admin={admin} onEdit={() => setEdit({ rec: r })} onDuplicate={() => setEdit({ rec: null, from: r })} />
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
            <button className="project-new" onClick={() => setEdit({ rec: null })}>
              <Plus size={22} />
              <span>{t('New recorder')}</span>
            </button>
          )}
        </div>
      </div>
      {edit && <RecorderModal rec={edit.rec} from={edit.from} onClose={() => setEdit(null)} />}
    </div>
  );
}

const STATE_LABEL: Record<string, string> = { recording: 'Recording', nosignal: 'No signal', starting: 'Connecting…', off: 'Stopped', error: 'Error', waiting: 'Programmed' };
const STATE_TONE: Record<string, string> = { recording: 'ok', nosignal: 'warn', starting: 'warn', off: '', error: 'bad', waiting: 'plan' };

function RecorderCard({ rec, st, admin, onEdit, onDuplicate }: { rec: Recorder; st?: RecorderStatus; admin: boolean; onEdit: () => void; onDuplicate: () => void }) {
  const { toast, timezone } = useApp(useShallow((s) => ({ toast: s.toast, timezone: s.timezone })));
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
    if (rec.enabled && !(await confirmDialog({ title: t('Stop recording “{name}”?', { name: rec.name }), message: t('The loop already recorded is kept.'), confirm: t('Stop'), danger: true }))) return;
    try {
      await api('POST', `/api/recorders/${rec.id}/enabled`, { enabled: !rec.enabled });
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
    void reloadRecorders();
  };
  const clearLoop = async () => {
    if (!(await confirmDialog({ title: t('Empty the loop of “{name}”?', { name: rec.name }), message: t('Everything recorded so far is deleted; the recording goes on. The clips stay.'), confirm: t('Empty the loop'), danger: true }))) return;
    try {
      await api('POST', `/api/recorders/${rec.id}/clear`);
      toast(t('Loop emptied'));
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
    void reloadRecorders();
  };

  return (
    <article className="project-card">
      <div
        className={`sc-thumb ${st?.oldest ? 'go' : ''}`}
        title={st?.oldest ? t('Extract') : undefined}
        onClick={() => st?.oldest && void navigate(`/extract/${rec.id}`)}
      >
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
        {state === 'starting' && <span className="sc-onpic mid">{(rec.protocol === 'rtmp' || rec.protocol === 'srt') && rec.mode === 'listener' ? t('Waiting for the source') : rec.protocol === 'decklink' ? t('Opening the card…') : t('Connecting…')}</span>}
        {state === 'off' && <span className="sc-onpic mid">{t('Stopped')}</span>}
        {state === 'error' && (
          <span className="sc-onpic lostmsg">
            <TriangleAlert size={14} /> {t('Error')}
          </span>
        )}
        {state === 'waiting' && (
          <span className="sc-onpic mid">
            <CalendarClock size={14} /> {st?.nextSlot ? t('Next slot: {when}', { when: `${day(st.nextSlot.start, timezone, locale())} ${clock(st.nextSlot.start, timezone).slice(0, 5)}` }) : t('No slot to come')}
          </span>
        )}
        {rec.protocol === 'web' && <span className="sc-onpic tr">{badge(rec)}</span>}
      </div>
      <div className="project-body">
        <div className="sc-card-title">
          <span className={`nav-dot ${recording ? 'ok' : state === 'off' || state === 'waiting' ? '' : state === 'error' ? 'bad' : 'warn'}`} />
          <strong title={[sourceAddress(rec), fmt].filter(Boolean).join('\n')}>{rec.name}</strong>
          <span className={`sc-state ${STATE_TONE[state] ?? ''}`}>{t(STATE_LABEL[state] ?? 'Stopped')}</span>
        </div>
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
        {recording && st && st.lost + st.damaged > 0 && (
          <span className="sc-warnline" data-tip={t('Packets lost: {lost} (network, or a server too busy) · damaged stream: {damaged} (blocks or freezes in the picture). The last 10 minutes.', { lost: st.lost, damaged: st.damaged })}>
            <TriangleAlert size={12} /> {t('Reception: {n} problems in 10 min', { n: st.lost + st.damaged })}
          </span>
        )}
        {recording && st && (st.previewLag >= 15 || st.previewDropped) && (
          <span className="sc-warnline" data-tip={t('The preview is encoded apart from the recording: when the processor is too busy, only the preview falls behind or skips. The recording and the clips stay whole. Fewer recorders, or more processor, put it right.')}>
            <TriangleAlert size={12} /> {st.previewLag >= 15 ? t('Server overloaded: preview {s} s late', { s: st.previewLag }) : t('Server overloaded: the preview skips')}
          </span>
        )}
        {st?.spaceLimited && (
          <span className="sc-warnline">
            <TriangleAlert size={12} /> {t('Loop shortened for lack of disk space')}
          </span>
        )}
        {st?.onAir && (
          <div className="sc-epg-now" title={st.next ? t('Next: {name}', { name: `${clock(st.next.start, timezone).slice(0, 5)} ${st.next.name}` }) : undefined}>
            <Tv size={12} />
            <span className="sc-epg-name">{st.onAir.name}</span>
            <span className="sc-epg-time">
              {clock(st.onAir.start, timezone).slice(0, 5)}–{clock(st.onAir.end, timezone).slice(0, 5)}
            </span>
            <span className="sc-epg-bar">
              <span style={{ width: `${Math.min(100, Math.max(0, ((now - st.onAir.start) / (st.onAir.end - st.onAir.start)) * 100))}%` }} />
            </span>
          </div>
        )}
        {!st?.onAir && st?.next && (
          <div className="sc-epg-now dim">
            <Tv size={12} />
            <span className="sc-epg-name">
              {t('Next: {name}', { name: st.next.name })}
            </span>
            <span className="sc-epg-time">{clock(st.next.start, timezone).slice(0, 5)}</span>
          </div>
        )}
        {state === 'error' && st?.message && (
          <span className="sc-errline" title={st.message}>
            <TriangleAlert size={12} /> {t(st.message)}
          </span>
        )}
        {/* Programmed slots: a stopped recorder records during them (a running one goes on anyway). */}
        {!rec.enabled && st?.slot ? (
          <div className="sc-slot-now">
            <CalendarClock size={12} />
            <span className="sc-epg-name">{st.slot.name || t('Programmed slot')}</span>
            <span className="sc-epg-time">{t('until {h}', { h: clock(st.slot.end, timezone).slice(0, 5) })}</span>
          </div>
        ) : !rec.enabled && st?.nextSlot ? (
          <div className="sc-slot-now dim">
            <CalendarClock size={12} />
            <span className="sc-epg-name">{st.nextSlot.name || t('Programmed slot')}</span>
            <span className="sc-epg-time">
              {day(st.nextSlot.start, timezone, locale())} {clock(st.nextSlot.start, timezone).slice(0, 5)}–{clock(st.nextSlot.end, timezone).slice(0, 5)}
            </span>
          </div>
        ) : null}
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
        {admin &&
          (rec.enabled ? (
            <button className="btn small" title={t('Stop recording')} onClick={() => void toggle()}>
              <Square size={12} /> {t('Stop')}
            </button>
          ) : (
            <button className="btn small sc-start" title={t('Start recording')} onClick={() => void toggle()}>
              <span className="sc-rec" /> {t('Record')}
            </button>
          ))}
        <div className="spacer" />
        {admin && (
          <>
            <button className="icon-btn ghost" title={t('Edit')} onClick={onEdit}>
              <Pencil size={14} />
            </button>
            {/* The other actions in a menu: the card stays narrow enough for four in a row. */}
            <button
              className="icon-btn ghost"
              title={t('More')}
              onClick={(e) =>
                openContextMenu(e, [
                  { label: t('Duplicate'), icon: <Copy size={14} />, onClick: onDuplicate },
                  { label: t('Empty the loop'), icon: <Eraser size={14} />, onClick: () => void clearLoop(), disabled: !st?.oldest },
                  'sep',
                  { label: t('Delete'), icon: <Trash2 size={14} />, danger: true, onClick: () => void remove() },
                ])
              }
            >
              <MoreHorizontal size={14} />
            </button>
          </>
        )}
      </div>
    </article>
  );
}

const blank: Recorder = { id: '', name: '', protocol: 'srt', url: '', mode: 'listener', port: 9000, path: 'live/stream', latency: 500, passphrase: '', iface: '', loopHours: 24, bitrate: 10, enabled: true };

interface Space {
  capacity: number | null;
  recorders: { id: string; enabled: boolean; need: number | null }[];
}

function RecorderModal({ rec, from, onClose }: { rec: Recorder | null; from?: Recorder; onClose: () => void }) {
  const { recorders, toast, status } = useApp(useShallow((s) => ({ recorders: s.recorders, toast: s.toast, status: s.status })));
  const [space, setSpace] = useState<Space | null>(null);
  useEffect(() => {
    api<Space>('GET', '/api/storage').then(setSpace, () => setSpace(null));
  }, []);
  const firstPort = useMemo(() => {
    const used = new Set(recorders.map((r) => r.port));
    let p = 9000;
    while (used.has(p)) p++;
    return p;
  }, [recorders]);
  const [c, setC] = useState<Recorder>(() => {
    if (rec) return rec;
    if (from) {
      const listens = (from.protocol === 'rtmp' || from.protocol === 'srt') && from.mode === 'listener';
      // The copy keeps every setting; a listening one gets a free port. The passphrase is copied by the server.
      const measured = (status[from.id]?.measured ?? 0) / 1e6;
      return { ...from, id: '', name: t('{name} (copy)', { name: from.name }), port: listens ? firstPort : from.port, bitrate: Math.round(measured * 10) / 10 || from.bitrate };
    }
    return { ...blank, port: firstPort };
  });
  const [busy, setBusy] = useState(false);
  const set = (patch: Partial<Recorder>) => setC((x) => ({ ...x, ...patch }));
  const listens = (c.protocol === 'rtmp' || c.protocol === 'srt') && c.mode === 'listener';
  const placeholder = { udp: 'udp://239.1.1.1:5000', rtp: 'rtp://239.1.1.1:5004', rtmp: 'rtmp://server/live/key', srt: 'srt://server:9000', web: 'https://www.youtube.com/watch?v=…', decklink: '' }[c.protocol];
  const web = c.protocol === 'web';
  const card = c.protocol === 'decklink';
  const importCookies = (file: File | undefined) => {
    if (!file) return;
    void file.text().then((text) => set({ cookies: text }));
  };
  const pushUrl = c.protocol === 'rtmp' ? `rtmp://${location.hostname}:${c.port}/${c.path}` : `srt://${location.hostname}:${c.port}`;

  // The loop's space: reserved from the bitrate (the measured one if higher), against what the disk gives the loops.
  // The bitrate measured (else the one of former settings, else 8 Mb/s, as the server).
  const measured = rec ? (status[rec.id]?.measured ?? 0) / 1e6 : 0;
  const need = c.enabled ? loopNeed(measured || c.bitrate, c.loopHours) : 0;
  const others = space ? space.recorders.filter((r) => r.enabled && r.id !== rec?.id).reduce((a, r) => a + (r.need ?? 0), 0) : 0;
  const left = space?.capacity != null ? Math.max(0, space.capacity - others) : null;
  const tooBig = left !== null && need > left;
  const gb = (b: number) => `${(b / 1e9).toLocaleString(locale(), { maximumFractionDigits: b < 1e10 ? 1 : 0 })} ${lang() === 'fr' ? 'Go' : 'GB'}`;

  const save = async () => {
    setBusy(true);
    try {
      if (rec) await api('PUT', `/api/recorders/${rec.id}`, c);
      else await api('POST', from ? `/api/recorders?copyOf=${encodeURIComponent(from.id)}` : '/api/recorders', c);
      onClose();
    } catch (e) {
      toast(t((e as Error).message), 'error');
    } finally {
      setBusy(false);
      void reloadRecorders();
    }
  };

  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form className="modal wide" onPointerDown={(e) => e.stopPropagation()} onSubmit={(e) => { e.preventDefault(); void save(); }}>
        <header>
          <h2>{rec ? t('Edit {name}', { name: rec.name }) : from ? t('Duplicate {name}', { name: from.name }) : t('New recorder')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="set-grid two">
          <Field label={t('Name')}>
            <input value={c.name} autoFocus onChange={(e) => set({ name: e.target.value })} />
          </Field>
          <Field label={t('Source')} group>
            <Dropdown value={c.protocol} options={PROTOCOLS.map((p) => ({ value: p.value, label: p.label, detail: p.detail }))} onChange={(v) => set({ protocol: v as Protocol, ...(v === 'web' ? { mode: 'caller' as const, url: c.url.startsWith('http') ? c.url : '' } : {}), ...(v === 'decklink' ? { bitrate: c.bitrate >= 5 && c.bitrate <= 200 && c.protocol === 'decklink' ? c.bitrate : 25, device: c.device ?? 0, connection: c.connection ?? 'auto', videoMode: c.videoMode ?? 'auto', audioInput: c.audioInput ?? 'auto', audioChannels: c.audioChannels ?? 2 } : {}) })} />
          </Field>
          {(c.protocol === 'rtmp' || c.protocol === 'srt') && (
            <Field label={t('Mode')} group>
              <Segmented value={c.mode} options={[{ value: 'listener', label: t('Listen'), title: t('The source connects to this server') }, { value: 'caller', label: t('Call'), title: t('This server connects to the source') }]} onChange={(v) => set({ mode: v as Recorder['mode'] })} />
            </Field>
          )}
          {card ? (
            <DecklinkFields c={c} set={set} />
          ) : listens ? (
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
            <Field label={web ? t('Address of the live') : t('Address')} hint={web ? (c.url.startsWith('http') && platformOf(c.url) !== 'web' ? PLATFORM_NAME[platformOf(c.url)] : 'YouTube · Facebook · Instagram · X · LinkedIn') : undefined}>
              <input value={c.url} placeholder={placeholder} spellCheck={false} onChange={(e) => set({ url: e.target.value })} />
            </Field>
          )}
          {web && (
            <Field
              className="span2"
              group
              label={
                <>
                  {t('Cookies of a signed-in account')}
                  <InfoTip text={t('Only for lives that need an account (Instagram, some Facebook, LinkedIn or X lives). Export the cookies.txt of that account from a browser (extension “Get cookies.txt”) and import it here. Use an account made for this.')} />
                </>
              }
              hint={c.cookies === MASKED ? t('Cookies saved: they stay until you import others or remove them.') : t('Optional. The page is read without an account if empty.')}
            >
              <div className="sc-cookies">
                {c.cookies === MASKED ? (
                  <span className="sc-cookies-set">{t('Cookies saved')}</span>
                ) : (
                  <textarea rows={3} value={c.cookies ?? ''} spellCheck={false} placeholder="# Netscape HTTP Cookie File" onChange={(e) => set({ cookies: e.target.value })} />
                )}
                <div className="sc-cookies-btns">
                  <label className="btn small">
                    {t('Import cookies.txt')}
                    <input type="file" accept=".txt,text/plain" hidden onChange={(e) => importCookies(e.target.files?.[0])} />
                  </label>
                  {!!c.cookies && (
                    <button type="button" className="btn small ghost" onClick={() => set({ cookies: '' })}>
                      {t('Remove')}
                    </button>
                  )}
                </div>
              </div>
            </Field>
          )}
          {c.protocol === 'srt' && (
            <>
              <Field label={<>{t('Latency')}<InfoTip text={t('Over the internet: about 4 times the round trip, 500 ms or more. Too low, the lost packets come back too late: glitches.')} /></>}>
                <NumberField value={c.latency} min={20} max={8000} step={10} suffix="ms" onChange={(v) => set({ latency: v })} />
              </Field>
              <Field label={t('Passphrase')}>
                <input type="password" value={c.passphrase} placeholder={t('Optional')} autoComplete="new-password" onChange={(e) => set({ passphrase: e.target.value })} />
              </Field>
            </>
          )}
          <Field label={t('Loop length')} group className="span2">
            <div className="sc-looplen">
              <input type="range" min={0} max={LOOP_STEPS.length - 1} step={1} value={stepOf(c.loopHours)} aria-label={t('Loop length')} onChange={(e) => set({ loopHours: LOOP_STEPS[Number(e.target.value)] })} />
              <b className="sc-looplen-v">{c.loopHours} h</b>
              <div className="sc-looplen-presets">
                {LOOP_PRESETS.map((h) => (
                  <button key={h} type="button" className={`btn small ${c.loopHours === h ? 'on' : ''}`} onClick={() => set({ loopHours: h })}>
                    {h} h
                  </button>
                ))}
              </div>
            </div>
          </Field>
          {c.enabled && (
            <div className={`sc-space span2 ${tooBig ? 'bad' : ''}`}>
              <span>{t('Reserved for the loop: {need}', { need: gb(need) })}</span>
              {left !== null && <span>{t('available for the loops: {left}', { left: gb(left) })}</span>}
              {tooBig && <strong>{t('Not enough space: a shorter loop or more disk.')}</strong>}
            </div>
          )}
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
          <button type="submit" className="btn primary" disabled={busy || !c.name.trim() || tooBig}>
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

/** What the server has for DeckLink cards. */
interface DecklinkInfo {
  gstreamer: boolean;
  driver: boolean;
  nodes: number;
  devices: { number: number; name: string }[];
}

/** A DeckLink input: the card, its inputs, the format, the sound and the recording's bitrate. */
function DecklinkFields({ c, set }: { c: Recorder; set: (patch: Partial<Recorder>) => void }) {
  const [info, setInfo] = useState<DecklinkInfo | null>(null);
  useEffect(() => {
    void api<DecklinkInfo>('GET', '/api/decklink').then(setInfo, () => setInfo(null));
  }, []);
  const device = c.device ?? 0;
  const channels = c.audioChannels ?? 2;
  const devices = info?.devices ?? [];
  return (
    <>
      {info && (!info.gstreamer || !info.driver) && (
        <div className="sc-space bad span2">
          <strong>{!info.driver ? t('Blackmagic’s Desktop Video driver is not installed on the server.') : t('GStreamer’s DeckLink module is missing on the server.')}</strong>
          <span>{!info.driver ? t('Install Desktop Video (Blackmagic’s site, Support page), restart the server, then run the install script again. In a virtual machine, the card must be passed through to it (PCI passthrough).') : t('Run the install script again: it installs GStreamer and its modules.')}</span>
        </div>
      )}
      <Field label={t('Card')} hint={devices.length === 0 ? t('Cards are numbered from 1 in the order the driver sees them.') : undefined}>
        {devices.length > 0 ? (
          <Dropdown value={String(device)} options={devices.map((d) => ({ value: String(d.number), label: d.name || t('Card {n}', { n: d.number + 1 }), detail: String(d.number + 1) }))} onChange={(v) => set({ device: Number(v) })} />
        ) : (
          <NumberField value={device + 1} min={1} max={64} onChange={(v) => set({ device: Math.max(0, v - 1) })} />
        )}
      </Field>
      <Field label={t('Video input')}>
        <Dropdown value={c.connection ?? 'auto'} options={DL_CONNECTIONS.map((v) => ({ value: v, label: dlConnection(v) }))} onChange={(v) => set({ connection: v })} />
      </Field>
      <Field label={<>{t('Format')}<InfoTip text={t('Automatic works with the cards that detect the format of their input. Otherwise choose it: a wrong format gives no picture.')} /></>}>
        <Dropdown value={c.videoMode ?? 'auto'} options={DL_MODES.map((v) => ({ value: v, label: dlMode(v) }))} onChange={(v) => set({ videoMode: v })} />
      </Field>
      <Field label={t('Audio input')}>
        <Dropdown value={c.audioInput ?? 'auto'} options={DL_AUDIO_INPUTS.map((v) => ({ value: v, label: dlAudioInput(v) }))} onChange={(v) => set({ audioInput: v })} />
      </Field>
      <Field label={t('Audio channels')} group hint={tn(channels / 2, 'Recorded as {n} stereo track', 'Recorded as {n} stereo tracks')}>
        <Segmented value={String(channels)} options={[2, 8, 16].map((n) => ({ value: String(n), label: String(n) }))} onChange={(v) => set({ audioChannels: Number(v) })} />
      </Field>
      <Field label={<>{t('Recording bitrate')}<InfoTip text={t('The card gives raw pictures: the server encodes them in H.264 at this bitrate (about 1 to 2 processor cores per HD input). 25 Mb/s keeps the HD quality for clips; more takes more disk.')} /></>}>
        <NumberField value={c.bitrate} min={5} max={200} step={1} suffix="Mb/s" onChange={(v) => set({ bitrate: v })} />
      </Field>
    </>
  );
}
