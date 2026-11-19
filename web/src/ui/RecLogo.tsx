/* A recorder's logo (the channel's, the show's), small, next to its name. Horizontal and square
 * logos get the same weight: a horizontal one is wider and lower, a square one a little higher. */
import { useState } from 'react';
import type { Recorder } from '../types';

/** The logos' shapes once loaded (by address): shown right the next times. */
const shapes = new Map<string, 'wide' | 'square'>();

export function RecLogo({ rec, size = 'md' }: { rec?: Pick<Recorder, 'id' | 'logo'>; size?: 'sm' | 'md' }) {
  const src = rec?.logo ? `/api/recorders/${rec.id}/logo?v=${rec.logo}` : '';
  const [shape, setShape] = useState(() => shapes.get(src));
  if (!src) return null;
  return (
    <img
      className={`sc-logo ${size} ${shape ?? 'wide'}`}
      src={src}
      alt=""
      loading="lazy"
      onLoad={(e) => {
        const { naturalWidth: w, naturalHeight: h } = e.currentTarget;
        const s = w && h && w / h < 1.4 ? 'square' : 'wide';
        shapes.set(src, s);
        setShape(s);
      }}
    />
  );
}
