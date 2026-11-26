// The sound of a player: the <video> goes through Web Audio so that we can
// measure it (VU-meters, before the volume), set its volume, and play short
// grains when the playhead moves by hand (scrubbing).

/** Two channels to two (factors: left from left, left from right, right from left, right from right). */
export type Matrix = [number, number, number, number];
const asMatrix = (m: number | Matrix): Matrix => (typeof m === 'number' ? [m, 0, 0, m] : m);

/** A sound's two channels (a mono one: on both) to the two out, each at its factor. */
class Router {
  readonly input: GainNode;
  readonly output: ChannelMergerNode;
  private readonly gains: GainNode[];
  constructor(
    private readonly ctx: AudioContext,
    m: Matrix,
  ) {
    this.input = ctx.createGain();
    this.input.channelCount = 2;
    this.input.channelCountMode = 'explicit';
    this.input.channelInterpretation = 'speakers';
    const split = ctx.createChannelSplitter(2);
    this.output = ctx.createChannelMerger(2);
    this.input.connect(split);
    this.gains = m.map((v) => {
      const g = ctx.createGain();
      g.gain.value = v;
      return g;
    });
    split.connect(this.gains[0], 0).connect(this.output, 0, 0);
    split.connect(this.gains[1], 1).connect(this.output, 0, 0);
    split.connect(this.gains[2], 0).connect(this.output, 0, 1);
    split.connect(this.gains[3], 1).connect(this.output, 0, 1);
  }

  set(m: Matrix) {
    this.gains.forEach((g, i) => g.gain.setTargetAtTime(m[i], this.ctx.currentTime, 0.015));
  }

  disconnect() {
    this.input.disconnect();
    this.output.disconnect();
  }
}

export class AudioOut {
  readonly ctx: AudioContext;
  /** Everything heard goes here first (measured), then through the volume. */
  private readonly bus: GainNode;
  /** The element's own sound, on its channels at its level (the Streamer's tracks, the Cut's level). */
  private readonly main: Router;
  /** Other sound tracks heard with it (the Streamer's tracks on), each on its channels at its level. */
  private readonly others = new Map<HTMLMediaElement, { src: MediaElementAudioSourceNode; route: Router }>();
  private readonly volume: GainNode;
  /** The Streamer's programme as heard: its level (with the image's fade) and its channels. */
  private readonly program: GainNode;
  private readonly matrix: GainNode[];
  private readonly meters: AnalyserNode[];
  private readonly data: Float32Array<ArrayBuffer>;
  private grainSource: AudioBufferSourceNode | null = null;
  private vol = 1;
  private muted = false;

  /** `gain`: the volume it starts at (muted: 0), set at once: not a moment of sound before. */
  constructor(media: HTMLMediaElement, gain = 1) {
    this.ctx = new AudioContext();
    this.bus = this.ctx.createGain();
    this.main = new Router(this.ctx, [1, 0, 0, 1]);
    this.volume = this.ctx.createGain();
    this.volume.gain.value = gain;
    const split = this.ctx.createChannelSplitter(2);
    this.meters = [0, 1].map(() => {
      const a = this.ctx.createAnalyser();
      a.fftSize = 2048; // 43 ms at 48 kHz: the meters, read 30 times a second, see every peak
      return a;
    });
    this.data = new Float32Array(2048);
    this.ctx.createMediaElementSource(media).connect(this.main.input);
    this.main.output.connect(this.bus);
    this.bus.connect(split);
    split.connect(this.meters[0], 0);
    split.connect(this.meters[1], 1);
    // bus → programme level → channels (left/right from left/right) → volume → out.
    this.program = this.ctx.createGain();
    const split2 = this.ctx.createChannelSplitter(2);
    const merge = this.ctx.createChannelMerger(2);
    this.matrix = [0, 1, 2, 3].map(() => this.ctx.createGain());
    this.bus.connect(this.program).connect(split2);
    // matrix: [left from left, left from right, right from left, right from right]
    split2.connect(this.matrix[0], 0).connect(merge, 0, 0);
    split2.connect(this.matrix[1], 1).connect(merge, 0, 0);
    split2.connect(this.matrix[2], 0).connect(merge, 0, 1);
    split2.connect(this.matrix[3], 1).connect(merge, 0, 1);
    this.setChannels('lr');
    merge.connect(this.volume).connect(this.ctx.destination);
  }

  /** The element's own sound: its level (a factor), or its channels each at its factor. */
  setMain(m: number | Matrix) {
    this.main.set(asMatrix(m));
  }

  /** The other tracks heard with it, each at its level or on its channels: those not given are left. */
  setOthers(levels: Map<HTMLMediaElement, number | Matrix>) {
    for (const [el, o] of this.others) {
      if (levels.has(el)) continue;
      o.route.disconnect();
      o.src.disconnect();
      this.others.delete(el);
    }
    for (const [el, m] of levels) {
      let o = this.others.get(el);
      if (!o) {
        const src = this.ctx.createMediaElementSource(el);
        const route = new Router(this.ctx, asMatrix(m));
        src.connect(route.input);
        route.output.connect(this.bus);
        o = { src, route };
        this.others.set(el, o);
      }
      o.route.set(asMatrix(m));
    }
  }

  /** The programme's level (a factor), reached in `ramp` seconds (a fade) or at once. */
  setProgram(gain: number, ramp = 0) {
    const g = this.program.gain;
    const now = this.ctx.currentTime;
    g.cancelScheduledValues(now);
    if (ramp > 0.05) {
      g.setValueAtTime(g.value, now);
      g.linearRampToValueAtTime(gain, now + ramp);
    } else {
      g.setTargetAtTime(gain, now, 0.015);
    }
  }

  /** The channels heard: as they are, the left or the right on both, or both mixed. */
  setChannels(mode: 'lr' | 'l' | 'r' | 'mono') {
    const m = { lr: [1, 0, 0, 1], l: [1, 0, 1, 0], r: [0, 1, 0, 1], mono: [0.5, 0.5, 0.5, 0.5] }[mode];
    this.matrix.forEach((g, i) => g.gain.setTargetAtTime(m[i], this.ctx.currentTime, 0.015));
  }

  /** Browsers start sound only after a click or a key. */
  resume() {
    if (this.ctx.state === 'suspended') void this.ctx.resume();
  }

  set(volume: number, muted: boolean) {
    this.vol = volume;
    this.muted = muted;
    this.volume.gain.setTargetAtTime(muted ? 0 : volume * volume, this.ctx.currentTime, 0.015);
  }

  get level() {
    return this.muted ? 0 : this.vol;
  }

  /** Peak of each channel over the last ~20 ms, 0..1. */
  peaks(): [number, number] {
    return this.meters.map((a) => {
      a.getFloatTimeDomainData(this.data);
      let p = 0;
      for (let i = 0; i < this.data.length; i++) {
        const v = Math.abs(this.data[i]);
        if (v > p) p = v;
      }
      return p;
    }) as [number, number];
  }

  /** Plays `dur` seconds of `buf` from `at`, with short fades (scrub sound). */
  grain(buf: AudioBuffer, at: number, dur: number) {
    this.resume();
    if (at < 0 || at >= buf.duration) return;
    this.grainSource?.stop();
    const src = this.ctx.createBufferSource();
    src.buffer = buf;
    const env = this.ctx.createGain();
    const now = this.ctx.currentTime;
    const fade = Math.min(0.006, dur / 4);
    env.gain.setValueAtTime(0, now);
    env.gain.linearRampToValueAtTime(1, now + fade);
    env.gain.setValueAtTime(1, now + dur - fade);
    env.gain.linearRampToValueAtTime(0, now + dur);
    src.connect(env).connect(this.bus);
    src.start(now, at, dur);
    this.grainSource = src;
    src.onended = () => {
      if (this.grainSource === src) this.grainSource = null;
    };
  }

  close() {
    this.grainSource?.stop();
    void this.ctx.close();
  }
}

/** Decodes proxy audio fragments (init + fragment) and keeps the last ones. */
export class GrainCache {
  private init = new Map<string, Promise<ArrayBuffer | null>>();
  private bufs = new Map<string, Promise<AudioBuffer | null>>();
  constructor(private readonly ctx: AudioContext) {}

  get(initUrl: string, segUrl: string): Promise<AudioBuffer | null> {
    let p = this.bufs.get(segUrl);
    if (!p) {
      p = (async () => {
        let i = this.init.get(initUrl);
        if (!i) {
          i = fetch(initUrl, { credentials: 'same-origin' }).then((r) => (r.ok ? r.arrayBuffer() : null)).catch(() => null);
          this.init.set(initUrl, i);
        }
        const [head, body] = await Promise.all([i, fetch(segUrl, { credentials: 'same-origin' }).then((r) => (r.ok ? r.arrayBuffer() : null)).catch(() => null)]);
        if (!head || !body) return null;
        const whole = new Uint8Array(head.byteLength + body.byteLength);
        whole.set(new Uint8Array(head), 0);
        whole.set(new Uint8Array(body), head.byteLength);
        try {
          return await this.ctx.decodeAudioData(whole.buffer);
        } catch {
          return null;
        }
      })();
      this.bufs.set(segUrl, p);
      // About a minute of sound around the playhead.
      if (this.bufs.size > 16) this.bufs.delete(this.bufs.keys().next().value!);
    }
    return p;
  }
}
