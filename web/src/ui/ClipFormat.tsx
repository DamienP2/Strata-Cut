/*
 * Extraction: the format a clip is made in (by its kind: video, sound or picture), each with its
 * weight for the clip as it is marked; the one chosen beside the extraction's button, in orange
 * when it would not fit in the space the clips have left.
 */
import { useEffect, useRef, useState } from 'react';
import { ChevronDown } from 'lucide-react';
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
  { id: 'master', kind: 'video', name: 'MP4 · H.264 master', detail: 'The quality kept, very close to the source', size: (b) => [bytes(b.bps * 1.2 + 256e3 * tracks(b), b.seconds), bytes(b.bps * 2 + 256e3 * tracks(b), b.seconds)] },
  { id: 'web', kind: 'video', name: 'MP4 · H.264 web', detail: '1080p · 6 Mb/s · AAC 160 kb/s', size: (b) => [bytes(6e6 + 160e3, b.seconds)] },
  { id: 'light', kind: 'video', name: 'MP4 · H.264 light', detail: '720p · 2.5 Mb/s · AAC 128 kb/s', size: (b) => [bytes(2.5e6 + 128e3, b.seconds)] },
  // ProRes 422: about 117 Mb/s in 1080 at 29.97, by the pixels and the pictures a second.
  { id: 'prores', kind: 'video', name: 'MOV · ProRes 422', detail: 'For editing · PCM sound', size: (b) => [bytes((117e6 * pixels(b)) / (1920 * 1080) * (fps(b) / 29.97) + 1536e3 * tracks(b), b.seconds)] },
  { id: 'copy', kind: 'video', name: 'MPEG-TS · copy', detail: 'Not encoded again, almost at once · cut on a key picture', size: (b) => [bytes(b.bps, b.seconds)] },
  { id: 'm4a', kind: 'audio', name: 'M4A · AAC 256 kb/s', detail: 'Every track', size: (b) => [bytes(256e3 * tracks(b), b.seconds)] },
  { id: 'mp3', kind: 'audio', name: 'MP3 · 192 kb/s', detail: 'The first track', size: (b) => [bytes(192e3, b.seconds)] },
  { id: 'wav', kind: 'audio', name: 'WAV · 48 kHz 16 bits', detail: 'Every track', size: (b) => [bytes(1536e3 * tracks(b), b.seconds)] },
  { id: 'jpg', kind: 'image', name: 'JPEG', detail: 'High quality', size: (b) => [pixels(b) * 0.25] },
  { id: 'png', kind: 'image', name: 'PNG', detail: 'Lossless', size: (b) => [pixels(b) * 1.3] },
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

/** The format menu and the chosen one's weight. */
export function ClipFormat({ kind, value, onChange, basis, free }: { kind: ClipKind; value: string; onChange: (id: string) => void; basis: Basis; free: number | null }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => !box.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener('pointerdown', away);
    return () => window.removeEventListener('pointerdown', away);
  }, [open]);
  const list = FORMATS.filter((f) => f.kind === kind);
  const cur = list.find((f) => f.id === value) ?? list[0];
  const est = cur.size(basis);
  const over = free !== null && est[est.length - 1] > free;
  return (
    <span className="cf" ref={box}>
      <button type="button" className="cf-btn" aria-haspopup="listbox" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        {t(cur.name)}
        <ChevronDown size={13} />
      </button>
      <span className={`cf-est ${over ? 'over' : ''}`} data-tip={over ? t('More than the free space of the clips ({free})', { free: say(free ?? 0) }) : undefined}>
        <b>{kind === 'image' || basis.seconds > 0 ? sizeText(est) : '—'}</b>
        <small>{t('estimated')}</small>
      </span>
      {open && (
        <div className="cf-menu" role="listbox">
          {list.map((f) => (
            <button
              key={f.id}
              type="button"
              role="option"
              aria-selected={f.id === cur.id}
              className={f.id === cur.id ? 'on' : ''}
              onClick={() => {
                onChange(f.id);
                setOpen(false);
              }}
            >
              <b>{t(f.name)}</b>
              <small>{t(f.detail)}</small>
              <em>{kind === 'image' || basis.seconds > 0 ? sizeText(f.size(basis)) : ''}</em>
            </button>
          ))}
        </div>
      )}
    </span>
  );
}
