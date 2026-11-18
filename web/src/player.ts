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

export class LoopPlayer {
  run: RunInfo | null = null;
  base = 0;
  fps = 25;
  /** The source time of the frame shown. */
  t = 0;
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
  private timer = 0;
  private reverse = 0;
  private frameCb = 0;

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
    this.timer = window.setInterval(() => void this.refresh(), 2000);
  }

  destroy() {
    this.alive.dead = true;
    clearInterval(this.timer);
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
    const r = this.run;
    if (!r || r.live) return;
    const last = r.video?.segs.at(-1);
    if (last && this.t >= last[2] - 1) void this.seekWall(r.to + 500);
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
      const b = new Buffer(sb, (f) => `/api/recorders/${this.recorder}/runs/${r.id}/${name}/${f}`);
      for (const s of tr.segs) b.segs.set(s[0], s);
      this.bufs.push(b);
      if (name === 'audio') this.audioBuf = b;
    }
    // Every buffer exists before the first append (MediaSource accepts no new one after).
    await Promise.all(this.bufs.map((b) => b.append('init', alive)));
    this.onRun(r);
  }

  /** Loads the fragments around the source time t. */
  private async fill(t = this.video.currentTime + this.base, ahead = 12) {
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

  /** New fragments of a live run. */
  private async refresh() {
    const r = this.run;
    if (!r || this.alive.dead) return;
    // Live: when the source came back, the recording goes on in a new run.
    if (this.live) {
      const now = this.now();
      // The newest recording (the one that ends last), only if it is newer than this one.
      const runs = await this.fetchInfo(now - 120_000, now + 10_000);
      const latest = runs.reduce<RunInfo | null>((a, x) => (!a || x.to > a.to ? x : a), null);
      if (latest && latest.id !== r.id && latest.to > r.to && latest.to - latest.from > 4000) {
        await this.open(latest);
        if (this.live) await this.seekWall(latest.to - 4000, true);
        return;
      }
    }
    const near = this.wallOf(this.t);
    const runs = await this.fetchInfo(near - 20_000, near + 60_000);
    const same = runs.find((x) => x.id === r.id);
    if (same) this.merge(same);
    if (this.live && this.bufs[0]) {
      const lastEnd = Math.max(...[...this.bufs[0].segs.values()].map((s) => s[2]));
      const cur = this.video.currentTime + this.base;
      // Everything from where it plays to the edge (a stall gets no timeupdate to load it).
      await this.fill(cur, Math.max(8, lastEnd - cur + 2));
      // Stay close to the edge: too far behind, or stalled while the edge is further.
      const behind = lastEnd - (this.video.currentTime + this.base);
      const stalled = this.video.readyState < HTMLMediaElement.HAVE_FUTURE_DATA;
      if (!this.video.paused && (behind > 10 || (stalled && behind > 5))) {
        this.video.currentTime = lastEnd - 4 - this.base;
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

  /** Shows the frame at a server time (ms). */
  async seekWall(wall: number, play = false) {
    let r = this.run;
    const inRun = r && wall >= r.from && wall < r.to + (r.live ? 10_000 : 0);
    if (!inRun) {
      const runs = await this.fetchInfo(wall - 60_000, wall + 60_000);
      const target = runs.find((x) => wall >= x.from && wall < x.to) ?? runs.find((x) => x.from > wall) ?? runs.at(-1);
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
    this.onFrame(this.wallOf(t), t);
    if (scrub && !play && this.video.paused) this.onScrub(t);
    await this.fill(t);
    if (this.alive.dead) return;
    // A hair after the frame's start so that this very frame is shown.
    this.video.currentTime = t - this.base + 0.001;
    if (play) await this.video.play().catch(() => {});
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

  /** Follows the newest frames. */
  async goLive(now: number) {
    this.stopReverse();
    this.live = true;
    // The newest recording, asked afresh (the one in hand may have ended since: a cut, a restart).
    const runs = await this.fetchInfo(now - 120_000, now + 10_000);
    const latest = runs.reduce<RunInfo | null>((a, x) => (!a || x.to > a.to ? x : a), null);
    if (!latest) return void (await this.seekWall(now - 8000, true));
    if (latest.id !== this.run?.id) await this.open(latest);
    else this.merge(latest);
    if (!this.live) return;
    // A few seconds behind its last frame (never past what is recorded).
    await this.seekWall(Math.max(latest.from, Math.min(now - 8000, latest.to - 4000)), true);
  }
}
