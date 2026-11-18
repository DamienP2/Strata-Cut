// The sound of a player: the <video> goes through Web Audio so that we can
// measure it (VU-meters, before the volume), set its volume, and play short
// grains when the playhead moves by hand (scrubbing).

export class AudioOut {
  readonly ctx: AudioContext;
  /** Everything heard goes here first (measured), then through the volume. */
  private readonly bus: GainNode;
  private readonly volume: GainNode;
  private readonly meters: AnalyserNode[];
  private readonly data: Float32Array<ArrayBuffer>;
  private grainSource: AudioBufferSourceNode | null = null;
  private vol = 1;
  private muted = false;

  constructor(media: HTMLMediaElement) {
    this.ctx = new AudioContext();
    this.bus = this.ctx.createGain();
    this.volume = this.ctx.createGain();
    const split = this.ctx.createChannelSplitter(2);
    this.meters = [0, 1].map(() => {
      const a = this.ctx.createAnalyser();
      a.fftSize = 1024;
      return a;
    });
    this.data = new Float32Array(1024);
    this.ctx.createMediaElementSource(media).connect(this.bus);
    this.bus.connect(split);
    split.connect(this.meters[0], 0);
    split.connect(this.meters[1], 1);
    this.bus.connect(this.volume).connect(this.ctx.destination);
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
