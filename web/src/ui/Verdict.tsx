/*
 * A good level, for whom is not a sound engineer (the Streamer's tracks, the Extraction's level):
 * peaks between GOOD_LOW and GOOD_HIGH (dBFS); above LOUD, too loud; at CLIP, saturated. The
 * wand puts the peaks at TARGET.
 */
import { t } from '../i18n';

export const GOOD_LOW = -20;
export const GOOD_HIGH = -6;
export const LOUD = -3;
export const CLIP = -1;
export const TARGET = -9;
export const SILENT = -55;

export type VerdictKind = 'silent' | 'low' | 'good' | 'loud' | 'clip';
export const verdictOf = (peak: number): VerdictKind => (peak <= SILENT ? 'silent' : peak >= CLIP ? 'clip' : peak > LOUD ? 'loud' : peak < GOOD_LOW ? 'low' : 'good');
const VERDICT_LABEL: Record<VerdictKind, string> = { silent: 'Silent', low: 'Too low', good: 'Good', loud: 'Too loud', clip: 'Saturated' };

/** Says how a level is, in words and a colour. */
export function Verdict({ peak }: { peak: number }) {
  const v = verdictOf(peak);
  return <span className={`pg-verdict ${v}`}>{t(VERDICT_LABEL[v])}</span>;
}
