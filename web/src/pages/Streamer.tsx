/*
 * Streamer: a source sent live to destinations (Système › Destinations). The sources on the left,
 * the destinations on the right (as cards: each live's bitrate, time and sent); in the middle the
 * programme: the picture as it goes out (its overlay placed and sized on it), what is on air (the
 * source or its image, the fade, the overlay), the sound of its tracks; the source's pictures in
 * its settings. A destination already sending another source is greyed; one whose key is given
 * for each live asks for it here; each can be tried before going live.
 */
import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useShallow } from 'zustand/react/shallow';
import { AlertTriangle, Check, Play, Plug, Radio, RadioTower, Square } from 'lucide-react';
import { useDisplayPref } from '../displayPrefs';
import { api, can, serverNow, useApp } from '../store';
import { lang, locale, t, tc, tn } from '../i18n';
import { navigate } from '../router';
import { LoopPlayer } from '../player';
import { fpsOf, timecode } from '../tc';
import { Empty } from '../ui/Empty';
import { useAudioOut } from '../ui/AudioUi';
import { clockOf, mbps, useDestinations, useStreams, volume, type Destination, type StreamInfo } from '../streams';
import { encodingOf, ProtoTag, Spark } from '../settings/Destinations';
import type { Recorder } from '../types';
import { explainGuard } from '../ui/guard';
import { confirmDialog } from '../ui/dialogs';
import { SourceGroups } from '../ui/SourceGroups';
import { usePageTools } from '../ui/PageTools';
import { Mixer, PicturesModal, ProgramLayer, useProgram, type Tab } from './StreamerProgram';
import { OverlayEditor, PassBar, type Place } from './StreamerPass';

/** A message of the server, translated: whole, or its known beginning and what follows (FFmpeg's words). */
function said(m: string): string {
  const k = m.indexOf(': ');
  return k > 0 && t(m) === m ? `${t(m.slice(0, k))} : ${said(m.slice(k + 2))}` : t(m);
}

export function Streamer({ source }: { source?: string }) {
  const { recorders, status } = useApp(useShallow((s) => ({ recorders: s.recorders, status: s.status })));
  // With the lives that could not start (said on their destination, with why).
  const all = useStreams(1000, true);
  const streams = all.filter((s) => s.state !== 'failed');
  const [dests] = useDestinations();
  const [last, setLast] = useDisplayPref<string | null>('streamer.source', null);
  const known = (x?: string | null) => !!x && recorders.some((r) => r.id === x);
  // The source asked for; else the last one seen if it is received now, else the first received.
  const available = (x?: string | null) => known(x) && status[x!]?.state === 'recording';
  const id = known(source) ? source : available(last) ? last! : recorders.find((r) => available(r.id))?.id;
  useEffect(() => {
    if (id && id !== source) void navigate(`/streamer/${id}`, true);
    if (id && id !== last) setLast(id);
  }, [id, source]); // eslint-disable-line react-hooks/exhaustive-deps
  const rec = recorders.find((r) => r.id === id);
  return (
    <section className="sc-edit st-page">
      <SourceGroups
        kind="stream"
        current={id}
        base="/streamer"
        badge={(r) => {
          const n = streams.filter((s) => s.source === r.id).length;
          return n > 0 ? (
            <span className="sc-grp-air" data-tip={tn(n, 'Sent to {n} destination', 'Sent to {n} destinations')}>
              <Radio size={12} />
              {n}
            </span>
          ) : null;
        }}
      />
      {rec ? <Center key={rec.id} rec={rec} /> : <div className="sc-center"><Empty icon={RadioTower} text={t('No recorder')} /></div>}
      <aside className="panel right st-panel">{rec && <DestPanel rec={rec} dests={dests} streams={streams} failed={all.filter((s) => s.state === 'failed')} />}</aside>
    </section>
  );
}

/** The programme of the source: the picture as it goes out, what is on air, the sound. */
function Center({ rec }: { rec: Recorder }) {
  const { st, timezone } = useApp(useShallow((s) => ({ st: s.status[rec.id], timezone: s.timezone })));
  const run = useApp((s) => can.has(s.auth?.user, 'stream.run'));
  const prog = useProgram(rec.id);
  const video = useRef<HTMLVideoElement>(null);
  const audio = useAudioOut(video, 'streamer');
  const [wall, setWall] = useState<number | null>(null);
  const [settings, setSettings] = useState<Tab | null>(null);
  // Each passage (its AUTO fills while its fade goes).
  const [passing, setPassing] = useState({ image: 0, overlay: 0 });
  const overlayAt = (p: { x: number; y: number; size: number; overlayOn: boolean }) => ({ on: p.overlayOn, x: p.x, y: p.y, size: p.size });
  // The image on air or taken off (the source back): CUT at once, AUTO by its fade.
  const passImage = (cut: boolean) => {
    const p = prog.state?.program;
    if (!p || !run) return;
    prog.pass({ image: !p.imageOn, overlay: overlayAt(p) }, cut ? 0 : p.fadeSecs);
    setPassing((x) => ({ ...x, image: x.image + 1 }));
  };
  // The overlay on air or taken off, where it is placed.
  const passOverlay = (cut: boolean) => {
    const p = prog.state?.program;
    if (!p || !run || !p.overlay) return;
    prog.pass({ image: p.imageOn, overlay: { ...overlayAt(p), on: !p.overlayOn } }, cut ? 0 : (p.overlayFadeSecs ?? 1));
    setPassing((x) => ({ ...x, overlay: x.overlay + 1 }));
  };
  // The overlay moved (on the window, or from its window): kept; on air, there at once.
  const place = (at: Place) => {
    const p = prog.state?.program;
    if (!p || !run) return;
    prog.pass({ image: p.imageOn, overlay: { on: p.overlayOn, ...at } }, 0);
  };
  const keysRef = useRef({ passImage, passOverlay });
  keysRef.current = { passImage, passOverlay };
  // Space: the image's AUTO; Enter: its CUT; H: the overlay's AUTO (not while writing, nor in a
  // window, nor while piloting a page).
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      const which = e.code === 'Space' ? 'auto' : e.key === 'Enter' ? 'cut' : e.key === 'h' || e.key === 'H' ? 'overlay' : null;
      if (!which || e.repeat || e.ctrlKey || e.metaKey || e.altKey) return;
      const el = e.target as HTMLElement | null;
      if (el && (el.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT', 'BUTTON', 'A'].includes(el.tagName))) return;
      if (document.querySelector('.modal-back')) return;
      e.preventDefault();
      if (which === 'overlay') keysRef.current.passOverlay(false);
      else keysRef.current.passImage(which === 'cut');
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }, []);
  const player = useRef<LoopPlayer | null>(null);
  // The moment the picture shows (server ms): the meters show the sound of that moment.
  // The timestamp of the frame shown (the source's, ms): the meters show its sound.
  const shown = useRef<number | null>(null);
  const received = st?.state === 'recording';
  // A web page: its browser's bar, its reframing, its piloting.
  const page = usePageTools(rec.id, rec.protocol === 'page', run && received);
  // Only what is received now is shown: a stopped source shows nothing (not what is left of
  // its last recording).
  useEffect(() => {
    if (!received) {
      setWall(null);
      return;
    }
    const p = new LoopPlayer(video.current!, rec.id, serverNow);
    p.onFrame = (w, tt) => {
      setWall(w);
      shown.current = Math.round(tt * 1000);
    };
    p.onFollowers = setFollowers;
    player.current = p;
    void p.goLive(serverNow()).then(() => {
      if (heardRef.current) void p.setAudioTrack(heardRef.current);
      p.setOtherTracks(othersRef.current);
    });
    return () => {
      player.current = null;
      p.destroy();
      setFollowers(new Map());
    };
  }, [rec.id, received]);
  // The headphones (the browser's own: nothing of it goes out). A track alone (solo), at its
  // fader's level; else the output as it goes out: the tracks on together (the first played by
  // the picture's element, the others each by its own, on the picture's time), each at its
  // level, the output's level, the channels, and silent under the image with the same fade.
  const [solo, setSolo] = useState<number | null>(null);
  const pg = prog.state?.program;
  const nTracks = prog.state?.tracks.length ?? 0;
  const isOn = (k: number) => pg?.tracks[k]?.on ?? k === 0;
  const ons = Array.from({ length: nTracks }, (_, k) => k).filter(isOn);
  const firstOn = ons[0];
  const heard = solo ?? firstOn ?? 0;
  const heardRef = useRef(heard);
  heardRef.current = heard;
  const others = solo !== null ? [] : ons.slice(1);
  const othersRef = useRef(others);
  othersRef.current = others;
  const [followers, setFollowers] = useState<Map<number, HTMLMediaElement>>(new Map());
  useEffect(() => {
    void player.current?.setAudioTrack(heard);
  }, [heard]);
  useEffect(() => {
    player.current?.setOtherTracks(others);
  }, [others.join(',')]); // eslint-disable-line react-hooks/exhaustive-deps
  const gainOf = (k: number) => pg?.tracks[k]?.gain ?? 0;
  const factor = (db: number) => 10 ** (db / 20);
  // Each track at its fader; the output's level after (none under the image).
  const level = !pg || solo !== null ? 1 : pg.imageOn || firstOn === undefined ? 0 : factor(pg.master);
  useEffect(() => audio.out?.setMain(pg ? factor(gainOf(heard)) : 1)); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    audio.out?.setOthers(new Map([...followers].filter(([k]) => others.includes(k)).map(([k, el]) => [el, factor(gainOf(k))])));
  }); // eslint-disable-line react-hooks/exhaustive-deps
  // The image coming or going: the sound fades as the picture does; a fader: at once.
  const imageWas = useRef(pg?.imageOn);
  useEffect(() => {
    const switched = imageWas.current !== undefined && imageWas.current !== pg?.imageOn;
    imageWas.current = pg?.imageOn;
    audio.out?.setProgram(level, switched && solo === null ? prog.lastFade : 0);
  }, [audio.out, level]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => audio.out?.setChannels(solo !== null ? 'lr' : (pg?.channels ?? 'lr')), [audio.out, solo, pg?.channels]);
  const m = st?.media;
  const fps = fpsOf(m?.fpsNum, m?.fpsDen);
  return (
    <div className="sc-center pg-center">
      {page.bar && <div className="ps-bar">{page.bar}</div>}
      <div className="ps-one">
        <div className="ps-win air">
          <video ref={video} className="sc-video" playsInline style={received ? undefined : { visibility: 'hidden' }} />
          {!received && <span className="sc-onpic mid">{st?.state === 'starting' ? t('Connecting…') : t('Source stopped')}</span>}
          {received && page.mode === 'view' && <ProgramLayer rec={rec} prog={prog} />}
          {page.mode === 'view' && <OverlayEditor prog={prog} editable={run} onPlace={place} />}
          {page.over}
        </div>
        {page.below ?? (
          <div className="ps-tcrow">
            <span className="pg-tc">{wall ? timecode(wall, fps, timezone) : '--:--:--:--'}</span>
          </div>
        )}
      </div>
      {!page.below && prog.state && <PassBar rec={rec} prog={prog} editable={run} passing={passing} onImage={passImage} onOverlay={passOverlay} onPictures={setSettings} />}
      <Mixer rec={rec} prog={prog} shown={() => shown.current} monitor={{ volume: audio.volume, setVolume: audio.setVolume, muted: audio.muted, setMuted: audio.setMuted, solo, setSolo, heard }} />
      {settings && createPortal(<PicturesModal rec={rec} prog={prog} use={settings} onPlace={place} onClose={() => setSettings(null)} />, document.body)}
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

/** How long a trouble or a test's answer stays on a card. */
const SAID_MS = 8000;

function DestPanel({ rec, dests, streams, failed }: { rec: Recorder; dests: Destination[] | null; streams: StreamInfo[]; failed: StreamInfo[] }) {
  const run = useApp((s) => can.has(s.auth?.user, 'stream.run'));
  const { recorders, toast } = useApp(useShallow((s) => ({ recorders: s.recorders, toast: s.toast })));
  const [starting, setStarting] = useState(false);
  // A countdown shown: drawn again every second.
  const counting = streams.some((s) => s.stopAt);
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!counting) return;
    const i = window.setInterval(() => setTick((n) => n + 1), 1000);
    return () => clearInterval(i);
  }, [counting]);
  const [drafts, setDrafts] = useState<Record<string, Draft>>({});
  const draft = (d: Destination): Draft => drafts[d.id] ?? { key: '', url: d.url ?? '', videoKbps: d.videoKbps ?? 6000 };
  const setDraft = (d: Destination, p: Partial<Draft>) => setDrafts((x) => ({ ...x, [d.id]: { ...draft(d), ...p } }));
  const nameOf = (id: string) => recorders.find((r) => r.id === id)?.name ?? id;
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
    if (!(await confirmDialog({ title: t('Stop the live to “{name}”?', { name: d.name }), confirm: t('Stop'), danger: true }))) return;
    try {
      await api('DELETE', `/api/streams/${d.id}`);
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  // This source's lives: how many, their bitrate, all stopped at once.
  const mine = streams.filter((s) => s.source === rec.id);
  const stopAll = async () => {
    if (!(await confirmDialog({ title: tn(mine.length, 'Stop the live of “{source}”?', 'Stop the {n} lives of “{source}”?', { source: rec.name }), confirm: t('Stop all'), danger: true }))) return;
    try {
      await api('POST', '/api/streams/stop', { source: rec.id });
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
    let shown: Draft['test'];
    try {
      const r = await api<{ ms: number; keyChecked: boolean }>('POST', `/api/destinations/${d.id}/test`, overrides(d));
      shown = { busy: false, ok: true, text: r.keyChecked ? t('Test passed · {ms} ms: ready', { ms: r.ms }) : t('The server answers ({ms} ms): give the key of this live', { ms: r.ms }) };
    } catch (e) {
      shown = { busy: false, ok: false, text: t('Test failed: {why}', { why: t((e as Error).message) }) };
    }
    setDraft(d, { test: shown });
    // Read, then gone (unless another test said something since).
    window.setTimeout(() => setDrafts((x) => (x[d.id]?.test === shown ? { ...x, [d.id]: { ...x[d.id], test: undefined } } : x)), SAID_MS);
  };
  // A trouble said on a card a few seconds, from when it first shows (none while connecting: the
  // chip says it).
  const firstSeen = useRef(new Map<string, number>());
  const fresh = (key: string, text: string) => {
    if (!text) return '';
    const k = `${key}\n${text}`;
    const at = firstSeen.current.get(k) ?? Date.now();
    firstSeen.current.set(k, at);
    return Date.now() - at < SAID_MS ? text : '';
  };
  return (
    <section className="st-dests-panel">
      <header className="st-dests-head">
        <span className="label">{t('Destinations')}</span>
        {run && ready.length > 1 && (
          <button type="button" className="btn primary small" disabled={starting} onClick={() => void startAll()}>
            <Play size={12} /> {t('Start all ({n})', { n: ready.length })}
          </button>
        )}
      </header>
      {dests && dests.length === 0 && <Empty icon={Radio} text={t('No destination')} />}
      <div className="st-cards">
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
            // Started from here and refused by the destination: why (no new try by itself).
            const refused = !s ? failed.find((f) => f.destination === d.id && f.source === rec.id && serverNow() - f.started < SAID_MS) : undefined;
            const live = on && s?.state === 'live';
            const state = on && s ? (live ? clockOf(s.liveSince ? serverNow() - s.liveSince : 0) : s.state === 'connecting' ? t('Connecting…') : t('Reconnecting')) : busy && s ? t('Busy · {source}', { source: nameOf(s.source) }) : tc('dest', 'Ready');
            // Its server cut the link: the live stops at this moment unless it takes it again.
            const left = on && s?.stopAt ? Math.max(0, Math.ceil((s.stopAt - serverNow()) / 1000)) : null;
            return (
              <div key={d.id} className={`st-card ${live ? 'live' : on ? 'wait' : busy ? 'busy' : 'ready'}`}>
                <div className="st-card-head">
                  <ProtoTag p={d.protocol} />
                  <strong data-tip={`${d.host} · ${encodingOf(d)}`}>{d.name}</strong>
                  <span className="st-chip">{state}</span>
                </div>
                {left !== null && (
                  <div className="st-countdown" role="timer">
                    <span>
                      <AlertTriangle size={12} /> {t('The destination no longer answers')}
                    </span>
                    <b>{t('Stops in {n} s', { n: left })}</b>
                    <i style={{ width: `${(left / 120) * 100}%` }} />
                  </div>
                )}
                {on && s && (
                  <>
                    <div className="st-graph">
                      <Spark values={s.history} />
                      <b>{mbps(s.kbps, locale())}</b>
                    </div>
                    {(() => {
                      const bad = s.state === 'connecting' ? '' : fresh(d.id, s.message);
                      return (bad || s.note) && left === null ? (
                        <div className={`st-note ${bad ? 'bad' : ''}`}>
                          {bad ? <AlertTriangle size={11} /> : null} {said(bad || s.note)}
                        </div>
                      ) : null;
                    })()}
                    <div className="st-card-foot">
                      <span>{t('{v} sent', { v: volume(s.bytes, locale(), lang() === 'fr') })}</span>
                      {run && (
                        <button type="button" className="btn small stop-btn" onClick={() => void stop(d)}>
                          <Square size={9} /> {t('Stop')}
                        </button>
                      )}
                    </div>
                  </>
                )}
                {refused && (
                  <div className="st-note bad">
                    <AlertTriangle size={11} /> {refused.message.startsWith("the destination's server cut the link") ? said(refused.message) : t('Could not start: {why}', { why: said(refused.message) })}
                  </div>
                )}
                {run && !on && !busy && (
                  <>
                    {d.keyLive && <input className="st-input" type="password" value={x.key} autoComplete="off" spellCheck={false} placeholder={t('Stream key of this live')} onChange={(e) => setDraft(d, { key: e.target.value, test: undefined })} />}
                    {d.exposeUrl && <input className="st-input" value={x.url} spellCheck={false} placeholder={t('Server address')} onChange={(e) => setDraft(d, { url: e.target.value, test: undefined })} />}
                    {d.exposeBitrate && (
                      <label className="st-slider">
                        <span className="st-slider-head">
                          {t('Video bitrate')}
                          <b>{mbps(x.videoKbps, locale())}</b>
                        </span>
                        <input type="range" min={0.5} max={25} step={0.5} value={x.videoKbps / 1000} onChange={(e) => setDraft(d, { videoKbps: Math.round(Number(e.target.value) * 1000) })} />
                      </label>
                    )}
                    {x.test?.text && (
                      <span className={`st-note ${x.test.ok ? 'ok' : 'bad'}`}>
                        {x.test.ok ? <Check size={11} /> : <AlertTriangle size={11} />} {x.test.text}
                      </span>
                    )}
                    <div className="st-card-act">
                      <button type="button" className="btn ghost st-test" disabled={x.test?.busy} data-tip={t('Test')} onClick={() => void test(d)}>
                        <Plug size={13} className={x.test?.busy ? 'spin' : undefined} />
                      </button>
                      <button type="button" className="btn primary" disabled={starting || (d.keyLive && !x.key.trim())} onClick={() => void start(d)}>
                        <Play size={12} /> {t('Go live')}
                      </button>
                    </div>
                  </>
                )}
              </div>
            );
          })}
      </div>
      {mine.length > 0 && (
        <footer className="st-dests-foot">
          <i />
          <span>
            <b>{mine.length}</b> {t('on air')} · <b>{mbps(mine.reduce((a, s) => a + s.kbps, 0), locale())}</b>
          </span>
          <span className="spacer" />
          {run && (
            <button type="button" className="btn small stop-btn" onClick={() => void stopAll()}>
              <Square size={9} /> {t('Stop all')}
            </button>
          )}
        </footer>
      )}
    </section>
  );
}
