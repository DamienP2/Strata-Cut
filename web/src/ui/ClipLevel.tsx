/*
 * Extraction: the sound's level (heard, and in the clip). A button in the transport (a small
 * meter, the level) opens it: how the level is (as in the Streamer), the meters, the fader, and
 * the wand that sets it by itself.
 */
import { useEffect, useRef, useState } from 'react';
import { Wand2 } from 'lucide-react';
import type { AudioOut } from '../audio';
import { locale, t } from '../i18n';
import { useApp } from '../store';
import { SILENT, TARGET, Verdict } from './Verdict';

export const LEVEL_MIN = -20;
export const LEVEL_MAX = 12;

/** "+4,5 dB", "−3,0 dB", "0,0 dB". */
export const dbText = (g: number) => `${g > 0 ? '+' : g < 0 ? '−' : ''}${Math.abs(g).toLocaleString(locale(), { minimumFractionDigits: 1, maximumFractionDigits: 1 })} dB`;

const FLOOR = -60;
const dbOf = (p: number) => (p > 0 ? Math.max(FLOOR, 20 * Math.log10(p)) : FLOOR);
const share = (db: number) => Math.max(0, Math.min(1, (db - FLOOR) / -FLOOR));
const colour = (db: number) => (db > -6 ? 'var(--live)' : db > -18 ? 'var(--warn)' : 'var(--pvw)');

/** The player's two channels, as heard (after the level), drawn on the bars given (height or width). */
function useBars(out: AudioOut | null, bars: React.RefObject<(HTMLElement | null)[]>, dir: 'v' | 'h', holds?: React.RefObject<(HTMLElement | null)[]>) {
  useEffect(() => {
    if (!out) return;
    const level = [FLOOR, FLOOR];
    const hold = [FLOOR, FLOOR];
    const holdAt = [0, 0];
    let raf = 0;
    let last = performance.now();
    const draw = (now: number) => {
      raf = requestAnimationFrame(draw);
      if (now - last < 40) return;
      const dt = (now - last) / 1000;
      last = now;
      const peaks = out.peaks();
      for (let ch = 0; ch < 2; ch++) {
        const db = dbOf(peaks[ch]);
        level[ch] = db > level[ch] ? db : Math.max(db, level[ch] - 20 * dt);
        if (level[ch] >= hold[ch] || now - holdAt[ch] > 1500) {
          hold[ch] = level[ch];
          holdAt[ch] = now;
        }
        const b = bars.current?.[ch];
        if (b) {
          b.style[dir === 'v' ? 'height' : 'width'] = `${share(level[ch]) * 100}%`;
          b.style.background = colour(level[ch]);
        }
        const h = holds?.current?.[ch];
        if (h) {
          h.style.left = `${share(hold[ch]) * 100}%`;
          h.style.opacity = hold[ch] > FLOOR + 1 ? '0.85' : '0';
        }
      }
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, [out, bars, dir, holds]);
}

/**
 * `measure`: the loudest peak of the part marked (before the level), dB; null: none to measure.
 * Asked again as `measureKey` changes, while open.
 */
export function ClipLevel({ out, gain, onGain, measure, measureKey }: { out: AudioOut | null; gain: number; onGain: (g: number) => void; measure: () => Promise<number | null>; measureKey: string }) {
  const toast = useApp((s) => s.toast);
  const [open, setOpen] = useState(false);
  const [peak, setPeak] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  // Opened above the button, toward where there is room.
  const [side, setSide] = useState<'left' | 'right'>('right');
  const wrap = useRef<HTMLDivElement>(null);
  const mini = useRef<(HTMLElement | null)[]>([]);
  const bars = useRef<(HTMLElement | null)[]>([]);
  const holds = useRef<(HTMLElement | null)[]>([]);
  useBars(out, mini, 'v');
  useBars(open ? out : null, bars, 'h', holds);
  const measured = useRef(measure);
  measured.current = measure;
  useEffect(() => {
    if (!open) return;
    let dead = false;
    void measured.current().then((p) => !dead && setPeak(p), () => !dead && setPeak(null));
    return () => {
      dead = true;
    };
  }, [open, measureKey]);
  // Closed by a click elsewhere or Escape.
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => !wrap.current?.contains(e.target as Node) && setOpen(false);
    const key = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.stopPropagation();
      setOpen(false);
    };
    document.addEventListener('pointerdown', away, true);
    document.addEventListener('keydown', key, true);
    return () => {
      document.removeEventListener('pointerdown', away, true);
      document.removeEventListener('keydown', key, true);
    };
  }, [open]);
  const set = (g: number) => onGain(Math.max(LEVEL_MIN, Math.min(LEVEL_MAX, Math.round(g * 2) / 2)));
  // The wand: the part's peaks brought to the target.
  const wand = async () => {
    setBusy(true);
    try {
      const p = await measure();
      setPeak(p);
      if (p === null || p <= SILENT) {
        toast(t('This track is silent: nothing to set'), 'info');
        return;
      }
      set(TARGET - p);
    } catch {
      toast(t('This track is silent: nothing to set'), 'info');
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="xl-wrap" ref={wrap}>
      <button type="button" className={`xl-btn ${gain !== 0 ? 'mod' : ''} ${open ? 'open' : ''}`} aria-expanded={open} data-tip={open ? undefined : t('Sound level of the clip')} onClick={() => {
          // Toward the right edge of the button unless it would leave the centre column.
          const r = wrap.current?.getBoundingClientRect();
          const col = wrap.current?.closest('.sc-center')?.getBoundingClientRect();
          setSide(r && r.right - 340 < (col?.left ?? 0) ? 'left' : 'right');
          setOpen(!open);
        }}>
        <span className="xl-mini" aria-hidden="true">
          {[0, 1].map((ch) => (
            <span key={ch}>
              <i ref={(el) => void (mini.current[ch] = el)} />
            </span>
          ))}
        </span>
        <span className="xl-label">{t('Level')}</span> <b>{dbText(gain)}</b>
      </button>
      {open && (
        <div className={`xl-pop ${side}`} role="dialog" aria-label={t('Sound level of the clip')} onKeyDown={(e) => e.key !== 'Escape' && e.stopPropagation()}>
          <div className="xl-head">
            <span>{t('Sound level of the clip')}</span>
            {peak !== null && <Verdict peak={peak + gain} />}
            <span className="spacer" />
            <button type="button" className="xl-zero" disabled={gain === 0} onClick={() => set(0)}>
              0 dB
            </button>
          </div>
          <div className="xl-meters" aria-hidden="true">
            {['L', 'R'].map((c, ch) => (
              <div key={c} className="xl-row">
                <span>{c}</span>
                <span className="xl-bar">
                  <i ref={(el) => void (bars.current[ch] = el)} />
                  <em ref={(el) => void (holds.current[ch] = el)} />
                </span>
              </div>
            ))}
            <div className="xl-marks">
              {[-48, -36, -24, -12, -6, 0].map((m) => (
                <span key={m} style={{ left: `${share(m) * 100}%` }}>
                  {m}
                </span>
              ))}
            </div>
          </div>
          <div className="xl-fader">
            <span>{LEVEL_MIN}</span>
            <input type="range" min={LEVEL_MIN} max={LEVEL_MAX} step={0.5} value={gain} aria-label={t('Level')} onChange={(e) => set(Number(e.target.value))} onDoubleClick={() => set(0)} />
            <span>+{LEVEL_MAX}</span>
            <button type="button" className="pg-tool" disabled={busy} data-tip={t('Set the level by itself')} aria-label={t('Set the level by itself')} onClick={() => void wand()}>
              <Wand2 size={12} />
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
