// The app's own data, as the server sends it.

export type Protocol = 'udp' | 'rtp' | 'rtmp' | 'srt';
export const PROTOCOLS: Protocol[] = ['udp', 'rtp', 'rtmp', 'srt'];

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
  /** Loop length, hours. */
  loopHours: number;
  /** Expected bitrate, Mb/s: the loop's space is reserved from it (0 = unknown). */
  bitrate: number;
  enabled: boolean;
}

export type RecorderState = 'off' | 'starting' | 'recording' | 'nosignal' | 'error';

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
