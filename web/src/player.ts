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

  private ms: MediaSource | null = null;
  private bufs: Buffer[] = [];
  private map = new Map<number, [number, number, number]>();
  private alive = { dead: false };
  private timer = 0;
  private reverse = 0;
  private frameCb = 0;

  constructor(
    readonly video: HTMLVideoElement,
    readonly recorder: string,
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

  /** Stalled at the end of a run: go on with the next one. */
  private onWaiting = () => {
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

  wallOf(t: number): number {
    const m = [...this.map.values()];
    let best = m[0];
    for (const s of m) {
      if (s[0] <= t) best = s;
      if (s[0] <= t && t < s[1]) break;
    }
    return best ? best[2] + (t - best[0]) * 1000 : 0;
  }

  tOf(wall: number): number | null {
    let best: [number, number, number] | null = null;
    for (const s of this.map.values()) {
      if (s[2] <= wall) best = s;
      if (s[2] <= wall && wall < s[2] + (s[1] - s[0]) * 1000) break;
    }
    if (!best) return null;
    return best[0] + (wall - best[2]) / 1000;
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
    this.bufs = [];
    const ms = new MediaSource();
    this.ms = ms;
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
    const near = this.wallOf(this.t);
    const runs = await this.fetchInfo(near - 20_000, near + 60_000);
    const same = runs.find((x) => x.id === r.id);
    if (same) this.merge(same);
    if (this.live) {
      const end = this.run?.video?.segs.at(-1)?.[2] ?? (this.bufs[0] ? Math.max(...[...this.bufs[0].segs.values()].map((s) => s[2])) : 0);
      const lastEnd = Math.max(...(this.bufs[0] ? [...this.bufs[0].segs.values()].map((s) => s[2]) : [end]));
      await this.fill(lastEnd - 6, 8);
      // Stay close to the edge.
      if (!this.video.paused && lastEnd - (this.video.currentTime + this.base) > 10) this.video.currentTime = lastEnd - 4 - this.base;
    }
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
  async seekT(t: number, play = false) {
    if (!this.run) return;
    const first = this.run.video?.offset ?? t;
    const segs = this.bufs[0] ? [...this.bufs[0].segs.values()] : [];
    const last = segs.length ? Math.max(...segs.map((s) => s[2])) : t;
    t = this.snap(Math.min(Math.max(t, first), last - 1 / this.fps));
    this.t = t;
    this.onFrame(this.wallOf(t), t);
    await this.fill(t);
    if (this.alive.dead) return;
    // A hair after the frame's start so that this very frame is shown.
    this.video.currentTime = t - this.base + 0.001;
    if (play) await this.video.play().catch(() => {});
  }

  step(frames: number) {
    this.stopReverse();
    this.video.pause();
    void this.seekT(this.t + frames / this.fps);
  }

  play(rate = 1) {
    this.stopReverse();
    this.live = false;
    this.video.playbackRate = rate;
    void this.video.play().catch(() => {});
  }

  pause() {
    this.stopReverse();
    this.video.pause();
    void this.seekT(this.t);
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
    await this.seekWall(now - 8000, true);
  }
}
