import { Fragment, useEffect, useMemo, useState } from 'react';
import { useDisplayPref } from '../displayPrefs';
import { useShallow } from 'zustand/react/shallow';
import { CalendarClock, ChevronDown, Copy, Eraser, GripVertical, ImagePlus, MoreHorizontal, Pencil, Plus, RadioTower, Search, Square, Trash2, TriangleAlert, Tv, WifiOff, X } from 'lucide-react';
import { api, can, serverNow, useApp } from '../store';
import { RecLogo } from '../ui/RecLogo';
import { ContentFlags } from '../ui/ContentFlags';
import { lang, locale, t, tn } from '../i18n';
import { navigate } from '../router';
import { advisedBitrate, bitrate, clock, day, DECKLINK_MODES, formatFamily, formatName, scanFormat } from '../tc';
import { confirmDialog } from '../ui/dialogs';
import { Dropdown } from '../ui/Dropdown';
import { NumberField, Segmented } from '../ui/fields';
import { Field } from '../settings/ui';
import { InfoTip } from '../ui/InfoTip';
import type { Protocol, Recorder, RecorderStatus } from '../types';
import { clockOf, MASKED, useStreams, type StreamInfo } from '../streams';
import { explainGuard } from '../ui/guard';
import { showError } from '../ui/serverError';
import { gb, loopBytes, recWithLoop, type Space } from '../ui/RecLoop';
import { SOURCE_KINDS } from '../ui/sourceKinds';
import { openContextMenu } from '../ui/ContextMenu';

const PROTOCOLS: { value: Protocol; label: string; detail: string }[] = [
  { value: 'udp', label: 'UDP', detail: 'udp://' },
  { value: 'rtp', label: 'RTP', detail: 'rtp://' },
  { value: 'rtmp', label: 'RTMP', detail: 'rtmp://' },
  { value: 'srt', label: 'SRT', detail: 'srt://' },
  { value: 'hls', label: 'HLS stream', detail: '.m3u8' },
  { value: 'web', label: 'YouTube', detail: 'youtube.com' },
  { value: 'decklink', label: 'Hardware inputs', detail: 'SDI · HDMI' },
  { value: 'ndi', label: 'NDI', detail: 'NDI®' },
  { value: 'pattern', label: 'Test pattern', detail: 'lip sync' },
  { value: 'page', label: 'Web page', detail: 'browser' },
];
/** A web page's formats (as the server films them). */
export const PAGE_FORMATS = ['1080p25', '1080p50', '1080p2997', '1080p5994', '720p50', '720p5994'];
/** What changes as a source becomes another kind: each kind's defaults, what can be kept kept. */
function switchTo(c: Recorder, v: Protocol): Partial<Recorder> {
  return { protocol: v as Protocol, ...(v === 'web' || v === 'hls' ? { mode: 'caller' as const, url: c.url.startsWith('http') ? c.url : '' } : {}), ...(v === 'decklink' ? { bitrate: c.bitrate >= 5 && c.bitrate <= 200 && c.protocol === 'decklink' ? c.bitrate : 25, device: c.device ?? 0, connection: c.connection ?? 'auto', videoMode: c.videoMode ?? 'auto', audioInput: c.audioInput ?? 'auto', audioChannels: c.audioChannels ?? 2 } : {}), ...(v === 'pattern' ? { pattern: c.pattern || 'smpte', videoMode: PATTERN_FORMATS.includes(c.videoMode ?? '') ? c.videoMode : '1080i50', bitrate: c.bitrate >= 1 && c.bitrate <= 200 ? c.bitrate : 8 } : {}), ...(v === 'ndi' ? { mode: 'caller' as const, url: c.protocol === 'ndi' ? c.url : '', bitrate: c.bitrate >= 5 && c.bitrate <= 200 ? c.bitrate : 25 } : {}), ...(v === 'page' ? { ...PAGE_PRESET, videoMode: PAGE_FORMATS.includes(c.videoMode ?? '') ? c.videoMode : '1080p25', bitrate: c.bitrate >= 1 && c.bitrate <= 50 ? c.bitrate : 6 } : {}), ...(c.protocol === 'ndi' && v !== 'ndi' ? { url: '' } : {}) };
}
/** A new web page source, as made by « Add a web source ». */
const PAGE_PRESET: Partial<Recorder> = { protocol: 'page', mode: 'caller', url: '', videoMode: '1080p25', bitrate: 6, audioChannels: 2 };

/** The test pattern: its styles and its formats (as the server makes them). */
const PATTERNS: { value: string; label: string }[] = [
  { value: 'smpte', label: 'SMPTE HD bars' },
  { value: 'ebu', label: 'EBU 75 % bars' },
  { value: 'animated', label: 'Animated' },
  { value: 'lipsync', label: 'Lip sync (timecode)' },
];
const PATTERN_FORMATS = ['2160p50', '2160p25', '1080p50', '1080p25', '1080i50', '1080p5994', '1080p2997', '1080i5994', '720p50', '720p5994', '576i50', '480i5994'];

/** DeckLink: the card's inputs, the formats and the audio inputs, as the server knows them. */
const DL_CONNECTIONS = ['auto', 'sdi', 'hdmi', 'optical-sdi', 'component', 'composite', 'svideo'];
/** Every format, by family (SD, HD, 2K, UHD / 4K, 8K), "auto" first. */
const FAMILIES = ['', 'SD', 'HD', '2K', 'UHD / 4K', '8K'];
const DL_MODES = [...DECKLINK_MODES].sort((a, b) => FAMILIES.indexOf(formatFamily(a)) - FAMILIES.indexOf(formatFamily(b)));
const DL_AUDIO_INPUTS = ['auto', 'embedded', 'aes', 'analog'];
const dlConnection = (v: string) => ({ auto: t('Automatic'), sdi: 'SDI', hdmi: 'HDMI', 'optical-sdi': t('Optical SDI'), component: t('Component'), composite: t('Composite'), svideo: 'S-Video' })[v] ?? v;
/** "1080i50" → "1080i 50", "1080p5994" → "1080p 59.94" (French: 59,94). */
const dlMode = (v: string) => (v === 'auto' ? t('Automatic (detected by the card)') : formatName(v, lang() === 'fr'));
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

const CODECS: Record<string, string> = { h264: 'H.264', hevc: 'HEVC', mpeg2video: 'MPEG-2', mpeg4: 'MPEG-4', prores: 'ProRes', dnxhd: 'DNxHD' };

/** Where the source sends or what we pull, as shown to people. */
export function sourceAddress(r: Recorder): string {
  const host = location.hostname;
  if (r.protocol === 'rtmp' && r.mode === 'listener') return `rtmp://${host}:${r.port}/${r.path}`;
  if (r.protocol === 'srt' && r.mode === 'listener') return `srt://${host}:${r.port}`;
  if (r.protocol === 'decklink') return `${t('Input {n}', { n: (r.device ?? 0) + 1 })} · ${dlConnection(r.connection ?? 'auto')} · ${dlMode(r.videoMode ?? 'auto')}`;
  if (r.protocol === 'ndi') return r.url + (r.host ? ` · ${r.host}` : '');
  if (r.protocol === 'pattern') return `${t(PATTERNS.find((p) => p.value === r.pattern)?.label ?? 'Test pattern')} · ${formatName(r.videoMode ?? '1080i50', lang() === 'fr')}`;
  if (r.protocol === 'page') return `${t('Browser')} · ${formatName(r.videoMode ?? '1080p25', lang() === 'fr')}${(r.audioChannels ?? 2) > 0 ? ` · ${t('sound')}` : ''}`;
  return r.url + (r.iface && (r.protocol === 'udp' || r.protocol === 'rtp') ? ` · ${r.iface}` : '');
}

/** The list again, after an action (the live event may have been missed). */
async function reloadRecorders() {
  try {
    useApp.setState({ allRecorders: await api<Recorder[]>('GET', '/api/recorders') });
  } catch {
    /* the live events will tell */
  }
}



/** "23 h 41" */
function hours(ms: number): string {
  const m = Math.floor(ms / 60_000);
  return `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, '0')}`;
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

/** The page's groups: the kinds of sources. */
const GROUPS = SOURCE_KINDS;

/** A new source of this kind, with its kind's defaults. */
function presetFor(p: Protocol, name: string): Partial<Recorder> {
  return { ...switchTo(blank, p), name };
}

export function Recorders() {
  const { recorders, status, user } = useApp(useShallow((s) => ({ recorders: s.recorders, status: s.status, user: s.auth?.user })));
  const admin = can.admin(user);
  // The search and the filter stay as the user left them.
  const [q, setQ] = useDisplayPref('recorders.q', '');
  const [chip, setChip] = useDisplayPref<'all' | 'recording' | 'nosignal'>('recorders.filter', 'all');
  /** The modal: a new recorder (from = the one duplicated), or one to edit. */
  const [edit, setEdit] = useState<{ rec: Recorder | null; from?: Recorder; preset?: Partial<Recorder> } | null>(null);
  // From Système › Matériel: a new recorder on a card's input (`?decklink=<number>&format=…&name=…`).
  useEffect(() => {
    const q = new URLSearchParams(location.search);
    const device = q.get('decklink');
    if (device === null) return;
    const format = q.get('format') ?? '';
    setEdit({ rec: null, preset: { name: q.get('name') ?? '', protocol: 'decklink', device: Number(device), connection: 'auto', videoMode: DL_MODES.includes(format) ? format : 'auto', audioInput: 'auto', audioChannels: 2, bitrate: DL_MODES.includes(format) && format !== 'auto' ? advisedBitrate(format) : 25 } });
    void navigate('/system/sources', true);
  }, []);
  const counts = {
    all: recorders.length,
    recording: recorders.filter((r) => status[r.id]?.state === 'recording').length,
    nosignal: recorders.filter((r) => r.enabled && ['nosignal', 'error'].includes(status[r.id]?.state ?? '')).length,
  };
  // A kind of source only (its group's key), or all.
  const [kind, setKind] = useDisplayPref<string>('recorders.kind', 'all');
  const groupOf = (r: Recorder) => GROUPS.find((g) => g.protocols.includes(r.protocol))?.key ?? '';
  const shown = recorders.filter((r) => {
    if (kind !== 'all' && groupOf(r) !== kind) return false;
    if (q && !`${r.name} ${sourceAddress(r)} ${r.protocol}`.toLowerCase().includes(q.toLowerCase())) return false;
    if (chip === 'recording') return status[r.id]?.state === 'recording';
    if (chip === 'nosignal') return r.enabled && ['nosignal', 'error'].includes(status[r.id]?.state ?? '');
    return true;
  });
  // Which destinations each source is sent to now.
  const streams = useStreams(3000);
  // The groups in the order of their sources (the first of each), those still empty after.
  const order = useMemo(() => {
    const seen: string[] = [];
    for (const r of recorders) {
      const k = groupOf(r);
      if (k && !seen.includes(k)) seen.push(k);
    }
    return [...seen, ...GROUPS.map((g) => g.key).filter((k) => !seen.includes(k))].map((k) => GROUPS.find((g) => g.key === k)!);
  }, [recorders]); // eslint-disable-line react-hooks/exhaustive-deps
  // The loops emptied: of the stopped sources, or of all (a test pattern keeps none).
  const purge = async (all: boolean) => {
    const list = recorders.filter((r) => r.protocol !== 'pattern' && status[r.id]?.oldest != null && (all || status[r.id]?.state !== 'recording'));
    if (list.length === 0) {
      useApp.getState().toast(t('No loop to empty'), 'info');
      return;
    }
    const ok = await confirmDialog({
      title: all ? t('Empty the loops of all the sources?') : t('Empty the loops of the stopped sources?'),
      message: `${list.map((r) => r.name).join(', ')}\n\n${t('Everything recorded so far is deleted; the recordings go on. The clips stay.')}`,
      confirm: tn(list.length, 'Empty {n} loop', 'Empty {n} loops'),
      danger: true,
    });
    if (!ok) return;
    let failed = 0;
    for (const r of list) {
      try {
        await api('POST', `/api/recorders/${r.id}/clear`);
      } catch {
        failed++;
      }
    }
    useApp.getState().toast(failed ? t('{n} loops could not be emptied', { n: failed }) : tn(list.length, '{n} loop emptied', '{n} loops emptied'));
  };
  // Dragged to put in order: a source inside its kind, or a kind among the others.
  const [drag, setDrag] = useState<{ card?: string; group?: string } | null>(null);
  const [over, setOver] = useState<{ id: string; before: boolean } | null>(null);
  const inGroup = (k: string) => recorders.filter((r) => groupOf(r) === k);
  const saveOrder = async (ids: string[]) => {
    const byId = new Map(recorders.map((r) => [r.id, r]));
    useApp.setState({ allRecorders: ids.map((id) => byId.get(id)!).filter(Boolean) });
    try {
      await api('PUT', '/api/recorders/order', ids);
    } catch (e) {
      showError(e);
      void reloadRecorders();
    }
  };
  const dropCard = (target: Recorder, before: boolean) => {
    const id = drag?.card;
    if (!id || id === target.id) return;
    const k = groupOf(target);
    const list = inGroup(k).filter((r) => r.id !== id);
    const at = list.findIndex((r) => r.id === target.id) + (before ? 0 : 1);
    list.splice(at, 0, recorders.find((r) => r.id === id)!);
    void saveOrder(order.flatMap((g) => (g.key === k ? list : inGroup(g.key))).map((r) => r.id));
  };
  const dropGroup = (target: string, before: boolean) => {
    const from = drag?.group;
    if (!from || from === target) return;
    const keys = order.map((g) => g.key).filter((k) => k !== from);
    keys.splice(keys.indexOf(target) + (before ? 0 : 1), 0, from);
    void saveOrder(keys.flatMap((k) => inGroup(k)).map((r) => r.id));
  };
  const half = (e: React.DragEvent, dir: 'x' | 'y') => {
    const b = e.currentTarget.getBoundingClientRect();
    return dir === 'x' ? e.clientX < b.left + b.width / 2 : e.clientY < b.top + b.height / 2;
  };
  const endDrag = () => {
    setDrag(null);
    setOver(null);
  };
  return (
    <>
      <div>
        <div className="settings-head">
          <div>
            <h1>{t('Recorders')}</h1>
            <p className="muted">
              {tn(counts.all, '{n} recorder', '{n} recorders')} · {tn(counts.recording, '{n} recording', '{n} recording')}
            </p>
          </div>
          <div className="spacer" />
          {admin && recorders.length > 0 && (
            <button
              className="btn"
              onClick={(e) =>
                openContextMenu(e, [
                  { label: t('Of the stopped sources'), icon: <Eraser size={14} />, onClick: () => void purge(false) },
                  { label: t('Of all the sources'), icon: <Eraser size={14} />, danger: true, onClick: () => void purge(true) },
                ])
              }
            >
              <Eraser size={14} /> {t('Empty the loops')} <ChevronDown size={12} />
            </button>
          )}
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
        {recorders.length > 0 && (
          <div className="asr-chips src-kinds">
            <button className={`asr-chip ${kind === 'all' ? 'on' : ''}`} onClick={() => setKind('all')}>
              {t('All kinds')}
            </button>
            {order
              .filter((g) => inGroup(g.key).length > 0)
              .map((g) => (
                <button key={g.key} className={`asr-chip ${kind === g.key ? 'on' : ''}`} onClick={() => setKind(kind === g.key ? 'all' : g.key)}>
                  {g.icon} {t(g.label)} <span className="asr-chip-n">{inGroup(g.key).length}</span>
                </button>
              ))}
          </div>
        )}
        {shown.length === 0 && (q || chip !== 'all' || kind !== 'all') && (
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
        {/* By kind of source: each group with its count and its own « + ». */}
        {order.map((g) => {
          const mine = shown.filter((r) => g.protocols.includes(r.protocol));
          const adds = admin && !q && chip === 'all' && kind === 'all';
          if (!mine.length && !(adds && recorders.length === 0)) return null;
          const dropHere = drag?.group && over?.id === `g:${g.key}`;
          return (
            <section
              key={g.key}
              className={`src-group ${drag?.group === g.key ? 'dragging' : ''} ${dropHere ? (over.before ? 'drop-before' : 'drop-after') : ''}`}
              onDragOver={(e) => {
                if (!drag?.group) return;
                e.preventDefault();
                setOver({ id: `g:${g.key}`, before: half(e, 'y') });
              }}
              onDrop={(e) => {
                e.preventDefault();
                if (drag?.group) dropGroup(g.key, half(e, 'y'));
                endDrag();
              }}
            >
              <header className="src-group-head">
                {admin && (
                  <span
                    className="src-grip"
                    draggable
                    data-tip={t('Drag to put the kinds in order')}
                    onDragStart={(e) => {
                      e.dataTransfer.effectAllowed = 'move';
                      e.dataTransfer.setData('text/plain', g.key);
                      setDrag({ group: g.key });
                    }}
                    onDragEnd={endDrag}
                  >
                    <GripVertical size={14} />
                  </span>
                )}
                <span className="src-group-ico">{g.icon}</span>
                <h2>{t(g.label)}</h2>
                <span className="src-group-n">{mine.length}</span>
                <span className="src-group-line" />
                {adds && (
                  <button type="button" className="btn small ghost" onClick={() => setEdit({ rec: null, preset: presetFor(g.protocols[0], g.key === 'page' ? `Web ${recorders.filter((r) => r.protocol === 'page').length + 1}` : '') })}>
                    <Plus size={13} /> {t('Add')}
                  </button>
                )}
              </header>
              <div className="src-grid">
                {mine.map((r) => (
                  <div
                    key={r.id}
                    className={`src-slot ${drag?.card === r.id ? 'dragging' : ''} ${drag?.card && over?.id === r.id ? (over.before ? 'drop-before' : 'drop-after') : ''}`}
                    draggable={admin}
                    onDragStart={(e) => {
                      if ((e.target as HTMLElement).closest('button, input, a')) return e.preventDefault();
                      e.dataTransfer.effectAllowed = 'move';
                      e.dataTransfer.setData('text/plain', r.id);
                      setDrag({ card: r.id });
                    }}
                    onDragEnd={endDrag}
                    onDragOver={(e) => {
                      // A source moves inside its kind only.
                      const from = drag?.card ? recorders.find((x) => x.id === drag.card) : null;
                      if (!from || groupOf(from) !== g.key) return;
                      e.preventDefault();
                      e.stopPropagation();
                      setOver({ id: r.id, before: half(e, 'x') });
                    }}
                    onDrop={(e) => {
                      if (!drag?.card) return;
                      e.preventDefault();
                      e.stopPropagation();
                      dropCard(r, half(e, 'x'));
                      endDrag();
                    }}
                  >
                    <RecorderCard rec={r} st={status[r.id]} admin={admin} streams={streams.filter((x) => x.source === r.id)} onEdit={() => setEdit({ rec: r })} onDuplicate={() => setEdit({ rec: null, from: r })} />
                  </div>
                ))}
              </div>
            </section>
          );
        })}
      </div>
      {edit && <RecorderModal rec={edit.rec} from={edit.from} preset={edit.preset} onClose={() => setEdit(null)} />}
    </>
  );
}

const STATE_LABEL: Record<string, string> = { recording: 'Recording', nosignal: 'No signal', starting: 'Connecting…', off: 'Stopped', error: 'Error', waiting: 'Programmed' };
const STATE_TONE: Record<string, string> = { recording: 'ok', nosignal: 'warn', starting: 'warn', off: '', error: 'bad', waiting: 'plan' };

function RecorderCard({ rec, st, admin, streams, onEdit, onDuplicate }: { rec: Recorder; st?: RecorderStatus; admin: boolean; streams: StreamInfo[]; onEdit: () => void; onDuplicate: () => void }) {
  const { toast, timezone } = useApp(useShallow((s) => ({ toast: s.toast, timezone: s.timezone })));
  const tick = useTick(2000);
  const [img, setImg] = useState(true);
  const state = st?.state ?? 'off';
  const recording = state === 'recording';
  useEffect(() => setImg(true), [tick]);
  const have = st?.oldest && st.newest ? st.newest - st.oldest : 0;
  const fill = Math.min(100, (have / (rec.loopHours * 3_600_000)) * 100);
  const m = st?.media;
  const fmt = [scanFormat(m), m ? CODECS[m.codec] ?? m.codec.toUpperCase() : '', recording && st?.bitrate ? bitrate(st.bitrate, locale()) : ''].filter(Boolean).join(' · ');

  // REC: records all the time, with the loop chosen then; Stop: only during its slots (as in the Régie).
  const stop = async () => {
    if (!(await confirmDialog({ title: t('Stop recording “{name}”?', { name: rec.name }), message: t('What it recorded stays in its loop.'), confirm: t('Stop'), danger: true }))) return;
    try {
      await api('POST', `/api/recorders/${rec.id}/recording`, { mode: 'slots' });
    } catch (e) {
      if (!explainGuard(e)) showError(e);
    }
  };
  const clearLoop = async () => {
    if (!(await confirmDialog({ title: t('Empty the loop of “{name}”?', { name: rec.name }), message: t('Everything recorded so far is deleted; the recording goes on. The clips stay.'), confirm: t('Empty the loop'), danger: true }))) return;
    try {
      await api('POST', `/api/recorders/${rec.id}/clear`);
      toast(t('Loop emptied'));
    } catch (e) {
      showError(e);
    }
  };
  const remove = async () => {
    if (!(await confirmDialog({ title: t('Delete the recorder “{name}”?', { name: rec.name }), message: t('Its loop is deleted too. The clips stay.'), confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/recorders/${rec.id}`);
    } catch (e) {
      showError(e);
    }
    void reloadRecorders();
  };

  // A test pattern: made live in the Streamer only, never recorded.
  const live = rec.protocol === 'pattern';
  const more = (e: React.MouseEvent) =>
    openContextMenu(e, [
      { label: t('Duplicate'), icon: <Copy size={14} />, onClick: onDuplicate },
      ...(live ? [] : [{ label: t('Empty the loop'), icon: <Eraser size={14} />, disabled: !st?.oldest, onClick: () => void clearLoop() }]),
      'sep',
      { label: t('Delete'), icon: <Trash2 size={14} />, danger: true, onClick: () => void remove() },
    ]);
  // One line under the name: the programme on air, else the slot, else the next one.
  const info = st?.onAir
    ? { icon: <Tv size={11} />, name: st.onAir.name, time: `${clock(st.onAir.start, timezone).slice(0, 5)}–${clock(st.onAir.end, timezone).slice(0, 5)}`, dim: false }
    : !rec.enabled && st?.slot
      ? { icon: <CalendarClock size={11} />, name: st.slot.name || t('Programmed slot'), time: t('until {h}', { h: clock(st.slot.end, timezone).slice(0, 5) }), dim: false }
      : !rec.enabled && st?.nextSlot
        ? { icon: <CalendarClock size={11} />, name: st.nextSlot.name || t('Programmed slot'), time: `${day(st.nextSlot.start, timezone, locale())} ${clock(st.nextSlot.start, timezone).slice(0, 5)}`, dim: true }
        : st?.next
          ? { icon: <Tv size={11} />, name: t('Next: {name}', { name: st.next.name }), time: clock(st.next.start, timezone).slice(0, 5), dim: true }
          : null;

  return (
    <article className={`src-card ${recording ? 'rec' : ''}`}>
      <div className="src-thumb">
        {img && state !== 'off' ? <img src={`/api/recorders/${rec.id}/thumb.jpg?v=${recording ? tick : 0}`} alt="" onError={() => setImg(false)} /> : <div className="sc-pic lost" />}
        {(state === 'nosignal' || state === 'error') && (
          <span className="src-onpic bad" data-tip={state === 'error' ? t('Error') : t('No signal')}>
            {state === 'error' ? <TriangleAlert size={14} /> : <WifiOff size={14} />}
          </span>
        )}
        {state === 'starting' && <span className="src-onpic" data-tip={(rec.protocol === 'rtmp' || rec.protocol === 'srt') && rec.mode === 'listener' ? t('Waiting for the source') : t('Connecting…')}><span className="src-spin" /></span>}
        {recording && <ContentFlags st={st} />}
      </div>
      <div className="src-body">
        <div className="src-title">
          <span className={`nav-dot ${recording ? 'ok' : state === 'off' || state === 'waiting' ? '' : state === 'error' ? 'bad' : 'warn'}`} />
          <RecLogo rec={rec} size="sm" />
          <strong title={[sourceAddress(rec), fmt].filter(Boolean).join('\n')}>{rec.name}</strong>
          {rec.protocol === 'web' && <span className="src-badge">{badge(rec)}</span>}
          {live ? (
            <span className={`sc-state ${recording ? 'ok' : ''}`}>{recording ? t('Live in the Streamer') : t('Streamer only')}</span>
          ) : recording ? (
            <span className="sc-state rec">
              <span className="sc-rec" />
              REC
            </span>
          ) : (
            <span className={`sc-state ${STATE_TONE[state] ?? ''}`}>{t(STATE_LABEL[state] ?? 'Stopped')}</span>
          )}
        </div>
        <div className="src-sub">
          <span className="src-addr">{sourceAddress(rec) || fmt}</span>
        </div>
        {state === 'error' && st?.message ? (
          <span className="src-line bad" title={st.message}>
            <TriangleAlert size={11} /> <span className="src-line-name">{t(st.message)}</span>
          </span>
        ) : streams.length > 0 ? (
          <span className="src-line air">
            <span className="sc-rec" />
            <span className="src-line-name">{tn(streams.length, 'Sent to {n} destination', 'Sent to {n} destinations')}</span>
            {streams[0].liveSince ? <span className="src-line-time">{clockOf(serverNow() - streams[0].liveSince)}</span> : null}
          </span>
        ) : info ? (
          <span className={`src-line ${info.dim ? 'dim' : ''}`}>
            {info.icon} <span className="src-line-name">{info.name}</span>
            <span className="src-line-time">{info.time}</span>
          </span>
        ) : (
          <span className="src-line dim" />
        )}
        {!live && (
        <div className="src-loop" title={t('Loop')}>
          <span className="perf-bar tone-accent">
            <span className="perf-bar-fill" style={{ transform: `translateX(${fill - 100}%)` }} />
          </span>
          <span className="src-loop-v">
            <b>{hours(have)}</b> / {rec.loopHours} h
          </span>
        </div>
        )}
      </div>
      {admin && (
        <div className="src-actions">
          {live ? null : rec.enabled ? (
            <button type="button" className="btn small stop-btn" onClick={() => void stop()}>
              <Square size={10} /> {t('Stop')}
            </button>
          ) : (
            <button type="button" className="btn small sc-cal-start" data-tip={t('Record all the time: choose the loop')} onClick={(e) => recWithLoop(e, rec, (st?.measured ?? 0) / 1e6)}>
              <span className="sc-rec" /> REC <ChevronDown size={11} className="src-caret" />
            </button>
          )}
          <div className="src-icons">
            <button className="icon-btn ghost" data-tip={t('Edit')} aria-label={t('Edit')} onClick={onEdit}>
              <Pencil size={14} />
            </button>
            <button className="icon-btn ghost" data-tip={t('More')} aria-label={t('More')} onClick={more}>
              <MoreHorizontal size={14} />
            </button>
          </div>
        </div>
      )}
    </article>
  );
}

// Declarative: a new source does not record by itself (REC in Programmation).
const blank: Recorder = { id: '', name: '', protocol: 'srt', url: '', mode: 'listener', port: 9000, path: 'live/stream', latency: 500, passphrase: '', iface: '', loopHours: 24, bitrate: 10, enabled: false };



function RecorderModal({ rec, from, preset, onClose }: { rec: Recorder | null; from?: Recorder; preset?: Partial<Recorder>; onClose: () => void }) {
  const { recorders, status } = useApp(useShallow((s) => ({ recorders: s.recorders, status: s.status })));
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
      return { ...from, id: '', enabled: false, name: t('{name} (copy)', { name: from.name }), port: listens ? firstPort : from.port, bitrate: Math.round(measured * 10) / 10 || from.bitrate };
    }
    return { ...blank, port: firstPort, ...preset };
  });
  const [busy, setBusy] = useState(false);
  const set = (patch: Partial<Recorder>) => setC((x) => ({ ...x, ...patch }));
  // The cards' inputs and what each receives (asked again while the window is open), and the
  // format the chosen input receives (from its preview).
  const [dl, setDl] = useState<DecklinkInfo | null>(null);
  const [detected, setDetected] = useState('');
  const isCard = c.protocol === 'decklink';
  useEffect(() => {
    if (!isCard) return;
    let dead = false;
    const load = () => api<DecklinkInfo>('GET', '/api/decklink').then((i) => !dead && setDl(i), () => !dead && setDl(null));
    void load();
    const i = setInterval(() => void load(), 3000);
    return () => {
      dead = true;
      clearInterval(i);
    };
  }, [isCard]);
  const listens = (c.protocol === 'rtmp' || c.protocol === 'srt') && c.mode === 'listener';
  const placeholder = PLACEHOLDERS[c.protocol];
  const web = c.protocol === 'web';
  const card = c.protocol === 'decklink';
  // Another source (or a new recorder): tried before it is saved. HLS: tried also as it is, for
  // the qualities its playlist offers.
  const probe = useProbe(cleaned(c), !card && (!sameSource(rec, c) || c.protocol === 'hls') && triable(cleaned(c)));
  // HLS: the streams its playlist offers, in its order (the one chosen kept while it is tried).
  const [streams, setStreams] = useState<NonNullable<Probe['variants']>>([]);
  useEffect(() => {
    if (probe.result) setStreams(probe.result.variants ?? []);
  }, [probe.result]);
  useEffect(() => setStreams([]), [c.url, c.protocol]);
  const importCookies = (file: File | undefined) => {
    if (!file) return;
    void file.text().then((text) => set({ cookies: text }));
  };
  const pushUrl = c.protocol === 'rtmp' ? `rtmp://${location.hostname}:${c.port}/${c.path}` : `srt://${location.hostname}:${c.port}`;

  // The loop's space: reserved from the bitrate (the measured one if higher), against what the disk gives the loops.
  // The bitrate measured (else the one of former settings, else 8 Mb/s, as the server).
  const measured = rec ? (status[rec.id]?.measured ?? 0) / 1e6 : 0;
  const need = c.enabled ? loopBytes(measured || c.bitrate, c.loopHours) : 0;
  const others = space ? space.recorders.filter((r) => r.enabled && r.id !== rec?.id).reduce((a, r) => a + (r.need ?? 0), 0) : 0;
  const left = space?.capacity != null ? Math.max(0, space.capacity - others) : null;
  const tooBig = left !== null && need > left;

  // The logo: a file dropped or chosen, an address, or removed; sent once the recorder is saved.
  const [logo, setLogo] = useState<LogoChange>(null);
  const save = async () => {
    setBusy(true);
    try {
      let id = rec?.id ?? '';
      const sent = cleaned(c);
      if (rec) await api('PUT', `/api/recorders/${rec.id}`, sent);
      else id = (await api<{ id: string }>('POST', from ? `/api/recorders?copyOf=${encodeURIComponent(from.id)}` : '/api/recorders', sent)).id;
      if (logo && id) {
        try {
          if (logo.file) await api('PUT', `/api/recorders/${id}/logo`, logo.file);
          else if (logo.url) await api('POST', `/api/recorders/${id}/logo/url`, { url: logo.url });
          else if (logo.remove) await api('DELETE', `/api/recorders/${id}/logo`);
        } catch (e) {
          showError(e);
        }
      }
      onClose();
    } catch (e) {
      if (!explainGuard(e)) showError(e);
    } finally {
      setBusy(false);
      void reloadRecorders();
    }
  };

  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form className="modal xwide sc-rm" onPointerDown={(e) => e.stopPropagation()} onSubmit={(e) => { e.preventDefault(); void save(); }}>
        <header>
          <h2>{rec ? t('Edit {name}', { name: rec.name }) : from ? t('Duplicate {name}', { name: from.name }) : t('New recorder')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="sc-rm-body">
        <div className="sc-rm-form set-grid two">
          <Field label={t('Name')}>
            <input value={c.name} autoFocus onChange={(e) => set({ name: e.target.value })} />
          </Field>
          <Field label={t('Source')} group>
            <Dropdown value={c.protocol} options={PROTOCOLS.map((p) => ({ value: p.value, label: t(p.label), detail: t(p.detail) }))} onChange={(v) => set(switchTo(c, v as Protocol))} />
          </Field>
          {(c.protocol === 'rtmp' || c.protocol === 'srt') && (
            <Field label={t('Mode')} group>
              <Segmented value={c.mode} options={[{ value: 'listener', label: t('Listen'), title: t('The source connects to this server') }, { value: 'caller', label: t('Call'), title: t('This server connects to the source') }]} onChange={(v) => set({ mode: v as Recorder['mode'] })} />
            </Field>
          )}
          {card ? (
            <DecklinkFields c={c} set={set} info={dl} detected={detected} rec={rec} />
          ) : c.protocol === 'ndi' ? (
            <NdiFields c={c} set={set} />
          ) : c.protocol === 'page' ? (
            <>
              <Field label={t('Format')}>
                <Dropdown value={c.videoMode ?? '1080p25'} options={PAGE_FORMATS.map((f) => ({ value: f, label: formatName(f, lang() === 'fr') }))} onChange={(v) => set({ videoMode: v })} />
              </Field>
              <Field label={t('Sound of the page')} group>
                <Segmented value={(c.audioChannels ?? 2) > 0 ? 'on' : 'off'} options={[{ value: 'on', label: t('Recorded') }, { value: 'off', label: t('Silent') }]} onChange={(v) => set({ audioChannels: v === 'on' ? 2 : 0 })} />
              </Field>
              <RateField c={c} set={set} />
              <Field label={t('Recording bitrate')}>
                <NumberField value={c.bitrate} min={1} max={50} step={1} suffix="Mb/s" onChange={(v) => set({ bitrate: v })} />
              </Field>
            </>
          ) : c.protocol === 'pattern' ? (
            <>
              <Field label={t('Pattern')} group>
                <Segmented value={c.pattern || 'smpte'} options={PATTERNS.map((p) => ({ value: p.value, label: t(p.label) }))} onChange={(v) => set({ pattern: v })} />
              </Field>
              <Field label={t('Format')}>
                <Dropdown value={c.videoMode ?? '1080i50'} options={PATTERN_FORMATS.map((f) => ({ value: f, label: formatName(f, lang() === 'fr') }))} onChange={(v) => set({ videoMode: v })} />
              </Field>
              <RateField c={c} set={set} />
              <Field label={t('Recording bitrate')} hint={t('A flash and a beep together once a second (lip sync), something moving, a clock to the frame, the name and the logo of the source.')}>
                <NumberField value={c.bitrate} min={1} max={200} step={1} suffix="Mb/s" onChange={(v) => set({ bitrate: v })} />
              </Field>
            </>
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
            <Field label={web ? t('Address of the live') : c.protocol === 'hls' ? t('Address of the playlist (.m3u8)') : t('Address')} hint={web && c.url.startsWith('http') && platformOf(c.url) !== 'youtube' ? <span className="sc-warn-text">{t('Only YouTube lives are recorded.')}</span> : undefined}>
              <input value={c.url} placeholder={placeholder} spellCheck={false} onChange={(e) => set({ url: e.target.value })} />
            </Field>
          )}
          {c.protocol === 'hls' && (streams.length > 1 || !!c.variant) && (
            <Field label={t('Stream')} hint={t('The playlist offers several streams: only this one is read and recorded (with its sounds).')}>
              <Dropdown
                value={c.variant ?? ''}
                options={[
                  { value: '', label: t('Automatic (the best picture)') },
                  ...streams.map((v, i) => ({
                    value: v.uri,
                    label: `${i + 1}. ${v.name || (v.height ? `${v.height}p` : v.uri.split('/').pop())}`,
                    detail: [v.width ? `${v.width}×${v.height}` : '', v.bandwidth ? t('{n} Mb/s', { n: (v.bandwidth / 1e6).toLocaleString(locale(), { maximumFractionDigits: 1 }) }) : ''].filter(Boolean).join(' · '),
                  })),
                  ...(c.variant && !streams.some((v) => v.uri === c.variant) ? [{ value: c.variant, label: c.variant.split('/').pop() ?? c.variant }] : []),
                ]}
                onChange={(v) => set({ variant: v })}
              />
            </Field>
          )}
          {web && (
            <Field
              className="span2"
              group
              label={
                <>
                  {t('Cookies of a signed-in account')}
                  <InfoTip text={t('When YouTube asks to check this server is not a bot, or for lives that need an account. In a private window of a browser, sign in to YouTube (an account made for this), export its cookies.txt (extension “Get cookies.txt LOCALLY”), close the window and import the file here.')} />
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
          <LogoField rec={rec} change={logo} onChange={setLogo} />
        </div>
        <aside className="sc-rm-side">
          <RecorderSide c={c} rec={rec} st={rec ? status[rec.id] : undefined} dl={dl} detected={detected} onFormat={setDetected} probe={probe} />
          {c.enabled && (
            <div className={`sc-space ${tooBig ? 'bad' : ''}`}>
              <span>{t('Reserved for the loop: {need}', { need: gb(need) })}</span>
              {left !== null && <span>{t('available for the loops: {left}', { left: gb(left) })}</span>}
              {tooBig && <strong>{t('Not enough space: a shorter loop or more disk.')}</strong>}
            </div>
          )}
        </aside>
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
  /** The server was built with Blackmagic's SDK. */
  sdk: boolean;
  driver: boolean;
  nodes: number;
  devices: DecklinkDevice[];
}

/** A card's input: its number, the driver's name, its card (the name given to it, else its model), its rank on it, and what it receives now. */
interface DecklinkDevice {
  number: number;
  name: string;
  card?: string;
  input?: number;
  present?: boolean;
  state?: 'signal' | 'nosignal' | 'busy' | 'unknown' | 'absent' | 'checking';
  format?: string;
  /** The recorder using it. */
  recorder?: string | null;
}

/** The label of a card's input: its card (its name, else its model) and its rank on it. */
function inputLabel(d: DecklinkDevice): string {
  return d.card ? `${d.card} · ${t('Input {n}', { n: d.input ?? 1 })}` : d.name || t('Card {n}', { n: d.number + 1 });
}

/** A hardware input: the card's input (only those receiving a signal, free), the format, the sound and the recording's bitrate. */
/** An NDI source: chosen among those the network announces (or its name written), an address
 * where to look for it when it is not announced here, and the bitrate it is recorded at. */
function NdiFields({ c, set }: { c: Recorder; set: (patch: Partial<Recorder>) => void }) {
  const [info, setInfo] = useState<{ installed: boolean; sources: { name: string; address: string }[] } | null>(null);
  const [n, setN] = useState(0);
  const host = c.host ?? '';
  useEffect(() => {
    let dead = false;
    // A moment after the address is written; again every 5 s (the sources come and go).
    const load = () => api<{ installed: boolean; sources: { name: string; address: string }[] }>('GET', `/api/ndi?host=${encodeURIComponent(host)}`).then((i) => !dead && setInfo(i), () => {});
    const first = setTimeout(load, 400);
    const again = setInterval(load, 5000);
    return () => {
      dead = true;
      clearTimeout(first);
      clearInterval(again);
    };
  }, [host, n]);
  const found = info?.sources ?? [];
  const options = [...found.map((s) => ({ value: s.name, label: s.name, detail: s.address })), ...(c.url && !found.some((s) => s.name === c.url) ? [{ value: c.url, label: c.url, detail: t('not found now') }] : [])];
  return (
    <>
      {info && !info.installed && (
        <div className="sc-alert span2">
          <TriangleAlert size={14} />
          <span>{t('The NDI library is not installed on this server (see the installation guide, “NDI”).')}</span>
        </div>
      )}
      <Field label={t('NDI source')} group className="span2">
        <div className="sc-ndi-pick">
          {options.length > 0 ? <Dropdown value={c.url} options={options} onChange={(v) => set({ url: v })} /> : <span className="muted">{info ? t('No NDI source found on the network') : t('Looking for the NDI sources…')}</span>}
          <button type="button" className="icon-btn" data-tip={t('Search again')} aria-label={t('Search again')} onClick={() => setN((x) => x + 1)}>
            <Search size={14} />
          </button>
        </div>
      </Field>
      <Field label={t('Name of the source')}>
        <input value={c.url} placeholder="MACHINE (Canal 1)" spellCheck={false} onChange={(e) => set({ url: e.target.value })} />
      </Field>
      <Field label={t('Address (optional)')}>
        <input value={host} placeholder="192.168.1.50" spellCheck={false} onChange={(e) => set({ host: e.target.value })} />
      </Field>
      <RateField c={c} set={set} />
      <Field label={t('Recording bitrate')}>
        <NumberField value={c.bitrate} min={5} max={200} step={1} suffix="Mb/s" onChange={(v) => set({ bitrate: v })} />
      </Field>
    </>
  );
}

function DecklinkFields({ c, set, info, detected, rec }: { c: Recorder; set: (patch: Partial<Recorder>) => void; info: DecklinkInfo | null; detected: string; rec: Recorder | null }) {
  const device = c.device ?? 0;
  const channels = c.audioChannels ?? 2;
  // The bitrate advised for the format recorded.
  const format = c.videoMode && c.videoMode !== 'auto' ? c.videoMode : detected;
  const advised = format ? advisedBitrate(format) : 0;
  // The inputs receiving a signal that no other recorder uses (and the one chosen, whatever it receives).
  const all = info?.devices ?? [];
  const devices = all.filter((d) => d.number === device || (d.state === 'signal' && (!d.recorder || d.recorder === rec?.id)));
  const checking = all.some((d) => d.state === 'checking');
  // A new recorder starts on an input that has a signal.
  const firstLive = all.find((d) => d.state === 'signal' && !d.recorder);
  const chosen = all.find((d) => d.number === device);
  useEffect(() => {
    if (!rec && firstLive && (!chosen || chosen.state !== 'signal' || chosen.recorder)) set({ device: firstLive.number });
  }, [firstLive?.number, chosen?.state, chosen?.recorder]); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <>
      {info && (!info.sdk || !info.driver) && (
        <div className="sc-space bad span2">
          <strong>{t('Blackmagic’s Desktop Video driver is not installed on the server.')}</strong>
          <span>{!info.driver ? t('Download Desktop Video for Linux from Blackmagic’s site on your computer, copy it to the server (scp … :~/) and run the install script again: it installs it, step by step. In a virtual machine, the card must first be passed through to it (PCI passthrough).') : t('This server was built without DeckLink support: install a release of Strata LiveCut.')}</span>
        </div>
      )}
      <Field
        className="span2"
        label={t('Input')}
        hint={
          all.length === 0
            ? t('Cards are numbered from 1 in the order the driver sees them.')
            : !all.some((d) => d.state === 'signal' && !d.recorder)
              ? checking
                ? t('Looking at the inputs…')
                : t('No free input receives a signal now: plug a source in (Système › Matériel shows each input).')
              : t('Only the inputs receiving a signal, and no other recorder uses, are listed.')
        }
      >
        {all.length > 0 ? (
          <Dropdown
            value={String(device)}
            options={devices.map((d) => ({
              value: String(d.number),
              label: inputLabel(d) + (d.present === false ? ` · ${t('not detected')}` : ''),
              detail: d.state === 'signal' ? (d.format ? formatName(d.format, lang() === 'fr') : t('Signal')) : d.state === 'busy' ? t('Busy') : t('No signal'),
            }))}
            onChange={(v) => set({ device: Number(v) })}
          />
        ) : (
          <NumberField value={device + 1} min={1} max={64} onChange={(v) => set({ device: Math.max(0, v - 1) })} />
        )}
      </Field>
      <Field label={t('Video input')}>
        <Dropdown value={c.connection ?? 'auto'} options={DL_CONNECTIONS.map((v) => ({ value: v, label: dlConnection(v) }))} onChange={(v) => set({ connection: v })} />
      </Field>
      <Field
        label={<>{t('Format')}<InfoTip text={t('Automatic works with the cards that detect the format of their input. Otherwise choose it: a wrong format gives no picture.')} /></>}
        hint={
          detected && c.videoMode !== 'auto' && c.videoMode !== detected ? (
            <span className="sc-warn-text">
              {t('The input receives {f}.', { f: dlMode(detected) })}{' '}
              <button type="button" className="linklike" onClick={() => set({ videoMode: detected })}>
                {t('Use it')}
              </button>
            </span>
          ) : undefined
        }
      >
        <Dropdown value={c.videoMode ?? 'auto'} options={DL_MODES.map((v) => ({ value: v, label: dlMode(v), detail: formatFamily(v) }))} onChange={(v) => set({ videoMode: v })} />
      </Field>
      <Field label={t('Audio input')}>
        <Dropdown value={c.audioInput ?? 'auto'} options={DL_AUDIO_INPUTS.map((v) => ({ value: v, label: dlAudioInput(v) }))} onChange={(v) => set({ audioInput: v })} />
      </Field>
      <Field label={t('Audio channels')} group hint={tn(channels / 2, 'Recorded as {n} stereo track', 'Recorded as {n} stereo tracks')}>
        <Segmented value={String(channels)} options={[2, 8, 16].map((n) => ({ value: String(n), label: String(n) }))} onChange={(v) => set({ audioChannels: Number(v) })} />
      </Field>
      <RateField c={c} set={set} />
      <Field
        label={<>{t('Recording bitrate')}<InfoTip text={t('The card gives raw pictures: the server encodes them in H.264 at this bitrate (about 1 to 2 processor cores per HD input, 4 to 8 in UHD). 25 Mb/s keeps the HD quality for clips; more takes more disk.')} /></>}
        hint={
          advised && advised !== c.bitrate ? (
            <span>
              {t('Advised for {f}: {n} Mb/s.', { f: dlMode(format), n: advised })}{' '}
              <button type="button" className="linklike" onClick={() => set({ bitrate: advised })}>
                {t('Use it')}
              </button>
            </span>
          ) : undefined
        }
      >
        <NumberField value={c.bitrate} min={5} max={200} step={1} suffix="Mb/s" onChange={(v) => set({ bitrate: v })} />
      </Field>
    </>
  );
}

/** What a card's input receives now: a still every 2 s and its format. */
function InputPreview({ device, connection, mode, onFormat }: { device: number; connection: string; mode: string; onFormat: (f: string) => void }) {
  const [shot, setShot] = useState<{ src: string | null; format: string } | null>(null);
  useEffect(() => {
    let dead = false;
    let timer = 0;
    let last: string | null = null;
    setShot(null);
    const grab = async () => {
      try {
        const r = await fetch(`/api/decklink/${device}/preview.jpg?connection=${connection}&mode=${mode}&t=${Date.now()}`, { credentials: 'same-origin' });
        if (dead) return;
        if (r.status === 200) {
          const src = URL.createObjectURL(await r.blob());
          if (dead) return URL.revokeObjectURL(src);
          const format = r.headers.get('x-format') ?? '';
          if (last) URL.revokeObjectURL(last);
          last = src;
          setShot({ src, format });
          onFormat(format);
        } else {
          setShot({ src: null, format: '' });
          onFormat('');
        }
      } catch {
        if (!dead) setShot({ src: null, format: '' });
      }
      if (!dead) timer = window.setTimeout(() => void grab(), 2000);
    };
    void grab();
    return () => {
      dead = true;
      clearTimeout(timer);
      if (last) URL.revokeObjectURL(last);
    };
  }, [device, connection, mode]); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <div className="sc-dlprev">
      <div className="sc-dlprev-pic">
        {shot?.src ? <img src={shot.src} alt="" /> : <span className="muted">{shot ? t('No signal on this input') : t('Looking at the input…')}</span>}
        {shot?.src && <span className="sc-dlprev-live">{t('Preview')}</span>}
      </div>
      <span className={`sc-dlprev-state ${shot?.src ? 'on' : ''}`}>
        {shot?.src ? (shot.format ? t('Signal: {f}', { f: formatName(shot.format, lang() === 'fr') }) : t('Signal')) : shot ? t('No signal') : ''}
      </span>
    </div>
  );
}

/** A recorder's still, every 3 s. */
function RecorderThumb({ id }: { id: string }) {
  const n = useTick(3000);
  const [ok, setOk] = useState(true);
  // Tried again at each tick (the picture may come later).
  useEffect(() => setOk(true), [n]);
  return (
    <div className="sc-dlprev">
      <div className="sc-dlprev-pic">
        {ok ? <img src={`/api/recorders/${id}/thumb.jpg?n=${n}`} alt="" onError={() => setOk(false)} onLoad={() => setOk(true)} /> : <span className="muted">{t('No picture yet')}</span>}
      </div>
    </div>
  );
}

/** What a source carries, tried before its recorder is made (the server reads it a few seconds). */
interface Probe {
  still: string | null;
  video: { codec: string; width: number; height: number; fps: number; interlaced: boolean } | null;
  audio: number;
  epg: boolean;
  services: string[];
  /** HLS: the qualities its playlist offers (several, else none). */
  variants?: { uri: string; name: string; width: number; height: number; bandwidth: number; fps: number }[];
}

/** The settings being edited are those it records (its state, its picture are then its own). */
function sameSource(rec: Recorder | null, c: Recorder): boolean {
  if (!rec || rec.protocol !== c.protocol) return false;
  if (c.protocol === 'decklink') return rec.device === c.device;
  if (c.protocol === 'pattern') return rec.pattern === c.pattern && rec.videoMode === c.videoMode && rec.bitrate === c.bitrate && rec.name === c.name;
  if (c.protocol === 'ndi') return rec.url === c.url && (rec.host ?? '') === (c.host ?? '');
  if (c.protocol === 'page') return rec.videoMode === c.videoMode && rec.bitrate === c.bitrate && (rec.audioChannels ?? 2) === (c.audioChannels ?? 2);
  return rec.url === c.url && rec.mode === c.mode && rec.port === c.port && rec.iface === c.iface && (rec.variant ?? '') === (c.variant ?? '');
}

/** The source as written can be tried: an address of its kind, a caller (a listening one waits for its source). */
/** How each source's address is written (shown in its field). */
const PLACEHOLDERS: Record<Protocol, string> = { udp: 'udp://239.1.1.1:5000', rtp: 'rtp://239.1.1.1:5004', rtmp: 'rtmp://server/live/key', srt: 'srt://server:9000', hls: 'https://server/live/stream.m3u8', web: 'https://www.youtube.com/watch?v=…', decklink: '', pattern: '', ndi: '', page: '' };

/** The address as written, made usable: no spaces around it, the scheme added when left out
 * ("1.2.3.4:9000" → "srt://1.2.3.4:9000") or written in capitals. */
export function cleaned(c: Recorder): Recorder {
  // Spaces and invisible characters (pasted with it) are no part of an address.
  const url = c.url.replace(/[\s\u200B-\u200D\u2060\uFEFF]/g, '');
  if (c.protocol === 'decklink' || c.protocol === 'web' || c.protocol === 'pattern' || c.protocol === 'page') return { ...c, url };
  // An NDI name is kept as written ("MACHINE (Channel 1)").
  if (c.protocol === 'ndi') return { ...c, url: c.url.trim() };
  if (!url) return c;
  const m = /^([a-z0-9]+):\/\//i.exec(url);
  return { ...c, url: m ? m[1].toLowerCase() + url.slice(m[1].length) : `${c.protocol === 'hls' ? 'https' : c.protocol}://${url}` };
}

function triable(c: Recorder): boolean {
  if (c.protocol === 'decklink' || c.protocol === 'page') return false;
  if (c.protocol === 'pattern') return true;
  if (c.protocol === 'ndi') return c.url.trim().length > 0;
  if ((c.protocol === 'rtmp' || c.protocol === 'srt') && c.mode === 'listener') return false;
  if (c.protocol === 'web') return /^https?:\/\/\S+\.\S+/.test(c.url) && platformOf(c.url) === 'youtube';
  if (c.protocol === 'hls') return /^https?:\/\/\S+\.\S+/.test(c.url);
  return new RegExp(`^${c.protocol}://\\S+`).test(c.url) && /[:.]/.test(c.url.slice(c.protocol.length + 3));
}

/** Tries the source a moment after it is written (and again when asked). */
function useProbe(c: Recorder, on: boolean) {
  const [state, setState] = useState<{ busy: boolean; result: Probe | null; error: string }>({ busy: false, result: null, error: '' });
  const [again, setAgain] = useState(0);
  const key = on ? JSON.stringify([c.protocol, c.url, c.mode, c.iface, c.latency, c.passphrase, (c.cookies ?? '').length, c.variant ?? '', c.pattern ?? '', c.videoMode ?? '', c.host ?? '', c.protocol === 'pattern' ? c.name : '', again]) : '';
  useEffect(() => {
    if (!key) return setState({ busy: false, result: null, error: '' });
    let dead = false;
    const timer = setTimeout(() => {
      setState((s) => ({ ...s, busy: true, error: '' }));
      api<Probe>('POST', '/api/recorders/probe', c).then(
        (result) => !dead && setState({ busy: false, result, error: '' }),
        (e) => !dead && setState({ busy: false, result: null, error: (e as Error).message || 'nothing received from this source' }),
      );
    }, again || c.protocol === 'pattern' ? 0 : 1200);
    return () => {
      dead = true;
      clearTimeout(timer);
    };
  }, [key]); // eslint-disable-line react-hooks/exhaustive-deps
  return { ...state, retry: () => setAgain((n) => n + 1) };
}

/** The right side of the recorder's window: what it receives, its technical details, its programme guide. */
function RecorderSide({ c, rec, st, dl, detected, onFormat, probe }: { c: Recorder; rec: Recorder | null; st?: RecorderStatus; dl: DecklinkInfo | null; detected: string; onFormat: (f: string) => void; probe: ReturnType<typeof useProbe> }) {
  const timezone = useApp((s) => s.timezone);
  const fr = lang() === 'fr';
  const card = c.protocol === 'decklink';
  const device = c.device ?? 0;
  const input = card ? dl?.devices.find((d) => d.number === device) : undefined;
  // The settings being edited are what it records: its state only while they are unchanged.
  const unchanged = sameSource(rec, c);
  const media = st && unchanged ? st.media : null;
  const listening = (c.protocol === 'rtmp' || c.protocol === 'srt') && c.mode === 'listener';
  const tried = probe.result;
  const rows: [string, string][] = [];
  if (c.protocol === 'pattern' || c.protocol === 'ndi' || c.protocol === 'page') {
    rows.push([t('Source'), sourceAddress(c) || '—']);
    rows.push([t('Recording bitrate'), `${c.bitrate} Mb/s · ${c.cbr ? 'CBR' : 'VBR'} · H.264 / AAC`]);
  } else if (card) {
    rows.push([t('Input'), input ? inputLabel(input) : t('Input {n}', { n: device + 1 })]);
    rows.push([t('Format'), detected ? formatName(detected, fr) : dlMode(c.videoMode ?? 'auto')]);
    rows.push([t('Audio'), `${dlAudioInput(c.audioInput ?? 'auto')} · ${tn((c.audioChannels ?? 2) / 2, '{n} stereo track', '{n} stereo tracks')}`]);
    rows.push([t('Recording bitrate'), `${c.bitrate} Mb/s · ${c.cbr ? 'CBR' : 'VBR'} · H.264 / AAC`]);
  } else {
    rows.push([t('Source'), sourceAddress(c) || '—']);
    if (c.protocol === 'srt') rows.push([t('Latency'), `${c.latency} ms`]);
  }
  if (media) {
    rows.push([t('Video'), [CODECS[media.codec] ?? media.codec.toUpperCase(), `${media.width}×${media.height}`, formatName(scanFormat(media), fr)].filter(Boolean).join(' · ')]);
    if (!card) rows.push([t('Audio'), tn(media.audioTracks, '{n} track', '{n} tracks')]);
  } else if (tried) {
    const v = tried.video;
    if (v) {
      // As the formats are named elsewhere ("1080i50", "720p5994").
      const r = v.interlaced ? v.fps * 2 : v.fps;
      const name = v.fps && v.height ? `${v.height}${v.interlaced ? 'i' : 'p'}${Math.abs(r - Math.round(r)) < 0.01 ? Math.round(r) : Math.round(r * 100)}` : '';
      rows.push([t('Video'), [CODECS[v.codec] ?? v.codec.toUpperCase(), v.width ? `${v.width}×${v.height}` : '', name ? formatName(name, fr) : ''].filter(Boolean).join(' · ')]);
    } else rows.push([t('Video'), t('None')]);
    rows.push([t('Audio'), tn(tried.audio, '{n} track', '{n} tracks')]);
    if (tried.services.length) rows.push([t('Service'), tried.services.join(', ')]);
  }
  if (st && rec) {
    rows.push([t('State'), t(STATE_LABEL[st.state] ?? 'Stopped')]);
    if (st.state === 'recording' && st.bitrate) rows.push([t('Bitrate received'), bitrate(st.bitrate, locale())]);
  }
  const ts = c.protocol === 'udp' || c.protocol === 'rtp' || c.protocol === 'srt';
  const prog = (p: { name: string; start: number; end: number }) => `${clock(p.start, timezone).slice(0, 5)}–${clock(p.end, timezone).slice(0, 5)} ${p.name}`;
  return (
    <>
      {/* A web page: no picture to try, no programme guide (it is seen and piloted in the Streamer). */}
      {c.protocol !== 'page' && (
      <section className="sc-rm-sec">
        <h3>{t('Preview')}</h3>
        {card ? (
          <InputPreview device={device} connection={c.connection ?? 'auto'} mode={c.videoMode ?? 'auto'} onFormat={onFormat} />
        ) : unchanged && rec ? (
          <RecorderThumb id={rec.id} />
        ) : (
          <div className="sc-dlprev">
            <div className="sc-dlprev-pic">
              {tried?.still ? (
                <img src={tried.still} alt="" />
              ) : (
                <span className={`sc-rm-note ${probe.error ? 'sc-warn-text' : 'muted'}`}>
                  {c.protocol === 'pattern'
                    ? probe.error
                      ? t(probe.error)
                      : ''
                    : listening
                    ? t('The source connects to this server: its picture shows as soon as it does, once the recorder is created.')
                    : probe.busy
                      ? c.protocol === 'web'
                        ? t('Reading the page and the live…')
                        : t('Trying the source…')
                      : probe.error
                        ? t(probe.error)
                        : tried
                          ? t('This source has no picture.')
                          : c.url.trim() && !triable(cleaned(c))
                            ? t('Incomplete address: it is written as {example}.', { example: PLACEHOLDERS[c.protocol] })
                            : t('Write the address of the source: it is tried at once.')}
                </span>
              )}
              {tried?.still && <span className="sc-dlprev-live">{t('Preview')}</span>}
            </div>
            {!listening && c.protocol !== 'pattern' && triable(cleaned(c)) && (
              <span className={`sc-dlprev-state ${tried ? 'on' : ''}`}>
                {probe.busy ? t('Trying the source…') : tried ? t('The source answers') : probe.error ? t('The source does not answer') : ''}
                <button type="button" className="linklike" disabled={probe.busy} onClick={probe.retry} style={{ marginLeft: 8 }}>
                  {t('Try again')}
                </button>
              </span>
            )}
          </div>
        )}
      </section>
      )}
      <section className="sc-rm-sec">
        <h3>{t('Technical')}</h3>
        <dl className="sc-rm-info">
          {rows.map(([k, v]) => (
            <Fragment key={k}>
              <dt>{k}</dt>
              <dd>{v}</dd>
            </Fragment>
          ))}
        </dl>
      </section>
      {c.protocol !== 'page' && (
      <section className="sc-rm-sec">
        <h3>{t('Programme guide (EPG)')}</h3>
        {st?.onAir ? (
          <dl className="sc-rm-info">
            <dt>{t('On air')}</dt>
            <dd>{prog(st.onAir)}</dd>
            {st.next && (
              <>
                <dt>{t('Next')}</dt>
                <dd>{prog(st.next)}</dd>
              </>
            )}
          </dl>
        ) : (
          <p className="muted sc-rm-note">
            {!ts
              ? t('This kind of source carries no programme guide.')
              : tried
                ? tried.epg
                  ? t('The stream carries a programme guide: it is read once recording.')
                  : t('The stream carries no programme guide.')
                : rec
                  ? t('This source sends no programme guide (yet).')
                  : t('Read from the stream once it is recorded (MPEG-TS: UDP, RTP, SRT).')}
          </p>
        )}
      </section>
      )}
    </>
  );
}

/** A logo to send with the recorder: a file, an address, or none any more. */
type LogoChange = { file?: File; url?: string; remove?: boolean } | null;

/** The recorder's logo: dropped or chosen (PNG, JPEG, WebP, GIF, SVG, 2 MB), or from an address. */
function LogoField({ rec, change, onChange }: { rec: Recorder | null; change: LogoChange; onChange: (c: LogoChange) => void }) {
  const toast = useApp((s) => s.toast);
  const [over, setOver] = useState(false);
  const [url, setUrl] = useState('');
  const [preview, setPreview] = useState<string | null>(null);
  useEffect(() => {
    if (!change?.file) return setPreview(null);
    const u = URL.createObjectURL(change.file);
    setPreview(u);
    return () => URL.revokeObjectURL(u);
  }, [change]);
  const take = (f: File | undefined) => {
    if (!f) return;
    if (!/^image\/(png|jpeg|webp|gif|svg\+xml)$/.test(f.type)) return toast(t('the logo is a PNG, JPEG, WebP, GIF or SVG image'), 'error');
    if (f.size > 2 << 20) return toast(t('the logo is 2 MB at most'), 'error');
    onChange({ file: f });
  };
  const shown = change?.remove ? null : change?.file ? preview : change?.url ? change.url : rec?.logo ? `/api/recorders/${rec.id}/logo?v=${rec.logo}` : null;
  return (
    <Field label={t('Logo')} group className="span2" hint={t('PNG, JPEG, WebP, GIF or SVG, 2 MB at most. Shown with the recorder’s name.')}>
      <div className="sc-logo-field">
        <label
          className={`sc-logo-drop ${over ? 'over' : ''}`}
          onDragOver={(e) => {
            e.preventDefault();
            setOver(true);
          }}
          onDragLeave={() => setOver(false)}
          onDrop={(e) => {
            e.preventDefault();
            setOver(false);
            const f = e.dataTransfer.files?.[0];
            if (f) return take(f);
            // An image dragged from another page: its address.
            const u = e.dataTransfer.getData('text/uri-list') || e.dataTransfer.getData('text/plain');
            if (/^https?:\/\//.test(u)) onChange({ url: u.trim() });
          }}
        >
          {shown ? <img src={shown} alt="" /> : <ImagePlus size={22} />}
          <span>{shown ? t('Drop another image, or click to choose one') : t('Drop an image here, or click to choose one')}</span>
          <input type="file" accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml" hidden onChange={(e) => take(e.target.files?.[0])} />
        </label>
        <div className="sc-logo-url">
          <input value={url} placeholder={t('or the address of an image (https://…)')} spellCheck={false} onChange={(e) => setUrl(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
          <button type="button" className="btn small" disabled={!/^https?:\/\/./.test(url.trim())} onClick={() => onChange({ url: url.trim() })}>
            {t('Use it')}
          </button>
          {shown && (
            <button type="button" className="btn small ghost" onClick={() => onChange(rec?.logo ? { remove: true } : null)}>
              {t('Remove')}
            </button>
          )}
        </div>
      </div>
    </Field>
  );
}

/** For a source the server encodes: a variable rate (the quality kept, the bitrate a ceiling) or a constant one. */
function RateField({ c, set }: { c: Pick<Recorder, 'cbr'>; set: (p: Partial<Recorder>) => void }) {
  return (
    <Field label={t('Rate')} group>
      <Segmented
        value={c.cbr ? 'cbr' : 'vbr'}
        options={[
          { value: 'vbr', label: t('Variable (VBR)') },
          { value: 'cbr', label: t('Constant (CBR)') },
        ]}
        onChange={(v) => set({ cbr: v === 'cbr' })}
      />
    </Field>
  );
}
