import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { cachedStill, loadStill } from '../stills';
import { useDisplayPref } from '../displayPrefs';
import { useShallow } from 'zustand/react/shallow';
import { ArrowLeftToLine, ArrowRightToLine, CalendarPlus, Check, ChevronsRight, Crosshair, Download, Eraser, FastForward, Pause, Play, RadioTower, Rewind, RotateCcw, Scissors, StepBack, StepForward, X, ZoomIn, ZoomOut } from 'lucide-react';
import { api, can, serverNow, useApp, useChanged } from '../store';
import { t, locale } from '../i18n';
import { navigate } from '../router';
import { LoopPlayer, type RunInfo } from '../player';
import { behind, clock, day, duration, fpsOf, parseTimecode, timecode } from '../tc';
import { Empty } from '../ui/Empty';
import { Meters, VolumeControl, useAudioOut } from '../ui/AudioUi';
import { GrainCache } from '../audio';
import type { Clip, Job, Recorder } from '../types';

/** A marked frame: its run, its source time and its server time. */
interface Mark {
  run: string;
  t: number;
  wall: number;
}

interface Span {
  run: string;
  from: number;
  to: number;
}

/** A programme of the source's guide (DVB EIT), server times. */
interface Programme {
  id: number;
  start: number;
  end: number;
  name: string;
  text?: string;
}

/** The zoom: the timeline shows from 10 s to the whole loop; by default and reset: 1 h. */
const ZOOM_MIN = 10_000;
const ZOOM_RESET = 3_600_000;
const zoomMax = (loopHours: number) => Math.max(3_600_000, loopHours * 3_600_000);
/** Slider position 0 (whole loop) … 1000 (10 s), logarithmic. */
const toSlider = (span: number, max: number) => Math.round((1000 * Math.log(max / span)) / Math.log(max / ZOOM_MIN));
const fromSlider = (v: number, max: number) => max * Math.pow(ZOOM_MIN / max, v / 1000);
/** "10 s", "1 min", "2 h 30". */
function spanLabel(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s} s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  return m % 60 ? `${h} h ${String(m % 60).padStart(2, '0')}` : `${h} h`;
}
/** Durations of a strip still, ms (so that stills are reused while zooming). */
const TILE_STEPS = [200, 500, 1000, 2000, 5000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000, 900_000, 1_800_000, 3_600_000, 7_200_000, 10_800_000];

/** `value`, once it has not changed for `ms`. */
/** One still of the strip, loaded through the queue (a few at a time, the newest first). */
function Still({ url }: { url: string }) {
  const [src, setSrc] = useState<string | null | undefined>(() => cachedStill(url));
  // A still not there yet (near the live, the fragment is being written): asked again, a few times.
  const [tries, setTries] = useState(0);
  useEffect(() => {
    const known = cachedStill(url);
    setSrc(known);
    if (known) return;
    return loadStill(url, setSrc);
  }, [url, tries]);
  useEffect(() => {
    if (src !== null || tries >= 5) return;
    const i = setTimeout(() => setTries((n) => n + 1), 2500);
    return () => clearTimeout(i);
  }, [src, tries]);
  return src ? <img src={src} alt="" draggable={false} /> : null;
}

function useSettled<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const i = setTimeout(() => setV(value), ms);
    return () => clearTimeout(i);
  }, [value, ms]);
  return v;
}

export function Extract({ recorder }: { recorder?: string }) {
  const recorders = useApp((s) => s.recorders);
  // The recorder asked, else the last one opened, else the first.
  const [last, setLast] = useDisplayPref<string | null>('extract.recorder', null);
  const known = (x?: string | null) => !!x && recorders.some((r) => r.id === x);
  const id = known(recorder) ? recorder : known(last) ? last! : recorders[0]?.id;
  useEffect(() => {
    if (id && id !== recorder) void navigate(`/extract/${id}${location.search}`, true);
    if (id && id !== last) setLast(id);
  }, [id, recorder]); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <section className="sc-edit">
      <Sources current={id} />
      {id ? <Editor key={id} rec={recorders.find((r) => r.id === id)!} /> : <div className="sc-center"><Empty icon={RadioTower} text={t('No recorder')} /></div>}
      {!id && <aside className="panel right" />}
    </section>
  );
}

function Sources({ current }: { current?: string }) {
  const { recorders, status } = useApp(useShallow((s) => ({ recorders: s.recorders, status: s.status })));
  return (
    <aside className="lib-nav">
      <div className="lib-nav-section">
        <span className="grow">{t('Recorders')}</span>
      </div>
      {recorders.map((r) => {
        const st = status[r.id]?.state;
        return (
          <div key={r.id} className={`lib-nav-item sc-src ${r.id === current ? 'on' : ''}`} onClick={() => void navigate(`/extract/${r.id}`)}>
            <span className={`nav-dot ${st === 'recording' ? 'ok' : st === 'off' || st === 'waiting' ? '' : st === 'error' ? 'bad' : 'warn'}`} />
            {r.logo && <img className="sc-logo" src={`/api/recorders/${r.id}/logo?v=${r.logo}`} alt="" />}
            <span className="sc-src-t">
              <span className="sc-src-name">{r.name}</span>
              <span className="sc-src-prog">{status[r.id]?.onAir?.name ?? (st === 'recording' ? t('Recording') : st === 'off' ? t('Stopped') : st === 'waiting' ? t('Programmed') : st === 'error' ? t('Error') : t('No signal'))}</span>
            </span>
          </div>
        );
      })}
    </aside>
  );
}

function Editor({ rec }: { rec: Recorder }) {
  const { timezone, user, jobs, toast } = useApp(useShallow((s) => ({ timezone: s.timezone, user: s.auth?.user, jobs: s.jobs, toast: s.toast })));
  const videoRef = useRef<HTMLVideoElement>(null);
  const playerRef = useRef<LoopPlayer | null>(null);
  const [pos, setPos] = useState<Mark | null>(null);
  const [run, setRun] = useState<RunInfo | null>(null);
  const [playing, setPlaying] = useState(false);
  const [live, setLive] = useState(true);
  const [mIn, setIn] = useState<Mark | null>(null);
  const [mOut, setOut] = useState<Mark | null>(null);
  const [name, setName] = useState('');
  const [span, setSpan] = useDisplayPref('extract.zoom.h', ZOOM_RESET);
  const [spans, setSpans] = useState<Span[]>([]);
  const [clips, setClips] = useState<Clip[]>([]);
  const [guide, setGuide] = useState<Programme[]>([]);
  const fps = fpsOf(run?.media?.fpsNum, run?.media?.fpsDen);
  // The source's programme guide, for the loop and a few hours ahead.
  useEffect(() => {
    let dead = false;
    const load = async () => {
      const now = serverNow();
      try {
        const g = await api<{ events: Programme[] }>('GET', `/api/recorders/${rec.id}/epg?from=${Math.round(now - rec.loopHours * 3_600_000)}&to=${Math.round(now + 6 * 3_600_000)}`);
        if (!dead) setGuide(g.events);
      } catch {
        /* the next try */
      }
    };
    void load();
    const i = setInterval(() => void load(), 60_000);
    return () => {
      dead = true;
      clearInterval(i);
    };
  }, [rec.id, rec.loopHours]);
  const audio = useAudioOut(videoRef);

  // The player lives as long as this recorder is open.
  useEffect(() => {
    const v = videoRef.current!;
    const p = new LoopPlayer(v, rec.id, serverNow);
    playerRef.current = p;
    let frame = 0;
    let last: Mark | null = null;
    p.onFrame = (wall, tt) => {
      last = { run: p.run?.id ?? '', t: tt, wall };
      if (!frame)
        frame = requestAnimationFrame(() => {
          frame = 0;
          setPos(last);
        });
    };
    p.onRun = (r) => setRun(r ? { ...r } : null);
    const onPlay = () => setPlaying(true);
    const onPause = () => setPlaying(p.playing);
    v.addEventListener('play', onPlay);
    v.addEventListener('pause', onPause);
    // `?at=<ms>` (from the Schedule page): that moment, else the live.
    const at = Number(new URLSearchParams(location.search).get('at'));
    if (at > 0) {
      setLive(false);
      void p.seekWall(at).then(() => {
        if (p.run) return;
        // That moment is no longer in the loop: the live.
        toast(t('This moment is no longer in the loop'), 'error');
        setLive(true);
        void p.goLive(serverNow());
      });
    } else void p.goLive(serverNow());
    return () => {
      cancelAnimationFrame(frame);
      v.removeEventListener('play', onPlay);
      v.removeEventListener('pause', onPause);
      p.destroy();
      playerRef.current = null;
    };
  }, [rec.id]);

  const loadSpans = useCallback(async () => {
    try {
      const c = await api<{ spans: Span[] }>('GET', `/api/recorders/${rec.id}/coverage`);
      setSpans(c.spans);
    } catch {
      /* the next try */
    }
  }, [rec.id]);
  useEffect(() => {
    void loadSpans();
    const i = setInterval(() => void loadSpans(), 5000);
    return () => clearInterval(i);
  }, [loadSpans]);

  const loadClips = useCallback(async () => {
    try {
      setClips((await api<Clip[]>('GET', '/api/clips')).filter((c) => c.recorder === rec.id));
    } catch {
      /* */
    }
  }, [rec.id]);
  useEffect(() => void loadClips(), [loadClips]);
  useChanged('clips', loadClips);

  // Moving the playhead by hand plays a short grain of sound at the new frame.
  const out = audio.out;
  useEffect(() => {
    const pl = playerRef.current;
    if (!pl || !out) return;
    const cache = new GrainCache(out.ctx);
    pl.onScrub = (tt) => {
      const a = pl.audioAt(tt);
      if (!a) return;
      void cache.get(a.init, a.seg).then((buf) => {
        if (buf && pl.t === tt && pl.video.paused) out.grain(buf, tt - a.t0, Math.max(2 / pl.fps, 0.08));
      });
    };
    return () => {
      pl.onScrub = () => {};
    };
  }, [out, rec.id]);

  const p = () => playerRef.current;
  /** Touching the playhead pauses (the live too) and shows that frame. */
  const seekWall = useCallback((wall: number) => {
    setLive(false);
    const pl = playerRef.current;
    if (!pl) return;
    pl.halt();
    void pl.seekWall(wall);
  }, []);

  /** In and out on a whole programme (within what is recorded); its name for the clip. */
  const markProgramme = async (g: Programme): Promise<{ a: Mark; b: Mark } | null> => {
    // A clip stays in one continuous recording: the one holding most of the programme.
    const best = spans
      .map((x) => ({ from: Math.max(g.start, x.from), to: Math.min(g.end, x.to) }))
      .filter((x) => x.to > x.from)
      .sort((a, b) => b.to - b.from - (a.to - a.from))[0];
    const none = () => {
      toast(t('This programme is not recorded'), 'error');
      return null;
    };
    if (!best) return none();
    const from = best.from;
    const to = best.to - 1000 / fps;
    if (to <= from) return none();
    setLive(false);
    const pl = p();
    if (!pl) return null;
    pl.halt();
    await pl.seekWall(to);
    const out = { run: pl.run?.id ?? '', t: pl.t, wall: pl.wallOf(pl.t) };
    await pl.seekWall(from);
    const inn = { run: pl.run?.id ?? '', t: pl.t, wall: pl.wallOf(pl.t) };
    setIn(inn);
    setOut(out);
    setName(g.name);
    return { a: inn, b: out };
  };
  /** A recorded programme extracted at once (in and out on it, the clip asked). */
  const extractProgramme = async (g: Programme) => {
    const m = await markProgramme(g);
    if (!m) return;
    if (m.a.run !== m.b.run) return toast(t('The in and out points must be in the same recording'), 'error');
    try {
      await api('POST', '/api/clips', { recorder: rec.id, run: m.a.run, t: m.a.t, frames: Math.round((m.b.t - m.a.t) * fps) + 1, name: g.name });
      toast(t('Extraction of “{name}” started', { name: g.name }));
      setName('');
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  // The guide for the right panel: on air first, then what was recorded (newest first), then what comes.
  const programmes = useMemo(() => {
    const now = serverNow();
    // Its share in one continuous recording (a clip stays in one).
    const share = (g: Programme) => Math.max(0, ...spans.map((x) => Math.min(g.end, x.to) - Math.max(g.start, x.from))) / (g.end - g.start);
    const all = guide.map((g) => ({ ...g, share: share(g) }));
    const onNow = all.filter((g) => g.start <= now && now < g.end);
    const past = all.filter((g) => g.end <= now).sort((a, b) => b.start - a.start);
    const soon = all.filter((g) => g.start > now).sort((a, b) => a.start - b.start).slice(0, 6);
    return [...onNow, ...past, ...soon];
  }, [guide, spans]);
  const clearMarks = () => {
    setIn(null);
    setOut(null);
    setName('');
  };
  /** A programme to come: its recording programmed (the recorder starts for it if it is stopped). */
  const programmeIt = async (g: Programme) => {
    const day = new Intl.DateTimeFormat('en-CA', { timeZone: timezone, year: 'numeric', month: '2-digit', day: '2-digit' }).format(g.start);
    try {
      await api('POST', '/api/schedules', { recorder: rec.id, name: g.name, date: day, days: [], start: clock(g.start, timezone).slice(0, 5), end: clock(g.end, timezone).slice(0, 5) });
      toast(t('Recording of “{name}” programmed', { name: g.name }));
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  const markIn = () => pos && setIn(pos);
  const markOut = () => pos && setOut(pos);
  const step = (n: number) => {
    setLive(false);
    p()?.step(n);
  };
  const togglePlay = () => {
    const pl = p();
    if (!pl) return;
    setLive(false);
    if (pl.playing) pl.pause();
    else pl.play();
  };
  const goLive = () => {
    setLive(true);
    void p()?.goLive(serverNow());
  };

  // J K L, arrows, I O, space.
  const speed = useRef(0);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement;
      if (el.closest('input, textarea, [contenteditable="true"], .modal-back') || e.ctrlKey || e.metaKey || e.altKey) return;
      const pl = p();
      if (!pl) return;
      const k = e.key.toLowerCase();
      let done = true;
      if (k === 'arrowleft') step(e.shiftKey ? -Math.round(fps) : -1);
      else if (k === 'arrowright') step(e.shiftKey ? Math.round(fps) : 1);
      else if (k === 'i' && e.shiftKey) mIn && seekWall(mIn.wall);
      else if (k === 'o' && e.shiftKey) mOut && seekWall(mOut.wall);
      else if (k === 'm') audio.setMuted(!audio.muted);
      else if (k === 'l' && e.shiftKey) goLive();
      else if (k === 'backspace') clearMarks();
      else if (k === 'enter') canCut && void cut();
      else if (k === 'i') markIn();
      else if (k === 'o') markOut();
      else if (k === ' ' || k === 'k') {
        speed.current = 0;
        if (k === ' ') togglePlay();
        else {
          setLive(false);
          pl.pause();
        }
      } else if (k === 'l') {
        speed.current = speed.current > 0 ? Math.min(speed.current * 2, 8) : 1;
        setLive(false);
        pl.play(speed.current);
      } else if (k === 'j') {
        speed.current = speed.current < 0 ? Math.max(speed.current * 2, -8) : -1;
        setLive(false);
        pl.playReverse(-speed.current);
        setPlaying(true);
      } else done = false;
      if (done) e.preventDefault();
    };
    addEventListener('keydown', onKey);
    return () => removeEventListener('keydown', onKey);
  });

  const admin = can.admin(user);
  const frames = mIn && mOut && mIn.run === mOut.run && mOut.t >= mIn.t ? Math.round((mOut.t - mIn.t) * fps) + 1 : 0;
  const canCut = can.extract(user) && frames > 0;
  const cut = async () => {
    if (!mIn || !mOut) return;
    if (mIn.run !== mOut.run) return toast(t('The in and out points must be in the same recording'), 'error');
    try {
      await api('POST', '/api/clips', { recorder: rec.id, run: mIn.run, t: mIn.t, frames, name: name.trim() || `${rec.name} ${timecode(mIn.wall, fps, timezone)}` });
      setName('');
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };

  const typed = (text: string, set: (m: Mark | null) => void) => {
    const near = pos?.wall ?? serverNow();
    const wall = parseTimecode(text, near, fps, timezone);
    if (wall === null) return toast(t('Invalid timecode'), 'error');
    setLive(false);
    const pl = p();
    if (!pl) return;
    pl.halt();
    void pl.seekWall(wall).then(() => set({ run: pl.run?.id ?? '', t: pl.t, wall: pl.wallOf(pl.t) }));
  };

  const act = useRef<GuideActs>({ markProgramme, extractProgramme, programmeIt, seekWall });
  act.current = { markProgramme, extractProgramme, programmeIt, seekWall };

  const now = serverNow();
  const ago = pos ? now - pos.wall : 0;
  const myJobs = jobs;

  return (
    <>
      <div className="sc-center">
        <div className="sc-viewer-wrap">
          <div className="sc-viewer">
            <video ref={videoRef} className="sc-video" playsInline />
            {run?.audio && <Meters out={audio.out} />}
          </div>
        </div>
        <div className="sc-transport">
          <div className="sc-tc-big">{pos ? timecode(pos.wall, fps, timezone) : '--:--:--:--'}</div>
          <div className="sc-ctrls">
            <button className="ctl" data-tip={t('Go to the in point')} data-keys="⇧ I" disabled={!mIn} onClick={() => mIn && seekWall(mIn.wall)}>
              <ArrowLeftToLine size={18} />
            </button>
            <button className="ctl" data-tip={t('Previous frame')} data-keys="←" onClick={() => step(-1)}>
              <StepBack size={18} />
            </button>
            <button className="ctl" data-tip={t('Play backwards')} data-keys="J" onClick={() => { setLive(false); p()?.playReverse(); setPlaying(true); }}>
              <Rewind size={18} />
            </button>
            <button className="ctl play" data-tip={playing ? t('Pause') : t('Play')} data-keys="K" onClick={togglePlay}>
              {playing ? <Pause size={18} /> : <Play size={18} />}
            </button>
            <button className="ctl" data-tip={t('Play faster')} data-keys="L" onClick={() => { setLive(false); p()?.play(2); }}>
              <FastForward size={18} />
            </button>
            <button className="ctl" data-tip={t('Next frame')} data-keys="→" onClick={() => step(1)}>
              <StepForward size={18} />
            </button>
            <button className="ctl" data-tip={t('Go to the out point')} data-keys="⇧ O" disabled={!mOut} onClick={() => mOut && seekWall(mOut.wall)}>
              <ArrowRightToLine size={18} />
            </button>
          </div>
          <div className="sc-sep" />
          <button className="ctl wide" data-tip={t('Set the in point here')} data-keys="I" onClick={markIn} disabled={!pos}>
            <MarkInIcon />
          </button>
          <button className="ctl wide" data-tip={t('Set the out point here')} data-keys="O" onClick={markOut} disabled={!pos}>
            <MarkOutIcon />
          </button>
          <div className="spacer" />
          <VolumeControl volume={audio.volume} setVolume={audio.setVolume} muted={audio.muted} setMuted={audio.setMuted} />
          <div className="sc-sep" />
          {/* As before: on the live, a red badge; behind it, how far, and the way back. */}
          {live && ago < 30_000 ? (
            <span className="sc-live-on" data-tip={t('The newest frames, as they are recorded')}>
              <span className="sc-rec" /> {t('LIVE')}
            </span>
          ) : (
            <button className="btn sc-live-go" data-tip={t('Back to live')} data-keys="⇧ L" onClick={goLive}>
              <ChevronsRight size={14} /> {t('Live')}
              {pos && <span className="sc-live-behind">{behind(ago)}</span>}
            </button>
          )}
        </div>
        {/* The clip: its in and out (the timeline's E and S), its length, its name. */}
        <div className="sc-clipbar">
          <span className="sc-clipbar-l">{t('Clip')}</span>
          <MarkTag kind="in" mark={mIn} fps={fps} timezone={timezone} onType={(s) => typed(s, setIn)} onClear={() => setIn(null)} />
          <MarkTag kind="out" mark={mOut} fps={fps} timezone={timezone} onType={(s) => typed(s, setOut)} onClear={() => setOut(null)} />
          <span className="sc-clipdur">{duration(frames, fps)}</span>
          <input className="sc-clipname" value={name} placeholder={mIn ? `${rec.name} ${timecode(mIn.wall, fps, timezone)}` : t('Name of the clip')} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
          <button className="ctl" data-tip={t('Clear the in and out points')} data-keys="⌫" disabled={!mIn && !mOut} onClick={clearMarks}>
            <Eraser size={18} />
          </button>
          {can.extract(user) && (
            <button className="ctl primary" data-tip={t('Extract')} data-keys="⏎" disabled={!canCut} onClick={() => void cut()}>
              <Scissors size={18} />
            </button>
          )}
        </div>
        <JobStrip jobs={myJobs} recId={rec.id} />
      </div>
      {/* The timeline takes the whole width, under the panels. */}
      <Timeline rec={rec} pos={pos} playing={playing} fps={fps} span={span} setSpan={setSpan} spans={spans} clips={clips} mIn={mIn} mOut={mOut} onSeek={seekWall} timezone={timezone} guide={guide} onProgramme={(g) => void markProgramme(g)} />

      <GuidePanel recId={rec.id} programmes={programmes} timezone={timezone} canExtract={can.extract(user)} admin={admin} act={act} />
    </>
  );
}

/** The timeline's layers that do not move with the playhead: drawn again when the view,
 * the time (once a second) or the data change, not at each frame shown. */
const Layers = memo(function Layers(props: { recId: string; start: number; span: number; timezone: string; ticks: { step: number; out: number[] }; guide: Programme[]; onAir: string; strip: { d: number; out: number[] }; now: number; spans: Span[]; wavePath: string; gaps: [number, number][] }) {
  const { recId, start, span, timezone, ticks, guide, onAir, strip, now, spans, wavePath, gaps } = props;
  const end = start + span;
  const x = (wall: number) => ((wall - start) / span) * 100;
  const recorded = (w: number) => spans.some((s) => w >= s.from && w < s.to);
  return (
    <>
      <div className="sc-ruler">
        {ticks.out.filter((v) => x(v) < 94).map((v) => (
          <span key={v} style={{ left: `${x(v)}%` }}>
            {ticks.step >= 60_000 ? clock(v, timezone).slice(0, 5) : clock(v, timezone)}
          </span>
        ))}
      </div>
      {guide.length > 0 && (
        <div className="sc-epg">
          {guide
            .filter((g) => g.end > start && g.start < end)
            .map((g) => {
              const l = Math.max(0, x(g.start));
              const r = Math.min(100, x(g.end));
              const on = onAir === String(g.id) + g.start;
              return (
                <div
                  key={String(g.id) + g.start}
                  data-key={String(g.id) + g.start}
                  className={`sc-epg-item ${on ? 'on' : ''}`}
                  style={{ left: `${l}%`, width: `${Math.max(0, r - l)}%` }}
                  title={`${g.name}\n${clock(g.start, timezone).slice(0, 5)} → ${clock(g.end, timezone).slice(0, 5)}${g.text ? `\n${g.text}` : ''}\n\n${t('Click: go to its start · Shift+click: in and out on the whole programme')}`}
                >
                  <span>{g.name}</span>
                </div>
              );
            })}
        </div>
      )}
      <div className="sc-strip">
        {strip.out.map((w) => (
          <div key={w} className="sc-tile" style={{ left: `${x(w - strip.d / 2)}%`, width: `${(strip.d / span) * 100}%` }}>
            {recorded(w) && w < now - 2000 && <Still url={`/api/recorders/${recId}/frame.jpg?at=${Math.round(w)}&h=54${strip.d >= 2000 ? '&fast=1' : ''}`} />}
          </div>
        ))}
      </div>
      <div className="sc-wave">
        <svg viewBox="0 0 1000 30" preserveAspectRatio="none" aria-hidden="true">
          <path d={wavePath} fill="currentColor" />
        </svg>
      </div>
      {gaps.map(([a, b], i) => (
        <div key={i} className="sc-gap" style={{ left: `${x(a)}%`, width: `${x(b) - x(a)}%` }} title={t('Not recorded')} />
      ))}
    </>
  );
});

/** What the guide panel asks of the editor (the latest functions, behind a stable ref). */
interface GuideActs {
  markProgramme: (g: Programme) => Promise<unknown>;
  extractProgramme: (g: Programme) => Promise<unknown>;
  programmeIt: (g: Programme) => Promise<unknown>;
  seekWall: (wall: number) => void;
}
type Listed = Programme & { share: number };

/** The programme guide: drawn again when it changes, not at each frame shown. */
const GuidePanel = memo(function GuidePanel({ recId, programmes, timezone, canExtract, admin, act }: { recId: string; programmes: Listed[]; timezone: string; canExtract: boolean; admin: boolean; act: { current: GuideActs } }) {
  const now = useNow(30_000);
  return (
    <aside className="panel right sc-guide-panel">
      <section className="insp-section sc-guide-sec">
        <header>
          <span>{t('Programme guide')}</span>
        </header>
        {programmes.length === 0 ? (
          <p className="muted sc-guide-none">{t('This source sends no programme guide.')}</p>
        ) : (
          <div className="sc-guide">
            {programmes.map((g) => {
              const state = g.end > now && g.start <= now ? 'live' : g.start > now ? 'soon' : g.share >= 0.98 ? 'ok' : g.share >= 0.5 ? 'part' : 'none';
              const usable = state === 'ok' || state === 'part';
              const pick = () => (usable ? void act.current.markProgramme(g) : state === 'live' ? act.current.seekWall(g.start) : undefined);
              return (
                <div key={`${g.id}-${g.start}`} className={`sc-gitem ${state}`} data-tip={g.text || undefined}>
                  <button type="button" className="sc-gitem-pick" onClick={pick} disabled={state === 'soon' || state === 'none'}>
                    <span className="sc-gthumb">{(usable || state === 'live') && <Still url={`/api/recorders/${recId}/frame.jpg?at=${Math.round(g.start + Math.min(60_000, (Math.min(g.end, now) - g.start) / 2))}&h=72&fast=1`} />}</span>
                    <span className="sc-gtext">
                      <span className="sc-gtime">
                        {clock(g.start, timezone).slice(0, 5)}–{clock(g.end, timezone).slice(0, 5)}
                      </span>
                      <span className="sc-gname">{g.name}</span>
                      <span className={`sc-gpill ${state}`}>{state === 'live' ? t('On air') : state === 'soon' ? t('To come') : state === 'ok' ? t('Recorded') : state === 'part' ? t('Partly recorded {p} %', { p: Math.round(g.share * 100) }) : t('Not recorded')}</span>
                    </span>
                  </button>
                  {usable && canExtract && (
                    <button className="ctl" data-tip={t('Extract it now')} onClick={() => void act.current.extractProgramme(g)}>
                      <Scissors size={16} />
                    </button>
                  )}
                  {state === 'soon' && admin && (
                    <button className="ctl" data-tip={t('Programme its recording')} onClick={() => void act.current.programmeIt(g)}>
                      <CalendarPlus size={16} />
                    </button>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </section>
    </aside>
  );
});

/** The server's time, again every `ms` (for what changes with time but not at each frame). */
function useNow(ms: number): number {
  const [now, setNow] = useState(() => serverNow());
  useEffect(() => {
    const i = setInterval(() => setNow(serverNow()), ms);
    return () => clearInterval(i);
  }, [ms]);
  return now;
}

/** The mark-in icon: an opening bracket and its point; the mark-out one, the closing bracket. */
function MarkInIcon() {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M10 4H6v16h4" />
      <circle cx="15" cy="12" r="2.6" fill="currentColor" />
    </svg>
  );
}
function MarkOutIcon() {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M14 4h4v16h-4" />
      <circle cx="9" cy="12" r="2.6" fill="currentColor" />
    </svg>
  );
}

/** An in or out point as on the timeline (E / S), its timecode typed in, a × to clear it. */
function MarkTag({ kind, mark, fps, timezone, onType, onClear }: { kind: 'in' | 'out'; mark: Mark | null; fps: number; timezone: string; onType: (s: string) => void; onClear: () => void }) {
  const shown = mark ? timecode(mark.wall, fps, timezone) : '';
  const [text, setText] = useState(shown);
  useEffect(() => setText(shown), [shown]);
  return (
    <span className={`sc-mtag ${kind} ${mark ? 'set' : ''}`} data-tip={kind === 'in' ? t('In point (type a timecode)') : t('Out point (type a timecode)')}>
      <i>{kind === 'in' ? t('In').charAt(0) : t('Out').charAt(0)}</i>
      <input
        value={text}
        placeholder="--:--:--:--"
        onChange={(e) => setText(e.target.value)}
        onFocus={(e) => e.currentTarget.select()}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === 'Enter') e.currentTarget.blur();
          if (e.key === 'Escape') setText(shown);
        }}
        onBlur={() => text !== shown && text.trim() && onType(text)}
      />
      {mark && (
        <button type="button" className="sc-mtag-x" data-tip={t('Clear')} onClick={onClear}>
          <X size={12} />
        </button>
      )}
    </span>
  );
}

/**
 * The extractions, under the clip bar where they are started: a wide bar each (name, %, time
 * left, cancel); a clip ready stays (play it in Clips, download it) until it is put away.
 */
function JobStrip({ jobs, recId }: { jobs: Job[]; recId: string }) {
  const { user, toast } = useApp(useShallow((s) => ({ user: s.auth?.user, toast: s.toast })));
  // When each job was first seen running and how far: the time left from its pace.
  const seen = useRef(new Map<string, { at: number; p: number }>());
  const remove = async (j: Job) => {
    try {
      await api('DELETE', `/api/jobs/${j.id}`);
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  const mine = jobs.filter((j) => j.recorder === recId || j.state === 'queued' || j.state === 'running');
  if (mine.length === 0) return null;
  const now = Date.now();
  return (
    <div className="sc-jobstrip">
      {mine.map((j) => {
        let left = '';
        if (j.state === 'running') {
          const s0 = seen.current.get(j.id);
          if (!s0) seen.current.set(j.id, { at: now, p: j.progress });
          else if (j.progress > s0.p + 0.02) {
            const secs = ((now - s0.at) / 1000 / (j.progress - s0.p)) * (1 - j.progress);
            left = secs < 60 ? t('{n} s left', { n: Math.max(1, Math.round(secs)) }) : t('{n} min left', { n: Math.round(secs / 60) });
          }
        }
        return (
          <div key={j.id} className={`sc-jobline ${j.state}`}>
            <Scissors size={15} className="sc-jobline-ic" />
            <strong className="sc-jobline-name" title={j.name}>
              {j.name}
            </strong>
            {j.state === 'running' || j.state === 'queued' ? (
              <>
                <span className="sc-jobline-bar">
                  <span style={{ transform: `translateX(${(j.state === 'queued' ? 0 : j.progress) * 100 - 100}%)` }} />
                </span>
                <span className="sc-jobline-pct">{j.state === 'queued' ? t('Waiting') : `${Math.round(j.progress * 100)} %`}</span>
                <span className="sc-jobline-left muted">{left}</span>
                {can.extract(user) && (
                  <button className="ctl" data-tip={t('Cancel')} onClick={() => void remove(j)}>
                    <X size={16} />
                  </button>
                )}
              </>
            ) : j.state === 'done' ? (
              <>
                <span className="sc-jobline-done">
                  <Check size={15} /> {t('Clip ready')}
                </span>
                <div className="spacer" />
                <button className="btn small" onClick={() => void navigate('/clips')}>
                  {t('See it in Clips')}
                </button>
                {j.clip && (
                  <a className="btn small" href={`/api/clips/${j.clip}/download`}>
                    <Download size={13} /> {t('Download')}
                  </a>
                )}
                <button className="ctl" data-tip={t('Put away')} onClick={() => void remove(j)}>
                  <X size={16} />
                </button>
              </>
            ) : (
              <>
                <span className="sc-jobline-err">{t(j.message)}</span>
                <div className="spacer" />
                <button className="ctl" data-tip={t('Put away')} onClick={() => void remove(j)}>
                  <X size={16} />
                </button>
              </>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** Steps of the ruler, ms. */
const TICKS = [1000, 2000, 5000, 10_000, 30_000, 60_000, 120_000, 300_000, 600_000, 1_800_000, 3_600_000, 7_200_000, 10_800_000, 21_600_000];

function Timeline(props: { rec: Recorder; pos: Mark | null; playing: boolean; fps: number; span: number; setSpan: (ms: number) => void; spans: Span[]; clips: Clip[]; mIn: Mark | null; mOut: Mark | null; onSeek: (wall: number) => void; timezone: string; guide: Programme[]; onProgramme: (g: Programme) => void }) {
  const { rec, pos, playing, fps, span, setSpan, spans, clips, mIn, mOut, onSeek, timezone, guide, onProgramme } = props;
  const maxSpan = zoomMax(rec.loopHours);
  const tracks = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(800);
  const [start, setStart] = useState(() => (pos?.wall ?? serverNow()) - span / 2);
  const scrubbing = useRef(false);
  useEffect(() => {
    const el = tracks.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWidth(el.clientWidth || 800));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  /** The view stays within the loop (a little margin at both ends). */
  const clampStart = useCallback(
    (s: number, sp: number) => {
      const now = serverNow();
      const lo = now - rec.loopHours * 3_600_000 - sp * 0.05;
      const hi = now + sp * 0.05 - sp;
      return Math.min(Math.max(s, lo), Math.max(lo, hi));
    },
    [rec.loopHours],
  );
  // Zooming keeps a point in place: the pointer (Ctrl + wheel), else the playhead when in view, else the middle.
  const zoomAnchor = useRef<number | null>(null);
  const prevSpan = useRef(span);
  useEffect(() => {
    const old = prevSpan.current;
    prevSpan.current = span;
    if (old === span) return;
    setStart((st) => {
      const anchor = zoomAnchor.current ?? (pos && pos.wall >= st && pos.wall <= st + old ? pos.wall : st + old / 2);
      zoomAnchor.current = null;
      return clampStart(anchor - ((anchor - st) * span) / old, span);
    });
  }, [span]); // eslint-disable-line react-hooks/exhaustive-deps
  // The view follows the playhead when it moves, unless the view was moved by hand during playback.
  const follow = useRef(true);
  const lastWall = useRef<number | null>(null);
  useEffect(() => {
    if (!pos || scrubbing.current) return;
    const moved = lastWall.current !== pos.wall;
    lastWall.current = pos.wall;
    if (!moved) return;
    if (!follow.current && !playing) follow.current = true; // a frame moved by hand
    if (!follow.current) return;
    if (pos.wall < start + span * 0.02 || pos.wall > start + span * 0.98) setStart(clampStart(pos.wall - span * (pos.wall > start ? 0.8 : 0.2), span));
  }, [pos, start, span, playing, clampStart]);
  /** Moves the view by hand (the playhead stays). */
  const pan = useCallback(
    (to: number | ((s: number) => number)) => {
      follow.current = false;
      setStart((s) => clampStart(typeof to === 'function' ? to(s) : to, span));
    },
    [span, clampStart],
  );
  const centre = () => {
    if (!pos) return;
    follow.current = true;
    setStart(clampStart(pos.wall - span / 2, span));
  };
  // Wheel / trackpad: move in time; Ctrl or ⌘ (and pinch): zoom around the pointer.
  const view = useRef({ start, span, width });
  view.current = { start, span, width };
  useEffect(() => {
    const el = tracks.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const { start: st, span: sp, width: w } = view.current;
      if (e.ctrlKey || e.metaKey) {
        const r = el.getBoundingClientRect();
        zoomAnchor.current = st + ((e.clientX - r.left) / r.width) * sp;
        const next = Math.min(zoomMax(rec.loopHours), Math.max(ZOOM_MIN, sp * Math.exp(e.deltaY * 0.004)));
        setSpan(next);
        return;
      }
      const d = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
      pan((s) => s + (d / w) * sp);
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  }, [pan, setSpan, rec.loopHours]);

  const end = start + span;
  const x = (wall: number) => ((wall - start) / span) * 100;
  // Once a second: what depends on the time (not each frame shown) is drawn again only then.
  const now = useNow(1000);
  const loopFrom = now - rec.loopHours * 3_600_000;

  const ticks = useMemo(() => {
    const step = TICKS.find((s) => (s / span) * width >= 90) ?? TICKS[TICKS.length - 1];
    const out: number[] = [];
    for (let v = Math.ceil(start / step) * step; v < end; v += step) out.push(v);
    return { step, out };
  }, [start, end, span, width]);

  // Stills about 96 px wide, on a fixed grid of durations: reused while zooming,
  // and asked for only once the zoom has settled.
  const settledSpan = useSettled(span, 200);
  const strip = useMemo(() => {
    const want = settledSpan / Math.max(1, Math.floor(width / 96));
    const d = TILE_STEPS.find((x) => x >= want) ?? TILE_STEPS[TILE_STEPS.length - 1];
    const out: number[] = [];
    for (let v = Math.floor(start / d) * d; v < end; v += d) out.push(v + d / 2);
    return { d, out };
  }, [start, end, settledSpan, width]);

  // The sound level under the stills (one channel), for the window.
  const [wave, setWave] = useState<{ from: number; to: number; levels: number[] } | null>(null);
  const waveKey = useSettled(`${Math.round(start)}|${Math.round(span)}|${width}`, 150);
  const liveWindow = end > now - 10_000;
  useEffect(() => {
    let dead = false;
    const [a, sp] = waveKey.split('|').map(Number);
    const load = async () => {
      try {
        const w = await api<{ from: number; to: number; levels: number[] }>('GET', `/api/recorders/${rec.id}/wave?from=${a}&to=${a + sp}&n=${Math.max(50, Math.round(width / 2))}`);
        if (!dead) setWave(w);
      } catch {
        /* the next try */
      }
    };
    void load();
    const i = liveWindow ? setInterval(() => void load(), 4000) : 0;
    return () => {
      dead = true;
      clearInterval(i);
    };
  }, [waveKey, rec.id, width, liveWindow]);
  const wavePath = useMemo(() => {
    if (!wave) return '';
    const n = wave.levels.length;
    const col = (wave.to - wave.from) / n;
    const X = (i: number) => (((wave.from + i * col - start) / span) * 1000).toFixed(1);
    let d = '';
    let i = 0;
    while (i < n) {
      if (wave.levels[i] < 0) {
        i++;
        continue;
      }
      let j = i;
      while (j < n && wave.levels[j] >= 0) j++;
      const top: string[] = [];
      const bottom: string[] = [];
      for (let k = i; k < j; k++) {
        const a = Math.max(0.4, (wave.levels[k] / 255) * 14);
        top.push(`${X(k)} ${(15 - a).toFixed(1)} ${X(k + 1)} ${(15 - a).toFixed(1)}`);
        bottom.unshift(`${X(k + 1)} ${(15 + a).toFixed(1)} ${X(k)} ${(15 + a).toFixed(1)}`);
      }
      d += `M${top.join(' L')} L${bottom.join(' L')}Z`;
      i = j;
    }
    return d;
  }, [wave, start, span]);

  const gaps = useMemo(() => {
    const out: [number, number][] = [];
    let cursor = Math.max(start, loopFrom);
    for (const s of [...spans].sort((a, b) => a.from - b.from)) {
      if (s.to <= cursor) continue;
      if (s.from > cursor && cursor < end) out.push([cursor, Math.min(s.from, end)]);
      cursor = Math.max(cursor, s.to);
    }
    return out;
  }, [spans, start, end, loopFrom]);

  const wallAt = (clientX: number) => {
    const r = tracks.current!.getBoundingClientRect();
    return start + ((clientX - r.left) / r.width) * span;
  };
  const pending = useRef<number | null>(null);
  const busy = useRef(false);
  const scrubTo = (wall: number) => {
    pending.current = wall;
    if (busy.current) return;
    busy.current = true;
    const go = () => {
      const w = pending.current;
      pending.current = null;
      if (w === null) {
        busy.current = false;
        return;
      }
      Promise.resolve(onSeek(w)).finally(() => setTimeout(go, 30));
    };
    go();
  };

  // Pointer on the timeline: dragging the playhead, moving the view, or a click.
  const drag = useRef<{ mode: 'pending' | 'pan' | 'scrub'; x0: number; s0: number } | null>(null);
  const [grab, setGrab] = useState<'' | 'on-head' | 'grabbing' | 'scrubbing'>('');
  const nearHead = (clientX: number) => {
    if (!pos || !tracks.current) return false;
    const r = tracks.current.getBoundingClientRect();
    return Math.abs(clientX - (r.left + ((pos.wall - start) / span) * r.width)) <= 7;
  };

  const onAirKey = pos ? guide.find((g) => g.start <= pos.wall && pos.wall < g.end) : undefined;
  // Overview: the whole loop.
  const oFrom = loopFrom;
  const oSpan = now - loopFrom;
  const ox = (w: number) => Math.min(100, Math.max(0, ((w - oFrom) / oSpan) * 100));
  const oldest = spans.length ? Math.min(...spans.map((s) => s.from)) : now;
  const sel = mIn && mOut && mOut.wall >= mIn.wall ? [mIn.wall, mOut.wall + 1000 / fps] : null;

  return (
    <div className="sc-timeline">
      <div className="sc-tl-head">
        <div className="sc-zoom" title={t('Zoom')}>
          <ZoomOut size={14} />
          <input type="range" min={0} max={1000} step={1} value={toSlider(span, maxSpan)} aria-label={t('Zoom')} onChange={(e) => setSpan(fromSlider(Number(e.target.value), maxSpan))} onKeyDown={(e) => e.stopPropagation()} />
          <ZoomIn size={14} />
          <span className="sc-zoom-v">{spanLabel(span)}</span>
          <button type="button" className="icon-btn ghost" title={t('Zoom to 1 h')} disabled={Math.abs(span - ZOOM_RESET) < 500} onClick={() => setSpan(ZOOM_RESET)}>
            <RotateCcw size={14} />
          </button>
        </div>
        <span className="muted">
          {day(start, timezone, locale())} {clock(start, timezone)} → {clock(end, timezone)}
        </span>
        {pos && (pos.wall < start || pos.wall > end) && (
          <button type="button" className="btn small" title={t('Centre on the playhead')} onClick={centre}>
            <Crosshair size={13} /> {t('Playhead')}
          </button>
        )}
        <div className="spacer" />
        <span className="muted">
          {t('Loop {h} h · since {when}', { h: rec.loopHours, when: `${day(oldest, timezone, locale())} ${clock(oldest, timezone)}` })}
        </span>
      </div>
      <div
        className="sc-overview"
        title={t('The whole loop: click or drag to move the view')}
        onPointerDown={(e) => {
          if (e.button !== 0) return;
          e.currentTarget.setPointerCapture(e.pointerId);
          const r = e.currentTarget.getBoundingClientRect();
          pan(oFrom + ((e.clientX - r.left) / r.width) * oSpan - span / 2);
        }}
        onPointerMove={(e) => {
          if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
          const r = e.currentTarget.getBoundingClientRect();
          pan(oFrom + ((e.clientX - r.left) / r.width) * oSpan - span / 2);
        }}
      >
        {spans.map((s, i) => (
          <div key={i} className="sc-ov-rec" style={{ left: `${ox(s.from)}%`, width: `max(2px, ${ox(s.to) - ox(s.from)}%)` }} />
        ))}
        {clips.map((c) => (
          <div key={c.id} className="sc-ov-clip" style={{ left: `${ox(c.start)}%` }} title={c.name} />
        ))}
        <div className="sc-ov-win" style={{ left: `${ox(start)}%`, width: `${Math.max(0.4, ox(end) - ox(start))}%` }} />
        {pos && <div className="sc-ov-head" style={{ left: `${ox(pos.wall)}%` }} />}
      </div>
      <div
        className={`sc-tracks ${grab}`}
        ref={tracks}
        onPointerDown={(e) => {
          if (e.button !== 0) return;
          e.currentTarget.setPointerCapture(e.pointerId);
          // On the playhead or anywhere on the ruler (the hours): drag it.
          // Elsewhere: drag = move the view, click = put the playhead there.
          const onRuler = !!(e.target as HTMLElement).closest('.sc-ruler');
          if (nearHead(e.clientX) || onRuler) {
            drag.current = { mode: 'scrub', x0: e.clientX, s0: start };
            scrubbing.current = true;
            setGrab('scrubbing');
            scrubTo(wallAt(e.clientX));
          } else {
            drag.current = { mode: 'pending', x0: e.clientX, s0: start };
          }
        }}
        onPointerMove={(e) => {
          const d = drag.current;
          if (!d) {
            setGrab(nearHead(e.clientX) || (e.target as HTMLElement).closest('.sc-ruler') ? 'on-head' : '');
            return;
          }
          if (d.mode === 'scrub') scrubTo(wallAt(e.clientX));
          else if (d.mode === 'pan' || Math.abs(e.clientX - d.x0) > 4) {
            d.mode = 'pan';
            setGrab('grabbing');
            pan(d.s0 - ((e.clientX - d.x0) / width) * span);
          }
        }}
        onPointerUp={(e) => {
          const d = drag.current;
          drag.current = null;
          scrubbing.current = false;
          setGrab(nearHead(e.clientX) ? 'on-head' : '');
          if (d?.mode !== 'pending') return;
          // A click on a programme: its start (Shift: in and out on the whole programme).
          const item = (e.target as HTMLElement).closest<HTMLElement>('.sc-epg-item');
          const g = item ? guide.find((x) => String(x.id) + x.start === item.dataset.key) : undefined;
          if (g && e.shiftKey) onProgramme(g);
          else if (g) onSeek(Math.max(g.start, oldest));
          else onSeek(wallAt(e.clientX));
        }}
        onPointerCancel={() => {
          drag.current = null;
          scrubbing.current = false;
          setGrab('');
        }}
        onPointerLeave={() => !drag.current && setGrab('')}
      >
        <Layers recId={rec.id} start={start} span={span} timezone={timezone} ticks={ticks} guide={guide} onAir={onAirKey ? String(onAirKey.id) + onAirKey.start : ''} strip={strip} now={now} spans={spans} wavePath={wavePath} gaps={gaps} />
        {sel && <div className="sc-sel" style={{ left: `${x(sel[0])}%`, width: `${x(sel[1]) - x(sel[0])}%` }} />}
        {/* The in and out points, each shown as soon as it is set. */}
        {mIn && <div className="sc-mark in" style={{ left: `${x(mIn.wall)}%` }}><b>{t('In').charAt(0)}</b></div>}
        {mOut && <div className="sc-mark out" style={{ left: `${x(mOut.wall + 1000 / fps)}%` }}><b>{t('Out').charAt(0)}</b></div>}
        {pos && x(pos.wall) >= 0 && x(pos.wall) <= 100 && <div className="sc-head" style={{ left: `${x(pos.wall)}%` }} />}
      </div>
    </div>
  );
}
