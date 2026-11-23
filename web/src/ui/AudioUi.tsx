// VU-meters over the picture, and the volume / mute control of a player.
import { useEffect, useRef, useState, type RefObject } from 'react';
import { Volume1, Volume2, VolumeX } from 'lucide-react';
import { AudioOut } from '../audio';
import { useDisplayPref } from '../displayPrefs';
import { t } from '../i18n';

/** Wires a <video> to Web Audio with the user's volume; sound starts at the first click or key. */
export function useAudioOut(video: RefObject<HTMLVideoElement | null>, page = 'player') {
  const [out, setOut] = useState<AudioOut | null>(null);
  // Each page its own volume and mute (the Extraction's and the Streamer's headphones apart).
  const [volume, setVolume] = useDisplayPref<number>(`${page}.volume`, 0.8);
  const [muted, setMuted] = useDisplayPref<boolean>(`${page}.muted`, false);
  const now = useRef({ volume, muted });
  now.current = { volume, muted };
  useEffect(() => {
    const v = video.current;
    if (!v) return;
    // At its volume from its first sound (muted: none at all).
    const o = new AudioOut(v, now.current.muted ? 0 : now.current.volume ** 2);
    setOut(o);
    const wake = () => o.resume();
    addEventListener('pointerdown', wake, true);
    addEventListener('keydown', wake, true);
    return () => {
      removeEventListener('pointerdown', wake, true);
      removeEventListener('keydown', wake, true);
      o.close();
      setOut(null);
    };
  }, [video]);
  useEffect(() => out?.set(volume, muted), [out, volume, muted]);
  return { out, volume, setVolume, muted, setMuted };
}

const FLOOR = -60;
const TICKS = [0, -6, -12, -18, -24, -36, -48];
const dbOf = (p: number) => (p > 0 ? Math.max(FLOOR, 20 * Math.log10(p)) : FLOOR);

/** Two vertical peak meters (dBFS, peak hold), drawn over the picture. */
export function Meters({ out }: { out: AudioOut | null }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const c = canvas.current;
    if (!c || !out) return;
    const g = c.getContext('2d')!;
    const css = getComputedStyle(c);
    const color = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
    const green = color('--pvw', '#22c55e');
    const yellow = color('--warn', '#f5a524');
    const red = color('--live', '#ff3b5c');
    const level = [FLOOR, FLOOR];
    const hold = [FLOOR, FLOOR];
    const holdAt = [0, 0];
    let raf = 0;
    let last = performance.now();
    let shown = '';
    // The scale, the bars' backgrounds and L / R: drawn once per size, then copied.
    const bg = document.createElement('canvas');
    let bgKey = '';
    const barW = 7;
    const draw = (now: number) => {
      raf = requestAnimationFrame(draw);
      // 30 measures and images a second are enough for a meter (the analyser sees the last 40 ms).
      if (now - last < 32) return;
      const dt = (now - last) / 1000;
      last = now;
      const peaks = out.peaks();
      for (let ch = 0; ch < 2; ch++) {
        const db = dbOf(peaks[ch]);
        // Fast attack, ~20 dB/s fall.
        level[ch] = db > level[ch] ? db : Math.max(db, level[ch] - 20 * dt);
        if (level[ch] >= hold[ch] || now - holdAt[ch] > 1500) {
          hold[ch] = level[ch];
          holdAt[ch] = now;
        }
      }
      // Nothing drawn while nothing moves (pause, silence).
      const dpr = devicePixelRatio || 1;
      const w = c.clientWidth;
      const h = c.clientHeight;
      const key = `${w}x${h}@${dpr}`;
      const state = `${key}|${level.map((v) => v.toFixed(1))}|${hold.map((v) => v.toFixed(1))}`;
      if (state === shown) return;
      shown = state;
      const top = 6;
      const bottom = h - 16;
      const y = (db: number) => top + ((0 - db) / -FLOOR) * (bottom - top);
      const xs = [w - 2 * barW - 5, w - barW - 2];
      if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) {
        c.width = Math.round(w * dpr);
        c.height = Math.round(h * dpr);
      }
      if (bgKey !== key) {
        bgKey = key;
        bg.width = c.width;
        bg.height = c.height;
        const b = bg.getContext('2d')!;
        b.setTransform(dpr, 0, 0, dpr, 0, 0);
        b.font = '9px ui-monospace, monospace';
        b.textAlign = 'right';
        b.textBaseline = 'middle';
        for (const tk of TICKS) {
          b.fillStyle = 'rgba(255,255,255,0.55)';
          b.fillText(String(-tk), xs[0] - 4, y(tk));
          b.fillStyle = 'rgba(255,255,255,0.18)';
          b.fillRect(xs[0] - 2, Math.round(y(tk)), 2 * barW + 5, 1);
        }
        b.textAlign = 'center';
        b.textBaseline = 'alphabetic';
        for (let ch = 0; ch < 2; ch++) {
          b.fillStyle = 'rgba(255,255,255,0.08)';
          b.fillRect(xs[ch], top, barW, bottom - top);
          b.fillStyle = 'rgba(255,255,255,0.7)';
          b.fillText(ch ? 'R' : 'L', xs[ch] + barW / 2, h - 4);
        }
      }
      g.setTransform(1, 0, 0, 1, 0, 0);
      g.clearRect(0, 0, c.width, c.height);
      g.drawImage(bg, 0, 0);
      g.setTransform(dpr, 0, 0, dpr, 0, 0);
      for (let ch = 0; ch < 2; ch++) {
        const x = xs[ch];
        const seg = (from: number, to: number, col: string) => {
          if (level[ch] <= from) return;
          const a = y(Math.min(level[ch], to));
          g.fillStyle = col;
          g.fillRect(x, a, barW, y(from) - a);
        };
        seg(FLOOR, -18, green);
        seg(-18, -6, yellow);
        seg(-6, 0, red);
        if (hold[ch] > FLOOR + 1) {
          g.fillStyle = hold[ch] > -6 ? red : hold[ch] > -18 ? yellow : green;
          g.fillRect(x, Math.round(y(hold[ch])) - 1, barW, 2);
        }
      }
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, [out]);
  return <canvas ref={canvas} className="sc-meters" aria-hidden="true" />;
}

/** Mute button and volume slider. */
export function VolumeControl({ volume, setVolume, muted, setMuted }: { volume: number; setVolume: (v: number) => void; muted: boolean; setMuted: (m: boolean) => void }) {
  const Icon = muted || volume === 0 ? VolumeX : volume < 0.5 ? Volume1 : Volume2;
  return (
    <div className="sc-volume">
      <button type="button" className={`ctl ${muted ? 'on' : ''}`} data-tip={muted ? t('Sound on') : t('Mute')} data-keys="M" aria-pressed={muted} onClick={() => setMuted(!muted)}>
        <Icon size={18} />
      </button>
      <input
        type="range"
        min={0}
        max={1}
        step={0.01}
        value={muted ? 0 : volume}
        data-tip={t('Volume')}
        aria-label={t('Volume')}
        onChange={(e) => {
          setVolume(Number(e.target.value));
          if (muted) setMuted(false);
        }}
        onKeyDown={(e) => e.stopPropagation()}
      />
    </div>
  );
}
