import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { ArrowLeftToLine, ArrowRightToLine, Check, ChevronsLeft, ChevronsRight, Download, Pause, Play, RadioTower, Scissors, StepBack, StepForward, X } from 'lucide-react';
import { api, can, serverNow, useApp, useChanged } from '../store';
import { t, tn, locale } from '../i18n';
import { navigate } from '../router';
import { LoopPlayer, type RunInfo } from '../player';
import { behind, clock, day, duration, fpsOf, parseTimecode, scanFormat, timecode } from '../tc';
import { Segmented } from '../ui/fields';
import { Empty } from '../ui/Empty';
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

const ZOOMS = [
  { ms: 86_400_000, label: '24 h' },
  { ms: 3_600_000, label: '1 h' },
  { ms: 600_000, label: '10 min' },
  { ms: 60_000, label: '1 min' },
  { ms: 10_000, label: '10 s' },
];

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
  const [zoom, setZoom] = useState(3);
  const [spans, setSpans] = useState<Span[]>([]);
  const [clips, setClips] = useState<Clip[]>([]);
  const fps = fpsOf(run?.media?.fpsNum, run?.media?.fpsDen);

  // The player lives as long as this recorder is open.
  useEffect(() => {
    const v = videoRef.current!;
    const p = new LoopPlayer(v, rec.id);
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

  const p = () => playerRef.current;
  const seekWall = useCallback((wall: number) => {
    setLive(false);
    if (p()) p()!.live = false;
    void p()?.seekWall(wall, p()?.playing);
  }, []);

  const markIn = () => pos && setIn(pos);
  const markOut = () => pos && setOut(pos);
  const step = (n: number) => {
    setLive(false);
    p()?.step(n);
  };
  const togglePlay = () => {
    const pl = p();
    if (!pl) return;
    if (pl.playing) pl.pause();
    else {
      setLive(false);
      pl.play();
    }
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
      else if (k === 'i') markIn();
      else if (k === 'o') markOut();
      else if (k === ' ' || k === 'k') {
        speed.current = 0;
        if (k === ' ') togglePlay();
        else pl.pause();
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
            <span className="sc-onpic tl">{rec.name}</span>
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
          <button className={`btn small sc-live-btn ${live ? 'on' : ''}`} title={t('Back to live')} onClick={goLive}>
            <span className="sc-rec" /> {t('Live')}
          </button>
        </div>
        <Timeline rec={rec} pos={pos} fps={fps} zoom={zoom} setZoom={setZoom} spans={spans} clips={clips} mIn={mIn} mOut={mOut} onSeek={seekWall} timezone={timezone} />
      </div>

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

function Timeline(props: { rec: Recorder; pos: Mark | null; fps: number; zoom: number; setZoom: (z: number) => void; spans: Span[]; clips: Clip[]; mIn: Mark | null; mOut: Mark | null; onSeek: (wall: number) => void; timezone: string }) {
  const { rec, pos, fps, zoom, setZoom, spans, clips, mIn, mOut, onSeek, timezone } = props;
  const span = ZOOMS[zoom].ms;
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
  // Keep the playhead in view: recentre on zoom, page when it leaves.
  useEffect(() => setStart((pos?.wall ?? serverNow()) - span / 2), [span]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (!pos || scrubbing.current) return;
    if (pos.wall < start + span * 0.02 || pos.wall > start + span * 0.98) setStart(pos.wall - span * (pos.wall > start ? 0.8 : 0.2));
  }, [pos, start, span]);

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

  const tile = 96;
  const strip = useMemo(() => {
    const n = Math.max(1, Math.floor(width / tile));
    const d = span / n;
    const first = Math.floor(start / d) * d;
    const out: number[] = [];
    for (let v = first; v < end; v += d) out.push(v + d / 2);
    return { d, out };
  }, [start, end, span, width]);
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

  // Overview: the whole loop.
  const oFrom = loopFrom;
  const oSpan = now - loopFrom;
  const ox = (w: number) => Math.min(100, Math.max(0, ((w - oFrom) / oSpan) * 100));
  const oldest = spans.length ? Math.min(...spans.map((s) => s.from)) : now;
  const sel = mIn && mOut && mOut.wall >= mIn.wall ? [mIn.wall, mOut.wall + 1000 / fps] : null;

  return (
    <div className="sc-timeline">
      <div className="sc-tl-head">
        <Segmented value={String(zoom)} options={ZOOMS.map((z, i) => ({ value: String(i), label: z.label }))} onChange={(v) => setZoom(Number(v))} />
        <span className="muted">
          {day(start, timezone, locale())} {clock(start, timezone)} → {clock(end, timezone)}
        </span>
        <div className="spacer" />
        <span className="muted">
          {t('Loop {h} h · since {when}', { h: rec.loopHours, when: `${day(oldest, timezone, locale())} ${clock(oldest, timezone)}` })}
        </span>
      </div>
      <div className="sc-overview" title={t('The whole loop')} onPointerDown={(e) => {
        const r = e.currentTarget.getBoundingClientRect();
        onSeek(oFrom + ((e.clientX - r.left) / r.width) * oSpan);
      }}>
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
        className="sc-tracks"
        ref={tracks}
        onPointerDown={(e) => {
          if (e.button !== 0) return;
          e.currentTarget.setPointerCapture(e.pointerId);
          scrubbing.current = true;
          scrubTo(wallAt(e.clientX));
        }}
        onPointerMove={(e) => scrubbing.current && scrubTo(wallAt(e.clientX))}
        onPointerUp={() => (scrubbing.current = false)}
        onPointerCancel={() => (scrubbing.current = false)}
      >
        <div className="sc-ruler">
          {ticks.out.filter((v) => x(v) < 94).map((v) => (
            <span key={v} style={{ left: `${x(v)}%` }}>
              {ticks.step >= 60_000 ? clock(v, timezone).slice(0, 5) : clock(v, timezone)}
            </span>
          ))}
        </div>
        <div className="sc-strip">
          {strip.out.map((w) => (
            <div key={w} className="sc-tile" style={{ left: `${x(w - strip.d / 2)}%`, width: `${(strip.d / span) * 100}%` }}>
              {recorded(w) && w < now - 6000 && <img src={`/api/recorders/${rec.id}/frame.jpg?at=${Math.round(w)}&h=54`} alt="" draggable={false} onError={(e) => (e.currentTarget.style.visibility = 'hidden')} />}
            </div>
          ))}
        </div>
        {gaps.map(([a, b], i) => (
          <div key={i} className="sc-gap" style={{ left: `${x(a)}%`, width: `${x(b) - x(a)}%` }} title={t('Not recorded')} />
        ))}
        {sel && <div className="sc-sel" style={{ left: `${x(sel[0])}%`, width: `${x(sel[1]) - x(sel[0])}%` }} />}
        {pos && <div className="sc-head" style={{ left: `${x(pos.wall)}%` }} />}
      </div>
    </div>
  );
}
