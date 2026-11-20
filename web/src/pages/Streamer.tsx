/*
 * Streamer: a source sent live to destinations (Système › Destinations). Laid out as
 * Extraction: the sources on the left, the picture, the destinations on the right (one switch
 * each: on, it sends now), the bandwidth going out below. A destination already sending
 * another source is greyed; one whose key is given for each live asks for it here; each can
 * be tried before going live.
 */
import { useEffect, useRef, useState } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { AlertTriangle, Check, Play, Plug, Radio, RadioTower, Square, Upload } from 'lucide-react';
import { useDisplayPref } from '../displayPrefs';
import { api, serverNow, useApp } from '../store';
import { lang, locale, t, tn } from '../i18n';
import { navigate } from '../router';
import { LoopPlayer } from '../player';
import { fpsOf, scanFormat, timecode, formatName } from '../tc';
import { Empty } from '../ui/Empty';
import { RecLogo } from '../ui/RecLogo';
import { NumberField } from '../ui/fields';
import { Meters, VolumeControl, useAudioOut } from '../ui/AudioUi';
import { clockOf, mbps, useDestinations, useStreams, volume, type Destination, type StreamInfo } from '../streams';
import { encodingOf, ProtoTag, Spark } from '../settings/Destinations';
import type { Recorder } from '../types';
import { explainGuard } from '../ui/guard';

const COLORS = ['var(--accent)', 'var(--pvw)', 'var(--warn)', '#c084fc', '#38bdf8'];

export function Streamer({ source }: { source?: string }) {
  const recorders = useApp((s) => s.recorders);
  const streams = useStreams(1000);
  const [dests] = useDestinations();
  const [last, setLast] = useDisplayPref<string | null>('streamer.source', null);
  const known = (x?: string | null) => !!x && recorders.some((r) => r.id === x);
  const id = known(source) ? source : known(last) ? last! : recorders[0]?.id;
  useEffect(() => {
    if (id && id !== source) void navigate(`/streamer/${id}`, true);
    if (id && id !== last) setLast(id);
  }, [id, source]); // eslint-disable-line react-hooks/exhaustive-deps
  const rec = recorders.find((r) => r.id === id);
  const mine = streams.filter((s) => s.source === id);
  return (
    <section className="sc-edit st-page">
      <aside className="lib-nav">
        <div className="lib-nav-section">
          <span className="grow">{t('Sources')}</span>
        </div>
        {recorders.map((r) => (
          <SourceItem key={r.id} r={r} on={r.id === id} air={streams.filter((s) => s.source === r.id).length} />
        ))}
      </aside>
      {rec ? <Center key={rec.id} rec={rec} streams={mine} /> : <div className="sc-center"><Empty icon={RadioTower} text={t('No recorder')} /></div>}
      <aside className="panel right st-panel">{rec && <DestPanel rec={rec} dests={dests} streams={streams} />}</aside>
      <div className="st-bw">
        <Bandwidth streams={mine} dests={dests ?? []} />
      </div>
    </section>
  );
}

function SourceItem({ r, on, air }: { r: Recorder; on: boolean; air: number }) {
  const st = useApp((s) => s.status[r.id]?.state);
  return (
    <div className={`lib-nav-item sc-src ${on ? 'on' : ''}`} onClick={() => void navigate(`/streamer/${r.id}`)}>
      <span className={`nav-dot ${st === 'recording' ? 'ok' : st === 'off' || st === 'waiting' ? '' : st === 'error' ? 'bad' : 'warn'}`} />
      <RecLogo rec={r} size="sm" />
      <span className="sc-src-t">
        <span className="sc-src-name">{r.name}</span>
        <span className="sc-src-prog">{st === 'recording' ? t('Received') : st === 'off' || st === 'waiting' ? t('Not received now') : st === 'error' ? t('Error') : t('No signal')}</span>
      </span>
      {air > 0 && <span className="st-air">ON AIR</span>}
    </div>
  );
}

/** The picture of the source, live; what it receives; the lives of this source. */
function Center({ rec, streams }: { rec: Recorder; streams: StreamInfo[] }) {
  const { st, timezone, toast } = useApp(useShallow((s) => ({ st: s.status[rec.id], timezone: s.timezone, toast: s.toast })));
  const video = useRef<HTMLVideoElement>(null);
  const audio = useAudioOut(video);
  const [wall, setWall] = useState<number | null>(null);
  useEffect(() => {
    const p = new LoopPlayer(video.current!, rec.id, serverNow);
    p.onFrame = (w) => setWall(w);
    void p.goLive(serverNow());
    return () => p.destroy();
  }, [rec.id]);
  const m = st?.media;
  const fps = fpsOf(m?.fpsNum, m?.fpsDen);
  const received = st?.state === 'recording';
  const live = streams.filter((s) => s.state === 'live');
  const since = Math.min(...streams.map((s) => s.liveSince ?? Infinity));
  const out = streams.reduce((a, s) => a + s.kbps, 0);
  const stopAll = async () => {
    try {
      await api('POST', '/api/streams/stop', { source: rec.id });
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  return (
    <div className="sc-center">
      <div className="sc-viewer-wrap">
        <div className="sc-viewer">
          <video ref={video} className="sc-video" playsInline />
          {!received && <span className="sc-onpic mid">{st?.state === 'starting' ? t('Connecting…') : t('Not received now: it will be as soon as a destination is switched on.')}</span>}
          <Meters out={audio.out} />
        </div>
      </div>
      <div className="sc-transport">
        <div className="sc-tc-big">{wall ? timecode(wall, fps, timezone) : '--:--:--:--'}</div>
        <div className="sc-sep" />
        <span className="st-info">
          <b>{rec.name}</b>
          {m ? ` · ${m.width}×${m.height} · ${formatName(scanFormat(m), lang() === 'fr')}` : ''}
          {received && st?.bitrate ? ` · ${t('{b} received', { b: mbps(st.bitrate / 1000, locale()) })}` : ''}
        </span>
        <div className="spacer" />
        <VolumeControl volume={audio.volume} setVolume={audio.setVolume} muted={audio.muted} setMuted={audio.setMuted} />
      </div>
      <div className={`st-status ${streams.length ? 'on' : ''}`}>
        {streams.length ? (
          <>
            <span className="st-live-pic">
              <span className="sc-rec" />
              {live.length ? `${t('LIVE')} · ${clockOf(Number.isFinite(since) ? serverNow() - since : 0)}` : t('Connecting…')}
            </span>
            <span>{tn(streams.length, '{n} destination', '{n} destinations')}</span>
            <span>
              <Upload size={12} /> <b>{mbps(out, locale())}</b> {t('going out')}
            </span>
            <div className="spacer" />
            <button className="btn st-stop" onClick={() => void stopAll()}>
              <Square size={13} /> {t('Stop all')}
            </button>
          </>
        ) : (
          <span className="muted">{t('Not sent anywhere: switch a destination on, on the right.')}</span>
        )}
      </div>
    </div>
  );
}

/** What the operator gives for a live (when handed over), and the last test of each destination. */
interface Draft {
  key: string;
  url: string;
  videoKbps: number;
  test?: { busy: boolean; ok?: boolean; text?: string };
}

function DestPanel({ rec, dests, streams }: { rec: Recorder; dests: Destination[] | null; streams: StreamInfo[] }) {
  const { recorders, toast } = useApp(useShallow((s) => ({ recorders: s.recorders, toast: s.toast })));
  const [starting, setStarting] = useState(false);
  const [drafts, setDrafts] = useState<Record<string, Draft>>({});
  const draft = (d: Destination): Draft => drafts[d.id] ?? { key: '', url: d.url ?? '', videoKbps: d.videoKbps ?? 6000 };
  const setDraft = (d: Destination, p: Partial<Draft>) => setDrafts((x) => ({ ...x, [d.id]: { ...draft(d), ...p } }));
  const nameOf = (id: string) => recorders.find((r) => r.id === id)?.name ?? id;
  const mine = streams.filter((s) => s.source === rec.id);
  const overrides = (d: Destination) => {
    const x = draft(d);
    return { key: d.keyLive ? x.key : '', url: d.exposeUrl ? x.url : '', videoKbps: d.exposeBitrate ? x.videoKbps : 0 };
  };
  /** Starts (true: done; false: refused by the guard, which said why). */
  const start = async (d: Destination): Promise<boolean> => {
    try {
      await api('POST', '/api/streams', { destination: d.id, source: rec.id, ...overrides(d) });
    } catch (e) {
      if (explainGuard(e)) return false;
      toast(`${d.name} : ${t((e as Error).message)}`, 'error');
    }
    return true;
  };
  const stop = async (d: Destination) => {
    try {
      await api('DELETE', `/api/streams/${d.id}`);
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  // The free destinations ready to go (a key given for each live: once it is written).
  const ready = (dests ?? []).filter((d) => !streams.some((s) => s.destination === d.id) && (!d.keyLive || draft(d).key.trim()));
  const startAll = async () => {
    setStarting(true);
    for (const d of ready) if (!(await start(d))) break;
    setStarting(false);
  };
  const test = async (d: Destination) => {
    setDraft(d, { test: { busy: true } });
    try {
      const r = await api<{ ms: number; keyChecked: boolean }>('POST', `/api/destinations/${d.id}/test`, overrides(d));
      setDraft(d, { test: { busy: false, ok: true, text: r.keyChecked ? t('Test passed · {ms} ms: ready', { ms: r.ms }) : t('The server answers ({ms} ms): give the key of this live', { ms: r.ms }) } });
    } catch (e) {
      setDraft(d, { test: { busy: false, ok: false, text: t('Test failed: {why}', { why: t((e as Error).message) }) } });
    }
  };
  const out = mine.reduce((a, s) => a + s.kbps, 0);
  return (
    <section className="insp-section sc-guide-sec">
      <header className="st-panel-head">
        <span>{t('Destinations')}</span>
        {mine.length > 0 && <em>{`${tn(mine.length, '{n} live', '{n} live')} · ${mbps(out, locale())}`}</em>}
      </header>
      {ready.length > 1 && (
        <button type="button" className="btn primary st-all" disabled={starting} onClick={() => void startAll()}>
          <Play size={13} /> {t('Start all ({n})', { n: ready.length })}
        </button>
      )}
      {dests && dests.length === 0 && <Empty icon={Radio} text={t('No destination')} />}
      <div className="st-dests">
        {[...(dests ?? [])]
          // Those sending this source first, then the free ones, the busy ones last.
          .sort((a, b) => {
            const rank = (d: Destination) => {
              const s = streams.find((x) => x.destination === d.id);
              return !s ? 1 : s.source === rec.id ? 0 : 2;
            };
            return rank(a) - rank(b);
          })
          .map((d) => {
          const s = streams.find((x) => x.destination === d.id);
          const busy = !!s && s.source !== rec.id;
          const on = !!s && !busy;
          const x = draft(d);
          return (
            <div key={d.id} className={`st-dest ${on ? 'on' : ''} ${busy ? 'busy' : ''}`}>
              <div className="st-dest-top">
                <ProtoTag p={d.protocol} />
                <strong data-tip={`${d.host} · ${encodingOf(d)}`}>{d.name}</strong>
                {on ? (
                  <button type="button" className="btn small st-stop-one" onClick={() => void stop(d)}>
                    <Square size={11} /> {t('Stop')}
                  </button>
                ) : busy ? (
                  <button type="button" className="btn small" disabled>
                    {t('Busy')}
                  </button>
                ) : (
                  <button type="button" className="btn small primary" disabled={starting || (d.keyLive && !x.key.trim())} onClick={() => void start(d)}>
                    <Play size={11} /> {t('Go live')}
                  </button>
                )}
              </div>
              {busy && s && (
                <div className="st-note">
                  {t('Busy: sends {source}', { source: nameOf(s.source) })}
                  {s.liveSince ? ` ${t('for {d}', { d: clockOf(serverNow() - s.liveSince) })}` : ''}
                </div>
              )}
              {on && s && (
                <>
                  <Spark values={s.history} />
                  <div className="st-metrics">
                    <div>
                      <span>{t('Bitrate')}</span>
                      <b className="live">{mbps(s.kbps, locale())}</b>
                    </div>
                    <div>
                      <span>{t('For')}</span>
                      <b>{s.liveSince ? clockOf(serverNow() - s.liveSince) : '—'}</b>
                    </div>
                    <div>
                      <span>{t('Sent')}</span>
                      <b>{volume(s.bytes, locale(), lang() === 'fr')}</b>
                    </div>
                    <div>
                      <span>{t('State')}</span>
                      <b className={s.state === 'live' ? 'ok' : 'bad'}>{s.state === 'live' ? t('Live') : s.state === 'connecting' ? t('Connecting…') : t('Reconnecting')}</b>
                    </div>
                  </div>
                  {(s.message || s.note) && (
                    <div className={`st-note ${s.message ? 'bad' : ''}`}>
                      {s.message ? <AlertTriangle size={11} /> : null} {t(s.message || s.note)}
                    </div>
                  )}
                </>
              )}
              {!on && !busy && (
                <>
                  {d.keyLive && (
                    <label className="st-field">
                      <span>{t('Stream key of this live')}</span>
                      <input type="password" value={x.key} autoComplete="off" spellCheck={false} placeholder={t('Paste the key given by the platform')} onChange={(e) => setDraft(d, { key: e.target.value, test: undefined })} />
                    </label>
                  )}
                  {d.exposeUrl && (
                    <label className="st-field">
                      <span>{t('Server address')}</span>
                      <input value={x.url} spellCheck={false} onChange={(e) => setDraft(d, { url: e.target.value, test: undefined })} />
                    </label>
                  )}
                  {d.exposeBitrate && (
                    <label className="st-field">
                      <span>{t('Video bitrate')}</span>
                      <NumberField value={x.videoKbps / 1000} min={0.3} max={50} step={0.5} precision={1} suffix="Mb/s" onChange={(v) => setDraft(d, { videoKbps: Math.round(v * 1000) })} />
                    </label>
                  )}
                  <div className="st-test">
                    <button type="button" className="btn small" disabled={x.test?.busy} onClick={() => void test(d)}>
                      <Plug size={12} className={x.test?.busy ? 'spin' : undefined} /> {x.test?.busy ? t('Testing…') : t('Test')}
                    </button>
                    {x.test?.text && <span className={`st-note ${x.test.ok ? 'ok' : 'bad'}`}>{x.test.ok ? <Check size={11} /> : <AlertTriangle size={11} />} {x.test.text}</span>}
                  </div>
                </>
              )}
            </div>
          );
        })}
      </div>
    </section>
  );
}

/** The bandwidth going out (this source's lives), the last 10 minutes: the lines in an SVG
 * stretched to the width, the numbers in HTML (not stretched with it). */
function Bandwidth({ streams, dests }: { streams: StreamInfo[]; dests: Destination[] }) {
  const n = 600;
  const lines = streams.map((s) => {
    const h = s.history.slice(-n);
    return Array.from({ length: n }, (_, i) => h[i - (n - h.length)] ?? 0);
  });
  const total = Array.from({ length: n }, (_, i) => lines.reduce((a, l) => a + l[i], 0));
  const peak = Math.max(0, ...total);
  const max = Math.max(1000, Math.ceil((peak * 1.2) / 4000) * 4000);
  const pts = (vals: number[]) => vals.map((v, i) => `${((i / (n - 1)) * 1000).toFixed(1)},${(100 - (v / max) * 100).toFixed(2)}`).join(' ');
  const now = total[n - 1] ?? 0;
  const name = (id: string) => dests.find((d) => d.id === id)?.name ?? id;
  return (
    <div className="st-bw-in">
      <div className="st-bw-head">
        <strong>{t('Outgoing bandwidth')}</strong>
        {streams.length > 0 && <span>{t('{now} now · peak {peak} · last 10 minutes', { now: mbps(now, locale()), peak: mbps(peak, locale()) })}</span>}
        <div className="spacer" />
        {streams.length > 0 && (
          <span className="lg">
            <i style={{ background: 'var(--live)' }} />
            {t('Total')}
          </span>
        )}
        {streams.length > 1 &&
          streams.map((s, i) => (
            <span key={s.destination} className="lg">
              <i style={{ background: COLORS[i % COLORS.length] }} />
              {name(s.destination)}
            </span>
          ))}
      </div>
      <div className="st-bw-chart">
        <div className="st-bw-plot">
          <svg viewBox="0 0 1000 100" preserveAspectRatio="none" aria-hidden="true">
            {[0, 25, 50, 75, 100].map((y) => (
              <line key={y} x1="0" x2="1000" y1={y} y2={y} className="grid" vectorEffect="non-scaling-stroke" />
            ))}
            {streams.length > 0 && (
              <>
                <polygon points={`0,100 ${pts(total)} 1000,100`} className="area" />
                <polyline points={pts(total)} fill="none" stroke="var(--live)" strokeWidth="1.6" vectorEffect="non-scaling-stroke" />
                {lines.length > 1 && lines.map((l, i) => <polyline key={i} points={pts(l)} fill="none" stroke={COLORS[i % COLORS.length]} strokeWidth="1.2" vectorEffect="non-scaling-stroke" />)}
              </>
            )}
          </svg>
          <div className="st-bw-x">
            {['−10 min', '−8', '−6', '−4', '−2', t('now')].map((l) => (
              <span key={l}>{l}</span>
            ))}
          </div>
        </div>
        <div className="st-bw-y">
          {[1, 0.75, 0.5, 0.25, 0].map((f) => (
            <span key={f}>{((max * f) / 1000).toLocaleString(locale(), { maximumFractionDigits: 1 })}</span>
          ))}
        </div>
      </div>
    </div>
  );
}


