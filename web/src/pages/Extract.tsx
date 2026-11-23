import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { cachedStill, loadStill } from '../stills';
import { useDisplayPref } from '../displayPrefs';
import { useShallow } from 'zustand/react/shallow';
import { ArrowLeftToLine, ArrowRightToLine, AudioLines, CalendarPlus, Camera, Captions, Check, ChevronLeft, ChevronRight, ChevronsRight, Crosshair, Download, Eraser, FastForward, Film, History, Pause, Play, PlayCircle, RadioTower, Rewind, RotateCcw, Scissors, StepBack, StepForward, VideoOff, X, ZoomIn, ZoomOut } from 'lucide-react';
import { api, can, serverNow, useApp, useChanged } from '../store';
import { SourceGroups } from '../ui/SourceGroups';
import { t, locale } from '../i18n';
import { navigate } from '../router';
import { LoopPlayer, type RunInfo } from '../player';
import { ClipPreview } from './ClipPreview';
import { behind, clock, day, duration, fpsOf, parseTimecode, timecode } from '../tc';
import { Empty } from '../ui/Empty';
import { formatDuration } from '../ui/format';
import { Meters, VolumeControl, useAudioOut } from '../ui/AudioUi';
import { GrainCache } from '../audio';
import type { Clip, Job, Recorder } from '../types';
import { trackLabel } from '../audioLabel';

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
/** The zoom at first (and when reset): 5 minutes. */
const ZOOM_RESET = 300_000;
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
  const { recorders, status } = useApp(useShallow((s) => ({ recorders: s.recorders, status: s.status })));
  // The recorder asked, else the last one opened, else the first with something to show (it
  // records, or its loop holds something).
  const [last, setLast] = useDisplayPref<string | null>('extract.recorder', null);
  const known = (x?: string | null) => !!x && recorders.some((r) => r.id === x);
  const open = (x?: string | null) => known(x) && (status[x!]?.state === 'recording' || status[x!]?.oldest != null);
  // A source stopped with nothing recorded has nothing to show (greyed in the list): another,
  // once the states are known.
  const ready = Object.keys(status).length > 0;
  const fallback = open(last) ? last! : recorders.find((r) => open(r.id))?.id;
  const id = known(recorder) && (!ready || open(recorder) || !fallback) ? recorder : (fallback ?? recorders[0]?.id);
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
  return <SourceGroups kind="extract" current={current} base="/extract" />;
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
  const [span, setSpan] = useDisplayPref('extract.zoom.5m', ZOOM_RESET);
  const [spans, setSpans] = useState<Span[]>([]);
  const spansRef = useRef<Span[]>([]);
  spansRef.current = spans;
  /** The playhead in a period not recorded: that period (the picture says so instead of black). */
  const [hole, setHole] = useState<{ from: number; to: number } | null>(null);
  /** An in or out point let go on the timeline, set once its frame is shown. */
  const pendingMark = useRef<{ which: 'in' | 'out'; wall: number } | null>(null);
  /** What the clip holds: the picture with its sound, the sound alone, the picture under the playhead. */
  const [kind, setKind] = useDisplayPref<'video' | 'audio' | 'image'>('extract.kind', 'video');
  // The source's subtitles burnt into a video clip (when it has some).
  const [burnSubs, setBurnSubs] = useDisplayPref<boolean>('extract.subtitles', false);
  const [clips, setClips] = useState<Clip[]>([]);
  const [guide, setGuide] = useState<Programme[]>([]);
  const fps = fpsOf(run?.media?.fpsNum, run?.media?.fpsDen);
  const hasSubs = (run?.media?.subtitles?.length ?? 0) > 0;
  // The sound track heard (the player keeps it from a recording to the next).
  const [audioTrack, setAudioTrack] = useState(0);
  // A live only while the source records; stopped, its last recorded frame.
  const state = useApp((s) => s.status[rec.id]?.state);
  const newest = useApp((s) => s.status[rec.id]?.newest ?? null);
  const recording = state === 'recording';
  const newestRef = useRef(newest);
  newestRef.current = newest;
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
  const audio = useAudioOut(videoRef, 'extract');

  // The player lives as long as this recorder is open.
  useEffect(() => {
    const v = videoRef.current!;
    const p = new LoopPlayer(v, rec.id, serverNow);
    playerRef.current = p;
    let frame = 0;
    let last: Mark | null = null;
    p.onFrame = (wall, tt) => {
      setHole(null);
      last = { run: p.run?.id ?? '', t: tt, wall };
      // An in or out point dragged on the timeline: set on the frame it was let go at.
      const want = pendingMark.current;
      if (want && Math.abs(wall - want.wall) < 120) {
        pendingMark.current = null;
        (want.which === 'in' ? setIn : setOut)(last);
      }
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
    return () => {
      cancelAnimationFrame(frame);
      v.removeEventListener('play', onPlay);
      v.removeEventListener('pause', onPause);
      p.destroy();
      playerRef.current = null;
    };
  }, [rec.id]);

  /** The newest recorded frame, paused (a source stopped: no live). */
  const goLast = useCallback(() => {
    const pl = playerRef.current;
    setLive(false);
    if (!pl || newestRef.current === null) return;
    pl.halt();
    // Its last frame: a moment past the end is put on it.
    void pl.seekWall(newestRef.current);
  }, []);
  // Where the page starts, once the source's state is known: `?at=<ms>` (from the Schedule page),
  // else the live while it records, else its last recorded frame.
  const begun = useRef('');
  // The last frame shown when the source stopped: the live again by itself when it records anew.
  const parked = useRef<number | null>(null);
  const was = useRef(recording);
  useEffect(() => {
    const pl = playerRef.current;
    if (!pl || begun.current === rec.id || state === undefined) return;
    begun.current = rec.id;
    was.current = state === 'recording';
    parked.current = null;
    const at = Number(new URLSearchParams(location.search).get('at'));
    if (at > 0) {
      setLive(false);
      void pl.seekWall(at).then(() => {
        if (pl.run) return;
        // That moment is no longer in the loop.
        toast(t('This moment is no longer in the loop'), 'error');
        if (state === 'recording') {
          setLive(true);
          void pl.goLive(serverNow());
        } else goLast();
      });
    } else if (state === 'recording') {
      setLive(true);
      void pl.goLive(serverNow());
    } else {
      goLast();
      // Starting, or without signal for a moment: the live, once it records.
      if (state !== 'off') parked.current = newestRef.current;
    }
  }, [rec.id, state, goLast, toast]);
  // The source stops while it is followed live: on its last frame; when it records again, back to
  // the live by itself if the playhead was left there.
  useEffect(() => {
    const pl = playerRef.current;
    if (was.current === recording || begun.current !== rec.id) {
      was.current = recording;
      return;
    }
    was.current = recording;
    if (!pl) return;
    if (!recording && live) {
      goLast();
      parked.current = newestRef.current;
    } else if (recording && parked.current !== null) {
      const at = parked.current;
      parked.current = null;
      if (pl.video.paused && Math.abs(pl.wallOf(pl.t) - at) < 2000) {
        setLive(true);
        void pl.goLive(serverNow());
      }
    }
  }, [recording]); // eslint-disable-line react-hooks/exhaustive-deps

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
    // A moment not recorded (between two recordings): the playhead stays there, the picture says so.
    const all = [...spansRef.current].sort((a, b) => a.from - b.from);
    const before = all.filter((x) => x.to <= wall).at(-1);
    const after = all.find((x) => x.from > wall);
    if (all.length && !all.some((x) => wall >= x.from && wall < x.to) && wall < serverNow()) {
      // Not a frame: no in or out point there.
      pendingMark.current = null;
      setHole({ from: before?.to ?? wall, to: after?.from ?? serverNow() });
      setPos({ run: '', t: -1, wall });
      return;
    }
    setHole(null);
    void pl.seekWall(wall);
  }, []);
  /** The clip seen before it is made, in a window (the page's player paused meanwhile). */
  const [previewing, setPreviewing] = useState(false);
  const previewClip = () => {
    if (!mIn || !mOut) return;
    setLive(false);
    p()?.halt();
    setPreviewing(true);
  };

  /** An in or out point dragged on the timeline: the frame there becomes it. */
  const markAt = useCallback((which: 'in' | 'out', wall: number) => {
    pendingMark.current = { which, wall };
    seekWall(wall);
  }, [seekWall]);

  /** From a period not recorded: to the recording after it (or before it, going back). */
  const leaveHole = (forward: boolean, play = false) => {
    const pl = p();
    if (!hole || !pl) return false;
    setHole(null);
    void pl.seekWall(forward ? hole.to : hole.from - 1000 / fps, play);
    return true;
  };

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
      // Extraction started: the marks are cleared for the next clip.
      setIn(null);
      setOut(null);
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
  const markIn = () => pos && !hole && setIn(pos);
  const markOut = () => pos && !hole && setOut(pos);
  const step = (n: number) => {
    setLive(false);
    if (leaveHole(n > 0)) return;
    p()?.step(n);
  };
  const togglePlay = () => {
    const pl = p();
    if (!pl) return;
    setLive(false);
    if (leaveHole(true, true)) return;
    if (pl.playing) pl.pause();
    else pl.play();
  };
  const goLive = () => {
    if (!recording) return;
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

  const frames = mIn && mOut && mIn.run === mOut.run && mOut.t >= mIn.t ? Math.round((mOut.t - mIn.t) * fps) + 1 : 0;
  const canCut = can.extract(user) && (kind === 'image' ? !!pos && !hole && !!pos.run : frames > 0);
  const cut = async () => {
    // The picture under the playhead: no in or out needed (they stay).
    if (kind === 'image') {
      if (!pos || hole || !pos.run) return;
      try {
        await api('POST', '/api/clips', { recorder: rec.id, run: pos.run, t: pos.t, frames: 1, kind, name: name.trim() || `${rec.name} ${timecode(pos.wall, fps, timezone)}` });
        setName('');
      } catch (e) {
        toast(t((e as Error).message), 'error');
      }
      return;
    }
    if (!mIn || !mOut) return;
    if (mIn.run !== mOut.run) return toast(t('The in and out points must be in the same recording'), 'error');
    try {
      await api('POST', '/api/clips', { recorder: rec.id, run: mIn.run, t: mIn.t, frames, kind, subtitles: kind === 'video' && hasSubs && burnSubs, name: name.trim() || `${rec.name} ${timecode(mIn.wall, fps, timezone)}` });
      // Extraction started: the marks are cleared for the next clip.
      setIn(null);
      setOut(null);
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
            {hole && (
              <div className="sc-hole">
                <VideoOff size={34} strokeWidth={1.5} />
                <strong>{t('Not recorded')}</strong>
                <span>
                  {clock(hole.from, timezone).slice(0, 8)} → {clock(hole.to, timezone).slice(0, 8)} · {formatDuration((hole.to - hole.from) / 1000)}
                </span>
                <span className="sc-hole-hint">{t('Play or → goes on with the next recording.')}</span>
              </div>
            )}
            {run?.audio && <Meters out={audio.out} />}
            {/* Over the picture, top left: on the live, a red badge; behind it, how far, and the way back.
                A source that does not record has no live: nothing. */}
            {recording && (
              <div className="sc-live-pin">
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
            )}
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
          {/* Another sound track (another language, the audio description), when the source has some. */}
          {(run?.extraAudio ?? 0) > 0 && (
            <select
              className="sc-track"
              aria-label={t('Sound track')}
              data-tip={t('Sound track')}
              value={audioTrack}
              onChange={(e) => {
                const k = Number(e.target.value);
                setAudioTrack(k);
                void p()?.setAudioTrack(k);
              }}
            >
              {Array.from({ length: (run?.extraAudio ?? 0) + 1 }, (_, k) => (
                <option key={k} value={k}>
                  {`${k + 1} · ${trackLabel(run?.media?.audioLangs?.[k], run?.media?.audioKinds?.[k]).full}`}
                </option>
              ))}
            </select>
          )}
          <VolumeControl volume={audio.volume} setVolume={audio.setVolume} muted={audio.muted} setMuted={audio.setMuted} />
        </div>
        {/* The clip: its in and out (the timeline's E and S), its length, its name. */}
        <div className="sc-clipbar">
          <span className="sc-clipbar-l">{t('Clip')}</span>
          <div className="sc-kind" role="radiogroup" aria-label={t('What the clip holds')}>
            {([
              ['video', <Film key="v" size={14} />, t('Video'), t('The picture with its sound')],
              ['audio', <AudioLines key="a" size={14} />, t('Sound'), t('The sound alone (every track)')],
              ['image', <Camera key="i" size={14} />, t('Image'), t('The picture under the playhead')],
            ] as const).map(([k, icon, label, tip]) => (
              <button key={k} type="button" role="radio" aria-checked={kind === k} className={`sc-kind-b ${k} ${kind === k ? 'on' : ''}`} data-tip={tip} onClick={() => setKind(k)}>
                {icon}
                <span>{label}</span>
              </button>
            ))}
          </div>
          <MarkTag kind="in" mark={mIn} fps={fps} timezone={timezone} onType={(s) => typed(s, setIn)} onClear={() => setIn(null)} onMark={markIn} />
          <MarkTag kind="out" mark={mOut} fps={fps} timezone={timezone} onType={(s) => typed(s, setOut)} onClear={() => setOut(null)} onMark={markOut} />
          {kind === 'video' && hasSubs && (
            <button type="button" className={`sc-subs ${burnSubs ? 'on' : ''}`} aria-pressed={burnSubs} data-tip={t('Burn the subtitles into the picture')} onClick={() => setBurnSubs(!burnSubs)}>
              <Captions size={14} />
            </button>
          )}
          <span className="sc-clipdur">{kind === 'image' ? (pos ? timecode(pos.wall, fps, timezone) : '--:--:--:--') : duration(frames, fps)}</span>
          <input className="sc-clipname" value={name} placeholder={mIn ? `${rec.name} ${timecode(mIn.wall, fps, timezone)}` : t('Name of the clip')} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
          <button className="ctl" data-tip={t('Clear the in and out points')} data-keys="⌫" disabled={!mIn && !mOut} onClick={clearMarks}>
            <Eraser size={18} />
          </button>
          <button className="ctl sc-preview" data-tip={t('Preview the clip: from IN to OUT')} disabled={kind === 'image' || frames === 0} onClick={previewClip}>
            <PlayCircle size={18} />
          </button>
          {can.extract(user) && <ExtractButton jobs={myJobs} recId={rec.id} canCut={canCut} onCut={() => void cut()} kind={kind} />}
        </div>
      </div>
      {/* The timeline takes the whole width, under the panels. */}
      <Timeline rec={rec} pos={pos} playing={playing} fps={fps} span={span} setSpan={setSpan} spans={spans} clips={clips} mIn={mIn} mOut={mOut} onSeek={seekWall} onMarkAt={markAt} timezone={timezone} guide={guide} onProgramme={(g) => void markProgramme(g)} />

      {previewing && mIn && mOut && (
        <ClipPreview recId={rec.id} name={name.trim() || `${rec.name} ${timecode(mIn.wall, fps, timezone)}`} from={mIn} to={mOut} fps={fps} timezone={timezone} sound={kind === 'audio'} onClose={() => setPreviewing(false)} />
      )}
      <GuidePanel recId={rec.id} programmes={programmes} timezone={timezone} canExtract={can.extract(user)} admin={can.has(user, 'schedule.edit')} act={act} />
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
        {/* Each still only over what is recorded: cut clean where a recording starts or ends. */}
        {strip.out.flatMap((w) => {
          const a = w - strip.d / 2;
          const b = w + strip.d / 2;
          return spans
            .map((s) => [Math.max(a, s.from), Math.min(b, s.to, now - 2000)] as const)
            .filter(([l, r]) => r > l)
            .map(([l, r]) => {
              // The frame shown: the tile's middle when recorded, else near the start of what is (steady).
              const at = recorded(w) && w >= l && w < r ? w : l + Math.min((r - l) / 2, 1000);
              return (
                <div key={`${w}-${l}`} className="sc-tile" style={{ left: `${x(l)}%`, width: `${((r - l) / span) * 100}%` }}>
                  <Still url={`/api/recorders/${recId}/frame.jpg?at=${Math.round(at)}&h=54${strip.d >= 2000 ? '&fast=1' : ''}`} />
                </div>
              );
            });
        })}
      </div>
      <div className="sc-wave">
        <svg viewBox="0 0 1000 30" preserveAspectRatio="none" aria-hidden="true">
          {/* The sound only over what is recorded. */}
          <clipPath id={`sc-wave-clip-${recId}`}>
            {spans
              .filter((s) => s.to > start && s.from < end)
              .map((s) => {
                const l = Math.max(0, ((s.from - start) / span) * 1000);
                const r = Math.min(1000, ((Math.min(s.to, now) - start) / span) * 1000);
                return <rect key={s.from} x={l} y={0} width={Math.max(0, r - l)} height={30} />;
              })}
          </clipPath>
          <path d={wavePath} fill="currentColor" clipPath={`url(#sc-wave-clip-${recId})`} />
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
function MarkTag({ kind, mark, fps, timezone, onType, onClear, onMark }: { kind: 'in' | 'out'; mark: Mark | null; fps: number; timezone: string; onType: (s: string) => void; onClear: () => void; onMark: () => void }) {
  const shown = mark ? timecode(mark.wall, fps, timezone) : '';
  const [text, setText] = useState(shown);
  useEffect(() => setText(shown), [shown]);
  return (
    <span className={`sc-mtag ${kind} ${mark ? 'set' : ''}`} data-tip={kind === 'in' ? t('In point (type a timecode)') : t('Out point (type a timecode)')}>
      {/* A click: the point here, at the playhead. */}
      <button type="button" className="sc-mtag-k" data-tip={kind === 'in' ? t('Set the in point here') : t('Set the out point here')} data-keys={kind === 'in' ? 'I' : 'O'} onClick={onMark}>
        {kind === 'in' ? 'IN' : 'OUT'}
      </button>
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
 * The Extract button, which becomes the extractions' progress: a ring filling with the running
 * one's %, and how many wait. Over it, a bubble with each extraction (bar, time left, cancel; a
 * clip ready: play it in Clips, download it, put it away). A click extracts the marks (queued
 * after the others); without marks, it opens the bubble.
 */
function ExtractButton({ jobs, recId, canCut, onCut, kind }: { jobs: Job[]; recId: string; canCut: boolean; onCut: () => void; kind: 'video' | 'audio' | 'image' }) {
  const toast = useApp((s) => s.toast);
  const [open, setOpen] = useState(false);
  const closing = useRef(0);
  // When each job was first seen running and how far: the time left from its pace.
  const seen = useRef(new Map<string, { at: number; p: number }>());
  const list = jobs.filter((j) => j.recorder === recId || j.state === 'queued' || j.state === 'running');
  const running = list.find((j) => j.state === 'running');
  const waiting = list.filter((j) => j.state === 'queued').length;
  const ready = list.filter((j) => j.state === 'done').length;
  const busy = !!running || waiting > 0;
  const pct = running ? Math.round(running.progress * 100) : 0;
  useEffect(() => {
    if (list.length === 0) setOpen(false);
  }, [list.length]);
  const show = () => {
    clearTimeout(closing.current);
    if (list.length) setOpen(true);
  };
  const hide = () => {
    clearTimeout(closing.current);
    closing.current = window.setTimeout(() => setOpen(false), 250);
  };
  const remove = async (j: Job) => {
    try {
      await api('DELETE', `/api/jobs/${j.id}`);
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  const left = (j: Job) => {
    const now = Date.now();
    const s0 = seen.current.get(j.id);
    if (!s0) {
      seen.current.set(j.id, { at: now, p: j.progress });
      return '';
    }
    if (j.progress <= s0.p + 0.02) return '';
    const secs = ((now - s0.at) / 1000 / (j.progress - s0.p)) * (1 - j.progress);
    return secs < 60 ? t('{n} s left', { n: Math.max(1, Math.round(secs)) }) : t('{n} min left', { n: Math.round(secs / 60) });
  };
  return (
    <div className="sc-xwrap" onMouseEnter={show} onMouseLeave={hide}>
      <button
        className={`ctl primary sc-xbtn kind-${kind} ${busy ? 'busy' : ''}`}
        data-tip={busy ? undefined : kind === 'audio' ? t('Extract the sound') : kind === 'image' ? t('Extract the picture under the playhead') : t('Extract the video')}
        data-keys={busy ? undefined : '⏎'}
        disabled={!canCut && list.length === 0}
        onClick={() => (canCut ? onCut() : setOpen((o) => !o))}
      >
        {busy ? (
          <>
            <span className="sc-xring" style={{ ['--p' as string]: `${running ? pct : 0}%` }}>
              <Scissors size={11} />
            </span>
            <span className="sc-xpct">{running ? `${pct} %` : t('Waiting')}</span>
            {waiting > (running ? 0 : 1) && <span className="sc-xwait">+{running ? waiting : waiting - 1}</span>}
          </>
        ) : (
          <>
            <Scissors size={17} />
            {/* What it makes, in the mode's colour. */}
            <span className="sc-xkind">{kind === 'audio' ? t('Sound') : kind === 'image' ? t('Image') : t('Video')}</span>
            {ready > 0 && <span className="sc-xready" />}
          </>
        )}
      </button>
      {open && list.length > 0 && (
        <div className="sc-xpop" onMouseEnter={show} onMouseLeave={hide}>
          {list.map((j) => (
            <div key={j.id} className={`sc-xrow ${j.state}`}>
              <strong title={j.name}>{j.name}</strong>
              {j.state === 'running' && (
                <>
                  <span className="sc-xrow-r mono">
                    {Math.round(j.progress * 100)} %{left(j) && ` · ${left(j)}`}
                  </span>
                  <button className="icon-btn ghost" data-tip={t('Cancel')} onClick={() => void remove(j)}>
                    <X size={14} />
                  </button>
                  <span className="sc-xbar">
                    <span style={{ transform: `translateX(${j.progress * 100 - 100}%)` }} />
                  </span>
                </>
              )}
              {j.state === 'queued' && (
                <>
                  <span className="sc-xrow-r muted">{t('Waiting')}</span>
                  <button className="icon-btn ghost" data-tip={t('Cancel')} onClick={() => void remove(j)}>
                    <X size={14} />
                  </button>
                </>
              )}
              {j.state === 'done' && (
                <>
                  <span className="sc-xrow-r ok">
                    <Check size={13} /> {t('Ready')}
                  </span>
                  <span className="sc-xrow-acts">
                    <button className="icon-btn ghost" data-tip={t('See it in Clips')} onClick={() => void navigate('/clips')}>
                      <Play size={14} />
                    </button>
                    {j.clip && (
                      <a className="icon-btn ghost" data-tip={t('Download')} href={`/api/clips/${j.clip}/download`}>
                        <Download size={14} />
                      </a>
                    )}
                    <button className="icon-btn ghost" data-tip={t('Put away')} onClick={() => void remove(j)}>
                      <X size={14} />
                    </button>
                  </span>
                </>
              )}
              {j.state === 'error' && (
                <>
                  <span className="sc-xrow-r err" title={t(j.message)}>
                    {t('Failed')}
                  </span>
                  <button className="icon-btn ghost" data-tip={t('Put away')} onClick={() => void remove(j)}>
                    <X size={14} />
                  </button>
                  <span className="sc-xerr">{t(j.message)}</span>
                </>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** Steps of the ruler, ms. */
const TICKS = [1000, 2000, 5000, 10_000, 30_000, 60_000, 120_000, 300_000, 600_000, 1_800_000, 3_600_000, 7_200_000, 10_800_000, 21_600_000];

function Timeline(props: { rec: Recorder; pos: Mark | null; playing: boolean; fps: number; span: number; setSpan: (ms: number) => void; spans: Span[]; clips: Clip[]; mIn: Mark | null; mOut: Mark | null; onSeek: (wall: number) => void; onMarkAt: (which: 'in' | 'out', wall: number) => void; timezone: string; guide: Programme[]; onProgramme: (g: Programme) => void }) {
  const { rec, pos, playing, fps, span, setSpan, spans, mIn, mOut, onSeek, onMarkAt, timezone, guide, onProgramme } = props;
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
      // Room after the live for the playhead to stay at 3/4 of the view.
      const hi = now + sp * 0.3 - sp;
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
  // Before the frame is painted: the playhead held at 3/4 never shows a step aside.
  useLayoutEffect(() => {
    if (!pos || scrubbing.current) return;
    const moved = lastWall.current !== pos.wall;
    lastWall.current = pos.wall;
    if (!moved) return;
    if (!follow.current && !playing) follow.current = true; // a frame moved by hand
    if (!follow.current) return;
    // Playing, going right (the live): once at 3/4 of the view, the playhead stays there and the
    // timeline goes by under it. Moved by hand past 3/4: the view puts it at 1/4.
    if (pos.wall > start + span * 0.75) setStart(clampStart(pos.wall - span * (playing ? 0.75 : 0.25), span));
    else if (pos.wall < start + span * 0.02) setStart(clampStart(pos.wall - span * 0.2, span));
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
  // Asked for a little wider than the view, on a grid of quarter views: the view going by under
  // the playhead (the live) asks again only once it has moved a quarter.
  const waveStep = span / 4;
  const waveFrom = Math.floor(start / waveStep) * waveStep - waveStep;
  const waveKey = useSettled(`${Math.round(waveFrom)}|${Math.round(span * 1.75)}|${width}`, 150);
  const liveWindow = end > now - 10_000;
  useEffect(() => {
    let dead = false;
    const [a, sp] = waveKey.split('|').map(Number);
    const load = async () => {
      try {
        const w = await api<{ from: number; to: number; levels: number[] }>('GET', `/api/recorders/${rec.id}/wave?from=${a}&to=${a + sp}&n=${Math.max(50, Math.round((width / 2) * 1.75))}`);
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
  const drag = useRef<{ mode: 'pending' | 'pan' | 'scrub' | 'in' | 'out'; x0: number; s0: number } | null>(null);
  /** An in or out point being dragged: where it is now (drawn there until it is set). */
  const [moving, setMoving] = useState<{ which: 'in' | 'out'; wall: number } | null>(null);
  const [grab, setGrab] = useState<'' | 'on-head' | 'grabbing' | 'scrubbing'>('');
  const nearHead = (clientX: number) => {
    if (!pos || !tracks.current) return false;
    const r = tracks.current.getBoundingClientRect();
    return Math.abs(clientX - (r.left + ((pos.wall - start) / span) * r.width)) <= 7;
  };

  const onAirKey = pos ? guide.find((g) => g.start <= pos.wall && pos.wall < g.end) : undefined;
  const oldest = spans.length ? Math.min(...spans.map((s) => s.from)) : now;
  // The in and out as drawn (one being dragged, where it is now).
  const inAt = moving?.which === 'in' ? moving.wall : mIn?.wall;
  const outAt = moving?.which === 'out' ? moving.wall : mOut ? mOut.wall + 1000 / fps : undefined;
  const sel = inAt !== undefined && outAt !== undefined && outAt >= inAt ? [inAt, outAt] : null;

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
        {pos && (pos.wall < start || pos.wall > end) && (
          <button type="button" className="btn small" title={t('Centre on the playhead')} onClick={centre}>
            <Crosshair size={13} /> {t('Playhead')}
          </button>
        )}
        <div className="spacer" />
        {/* How far back the loop goes. */}
        <span className="sc-backto" data-tip={t('Loop of {h} h', { h: rec.loopHours })}>
          <History size={14} />
          {t('Back to')} <b>{day(oldest, timezone, locale())} {clock(oldest, timezone)}</b>
        </span>
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
          // An in or out point: dragged along, the picture following it; set where it is let go.
          const mark = (e.target as HTMLElement).closest<HTMLElement>('.sc-mark');
          if (mark) {
            const which = mark.classList.contains('in') ? 'in' : 'out';
            drag.current = { mode: which, x0: e.clientX, s0: start };
            scrubbing.current = true;
            setGrab('scrubbing');
            setMoving({ which, wall: wallAt(e.clientX) });
            return;
          }
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
          if (d.mode === 'in' || d.mode === 'out') {
            const w = wallAt(e.clientX);
            setMoving({ which: d.mode, wall: w });
            scrubTo(w);
          } else if (d.mode === 'scrub') scrubTo(wallAt(e.clientX));
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
          if (d?.mode === 'in' || d?.mode === 'out') {
            setMoving(null);
            onMarkAt(d.mode, wallAt(e.clientX));
            return;
          }
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
          setMoving(null);
          setGrab('');
        }}
        onPointerLeave={() => !drag.current && setGrab('')}
      >
        <Layers recId={rec.id} start={start} span={span} timezone={timezone} ticks={ticks} guide={guide} onAir={onAirKey ? String(onAirKey.id) + onAirKey.start : ''} strip={strip} now={now} spans={spans} wavePath={wavePath} gaps={gaps} />
        {/* In and out both set: what is outside them is darkened. */}
        {sel && (
          <>
            <div className="sc-outside" style={{ left: 0, width: `${Math.max(0, Math.min(100, x(sel[0])))}%` }} />
            <div className="sc-outside" style={{ left: `${Math.max(0, Math.min(100, x(sel[1])))}%`, right: 0 }} />
            <div className="sc-sel" style={{ left: `${x(sel[0])}%`, width: `${x(sel[1]) - x(sel[0])}%` }} />
          </>
        )}
        {/* The in and out points, the whole height of the timeline, each dragged by its handle or its line. */}
        {inAt !== undefined && (
          <div className={`sc-mark in ${moving?.which === 'in' ? 'moving' : ''}`} style={{ left: `${x(inAt)}%` }} data-tip={t('Drag to move the in point')}>
            <b>IN</b>
            <i className="sc-grip">
              <ChevronLeft size={12} strokeWidth={3} />
            </i>
          </div>
        )}
        {outAt !== undefined && (
          <div className={`sc-mark out ${moving?.which === 'out' ? 'moving' : ''}`} style={{ left: `${x(outAt)}%` }} data-tip={t('Drag to move the out point')}>
            <b>OUT</b>
            <i className="sc-grip">
              <ChevronRight size={12} strokeWidth={3} />
            </i>
          </div>
        )}
        {pos && x(pos.wall) >= 0 && x(pos.wall) <= 100 && <div className="sc-head" style={{ left: `${x(pos.wall)}%` }} />}
      </div>
    </div>
  );
}
