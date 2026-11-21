// Plays a recorder's loop in a <video> through MediaSource: the proxy
// fragments of one run (one continuous recording), video and audio in two
// buffers, each shifted so that the media time is the source time minus `base`.
// The source time `t` of a frame is the same in the proxy and in the original
// segments: the clip is cut on exactly the frame shown.
import type { MediaInfo } from './types';
import { fpsOf } from './tc';

type Seg = [number, number, number]; // n, t0, t1
interface TrackInfo {
  offset: number;
  codec: string | null;
  segs: Seg[];
}
export interface RunInfo {
  id: string;
  live: boolean;
  media: MediaInfo | null;
  from: number;
  to: number;
  map: [number, number, number][]; // t0, t1, wall0
  video: TrackInfo | null;
  audio: TrackInfo | null;
  /** The other sound tracks the preview made (their fragments numbered as the first's). */
  extraAudio?: number;
}

class Buffer {
  sb: SourceBuffer;
  queue: Promise<void> = Promise.resolve();
  appended = new Set<number>();
  segs = new Map<number, Seg>();
  constructor(
    sb: SourceBuffer,
    readonly url: (file: string) => string,
  ) {
    this.sb = sb;
  }
  private wait(): Promise<void> {
    if (!this.sb.updating) return Promise.resolve();
    return new Promise((r) => this.sb.addEventListener('updateend', () => r(), { once: true }));
  }
  /** Queued so that two appends never overlap. */
  run(f: () => void): Promise<void> {
    this.queue = this.queue.then(async () => {
      await this.wait();
      try {
        f();
      } catch {
        return;
      }
      await this.wait();
    });
    return this.queue;
  }
  async append(n: number | 'init', signal: { dead: boolean }) {
    if (n !== 'init') {
      if (this.appended.has(n)) return;
      this.appended.add(n);
    }
    const name = n === 'init' ? 'init.mp4' : `p${String(n).padStart(8, '0')}.m4s`;
    let data: ArrayBuffer;
    try {
      const res = await fetch(this.url(name), { credentials: 'same-origin' });
      if (!res.ok) throw new Error(String(res.status));
      data = await res.arrayBuffer();
    } catch {
      if (n !== 'init') this.appended.delete(n);
      return;
    }
    if (signal.dead) return;
    await this.run(() => this.sb.appendBuffer(data));
  }
  /** Frees what is far from t (media time). */
  evict(m: number, keepBefore: number, keepAfter: number, base: number) {
    const b = this.sb.buffered;
    if (!b.length) return;
    const start = b.start(0);
    const end = b.end(b.length - 1);
    if (m - keepBefore > start) void this.run(() => this.sb.remove(start, m - keepBefore));
    if (m + keepAfter < end) void this.run(() => this.sb.remove(m + keepAfter, end));
    for (const n of [...this.appended]) {
      const s = this.segs.get(n);
      if (s && (s[2] - base < m - keepBefore || s[1] - base > m + keepAfter)) this.appended.delete(n);
    }
  }
}

/** A run the browser can play: its preview has a picture already, and a few seconds of it. */
/** Live: how far behind the newest fragment it plays (s): a time delay. Smooth before near: a
 * few seconds behind, within what is recorded (the playhead stays on the stills), never
 * waiting for the next fragment. It grows at each stall (a server or a network slower than
 * that), and comes back down slowly once all goes well (taken at the next jump: the speed
 * never changes). */
const LIVE_DELAY = 7.0;
const LIVE_DELAY_MIN = 6.0;
const LIVE_DELAY_MAX = 15.0;

/**
 * A tick every `ms` that the browser does not slow down when the tab is in the background (a
 * page's own timers run once a minute at most after a few minutes hidden: the live then stopped
 * following its fragments, played what it had, and jumped): a worker's timer, which is not held
 * back. A page timer when a worker cannot be made.
 */
function steadyTicker(ms: number, fn: () => void): () => void {
  try {
    const src = `setInterval(() => postMessage(0), ${ms});`;
    const url = URL.createObjectURL(new Blob([src], { type: 'text/javascript' }));
    const w = new Worker(url);
    URL.revokeObjectURL(url);
    w.onmessage = fn;
    return () => w.terminate();
  } catch {
    const i = window.setInterval(fn, ms);
    return () => clearInterval(i);
  }
}

const playable = (r: RunInfo) => !!r.video?.codec && r.video.segs.length > 0 && r.to - r.from > 4000;
/** The newest run that can be played (else the newest at all). */
function newest(runs: RunInfo[]): RunInfo | null {
  const by = (list: RunInfo[]) => list.reduce<RunInfo | null>((a, x) => (!a || x.to > a.to ? x : a), null);
  return by(runs.filter(playable)) ?? by(runs);
}

export class LoopPlayer {
  run: RunInfo | null = null;
  base = 0;
  fps = 25;
  /** The source time of the frame shown. */
  t = 0;
  /** The sound track heard (0: the first). */
  audioTrack = 0;
  live = false;
  /** Called on each frame shown: (server time ms, source time s). */
  onFrame: (wall: number, t: number) => void = () => {};
  /** Called when the run changes (new media). */
  onRun: (run: RunInfo | null) => void = () => {};
  /** Called when a frame is shown by hand (paused): to hear it. */
  onScrub: (t: number) => void = () => {};

  private bufs: Buffer[] = [];
  private audioBuf: Buffer | null = null;
  private map = new Map<number, [number, number, number]>();
  private alive = { dead: false };
  private stopTicker: () => void = () => {};
  private reverse = 0;
  private frameCb = 0;
  /** Live: when the picture last moved (server ms), to start again if it stays stuck. */
  private moved = 0;
  private lastTime = -1;
  /** Live: the delay held now (s), and when it last stalled (server ms). */
  private delay = LIVE_DELAY;
  private stalledAt = 0;
  /** When it was last moved (server ms): the waiting just after is loading, not a stall. */
  private seekedAt = 0;
  private refreshing = false;
  /** What the player did lately (the live's first minute is reported if it goes back). */
  private trace: string[] = [];
  private liveSince = 0;
  private lastCT = -1;
  private reported = false;
  /** Seeks under way: until the picture is put where asked, where it plays is that place (the
   * video's own time is still the old one, 0 on a new recording). */
  private seeking = 0;
  private ticks = 0;

  constructor(
    readonly video: HTMLVideoElement,
    readonly recorder: string,
    /** The server's clock (the browser's may be off: the live edge is the server's). */
    readonly now: () => number = Date.now,
  ) {
    const tick = () => {
      if (this.alive.dead) return;
      const v = this.video as HTMLVideoElement & { requestVideoFrameCallback?: (cb: (now: number, meta: { mediaTime: number }) => void) => number };
      if (v.requestVideoFrameCallback) {
        this.frameCb = v.requestVideoFrameCallback((_n, meta) => {
          if (!this.video.paused) this.shown(meta.mediaTime + this.base);
          tick();
        });
      }
    };
    tick();
    this.video.addEventListener('timeupdate', this.onTime);
    this.video.addEventListener('waiting', this.onWaiting);
    // Live: every half second (the new fragments, the distance to the edge); else every 2 s.
    this.stopTicker = steadyTicker(500, () => {
      this.ticks++;
      this.watchBack();
      if (this.live || this.ticks % 4 === 0) void this.refresh();
    });
    document.addEventListener('visibilitychange', this.onVisible);
  }

  /** Back on the page while live: looked at at once, and put back at the live only if it fell
   * behind meanwhile (the browser may have held it while hidden). Not moved when it kept up:
   * a jump each time the page came back showed as a jolt. */
  private onVisible = () => {
    if (document.visibilityState === 'visible' && this.live && !this.alive.dead) void this.refresh();
  };

  destroy() {
    this.alive.dead = true;
    this.stopTicker();
    document.removeEventListener('visibilitychange', this.onVisible);
    clearInterval(this.reverse);
    const v = this.video as HTMLVideoElement & { cancelVideoFrameCallback?: (h: number) => void };
    v.cancelVideoFrameCallback?.(this.frameCb);
    this.video.removeEventListener('timeupdate', this.onTime);
    this.video.removeEventListener('waiting', this.onWaiting);
    this.video.pause();
    this.video.removeAttribute('src');
    this.video.load();
  }

  private onTime = () => {
    if (!this.video.paused && !(this.video as { requestVideoFrameCallback?: unknown }).requestVideoFrameCallback) this.shown(this.video.currentTime + this.base);
    void this.fill();
  };

  /** Stalled: load what is missing here (no timeupdate comes while waiting); at the end of a run, go on with the next one. */
  private onWaiting = () => {
    void this.fill();
    // Live: it waits for a fragment not there yet: more delay from now on, and asked at once.
    this.note('waiting');
    if (this.live && !this.video.paused && this.now() - this.seekedAt > 4000) {
      this.delay = Math.min(LIVE_DELAY_MAX, this.delay + 1.5);
      this.stalledAt = this.now();
      void this.refresh();
    }
    const r = this.run;
    if (!r || r.live) return;
    const last = r.video?.segs.at(-1);
    if (last && this.playhead() >= last[2] - 1) void this.seekWall(r.to + 500);
  };

  private shown(t: number) {
    this.t = this.snap(t);
    this.onFrame(this.wallOf(this.t), this.t);
  }

  /** The nearest frame time. */
  snap(t: number): number {
    const g = this.run?.video?.offset ?? 0;
    return g + Math.round((t - g) * this.fps) / this.fps;
  }

  /** The map's segments in order (rebuilt when the map changes): searched in halves. */
  private sorted: [number, number, number][] | null = null;
  private segments(): [number, number, number][] {
    if (!this.sorted) this.sorted = [...this.map.values()].sort((a, b) => a[0] - b[0]);
    return this.sorted;
  }
  /** The last segment starting at or before v (by source time, or by server time); -1 if none. */
  private static before(m: [number, number, number][], v: number, k: 0 | 2): number {
    let lo = 0;
    let hi = m.length - 1;
    let found = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (m[mid][k] <= v) {
        found = mid;
        lo = mid + 1;
      } else hi = mid - 1;
    }
    return found;
  }

  wallOf(t: number): number {
    const m = this.segments();
    if (!m.length) return 0;
    const best = m[Math.max(0, LoopPlayer.before(m, t, 0))];
    return best[2] + (t - best[0]) * 1000;
  }

  tOf(wall: number): number | null {
    const m = this.segments();
    const i = LoopPlayer.before(m, wall, 2);
    if (i < 0) return null;
    return m[i][0] + (wall - m[i][2]) / 1000;
  }

  private async fetchInfo(from: number, to: number): Promise<RunInfo[]> {
    const res = await fetch(`/api/recorders/${this.recorder}/media?from=${Math.floor(from)}&to=${Math.ceil(to)}`, { credentials: 'same-origin' });
    if (!res.ok) return [];
    return ((await res.json()) as { runs: RunInfo[] }).runs;
  }

  /** Adds what the server knows about the current run. */
  private merge(r: RunInfo) {
    if (!this.run || r.id !== this.run.id) return;
    this.run.live = r.live;
    this.run.to = r.to;
    this.run.from = r.from;
    if (r.media) this.run.media = r.media;
    for (const s of r.map) this.map.set(s[0], s);
    this.sorted = null;
    const tracks = [r.video, r.audio].filter(Boolean) as TrackInfo[];
    tracks.forEach((tr, i) => {
      const b = this.bufs[i];
      if (b) for (const s of tr.segs) b.segs.set(s[0], s);
    });
  }

  private async open(r: RunInfo) {
    this.note(`open run ${r.id} live=${r.live} from=${r.from} to=${r.to}`);
    this.alive.dead = true;
    this.alive = { dead: false };
    const alive = this.alive;
    this.run = r;
    this.fps = fpsOf(r.media?.fpsNum, r.media?.fpsDen);
    this.base = Math.floor(r.video?.offset ?? 0);
    this.map = new Map(r.map.map((s) => [s[0], s]));
    this.sorted = null;
    this.bufs = [];
    this.audioBuf = null;
    const ms = new MediaSource();
    this.video.src = URL.createObjectURL(ms);
    await new Promise((res) => ms.addEventListener('sourceopen', res, { once: true }));
    if (alive.dead) return;
    for (const [name, tr] of [['video', r.video], ['audio', r.audio]] as const) {
      if (!tr?.codec) continue;
      const type = `${name}/mp4; codecs="${tr.codec}"`;
      if (!MediaSource.isTypeSupported(type)) continue;
      const sb = ms.addSourceBuffer(type);
      sb.timestampOffset = tr.offset - this.base;
      // Another sound track: its own fragments, on the first track's list.
      const k = name === 'audio' && this.audioTrack > 0 && (r.extraAudio ?? 0) >= this.audioTrack ? this.audioTrack : 0;
      const dir = k ? `audio${k}` : name;
      const b = new Buffer(sb, (f) => `/api/recorders/${this.recorder}/runs/${r.id}/${dir}/${f}`);
      for (const s of tr.segs) b.segs.set(s[0], s);
      this.bufs.push(b);
      if (name === 'audio') this.audioBuf = b;
    }
    // Every buffer exists before the first append (MediaSource accepts no new one after).
    await Promise.all(this.bufs.map((b) => b.append('init', alive)));
    this.onRun(r);
  }

  /** Loads the fragments around the source time t. */
  /** Where it plays (source time): the frame asked while a seek is under way. */
  private playhead(): number {
    return this.seeking > 0 ? this.t : this.video.currentTime + this.base;
  }

  private async fill(t = this.playhead(), ahead = 12) {
    const alive = this.alive;
    await Promise.all(
      this.bufs.map(async (b) => {
        const need = [...b.segs.values()].filter((s) => s[2] > t - 1 && s[1] < t + ahead).sort((a, c) => a[0] - c[0]);
        for (const s of need) {
          if (alive.dead) return;
          await b.append(s[0], alive);
        }
        b.evict(t - this.base, 60, 90, this.base);
      }),
    );
  }

  /** The end of what can be played (s): the newest fragment that every track has. */
  private edge(): number | null {
    let e = Infinity;
    for (const b of this.bufs) {
      let m = -Infinity;
      for (const s of b.segs.values()) if (s[2] > m) m = s[2];
      if (m === -Infinity) return null;
      e = Math.min(e, m);
    }
    return Number.isFinite(e) ? e : null;
  }

  /** Plays LIVE_DELAY behind the edge (else a few seconds behind the recording's end). */
  private async seekEdge(fallback: number) {
    const e = this.edge();
    if (e === null) return void (await this.seekWall(fallback, true));
    await this.seekT(e - this.delay, true);
  }

  /** New fragments of a live run. */
  private async refresh() {
    if (this.refreshing) return;
    this.refreshing = true;
    try {
      await this.refreshNow();
    } finally {
      this.refreshing = false;
    }
  }

  private async refreshNow() {
    const r = this.run;
    if (!r || this.alive.dead) return;
    // Live: when the source came back, the recording goes on in a new run.
    if (this.live) {
      const now = this.now();
      // The newest recording that can be played (its preview started), only if newer than this one.
      const runs = await this.fetchInfo(now - 120_000, now + 10_000);
      const latest = newest(runs);
      if (latest && latest.id !== r.id && latest.to > r.to && playable(latest)) {
        await this.open(latest);
        if (this.live) await this.seekEdge(latest.to - 4000);
        return;
      }
      // This run opened before its preview had a picture: opened again now that it has one.
      const same = runs.find((x) => x.id === r.id);
      if (same && this.bufs.length === 0 && same.video?.codec) {
        await this.open(same);
        if (this.live) await this.seekEdge(same.to - 4000);
        return;
      }
      // Stuck (no picture for 10 s while the recording goes on): the live is found again.
      const t = this.video.currentTime;
      if (t !== this.lastTime || this.video.paused) {
        this.lastTime = t;
        this.moved = now;
      } else if (now - this.moved > 10_000 && latest && latest.to > now - 30_000) {
        this.moved = now;
        void this.goLive(now);
        return;
      }
    }
    // Where it plays: the video's own time while it plays (in a tab in the background no frame is
    // drawn, the last frame shown no longer moves: a minute later the live's new fragments were
    // out of the window asked, and the live stopped for 10 s).
    const near = this.wallOf(this.video.paused || this.seeking > 0 ? this.t : this.video.currentTime + this.base);
    const runs = await this.fetchInfo(near - (this.live ? 5_000 : 20_000), near + 60_000);
    const same = runs.find((x) => x.id === r.id);
    if (same) this.merge(same);
    const edge = this.live ? this.edge() : null;
    if (edge !== null) {
      // A seek under way: it puts the picture where it goes, nothing is moved meanwhile (the
      // video's own time is the old one: from there, minutes of fragments were loaded before the
      // live's, and the live's own thrown away to make room).
      if (this.seeking > 0) return;
      const cur = this.video.currentTime + this.base;
      // Everything from where it plays to the edge (a stall gets no timeupdate to load it).
      await this.fill(cur, Math.max(4, edge - cur + 1));
      if (!this.live || this.video.paused) return;
      // LIVE_DELAY behind the edge: a little faster or slower to hold it (a jump only when far
      // behind, or stalled while the edge is further).
      // A minute without a stall: a little less delay.
      if (this.now() - this.stalledAt > 60_000 && this.delay > LIVE_DELAY_MIN) {
        this.delay = Math.max(LIVE_DELAY_MIN, this.delay - 0.25);
        this.stalledAt = this.now();
      }
      const delay = this.delay;
      const behind = edge - (this.video.currentTime + this.base);
      const stalled = this.video.readyState < HTMLMediaElement.HAVE_FUTURE_DATA;
      // Always at its own speed: a speed changed now and then to hold the delay made the browser
      // stretch the sound each time (the edge moves by whole fragments, so the speed changed
      // several times a second): crackles heard in the live only. It plays at 1 and is put back
      // LIVE_DELAY behind the edge (one jump) only when it drifted far: far behind, stalled
      // with the edge further.
      if (this.video.playbackRate !== 1) this.video.playbackRate = 1;
      // Never back: near the edge (its fragments came slower than it plays) it goes on and, at
      // worst, waits a moment for the next one. Put back LIVE_DELAY behind the edge, it showed
      // the same seconds again (the live went back 3 s a few seconds after it opened).
      if (behind > delay + 6 || (stalled && behind > delay + 1)) {
        this.note(`refresh jump: behind ${behind.toFixed(2)} delay ${delay} stalled ${stalled} edge ${edge.toFixed(2)} → ${(edge - delay).toFixed(2)}`);
        this.seekedAt = this.now();
        this.video.currentTime = edge - delay - this.base;
        void this.video.play().catch(() => {});
      }
    }
  }

  /** The proxy audio fragment holding the source time t: its URLs and start. */
  audioAt(t: number): { init: string; seg: string; t0: number } | null {
    const b = this.audioBuf;
    if (!b) return null;
    for (const s of b.segs.values()) {
      if (s[1] <= t && t < s[2]) return { init: b.url('init.mp4'), seg: b.url(`p${String(s[0]).padStart(8, '0')}.m4s`), t0: s[1] };
    }
    return null;
  }

  /** A line of what the player did, with its time since the live started. */
  private note(what: string) {
    const v = this.video;
    const buf: string[] = [];
    for (let i = 0; i < v.buffered.length; i++) buf.push(`${v.buffered.start(i).toFixed(1)}-${v.buffered.end(i).toFixed(1)}`);
    const at = this.liveSince ? ((this.now() - this.liveSince) / 1000).toFixed(1) : '-';
    this.trace.push(`${at}s ${what} ct=${v.currentTime.toFixed(2)} rs=${v.readyState} buf=[${buf.join(',')}]`);
    if (this.trace.length > 40) this.trace.shift();
  }

  /** The live went back in its first minute (by itself or put back): what led to it, sent to the
   * server's journal (Système › Santé), once per player. */
  private watchBack() {
    const ct = this.video.currentTime;
    const back = this.lastCT - ct;
    this.lastCT = ct;
    if (!this.live || this.reported || this.video.paused || back < 0.5 || !this.liveSince || this.now() - this.liveSince > 60_000) return;
    this.reported = true;
    this.note(`WENT BACK ${back.toFixed(2)} s (delay ${this.delay}, edge ${this.edge()?.toFixed(2)}, base ${this.base})`);
    const body = JSON.stringify({ page: location.pathname, ua: navigator.userAgent, back, events: this.trace });
    void fetch('/api/player/trace', { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body }).catch(() => {});
  }

  /** Shows the frame at a server time (ms). */
  async seekWall(wall: number, play = false) {
    let r = this.run;
    const inRun = r && wall >= r.from && wall < r.to + (r.live ? 10_000 : 0);
    if (!inRun) {
      const runs = await this.fetchInfo(wall - 60_000, wall + 60_000);
      // A moment at a recording's very end (its last frame) is still that recording's.
      const target = runs.find((x) => wall >= x.from && wall <= x.to) ?? runs.find((x) => x.from > wall) ?? runs.at(-1);
      if (!target) return;
      await this.open(target);
      r = this.run;
      if (wall < target.from) wall = target.from;
    } else {
      const runs = await this.fetchInfo(wall - 30_000, wall + 60_000);
      const same = runs.find((x) => x.id === r!.id);
      if (same) this.merge(same);
    }
    const t = this.tOf(wall);
    if (t === null) return;
    await this.seekT(t, play);
  }

  /** Shows the frame at a source time. */
  async seekT(t: number, play = false, scrub = true) {
    if (!this.run) return;
    const first = this.run.video?.offset ?? t;
    const segs = this.bufs[0] ? [...this.bufs[0].segs.values()] : [];
    const last = segs.length ? Math.max(...segs.map((s) => s[2])) : t;
    t = this.snap(Math.min(Math.max(t, first), last - 1 / this.fps));
    this.t = t;
    this.seekedAt = this.now();
    this.onFrame(this.wallOf(t), t);
    if (scrub && !play && this.video.paused) this.onScrub(t);
    // Playing (the live): a few seconds are enough to start, the rest comes while it plays.
    this.seeking++;
    try {
      await this.fill(t, play ? 3 : 12);
    } finally {
      this.seeking--;
    }
    if (this.alive.dead) return;
    // A recording's last frame can end a little before what its segment announces: past what the
    // browser holds, the picture would stay black; the last frame it holds instead.
    const b = this.video.buffered;
    const at = t - this.base;
    let held = false;
    for (let i = 0; i < b.length; i++) if (at >= b.start(i) && at < b.end(i)) held = true;
    if (!held && b.length) {
      let end = -1;
      for (let i = 0; i < b.length; i++) if (b.end(i) <= at + 1e-6 && b.end(i) > end) end = b.end(i);
      if (end >= 0 && at - end < 0.5) {
        t = this.snap(end - 1 / this.fps + this.base + 0.001);
        this.t = t;
        this.onFrame(this.wallOf(t), t);
      }
    }
    // A hair after the frame's start so that this very frame is shown.
    this.video.currentTime = t - this.base + 0.001;
    if (this.live) this.note(`seek ${t.toFixed(2)} play=${play}`);
    if (play) {
      void this.fill(t, 12);
      await this.video.play().catch(() => {});
    }
  }

  step(frames: number) {
    this.halt();
    void this.seekT(this.t + frames / this.fps);
  }

  play(rate = 1) {
    this.stopReverse();
    this.live = false;
    this.video.playbackRate = rate;
    void this.video.play().catch(() => {});
  }

  /** Stops playing (live included) without moving. */
  halt() {
    this.stopReverse();
    this.live = false;
    this.video.pause();
  }

  pause() {
    this.stopReverse();
    this.live = false;
    this.video.pause();
    void this.seekT(this.t, false, false);
  }

  /** Browsers do not play backwards: step back frame by frame. */
  playReverse(rate = 1) {
    this.stopReverse();
    this.live = false;
    this.video.pause();
    const every = Math.max(40, 1000 / this.fps / rate);
    const step = Math.max(1, Math.round((rate * this.fps * every) / 1000));
    this.reverse = window.setInterval(() => void this.seekT(this.t - step / this.fps), every);
  }

  private stopReverse() {
    clearInterval(this.reverse);
    this.reverse = 0;
  }

  get playing() {
    return !this.video.paused || this.reverse !== 0;
  }

  /** Hears another sound track: the run opened again on it, where it was (live, playing or not). */
  async setAudioTrack(k: number, now: number) {
    if (k === this.audioTrack) return;
    this.audioTrack = k;
    const r = this.run;
    if (!r) return;
    const t = this.playhead();
    const playing = !this.video.paused;
    const live = this.live;
    await this.open(r);
    if (live) await this.goLive(now);
    else await this.seekT(t, playing);
  }

  /** Follows the newest frames. */
  async goLive(now: number) {
    this.stopReverse();
    this.live = true;
    this.liveSince = this.now();
    this.note('goLive');
    // The newest recording, asked afresh (the one in hand may have ended since: a cut, a restart);
    // one whose preview has not started yet, or of a few seconds, is passed over.
    const runs = await this.fetchInfo(now - 120_000, now + 10_000);
    const latest = newest(runs);
    this.moved = now;
    if (!latest) return void (await this.seekWall(now - 8000, true));
    if (latest.id !== this.run?.id || this.bufs.length === 0) await this.open(latest);
    else this.merge(latest);
    if (!this.live) return;
    // LIVE_DELAY behind its newest fragment (else a few seconds behind its last segment).
    this.video.playbackRate = 1;
    await this.seekEdge(Math.max(latest.from, Math.min(now - 8000, latest.to - 4000)));
  }
}
