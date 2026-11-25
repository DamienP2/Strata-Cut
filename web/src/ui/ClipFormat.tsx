/*
 * Extraction: the format a clip is made in (by its kind: video, sound or picture), each with its
 * weight for the clip as it is marked; chosen in the extraction's window, with where it goes.
 */
import { useEffect, useState } from 'react';
import { Check } from 'lucide-react';
import { autoFor, type UsableTarget } from '../sends';
import { api } from '../store';
import { lang, locale, t } from '../i18n';
import { volume } from '../streams';
import type { MediaInfo } from '../types';

export type ClipKind = 'video' | 'audio' | 'image';

/** What the estimate goes by: the clip's length, the source's measured bitrate, its picture and tracks. */
export interface Basis {
  seconds: number;
  /** bits/s, the source as recorded. */
  bps: number;
  media: MediaInfo | null;
}

interface Format {
  id: string;
  /** On the button. */
  short: string;
  kind: ClipKind;
  name: string;
  detail: string;
  /** Bytes: one value, or a range [low, high] (the quality kept: it depends on the picture). */
  size: (b: Basis) => number[];
}

const bytes = (bps: number, s: number) => (bps * s) / 8;
const tracks = (b: Basis) => Math.max(1, b.media?.audioTracks ?? 1);
const pixels = (b: Basis) => (b.media ? b.media.width * b.media.height : 1920 * 1080);
const fps = (b: Basis) => (b.media && b.media.fpsDen ? b.media.fpsNum / b.media.fpsDen : 25);

export const FORMATS: Format[] = [
  { id: 'master', short: 'MP4 master', kind: 'video', name: 'MP4 · H.264 master', detail: 'The quality kept, very close to the source', size: (b) => [bytes(b.bps * 1.2 + 256e3 * tracks(b), b.seconds), bytes(b.bps * 2 + 256e3 * tracks(b), b.seconds)] },
  { id: 'web', short: 'MP4 web', kind: 'video', name: 'MP4 · H.264 web', detail: '1080p · 6 Mb/s · AAC 160 kb/s', size: (b) => [bytes(6e6 + 160e3, b.seconds)] },
  { id: 'light', short: 'MP4 light', kind: 'video', name: 'MP4 · H.264 light', detail: '720p · 2.5 Mb/s · AAC 128 kb/s', size: (b) => [bytes(2.5e6 + 128e3, b.seconds)] },
  // ProRes 422: about 117 Mb/s in 1080 at 29.97, by the pixels and the pictures a second.
  { id: 'prores', short: 'ProRes', kind: 'video', name: 'MOV · ProRes 422', detail: 'For editing · PCM sound', size: (b) => [bytes((117e6 * pixels(b)) / (1920 * 1080) * (fps(b) / 29.97) + 1536e3 * tracks(b), b.seconds)] },
  { id: 'copy', short: 'TS copy', kind: 'video', name: 'MPEG-TS · copy', detail: 'Not encoded again, almost at once · cut on a key picture', size: (b) => [bytes(b.bps, b.seconds)] },
  { id: 'm4a', short: 'M4A', kind: 'audio', name: 'M4A · AAC 256 kb/s', detail: 'Every track', size: (b) => [bytes(256e3 * tracks(b), b.seconds)] },
  { id: 'mp3', short: 'MP3', kind: 'audio', name: 'MP3 · 192 kb/s', detail: 'The first track', size: (b) => [bytes(192e3, b.seconds)] },
  { id: 'wav', short: 'WAV', kind: 'audio', name: 'WAV · 48 kHz 16 bits', detail: 'Every track', size: (b) => [bytes(1536e3 * tracks(b), b.seconds)] },
  { id: 'jpg', short: 'JPEG', kind: 'image', name: 'JPEG', detail: 'High quality', size: (b) => [pixels(b) * 0.25] },
  { id: 'png', short: 'PNG', kind: 'image', name: 'PNG', detail: 'Lossless', size: (b) => [pixels(b) * 1.3] },
];

export const DEFAULT_FORMAT: Record<ClipKind, string> = { video: 'master', audio: 'm4a', image: 'jpg' };

const fr = () => lang() === 'fr';
const say = (b: number) => (b < 1e6 ? `${Math.max(1, Math.round(b / 1e3))} ${fr() ? 'ko' : 'kB'}` : volume(b, locale(), fr()));
/** "≈ 148 Mo", "≈ 226–315 Mo". */
export function sizeText(r: number[]): string {
  if (r.length > 1) {
    const [a, b] = [say(r[0]), say(r[1])];
    const unit = b.split(' ')[1];
    return `≈ ${a.split(' ')[1] === unit ? a.split(' ')[0] : a}–${b}`;
  }
  return `≈ ${say(r[0])}`;
}

/** The free space of the clips' disk (bytes; null: not known), looked at again every minute. */
export function useClipsFree(): number | null {
  const [free, setFree] = useState<number | null>(null);
  useEffect(() => {
    let on = true;
    const get = () => void api<{ free: number | null }>('GET', '/api/clips/space').then((r) => on && setFree(r.free), () => {});
    get();
    const i = window.setInterval(get, 60_000);
    return () => {
      on = false;
      window.clearInterval(i);
    };
  }, []);
  return free;
}

/** The chosen format's weight for the clip ("" when the clip has no length yet), and whether it is more than the space left. */
export function weightOf(kind: ClipKind, value: string, basis: Basis, free: number | null): { text: string; over: boolean } {
  const list = FORMATS.filter((f) => f.kind === kind);
  const cur = list.find((f) => f.id === value) ?? list[0];
  const est = cur.size(basis);
  return { text: kind === 'image' || basis.seconds > 0 ? sizeText(est) : '', over: free !== null && est[est.length - 1] > free };
}

/** The extraction window's choices: the clip's name, its format (each with its weight; the kind
 * is chosen in the Extraction), where else it goes (Extraits always, unsaid; the places chosen;
 * those it goes to by itself, shown ticked), each by its name and its folder. */
export function ExtractChoices(props: {
  name: string;
  placeholder: string;
  onName: (v: string) => void;
  kind: ClipKind;
  format: string;
  onFormat: (id: string) => void;
  basis: Basis;
  places: UsableTarget[];
  source: string;
  sendTo: string[];
  onSendTo: (ids: string[]) => void;
}) {
  const { kind, basis, places, source, sendTo } = props;
  const list = FORMATS.filter((f) => f.kind === kind);
  const cur = list.find((f) => f.id === props.format) ?? list[0];
  const shown = places.filter((p) => p.usable || autoFor(p, source));
  return (
    <div className="xc">
      <label className="xc-field">
        <span className="xc-lab">{t('Name')}</span>
        <input value={props.name} placeholder={props.placeholder} onChange={(e) => props.onName(e.target.value)} />
      </label>
      <div className="xc-field">
        <span className="xc-lab">{t('Format')}</span>
        <div className="xc-list" role="radiogroup">
          {list.map((f) => (
            <button key={f.id} type="button" role="radio" aria-checked={f.id === cur.id} className={`xc-fmt ${f.id === cur.id ? 'on' : ''}`} onClick={() => props.onFormat(f.id)}>
              <span className="xc-dot" />
              <b>{t(f.name)}</b>
              <small>{t(f.detail)}</small>
              <em>{kind === 'image' || basis.seconds > 0 ? sizeText(f.size(basis)) : ''}</em>
            </button>
          ))}
        </div>
      </div>
      {shown.length > 0 && (
      <div className="xc-field">
        <span className="xc-lab">{t('Send to')}</span>
        <div className="xc-list">
          {shown.map((p) => {
            const auto = autoFor(p, source);
            const on = auto || sendTo.includes(p.id);
            return (
              <button key={p.id} type="button" role="checkbox" aria-checked={on} disabled={auto} className={`xc-dst ${on ? 'on' : ''} ${auto ? 'lock' : ''}`} data-tip={auto ? t('Every clip of this source goes there by itself') : undefined} onClick={() => props.onSendTo(on ? sendTo.filter((x) => x !== p.id) : [...sendTo, p.id])}>
                <span className="xc-cb">{on && <Check size={11} />}</span>
                <b>{p.name}</b>
                <small>{p.folder}</small>
              </button>
            );
          })}
        </div>
      </div>
      )}
    </div>
  );
}
