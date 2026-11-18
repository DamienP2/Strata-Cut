import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { cachedStill, loadStill } from '../stills';
import { useShallow } from 'zustand/react/shallow';
import { ArrowLeftToLine, ArrowRightToLine, Check, ChevronsLeft, ChevronsRight, Crosshair, Download, Pause, Play, RadioTower, RotateCcw, Scissors, StepBack, StepForward, Tv, X, ZoomIn, ZoomOut } from 'lucide-react';
import { api, can, serverNow, useApp, useChanged } from '../store';
import { t, tn, locale } from '../i18n';
import { navigate } from '../router';
import { LoopPlayer, type RunInfo } from '../player';
import { behind, clock, day, duration, fpsOf, parseTimecode, scanFormat, timecode } from '../tc';
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

/** The zoom: the timeline shows from 10 s to the whole loop; reset = 1 min. */
const ZOOM_MIN = 10_000;
const ZOOM_RESET = 60_000;
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
  useEffect(() => {
    const known = cachedStill(url);
    setSrc(known);
    if (known !== undefined) return;
    return loadStill(url, setSrc);
  }, [url]);
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
  const id = recorder && recorders.some((r) => r.id === recorder) ? recorder : recorders[0]?.id;
  useEffect(() => {
    if (id && id !== recorder) void navigate(`/extract/${id}`, true);
  }, [id, recorder]);
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
          <div key={r.id} className={`lib-nav-item ${r.id === current ? 'on' : ''}`} onClick={() => void navigate(`/extract/${r.id}`)}>
            <RadioTower size={15} />
            <span className="grow">{r.name}</span>
            <span className="count">{r.protocol.toUpperCase()}</span>
            <span className={`nav-dot ${st === 'recording' ? 'ok' : st === 'off' ? '' : st === 'error' ? 'bad' : 'warn'}`} />
          </div>
        );
      })}
    </aside>
  );
}

function Editor({ rec }: { rec: Recorder }) {
  const { status, timezone, user, jobs, toast } = useApp(useShallow((s) => ({ status: s.status[rec.id], timezone: s.timezone, user: s.auth?.user, jobs: s.jobs, toast: s.toast })));
  const videoRef = useRef<HTMLVideoElement>(null);
  const playerRef = useRef<LoopPlayer | null>(null);
  const [pos, setPos] = useState<Mark | null>(null);
  const [run, setRun] = useState<RunInfo | null>(null);
  const [playing, setPlaying] = useState(false);
  const [live, setLive] = useState(true);
  const [mIn, setIn] = useState<Mark | null>(null);
  const [mOut, setOut] = useState<Mark | null>(null);
  const [name, setName] = useState('');
  const [span, setSpan] = useState(ZOOM_RESET);
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
  const onAir = pos ? guide.find((g) => g.start <= pos.wall && pos.wall < g.end) : undefined;
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
    void p.goLive(serverNow());
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
  const markProgramme = async (g: Programme) => {
    // A clip stays in one continuous recording: the one holding most of the programme.
    const best = spans
      .map((x) => ({ from: Math.max(g.start, x.from), to: Math.min(g.end, x.to) }))
      .filter((x) => x.to > x.from)
      .sort((a, b) => b.to - b.from - (a.to - a.from))[0];
    if (!best) return toast(t('This programme is not recorded'), 'error');
    const from = best.from;
    const to = best.to - 1000 / fps;
    if (to <= from) return toast(t('This programme is not recorded'), 'error');
    setLive(false);
    const pl = p();
    if (!pl) return;
    pl.halt();
    await pl.seekWall(to);
    const out = { run: pl.run?.id ?? '', t: pl.t, wall: pl.wallOf(pl.t) };
    await pl.seekWall(from);
    setIn({ run: pl.run?.id ?? '', t: pl.t, wall: pl.wallOf(pl.t) });
    setOut(out);
    setName(g.name);
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

  const now = serverNow();
  const media = run?.media ?? status?.media ?? null;
  const ago = pos ? now - pos.wall : 0;
  const myJobs = jobs;

  return (
    <>
      <div className="sc-center">
        <div className="sc-viewer-wrap">
          <div className="sc-viewer">
            <video ref={videoRef} className="sc-video" playsInline />
            {run?.audio && <Meters out={audio.out} />}
            <span className="sc-onpic tl">
              {rec.name}
              {onAir && <span className="sc-onair"> · {onAir.name}</span>}
            </span>
            {media && (
              <span className="sc-onpic tr mono">
                {scanFormat(media)}
                {media.audioTracks > 0 && ` · ${tn(media.audioTracks, '{n} track', '{n} tracks')}`}
              </span>
            )}
            {pos && (live && ago < 30_000 ? (
              <span className="sc-onpic br">
                <span className="sc-rec" /> {t('LIVE')}
              </span>
            ) : (
              <span className="sc-onpic br mono" title={t('Behind live')}>
                {behind(ago)}
              </span>
            ))}
          </div>
        </div>
        <div className="sc-transport">
          <div className="sc-tc-big">{pos ? timecode(pos.wall, fps, timezone) : '--:--:--:--'}</div>
          <div className="sc-ctrls">
            <button className="icon-btn ghost" title={t('Go to in  (Shift+I)')} disabled={!mIn} onClick={() => mIn && seekWall(mIn.wall)}>
              <ArrowRightToLine size={16} />
            </button>
            <button className="icon-btn ghost" title={t('Previous frame  (←)')} onClick={() => step(-1)}>
              <StepBack size={16} />
            </button>
            <button className="icon-btn ghost" title={t('Play backwards  (J)')} onClick={() => { setLive(false); p()?.playReverse(); setPlaying(true); }}>
              <ChevronsLeft size={16} />
            </button>
            <button className="icon-btn play" title={t('Play / pause  (K, Space)')} onClick={togglePlay}>
              {playing ? <Pause size={16} /> : <Play size={16} />}
            </button>
            <button className="icon-btn ghost" title={t('Play faster  (L)')} onClick={() => { setLive(false); p()?.play(2); }}>
              <ChevronsRight size={16} />
            </button>
            <button className="icon-btn ghost" title={t('Next frame  (→)')} onClick={() => step(1)}>
              <StepForward size={16} />
            </button>
            <button className="icon-btn ghost" title={t('Go to out  (Shift+O)')} disabled={!mOut} onClick={() => mOut && seekWall(mOut.wall)}>
              <ArrowLeftToLine size={16} />
            </button>
          </div>
          <div className="sc-sep" />
          <button className="btn small" title="I" onClick={markIn} disabled={!pos}>
            <ArrowRightToLine size={13} /> {t('In')}
          </button>
          <button className="btn small" title="O" onClick={markOut} disabled={!pos}>
            <ArrowLeftToLine size={13} /> {t('Out')}
          </button>
          <div className="spacer" />
          <VolumeControl volume={audio.volume} setVolume={audio.setVolume} muted={audio.muted} setMuted={audio.setMuted} />
          <button className={`btn small sc-live-btn ${live ? 'on' : ''}`} title={t('Back to live')} onClick={goLive}>
            <span className="sc-rec" /> {t('Live')}
          </button>
        </div>
      </div>
      {/* The timeline takes the whole width, under the panels. */}
      <Timeline rec={rec} pos={pos} playing={playing} fps={fps} span={span} setSpan={setSpan} spans={spans} clips={clips} mIn={mIn} mOut={mOut} onSeek={seekWall} timezone={timezone} guide={guide} onProgramme={(g) => void markProgramme(g)} />

      <aside className="panel right">
        <section className="insp-section">
          <header>
            <span>{t('Clip')}</span>
          </header>
          <div className="insp-body">
            <label className="set-field">
              <span className="set-label">{t('Name')}</span>
              <input value={name} placeholder={mIn ? `${rec.name} ${timecode(mIn.wall, fps, timezone)}` : rec.name} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
            </label>
            <TcField label={t('In')} mark={mIn} fps={fps} timezone={timezone} onTake={markIn} takeTitle={t('Take the current frame  (I)')} icon={<ArrowRightToLine size={14} />} onType={(s) => typed(s, setIn)} />
            <TcField label={t('Out')} mark={mOut} fps={fps} timezone={timezone} onTake={markOut} takeTitle={t('Take the current frame  (O)')} icon={<ArrowLeftToLine size={14} />} onType={(s) => typed(s, setOut)} />
            {onAir && (
              <button type="button" className="btn small sc-prog-btn" title={t('In and out on the whole programme under the playhead')} onClick={() => void markProgramme(onAir)}>
                <Tv size={13} /> <span>{t('Whole programme: {name}', { name: onAir.name })}</span>
              </button>
            )}
            <div className="sc-dur">
              <span className="muted">{t('Duration')}</span>
              <b>{duration(frames, fps)}</b>
            </div>
            {can.extract(user) && (
              <button className="btn primary block" disabled={!canCut} onClick={() => void cut()}>
                <Scissors size={14} /> {t('Extract')}
              </button>
            )}
          </div>
        </section>
        {myJobs.length > 0 && <Jobs jobs={myJobs} />}
      </aside>
    </>
  );
}

function TcField({ label, mark, fps, timezone, onTake, takeTitle, icon, onType }: { label: string; mark: Mark | null; fps: number; timezone: string; onTake: () => void; takeTitle: string; icon: React.ReactNode; onType: (s: string) => void }) {
  const shown = mark ? timecode(mark.wall, fps, timezone) : '';
  const [text, setText] = useState(shown);
  useEffect(() => setText(shown), [shown]);
  return (
    <label className="set-field">
      <span className="set-label">{label}</span>
      <div className="sc-tc-input">
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
        <button type="button" className="icon-btn" title={takeTitle} onClick={onTake}>
          {icon}
        </button>
      </div>
    </label>
  );
}

function Jobs({ jobs }: { jobs: Job[] }) {
  const { user, toast } = useApp(useShallow((s) => ({ user: s.auth?.user, toast: s.toast })));
  const running = jobs.filter((j) => j.state === 'queued' || j.state === 'running').length;
  const cancel = async (j: Job) => {
    try {
      await api('DELETE', `/api/jobs/${j.id}`);
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  return (
    <section className="insp-section" style={{ borderBottom: 'none' }}>
      <header>
        <span>{t('Extractions')}</span>
        {running > 0 && <span className="count">{running}</span>}
      </header>
      <div className="insp-body">
        {jobs.map((j) => (
          <div key={j.id} className="sc-job">
            <div className="sc-job-top">
              <strong title={j.name}>{j.name}</strong>
              {j.state === 'running' && <span className="muted">{Math.round(j.progress * 100)} %</span>}
              {j.state === 'queued' && <span className="muted">{t('Waiting')}</span>}
              {j.state === 'done' && j.clip && (
                <a className="icon-btn ghost" title={t('Download')} href={`/api/clips/${j.clip}/download`}>
                  <Download size={14} />
                </a>
              )}
              {j.state === 'done' && <span className="ok"><Check size={14} /></span>}
              {(j.state === 'queued' || j.state === 'running') && can.extract(user) && (
                <button className="icon-btn ghost" title={t('Cancel')} onClick={() => void cancel(j)}>
                  <X size={14} />
                </button>
              )}
            </div>
            {j.state === 'running' && (
              <span className="perf-bar tone-accent">
                <span className="perf-bar-fill" style={{ transform: `translateX(${j.progress * 100 - 100}%)` }} />
              </span>
            )}
            {j.state === 'error' && <span className="sc-job-err">{t(j.message)}</span>}
          </div>
        ))}
      </div>
    </section>
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
  const now = serverNow();
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
  const recorded = (w: number) => spans.some((s) => w >= s.from && w < s.to);

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
          <button type="button" className="icon-btn ghost" title={t('Zoom to 1 min')} disabled={Math.abs(span - ZOOM_RESET) < 500} onClick={() => setSpan(ZOOM_RESET)}>
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
                const on = pos && g.start <= pos.wall && pos.wall < g.end;
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
              {recorded(w) && w < now - 6000 && <Still url={`/api/recorders/${rec.id}/frame.jpg?at=${Math.round(w)}&h=54${strip.d >= 2000 ? '&fast=1' : ''}`} />}
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
        {sel && <div className="sc-sel" style={{ left: `${x(sel[0])}%`, width: `${x(sel[1]) - x(sel[0])}%` }} />}
        {/* The in and out points, each shown as soon as it is set. */}
        {mIn && <div className="sc-mark in" style={{ left: `${x(mIn.wall)}%` }}><b>{t('In').charAt(0)}</b></div>}
        {mOut && <div className="sc-mark out" style={{ left: `${x(mOut.wall + 1000 / fps)}%` }}><b>{t('Out').charAt(0)}</b></div>}
        {pos && x(pos.wall) >= 0 && x(pos.wall) <= 100 && <div className="sc-head" style={{ left: `${x(pos.wall)}%` }} />}
      </div>
    </div>
  );
}
