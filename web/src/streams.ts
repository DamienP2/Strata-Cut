/*
 * Streaming: the destinations (Système › Destinations) and the lives under way (Streamer),
 * as the server gives them; asked again when they change, and every second for the bitrates.
 */
import { useEffect, useState } from 'react';
import { api, useChanged } from './store';

export type DestProtocol = 'rtmp' | 'rtmps' | 'srt';

export interface Destination {
  id: string;
  name: string;
  protocol: DestProtocol;
  /** The server (admins; operators only when it is handed to them). */
  url?: string | null;
  /** Its host: all an operator sees of the address. */
  host: string;
  /** Admins: masked when set. */
  key?: string;
  keyLive: boolean;
  latency?: number;
  passphrase?: string;
  encode: 'copy' | 'h264';
  height: number;
  videoKbps?: number | null;
  audioKbps?: number;
  /** Encoded again: a variable rate, else constant (CBR). */
  vbr?: boolean;
  exposeUrl: boolean;
  exposeBitrate: boolean;
  /** Sends the clean source, rather than its programme (image, overlay, sound). */
  clean?: boolean;
}

export interface StreamInfo {
  destination: string;
  source: string;
  /** Who started it (account id, name). */
  byId?: string;
  by?: string;
  started: number;
  state: 'connecting' | 'live' | 'retrying' | 'failed';
  message: string;
  note: string;
  liveSince: number | null;
  duration: number | null;
  bytes: number;
  kbps: number;
  /** kb/s, one a second, the last 10 minutes. */
  history: number[];
  /** Its destination's server cut the link: when the live stops unless it takes it again (server ms). */
  stopAt?: number | null;
}

export const MASKED = '••••••••';

export const blankDestination = (): Destination => ({
  id: '',
  name: '',
  protocol: 'rtmps',
  url: '',
  host: '',
  key: '',
  keyLive: false,
  latency: 500,
  passphrase: '',
  encode: 'copy',
  height: 0,
  videoKbps: 6000,
  audioKbps: 160,
  exposeUrl: false,
  exposeBitrate: false,
});

export function useDestinations(): [Destination[] | null, () => void] {
  const [list, setList] = useState<Destination[] | null>(null);
  const load = () => void api<Destination[]>('GET', '/api/destinations').then(setList, () => setList([]));
  useEffect(load, []);
  useChanged('destinations', load);
  return [list, load];
}

/** The lives under way, every `ms` (and at once when one starts or stops). */
/** The lives under way (`failed`: also those that could not start, with why). */
export function useStreams(ms = 1000, failed = false): StreamInfo[] {
  const [list, setList] = useState<StreamInfo[]>([]);
  const load = () => void api<StreamInfo[]>('GET', '/api/streams').then((all) => setList(failed ? all : all.filter((s) => s.state !== 'failed')), () => {});
  useEffect(() => {
    load();
    const i = setInterval(load, ms);
    return () => clearInterval(i);
  }, [ms]);
  useChanged('streams', load);
  return list;
}

/** "6,1 Mb/s". */
export function mbps(kbps: number, locale?: string): string {
  return `${(kbps / 1000).toLocaleString(locale, { maximumFractionDigits: 1, minimumFractionDigits: 1 })} Mb/s`;
}

/** "1,9 Go". */
export function volume(bytes: number, locale: string | undefined, fr: boolean): string {
  const gb = bytes / 1e9;
  return gb >= 1 ? `${gb.toLocaleString(locale, { maximumFractionDigits: 1 })} ${fr ? 'Go' : 'GB'}` : `${Math.round(bytes / 1e6).toLocaleString(locale)} ${fr ? 'Mo' : 'MB'}`;
}

/** "00:42:13". */
export function clockOf(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const p = (n: number) => String(n).padStart(2, '0');
  return `${p(Math.floor(s / 3600))}:${p(Math.floor((s % 3600) / 60))}:${p(s % 60)}`;
}
