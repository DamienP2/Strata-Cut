// The app's own data, as the server sends it.

export type Protocol = 'udp' | 'rtp' | 'rtmp' | 'srt' | 'web';
export const PROTOCOLS: Protocol[] = ['udp', 'rtp', 'rtmp', 'srt', 'web'];

/** A recorder's settings. */
export interface Recorder {
  id: string;
  name: string;
  protocol: Protocol;
  /** udp / rtp: the address received (udp://239.1.1.1:5000); rtmp / srt as caller: the address pulled. */
  url: string;
  /** rtmp / srt: listen (the source pushes) or call (we pull). */
  mode: 'listener' | 'caller';
  /** Listening port (rtmp / srt listener). */
  port: number;
  /** rtmp listener: application/key (live/web). */
  path: string;
  /** srt: latency, ms. */
  latency: number;
  /** srt: passphrase ("••••••••" when set). */
  passphrase: string;
  /** Network interface address for multicast (udp / rtp), empty = any. */
  iface: string;
  /** web: cookies.txt of a signed-in account ("••••••••" when set). */
  cookies?: string;
  /** web: the platform found from the address (sent by the server). */
  platform?: string;
  /** Loop length, hours. */
  loopHours: number;
  /** Expected bitrate, Mb/s: the loop's space is reserved from it (0 = unknown). */
  bitrate: number;
  enabled: boolean;
}

export type RecorderState = 'off' | 'starting' | 'recording' | 'nosignal' | 'error' | 'waiting';

export interface MediaInfo {
  codec: string;
  width: number;
  height: number;
  /** Frames per second as a fraction. */
  fpsNum: number;
  fpsDen: number;
  interlaced: boolean;
  audioTracks: number;
}

/** A recorder's live state. */
export interface RecorderStatus {
  id: string;
  state: RecorderState;
  /** Since when, ms (server time). */
  since: number;
  message: string;
  media: MediaInfo | null;
  /** bits/s over the last seconds. */
  bitrate: number;
  /** The loop: oldest and newest recorded instants, ms; null when empty. */
  oldest: number | null;
  newest: number | null;
  bytes: number;
  /** The loop is shorter than asked: the disk limits removed its oldest part. */
  spaceLimited: boolean;
  /** The last bitrate measured while recording, bits/s (the loop's space follows it). */
  measured: number;
  /** The programme on air and the next one, from the source's guide (EPG). */
  onAir: Programme | null;
  next: Programme | null;
  /** Programmed: the slot recording now, and the next one. */
  slot: SlotTime | null;
  nextSlot: SlotTime | null;
  /** Reception problems in the last 10 minutes: packets lost before FFmpeg, damaged stream. */
  lost: number;
  damaged: number;
}

export interface SlotTime {
  name: string;
  start: number;
  end: number;
}

/** A programmed slot of a recorder (hours in the server's time zone). */
export interface Slot {
  id: string;
  recorder: string;
  name: string;
  /** One day, YYYY-MM-DD; empty when repeated. */
  date: string;
  /** Repeated on these days: 1 = Monday … 7 = Sunday. */
  days: number[];
  start: string;
  end: string;
  enabled: boolean;
  /** Sent by the server: the next (or current) occurrence. */
  next?: { start: number; end: number } | null;
  active?: boolean;
}

/** A programme of a source's guide (DVB EIT), server times in ms. */
export interface Programme {
  id: number;
  start: number;
  end: number;
  name: string;
  text?: string;
}

export interface Job {
  id: string;
  name: string;
  recorder: string;
  state: 'queued' | 'running' | 'done' | 'error';
  progress: number;
  message: string;
  clip: string | null;
}

export interface Clip {
  id: string;
  name: string;
  recorder: string;
  recorderName: string;
  /** First frame, ms (server time). */
  start: number;
  frames: number;
  fpsNum: number;
  fpsDen: number;
  format: string;
  size: number;
  createdAt: number;
  createdBy: string;
}
