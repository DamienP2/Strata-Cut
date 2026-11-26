/*
 * A sound track's two channels: as they are (stereo), the left or the right on both, or both
 * mixed on both; and, when both are heard, each at its own level. Streamer (each track) and Cut
 * (the clip): a button that opens the choice.
 */
import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Check, ChevronDown, Unlink } from 'lucide-react';
import type { Matrix } from '../audio';
import { t } from '../i18n';

export type Route = 'stereo' | 'left' | 'right' | 'mono';
export const ROUTES: Route[] = ['stereo', 'left', 'right', 'mono'];

/** What a track or a clip holds of its channels. */
export interface Channels {
  route: Route;
  split: boolean;
  /** Each channel's level (dB) when apart. */
  gainL: number;
  gainR: number;
}

export const STEREO: Channels = { route: 'stereo', split: false, gainL: 0, gainR: 0 };

export const routeName = (r: Route) => ({ stereo: t('Stereo'), left: t('Mono left'), right: t('Mono right'), mono: t('Mono left + right') })[r];
/** Short, under a track's name. */
export const routeShort = (r: Route, split: boolean) => (split && r === 'stereo' ? t('L | R') : { stereo: t('L · R'), left: t('MONO L'), right: t('MONO R'), mono: t('MONO L+R') }[r]);
/** Both channels heard: each may have its own level. */
export const bothHeard = (r: Route) => r === 'stereo' || r === 'mono';

/** The two channels to the two out (as `AudioOut` takes them). */
const lin = (db: number) => 10 ** (db / 20);

export function matrix(c: Partial<Channels>, gain: number): Matrix {
  const route = c.route ?? 'stereo';
  const apart = !!c.split && bothHeard(route);
  const [l, r] = apart ? [lin(c.gainL ?? 0), lin(c.gainR ?? 0)] : [lin(gain), lin(gain)];
  switch (route) {
    case 'left':
      return [l, 0, l, 0];
    case 'right':
      return [0, r, 0, r];
    case 'mono':
      return [l / 2, r / 2, l / 2, r / 2];
    default:
      return [l, 0, 0, r];
  }
}

/** A track's channels' peaks (dB, -90: silence) as they go out of it: its channels, its level(s). */
export function heard(levels: number[], c: Partial<Channels>, gain: number): [number, number] {
  const [l, r] = [levels[0] ?? -90, levels[1] ?? levels[0] ?? -90];
  const m = matrix(c, gain);
  const out = (a: number, b: number) => {
    const v = (l <= -90 ? 0 : a * lin(l)) + (r <= -90 ? 0 : b * lin(r));
    return v > 0 ? Math.max(-90, 20 * Math.log10(v)) : -90;
  };
  return [out(m[0], m[1]), out(m[2], m[3])];
}

/** Two channels in, two out: the lines say where each goes. */
export function RouteIcon({ route, size = 40 }: { route: Route; size?: number }) {
  const lines = { stereo: [[0, 0], [1, 1]], left: [[0, 0], [0, 1]], right: [[1, 0], [1, 1]], mono: [[0, 0], [0, 1], [1, 0], [1, 1]] }[route];
  const y = [6, 18];
  const used = new Set(lines.map((l) => l[0]));
  return (
    <svg className="ch-rt" width={size} height={(size * 24) / 40} viewBox="0 0 40 24" aria-hidden="true">
      {lines.map(([a, b]) => (
        <path key={`${a}${b}`} className="ln" d={`M9 ${y[a]} C20 ${y[a]} 20 ${y[b]} 31 ${y[b]}`} />
      ))}
      {[0, 1].map((i) => (
        <g key={i}>
          <circle className={`in ${used.has(i) ? 'u' : ''}`} cx="6" cy={y[i]} r="3" />
          <circle className="out" cx="34" cy={y[i]} r="3" />
        </g>
      ))}
    </svg>
  );
}

/**
 * The button and its menu. `wide`: as wide as where it stands, its menu too (Cut); else a small
 * badge (a Streamer track).
 */
export function ChannelPick({ value, onChange, wide, disabled }: { value: Pick<Channels, 'route' | 'split'>; onChange: (c: Pick<Channels, 'route' | 'split'>) => void; wide?: boolean; disabled?: boolean }) {
  const [open, setOpen] = useState(false);
  const btn = useRef<HTMLButtonElement>(null);
  const { route, split } = value;
  const mod = route !== 'stereo' || split;
  return (
    <>
      <button ref={btn} type="button" className={`ch-pick ${wide ? 'wide' : ''} ${mod ? 'mod' : ''} ${open ? 'open' : ''}`} disabled={disabled} aria-haspopup="menu" aria-expanded={open} data-tip={wide || open ? undefined : routeName(route)} onClick={() => setOpen(!open)}>
        {wide ? (
          <>
            <RouteIcon route={route} size={30} />
            <span className="ch-pick-name">
              {routeName(route)}
              {split && bothHeard(route) ? ` · ${t('L | R')}` : ''}
            </span>
          </>
        ) : (
          routeShort(route, split && bothHeard(route))
        )}
        <ChevronDown size={wide ? 13 : 10} className="ch-pick-dn" />
      </button>
      {open && btn.current && <ChannelMenu anchor={btn.current} wide={wide} value={value} onChange={onChange} onClose={() => setOpen(false)} />}
    </>
  );
}

function ChannelMenu({ anchor, wide, value, onChange, onClose }: { anchor: HTMLElement; wide?: boolean; value: Pick<Channels, 'route' | 'split'>; onChange: (c: Pick<Channels, 'route' | 'split'>) => void; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number; width: number } | null>(null);
  // Under its button (above when there is no room), within the window.
  useLayoutEffect(() => {
    const a = anchor.getBoundingClientRect();
    const width = wide ? a.width : 262;
    const h = ref.current?.offsetHeight ?? 220;
    const top = a.bottom + 4 + h > innerHeight - 8 ? Math.max(8, a.top - 4 - h) : a.bottom + 4;
    setPos({ left: Math.max(8, Math.min(a.left, innerWidth - width - 8)), top, width });
  }, [anchor, wide]);
  // Closed by a click elsewhere, Escape (before what holds it), or the window resized.
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const away = (e: PointerEvent) => !ref.current?.contains(e.target as Node) && !anchor.contains(e.target as Node) && close.current();
    const key = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.stopPropagation();
      close.current();
    };
    const resized = () => close.current();
    document.addEventListener('pointerdown', away, true);
    window.addEventListener('keydown', key, true);
    window.addEventListener('resize', resized);
    return () => {
      document.removeEventListener('pointerdown', away, true);
      window.removeEventListener('keydown', key, true);
      window.removeEventListener('resize', resized);
    };
  }, [anchor]);
  const { route, split } = value;
  return createPortal(
    <div ref={ref} className={`ch-menu ${wide ? 'wide' : ''}`} role="menu" style={pos ? { left: pos.left, top: pos.top, width: pos.width } : { visibility: 'hidden' }}>
      {ROUTES.map((r) => (
        <button key={r} type="button" role="menuitemradio" aria-checked={route === r} className={`ch-opt ${route === r ? 'on' : ''}`} onClick={() => onChange({ route: r, split: split && bothHeard(r) })}>
          <RouteIcon route={r} />
          <span>{routeName(r)}</span>
          <Check size={14} className="ch-ck" />
        </button>
      ))}
      <div className="ch-sep" />
      <button type="button" role="menuitemcheckbox" aria-checked={split} className="ch-opt" disabled={!bothHeard(route)} onClick={() => onChange({ route, split: !split })}>
        <Unlink size={13} className="ch-unlink" />
        <span>{t('Set L and R apart')}</span>
        <span className={`ch-tgl ${split && bothHeard(route) ? 'on' : ''}`} />
      </button>
    </div>,
    document.body,
  );
}
