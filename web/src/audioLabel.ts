/*
 * A sound track's label, from what its stream says: its language (ISO 639-2, three letters) and
 * its kind (the DVB audio type). Short (a strip, a list: "FR", "VO", "AD") and in full ("Français",
 * "Version originale", "Audiodescription · Français").
 *
 * The codes reserved for local use (qaa–qtz) carry, on European television, the original version
 * (qaa) and the audio description (qad).
 */
import { locale, t } from './i18n';

/** Bibliographic codes (fre, ger…) to their terminology code (fra, deu…): the one browsers name. */
const TERMINOLOGY: Record<string, string> = {
  alb: 'sqi', arm: 'hye', baq: 'eus', bur: 'mya', chi: 'zho', cze: 'ces', dut: 'nld', fre: 'fra', geo: 'kat', ger: 'deu',
  gre: 'ell', ice: 'isl', mac: 'mkd', mao: 'mri', may: 'msa', per: 'fas', rum: 'ron', slo: 'slk', tib: 'bod', wel: 'cym',
};

/** Terminology codes to their two letters (shown short). */
const TWO: Record<string, string> = {
  ara: 'AR', bel: 'BE', bos: 'BS', bre: 'BR', bul: 'BG', cat: 'CA', ces: 'CS', cos: 'CO', cym: 'CY', dan: 'DA', deu: 'DE',
  ell: 'EL', eng: 'EN', est: 'ET', eus: 'EU', fas: 'FA', fin: 'FI', fra: 'FR', gle: 'GA', glg: 'GL', heb: 'HE', hin: 'HI',
  hrv: 'HR', hun: 'HU', hye: 'HY', ind: 'ID', isl: 'IS', ita: 'IT', jpn: 'JA', kat: 'KA', kor: 'KO', lat: 'LA', lav: 'LV',
  lit: 'LT', ltz: 'LB', mkd: 'MK', msa: 'MS', mlt: 'MT', nld: 'NL', nor: 'NO', oci: 'OC', pol: 'PL', por: 'PT', ron: 'RO',
  rus: 'RU', slk: 'SK', slv: 'SL', spa: 'ES', sqi: 'SQ', srp: 'SR', swe: 'SV', tha: 'TH', tur: 'TR', ukr: 'UK', vie: 'VI',
  zho: 'ZH',
};

/** A language's name in the page's language (none: not known). */
function nameOf(code: string): string | null {
  try {
    const n = new Intl.DisplayNames(locale() ?? 'en', { type: 'language', fallback: 'none' }).of(code);
    return n ? n[0].toUpperCase() + n.slice(1) : null;
  } catch {
    return null;
  }
}

export interface TrackLabel {
  short: string;
  full: string;
}

/** The label of a track: `lang` as its stream says it ("" when not said), `kind` its DVB type. */
export function trackLabel(lang: string | undefined, kind?: string): TrackLabel {
  const code = TERMINOLOGY[(lang ?? '').trim().toLowerCase()] ?? (lang ?? '').trim().toLowerCase();
  let label: TrackLabel;
  if (code === 'qaa') label = { short: t('OV'), full: t('Original version') };
  else if (code === 'qad') label = { short: t('AD'), full: t('Audio description') };
  else if (code === 'mul') label = { short: t('Multi'), full: t('Several languages') };
  else if (code === 'zxx') label = { short: t('No speech'), full: t('No speech (ambience, international sound)') };
  else if (code === '' || code === 'und' || code === 'mis') label = { short: '—', full: t('Language not said') };
  else if (/^q[a-t][a-z]$/.test(code)) label = { short: code.toUpperCase(), full: t('Local use ({code})', { code: code.toUpperCase() }) };
  else label = { short: TWO[code] ?? code.toUpperCase(), full: nameOf(code) ?? code.toUpperCase() };
  // The kind the stream says, over the language.
  const lang2 = label.short === '—' ? '' : label.full;
  if (kind === 'ad' && code !== 'qad') return { short: t('AD'), full: [t('Audio description'), lang2].filter(Boolean).join(' · ') };
  if (kind === 'intl') return { short: t('Intl'), full: [t('International sound'), lang2].filter(Boolean).join(' · ') };
  if (kind === 'hi') return { short: label.short, full: `${label.full} · ${t('for the hard of hearing')}` };
  return label;
}
