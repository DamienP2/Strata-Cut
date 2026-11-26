/*
 * Santé: whom a message of the journal is about (a source by its name, the Streamer, the clips,
 * the network, the storage, the server…), what it says without that name, and what to do.
 */
import { Download, HardDrive, Network, Radio, Scissors, Server, Users } from 'lucide-react';
import { t } from '../i18n';
import { logText } from '../logText';
import type { Recorder } from '../types';
import { SOURCE_KINDS, kindOf } from './sourceKinds';

export type WhoKind = 'source' | 'streamer' | 'clips' | 'network' | 'storage' | 'users' | 'update' | 'server';

export interface Who {
  kind: WhoKind;
  /** Its name (a source's), else the kind's. */
  label: string;
  /** The source's protocol (its icon). */
  rec?: Recorder;
}

export interface Said {
  who: Who;
  /** What it says, in the interface's language, without the name in front. */
  text: string;
  /** What it means, or what to do. */
  hint?: string;
}

const KIND_LABEL: Record<Exclude<WhoKind, 'source'>, string> = {
  streamer: 'Streamer',
  clips: 'Clips',
  network: 'Network',
  storage: 'Storage',
  users: 'Connections',
  update: 'Updates',
  server: 'Server',
};

/** Its icon. */
export function WhoIcon({ who, size = 12 }: { who: Who; size?: number }) {
  if (who.kind === 'source') {
    const k = who.rec ? SOURCE_KINDS.find((x) => x.key === kindOf(who.rec!)) : undefined;
    return k ? <span className="lw-ico">{k.icon}</span> : <Radio size={size} />;
  }
  const I = { streamer: Radio, clips: Scissors, network: Network, storage: HardDrive, users: Users, update: Download, server: Server }[who.kind];
  return <I size={size} />;
}

/** What to do, by what the message says. */
const HINTS: [RegExp, string][] = [
  [/packets lost by this server|lost before FFmpeg|system dropped \d+ UDP|network buffers were full/, 'The server or its network card could not keep up: see Resources (processor, busiest core).'],
  [/processor over its limit/, 'When it is saturated, the UDP sources lose packets and their picture breaks up: see Resources.'],
  [/preview is \d+ s behind|preview drops/, 'The recording is whole; the server is busy: see Resources.'],
  [/Error opening input files|Input\/output error|Connection refused|cannot open/, 'The source does not open: check that the sender sends to this server.'],
  [/nothing received/, 'Nothing arrives: check the address, the network card or the multicast subscription.'],
  [/the server stopped unexpectedly|did not stop by itself/, 'Possible causes: memory full, power cut, machine stopped by force.'],
  [/not sent to/, 'Check the address, the port or the firewall of the file server.'],
  [/the source or the network, not this server/, 'The stream arrives damaged: the source or the network before this server.'],
  [/no signal on the card/, 'Check the cable and the device feeding the card.'],
  [/guard: .* refused/, 'The server was at its limit: stop a source or a live first.'],
];

/** Whom the message is about, and what it says. */
export function said(message: string, target: string, recs: Recorder[]): Said {
  const m = message.trim();
  const hint = HINTS.find(([re]) => re.test(m))?.[1];
  const of = (kind: Exclude<WhoKind, 'source'>): Who => ({ kind, label: t(KIND_LABEL[kind]) });
  // A source: its name in front ("<name>: …", "decklink <name>: …").
  const byName = [...recs].sort((a, b) => b.name.length - a.name.length);
  for (const r of byName) {
    const pre = [`${r.name}: `, `decklink ${r.name}: `, `ndi ${r.name}: `].find((p) => m.startsWith(p));
    if (!pre) continue;
    const text = strip(logText(m), r.name);
    return { who: { kind: 'source', label: r.name, rec: r }, text, hint: hint && t(hint) };
  }
  let who: Who;
  if (/^(streaming to |programme: )|\bsends .+ to |\bstops the stream to |^player: /.test(m) || target === 'streams' || target === 'program') who = of('streamer');
  else if (/^clip |^send/.test(m) || target === 'sends' || target === 'clips') who = of('clips');
  else if (/^network card|network cards dropped|system dropped \d+ UDP|^reception problems at the same moments/.test(m)) who = of('network');
  else if (/^disk space|loop was deleted|^storage/.test(m) || target === 'storage') who = of('storage');
  else if (/ signed in from |^sso|password/.test(m) || target === 'auth' || target === 'sso' || target === 'users') who = of('users');
  else if (/^update: /.test(m) || target === 'update') who = of('update');
  else who = of('server');
  // "guard: recording <name> refused": its source.
  const guard = m.match(/^guard: (?:recording|stream) (.+?) refused/);
  const rec = guard ? recs.find((r) => r.name === guard[1]) : undefined;
  if (rec) who = { kind: 'source', label: rec.name, rec };
  return { who, text: cap(logText(m)), hint: hint && t(hint) };
}

/** Without the name in front ("<name> : …" once translated), its first letter up. */
function strip(text: string, name: string) {
  for (const p of [`${name} : `, `${name}: `, `decklink ${name} : `, `ndi ${name} : `]) {
    if (text.startsWith(p)) return cap(text.slice(p.length));
  }
  return cap(text);
}

const cap = (s: string) => (s ? s[0].toUpperCase() + s.slice(1) : s);
