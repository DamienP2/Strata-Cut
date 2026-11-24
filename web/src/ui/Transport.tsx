/*
 * The player's controls, the same in the Extraction and in the clip's preview: to the IN, a frame
 * back, fast backwards, play / pause, fast forwards, a frame on, to the OUT; and their keys.
 *
 * Keys: K or Space play / pause (playing fast: back to normal speed), L ↑ 10 s on, J ↓ 10 s back,
 * ← → a frame, F fast forwards and R fast backwards (each press: ×1.5, ×2, ×4), M the sound off
 * or on.
 */
import { useEffect, useRef } from 'react';
import { ArrowLeftToLine, ArrowRightToLine, FastForward, Pause, Play, Rewind, StepBack, StepForward } from 'lucide-react';
import { t } from '../i18n';

/** The fast speeds, in turn at each press. */
const FAST = [1.5, 2, 4];

/** The next fast speed in that direction (`dir`: 1 forwards, -1 backwards); `rate` the one now. */
export function nextFast(rate: number, dir: 1 | -1): number {
  const now = Math.sign(rate) === dir ? Math.abs(rate) : 0;
  const i = FAST.indexOf(now);
  return dir * FAST[(i + 1) % FAST.length];
}

/** What the controls and keys do. */
export interface TransportActs {
  /** Play or pause. */
  toggle: () => void;
  /** Play at normal speed. */
  normal: () => void;
  /** Seconds on (or back: negative). */
  jump: (s: number) => void;
  /** Frames on (or back). */
  step: (n: number) => void;
  /** Plays at this speed (negative: backwards). */
  fast: (rate: number) => void;
  toIn?: () => void;
  toOut?: () => void;
  mute: () => void;
}

/** A speed as said on its button (×1.5). */
const said = (r: number) => `×${String(Math.abs(r)).replace('.', ',')}`;

/** The controls' row. `rate`: the speed now (1 normal; negative backwards), `playing`. */
export function Transport({ acts, rate, playing, hasIn, hasOut }: { acts: TransportActs; rate: number; playing: boolean; hasIn: boolean; hasOut: boolean }) {
  const back = playing && rate < 0;
  const ahead = playing && rate > 1;
  return (
    <div className="sc-ctrls">
      <button className="ctl" data-tip={t('Go to the in point')} data-keys="⇧ I" disabled={!hasIn || !acts.toIn} onClick={acts.toIn}>
        <ArrowLeftToLine size={18} />
      </button>
      <button className="ctl" data-tip={t('Previous frame')} data-keys="←" onClick={() => acts.step(-1)}>
        <StepBack size={18} />
      </button>
      <button className={`ctl ${back ? 'on' : ''}`} data-tip={t('Fast backwards: ×1.5, ×2, ×4')} data-keys="R" onClick={() => acts.fast(nextFast(rate, -1))}>
        <Rewind size={18} />
        {back && <span className="ctl-rate">{said(rate)}</span>}
      </button>
      <button className="ctl play" data-tip={playing ? t('Pause') : t('Play')} data-keys="K" onClick={() => (playing && rate !== 1 ? acts.normal() : acts.toggle())}>
        {playing && rate === 1 ? <Pause size={18} /> : <Play size={18} />}
      </button>
      <button className={`ctl ${ahead ? 'on' : ''}`} data-tip={t('Fast forwards: ×1.5, ×2, ×4')} data-keys="F" onClick={() => acts.fast(nextFast(rate, 1))}>
        <FastForward size={18} />
        {ahead && <span className="ctl-rate">{said(rate)}</span>}
      </button>
      <button className="ctl" data-tip={t('Next frame')} data-keys="→" onClick={() => acts.step(1)}>
        <StepForward size={18} />
      </button>
      <button className="ctl" data-tip={t('Go to the out point')} data-keys="⇧ O" disabled={!hasOut || !acts.toOut} onClick={acts.toOut}>
        <ArrowRightToLine size={18} />
      </button>
    </div>
  );
}

/**
 * The keys of the controls, while `on`. `rate` and `playing` as now (read at each key). Keys
 * typed in a field, or with Ctrl, Cmd or Alt, are left alone; `capture`: before the page's own
 * (a window over the page).
 */
export function useTransportKeys(acts: TransportActs, now: { rate: number; playing: boolean }, on = true, capture = false) {
  const ref = useRef({ acts, now });
  ref.current = { acts, now };
  useEffect(() => {
    if (!on) return;
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement;
      if (el.closest('input, textarea, select, [contenteditable="true"]') || e.ctrlKey || e.metaKey || e.altKey) return;
      if (!capture && el.closest('.modal-back')) return;
      const { acts: a, now: n } = ref.current;
      const k = e.key.toLowerCase();
      if (k === ' ' || k === 'k') n.playing && n.rate !== 1 ? a.normal() : a.toggle();
      else if (k === 'arrowright' || k === 'arrowleft') a.step(k === 'arrowright' ? 1 : -1);
      else if (e.shiftKey) return;
      else if (k === 'l' || k === 'arrowup') a.jump(10);
      else if (k === 'j' || k === 'arrowdown') a.jump(-10);
      else if (k === 'f') a.fast(nextFast(n.playing ? n.rate : 1, 1));
      else if (k === 'r') a.fast(nextFast(n.playing ? n.rate : 1, -1));
      else if (k === 'm') a.mute();
      else return;
      e.preventDefault();
      if (capture) e.stopPropagation();
    };
    addEventListener('keydown', onKey, capture);
    return () => removeEventListener('keydown', onKey, capture);
  }, [on, capture]);
}
