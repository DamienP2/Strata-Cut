/* The kinds of sources, as Système › Sources groups them and the Control Room filters them. */
import { Cable, Globe, Grid3x3, MonitorPlay, Network, RadioTower } from 'lucide-react';
import type { Protocol, Recorder } from '../types';

export interface SourceKind {
  key: string;
  label: string;
  icon: React.ReactNode;
  /** A new source of this kind is of its first protocol. */
  protocols: Protocol[];
}

export const SOURCE_KINDS: SourceKind[] = [
  { key: 'card', label: 'SDI / HDMI inputs', icon: <Cable size={14} />, protocols: ['decklink'] },
  { key: 'ndi', label: 'NDI', icon: <Network size={14} />, protocols: ['ndi'] },
  { key: 'net', label: 'Network streams', icon: <RadioTower size={14} />, protocols: ['srt', 'rtmp', 'udp', 'rtp'] },
  { key: 'web', label: 'Online videos', icon: <MonitorPlay size={14} />, protocols: ['web', 'hls'] },
  { key: 'page', label: 'Web pages', icon: <Globe size={14} />, protocols: ['page'] },
  { key: 'pattern', label: 'Test patterns', icon: <Grid3x3 size={14} />, protocols: ['pattern'] },
];

/** The kind of a source (its key). */
export const kindOf = (r: Pick<Recorder, 'protocol'>) => SOURCE_KINDS.find((k) => k.protocols.includes(r.protocol))?.key ?? '';
