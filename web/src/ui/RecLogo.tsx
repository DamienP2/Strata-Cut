/* A recorder's logo (the channel's, the show's), small: next to its name. */
import type { Recorder } from '../types';

export function RecLogo({ rec, size = 'md' }: { rec?: Pick<Recorder, 'id' | 'logo'>; size?: 'sm' | 'md' }) {
  if (!rec?.logo) return null;
  return <img className={`sc-logo ${size}`} src={`/api/recorders/${rec.id}/logo?v=${rec.logo}`} alt="" loading="lazy" />;
}
