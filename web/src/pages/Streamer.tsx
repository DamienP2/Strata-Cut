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
import { NumberField } from '../ui/fields';
import { Meters, useAudioOut } from '../ui/AudioUi';
import { clockOf, mbps, useDestinations, useStreams, volume, type Destination, type StreamInfo } from '../streams';
import { encodingOf, ProtoTag, Spark } from '../settings/Destinations';
import type { Recorder } from '../types';
import { explainGuard } from '../ui/guard';
import { SourceGroups } from '../ui/SourceGroups';
import { Antenne, Mixer, ProgramLayer, SettingsModal, useProgram } from './StreamerProgram';

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
      {rec ? <Center key={rec.id} rec={rec} streams={streams.filter((s) => s.source === rec.id)} /> : <div className="sc-center"><Empty icon={RadioTower} text={t('No recorder')} /></div>}
      <aside className="panel right st-panel">{rec && <DestPanel rec={rec} dests={dests} streams={streams} failed={all.filter((s) => s.state === 'failed')} />}</aside>
    </section>
  );
}

/** The programme of the source: the picture as it goes out, what is on air, the sound. */
function Center({ rec, streams }: { rec: Recorder; streams: StreamInfo[] }) {
  const { st, timezone, toast } = useApp(useShallow((s) => ({ st: s.status[rec.id], timezone: s.timezone, toast: s.toast })));
  const run = useApp((s) => can.has(s.auth?.user, 'stream.run'));
  const prog = useProgram(rec.id);
  const video = useRef<HTMLVideoElement>(null);
  const audio = useAudioOut(video);
  const [wall, setWall] = useState<number | null>(null);
  const [settings, setSettings] = useState(false);
  const player = useRef<LoopPlayer | null>(null);
  // The moment the picture shows (server ms): the meters show the sound of that moment.
  const shown = useRef<number | null>(null);
  const received = st?.state === 'recording';
  // Only what is received now is shown: a stopped source shows nothing (not what is left of
  // its last recording).
  useEffect(() => {
    if (!received) {
      setWall(null);
      return;
    }
    const p = new LoopPlayer(video.current!, rec.id, serverNow);
    p.onFrame = (w) => {
      setWall(w);
      shown.current = w;
    };
    player.current = p;
    void p.goLive(serverNow()).then(() => {
      if (heardRef.current) void p.setAudioTrack(heardRef.current);
    });
    return () => {
      player.current = null;
      p.destroy();
    };
  }, [rec.id, received]);
  // The headphones (the browser's own: nothing of it goes out). A track alone (solo), at its
  // fader's level; else the output as it goes out: the tracks on (the browser plays one, the
  // first of them), their level and the output's, the channels, and silent under the image with
  // the same fade.
  const [solo, setSolo] = useState<number | null>(null);
  const pg = prog.state?.program;
  const nTracks = Math.min(4, prog.state?.tracks.length ?? 0);
  const isOn = (k: number) => pg?.tracks[k]?.on ?? k === 0;
  const firstOn = Array.from({ length: nTracks }, (_, k) => k).find(isOn);
  const heard = solo ?? firstOn ?? 0;
  const heardRef = useRef(heard);
  heardRef.current = heard;
  useEffect(() => {
    void player.current?.setAudioTrack(heard);
  }, [heard]);
  const gainOf = (k: number) => pg?.tracks[k]?.gain ?? 0;
  const level = !pg ? 1 : solo !== null ? 10 ** (gainOf(solo) / 20) : pg.imageOn || firstOn === undefined ? 0 : 10 ** ((gainOf(heard) + pg.master) / 20);
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
  const stopAll = async () => {
    try {
      await api('POST', '/api/streams/stop', { source: rec.id });
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  return (
    <div className="sc-center pg-center">
      <div className="pg-top">
        <div className="pg-picture">
          <div className="pg-viewer">
            <video ref={video} className="sc-video" playsInline style={received ? undefined : { visibility: 'hidden' }} />
            {!received && <span className="sc-onpic mid">{st?.state === 'starting' ? t('Connecting…') : t('Source stopped')}</span>}
            {received && <ProgramLayer rec={rec} prog={prog} editable={run} />}
            {received && <Meters out={audio.out} />}
          </div>
          <div className="pg-meta">
            <span className="pg-tc">{wall ? timecode(wall, fps, timezone) : '--:--:--:--'}</span>
          </div>
        </div>
        <Antenne rec={rec} prog={prog} live={streams.length} kbps={mbps(streams.reduce((a, s) => a + s.kbps, 0), locale())} onStopAll={() => void stopAll()} onSettings={() => setSettings(true)} />
      </div>
      <Mixer rec={rec} prog={prog} shown={() => shown.current} monitor={{ volume: audio.volume, setVolume: audio.setVolume, muted: audio.muted, setMuted: audio.setMuted, solo, setSolo, heard }} />
      {settings && createPortal(<SettingsModal rec={rec} prog={prog} onClose={() => setSettings(false)} />, document.body)}
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

function DestPanel({ rec, dests, streams, failed }: { rec: Recorder; dests: Destination[] | null; streams: StreamInfo[]; failed: StreamInfo[] }) {
  const run = useApp((s) => can.has(s.auth?.user, 'stream.run'));
  const { recorders, toast } = useApp(useShallow((s) => ({ recorders: s.recorders, toast: s.toast })));
  const [starting, setStarting] = useState(false);
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
            const refused = !s ? failed.find((f) => f.destination === d.id && f.source === rec.id) : undefined;
            const state = on && s ? (s.state === 'live' ? clockOf(s.liveSince ? serverNow() - s.liveSince : 0) : s.state === 'connecting' ? t('Connecting…') : s.state === 'slate' ? t('Slate') : t('Reconnecting')) : busy && s ? t('Busy · {source}', { source: nameOf(s.source) }) : tc('dest', 'Ready');
            return (
              <div key={d.id} className={`st-card ${on ? 'on' : ''} ${busy ? 'busy' : ''}`}>
                <div className="st-card-head">
                  <ProtoTag p={d.protocol} />
                  <strong data-tip={`${d.host} · ${encodingOf(d)}`}>{d.name}</strong>
                  <span className={`st-state ${on && s?.state === 'live' ? 'live' : ''} ${on && s?.state !== 'live' ? 'warn' : ''}`}>{state}</span>
                </div>
                {on && s && (
                  <>
                    <Spark values={s.history} />
                    <div className="st-card-nums">
                      <b>{mbps(s.kbps, locale())}</b>
                      <span>{volume(s.bytes, locale(), lang() === 'fr')}</span>
                      <span className="spacer" />
                      {run && (
                        <button type="button" className="btn small st-stop-btn" onClick={() => void stop(d)}>
                          <Square size={10} /> {t('Stop')}
                        </button>
                      )}
                    </div>
                    {(s.message || s.note) && (
                      <div className={`st-note ${s.message ? 'bad' : ''}`}>
                        {s.message ? <AlertTriangle size={11} /> : null} {t(s.message || s.note)}
                      </div>
                    )}
                  </>
                )}
                {refused && (
                  <div className="st-note bad">
                    <AlertTriangle size={11} /> {t('Could not start: {why}', { why: t(refused.message) })}
                  </div>
                )}
                {run && !on && !busy && (
                  <>
                    {d.keyLive && <input className="st-input" type="password" value={x.key} autoComplete="off" spellCheck={false} placeholder={t('Stream key of this live')} onChange={(e) => setDraft(d, { key: e.target.value, test: undefined })} />}
                    {d.exposeUrl && <input className="st-input" value={x.url} spellCheck={false} placeholder={t('Server address')} onChange={(e) => setDraft(d, { url: e.target.value, test: undefined })} />}
                    {d.exposeBitrate && (
                      <label className="st-field">
                        <span>{t('Video bitrate')}</span>
                        <NumberField value={x.videoKbps / 1000} min={0.3} max={50} step={0.5} precision={1} suffix="Mb/s" onChange={(v) => setDraft(d, { videoKbps: Math.round(v * 1000) })} />
                      </label>
                    )}
                    {x.test?.text && (
                      <span className={`st-note ${x.test.ok ? 'ok' : 'bad'}`}>
                        {x.test.ok ? <Check size={11} /> : <AlertTriangle size={11} />} {x.test.text}
                      </span>
                    )}
                    <div className="st-card-act">
                      <button type="button" className="btn ghost" disabled={x.test?.busy} onClick={() => void test(d)}>
                        <Plug size={12} className={x.test?.busy ? 'spin' : undefined} /> {x.test?.busy ? t('Testing…') : t('Test')}
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
    </section>
  );
}
