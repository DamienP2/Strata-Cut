// Timecodes and times of day, in the server's time zone.

/** Frames per second as a number. */
export const fpsOf = (num?: number, den?: number) => (num && den ? num / den : 25);

const partsCache = new Map<string, Intl.DateTimeFormat>();
function fmt(tz: string): Intl.DateTimeFormat {
  let f = partsCache.get(tz);
  if (!f) {
    f = new Intl.DateTimeFormat('en-GB', { timeZone: tz, hourCycle: 'h23', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' });
    partsCache.set(tz, f);
  }
  return f;
}

type Parts = { y: number; mo: number; d: number; h: number; m: number; s: number };
/** The last seconds asked (a timecode shown 25 times a second asks the same one): formatToParts is slow. */
const recent = new Map<string, Parts>();

/** Date and time of day of an instant in a time zone. */
export function parts(ms: number, tz: string): Parts {
  const sec = Math.floor(ms / 1000);
  const key = `${tz}|${sec}`;
  const known = recent.get(key);
  if (known) return known;
  const p: Record<string, number> = {};
  for (const x of fmt(tz).formatToParts(new Date(sec * 1000))) if (x.type !== 'literal') p[x.type] = Number(x.value);
  const out = { y: p.year, mo: p.month, d: p.day, h: p.hour % 24, m: p.minute, s: p.second };
  if (recent.size >= 256) recent.delete(recent.keys().next().value!);
  recent.set(key, out);
  return out;
}

const two = (n: number) => String(n).padStart(2, '0');

/** Frame number within its second. */
export function frameOf(ms: number, fps: number) {
  const r = ((ms % 1000) + 1000) % 1000;
  return Math.min(Math.ceil(fps) - 1, Math.floor((r * fps) / 1000 + 1e-6));
}

/** "14:32:07:12" — time of day and frame. */
export function timecode(ms: number, fps: number, tz: string): string {
  const p = parts(ms, tz);
  return `${two(p.h)}:${two(p.m)}:${two(p.s)}:${two(frameOf(ms, fps))}`;
}

/** "14:32:07" */
export function clock(ms: number, tz: string): string {
  const p = parts(ms, tz);
  return `${two(p.h)}:${two(p.m)}:${two(p.s)}`;
}

/** A duration in frames as "00:00:15:08". */
export function duration(frames: number, fps: number): string {
  const f = Math.max(0, Math.round(frames));
  const perSec = Math.round(fps);
  const secs = Math.floor(f / perSec);
  return `${two(Math.floor(secs / 3600))}:${two(Math.floor(secs / 60) % 60)}:${two(secs % 60)}:${two(f % perSec)}`;
}

/** "-00:14:31" behind the live edge. */
export function behind(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  return `−${two(Math.floor(s / 3600))}:${two(Math.floor(s / 60) % 60)}:${two(s % 60)}`;
}

/**
 * A typed timecode ("14:31:52:04", "143152", "14:31") on the day of `near`
 * (the nearest such instant), as server time ms; null if it does not parse.
 */
export function parseTimecode(text: string, near: number, fps: number, tz: string): number | null {
  const digits = text.replace(/\D/g, '');
  if (!digits || digits.length > 8) return null;
  const d = digits.padEnd(8, '0');
  const [h, m, s, f] = [d.slice(0, 2), d.slice(2, 4), d.slice(4, 6), d.slice(6, 8)].map(Number);
  if (h > 23 || m > 59 || s > 59 || f >= Math.ceil(fps)) return null;
  const p = parts(near, tz);
  const nearInDay = ((p.h * 60 + p.m) * 60 + p.s) * 1000 + (((near % 1000) + 1000) % 1000);
  let delta = ((h * 60 + m) * 60 + s) * 1000 + ((f + 0.5) * 1000) / fps - nearInDay;
  if (delta > 12 * 3600_000) delta -= 24 * 3600_000;
  if (delta < -12 * 3600_000) delta += 24 * 3600_000;
  return near + delta;
}

/** "mer. 18 nov." in the interface language. */
const dayFormats = new Map<string, Intl.DateTimeFormat>();
export function day(ms: number, tz: string, locale?: string): string {
  const key = `${locale}|${tz}`;
  let f = dayFormats.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(locale, { timeZone: tz, weekday: 'short', day: 'numeric', month: 'short' });
    dayFormats.set(key, f);
  }
  return f.format(new Date(ms));
}

/** "1080i50" */
export function scanFormat(m: { height: number; fpsNum: number; fpsDen: number; interlaced: boolean } | null | undefined): string {
  if (!m) return '';
  const fps = fpsOf(m.fpsNum, m.fpsDen);
  const rate = m.interlaced ? fps * 2 : fps;
  const r = Math.abs(rate - Math.round(rate)) < 0.01 ? String(Math.round(rate)) : rate.toFixed(2);
  return `${m.height}${m.interlaced ? 'i' : 'p'}${r}`;
}

/** Bytes as "1,6 Go" / "96 Mo" (French units in French). */
export function size(bytes: number, locale?: string, fr = false): string {
  const units = fr ? ['o', 'Ko', 'Mo', 'Go', 'To'] : ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = bytes;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  return `${v.toLocaleString(locale, { maximumFractionDigits: v < 10 && i > 0 ? 1 : 0 })} ${units[i]}`;
}

/** bits/s as "12 Mb/s". */
export function bitrate(bps: number, locale?: string): string {
  if (bps >= 1e6) return `${(bps / 1e6).toLocaleString(locale, { maximumFractionDigits: bps < 1e7 ? 1 : 0 })} Mb/s`;
  return `${Math.round(bps / 1e3)} kb/s`;
}

/** Every video format of the DeckLink cards, as the server names them (GStreamer's names). */
export const DECKLINK_MODES = ['auto', 'ntsc', 'ntsc2398', 'pal', 'ntsc-p', 'pal-p', '1080p2398', '1080p24', '1080p25', '1080p2997', '1080p30', '1080i50', '1080i5994', '1080i60', '1080p50', '1080p5994', '1080p60', '720p50', '720p5994', '720p60', '1556p2398', '1556p24', '1556p25', '2kdcip2398', '2kdcip24', '2kdcip25', '2kdcip2997', '2kdcip30', '2kdcip50', '2kdcip5994', '2kdcip60', '2160p2398', '2160p24', '2160p25', '2160p2997', '2160p30', '2160p50', '2160p5994', '2160p60', 'ntsc-widescreen', 'ntsc2398-widescreen', 'pal-widescreen', 'ntsc-p-widescreen', 'pal-p-widescreen', '4kdcip2398', '4kdcip24', '4kdcip25', '4kdcip2997', '4kdcip30', '4kdcip50', '4kdcip5994', '4kdcip60', '8kp2398', '8kp24', '8kp25', '8kp2997', '8kp30', '8kp50', '8kp5994', '8kp60', '8kdcip2398', '8kdcip24', '8kdcip25', '8kdcip2997', '8kdcip30', '8kdcip50', '8kdcip5994', '8kdcip60'];

/** The family of a format: SD, HD, 2K, UHD / 4K, 8K. */
export function formatFamily(v: string): string {
  if (/^(pal|ntsc)/.test(v)) return 'SD';
  if (/^(720|1080)/.test(v)) return 'HD';
  if (/^(1556|2kdci)/.test(v)) return '2K';
  if (/^(2160|4kdci)/.test(v)) return 'UHD / 4K';
  if (/^8k/.test(v)) return '8K';
  return '';
}

/**
 * A format's name for people: "1080i50" → "1080i 50", "720p5994" → "720p 59,94" (French),
 * "pal" → "PAL 576i 50", "4kdcip24" → "4K DCI 2160p 24", "8kp25" → "8K 4320p 25".
 */
export function formatName(v: string, fr = true): string {
  if (!v) return '';
  const rate = (r: string) => (r.length > 2 ? `${r.slice(0, 2)}${fr ? ',' : '.'}${r.slice(2)}` : r);
  const wide = v.endsWith('-widescreen') ? ' 16:9' : '';
  const base = v.replace('-widescreen', '');
  const sd: Record<string, string> = { pal: 'PAL 576i 50', 'pal-p': 'PAL 576p 50', ntsc: `NTSC 480i ${rate('5994')}`, ntsc2398: `NTSC 480i ${rate('2398')}`, 'ntsc-p': `NTSC 480p ${rate('5994')}` };
  if (sd[base]) return sd[base] + wide;
  let m = /^(2k|4k|8k)dcip(\d+)$/.exec(base);
  if (m) return `${m[1].toUpperCase()} DCI ${{ '2k': '1080p', '4k': '2160p', '8k': '4320p' }[m[1]]} ${rate(m[2])}`;
  m = /^8kp(\d+)$/.exec(base);
  if (m) return `8K 4320p ${rate(m[1])}`;
  m = /^(\d+)([ip])(\d+)$/.exec(base);
  if (!m) return v;
  const prefix = m[1] === '2160' ? 'UHD ' : m[1] === '1556' ? '2K ' : '';
  return `${prefix}${m[1]}${m[2]} ${rate(m[3])}`;
}

/** The recording bitrate advised for a format (Mb/s): H.264 that keeps the quality for clips (×1.5 at 50 to 60 progressive frames). */
export function advisedBitrate(v: string): number {
  const base = ({ SD: 10, HD: 25, '2K': 35, 'UHD / 4K': 60, '8K': 150 } as Record<string, number>)[formatFamily(v)] ?? 25;
  const m = /p(\d+)$/.exec(v.replace('-widescreen', ''));
  const rate = m ? Number(m[1]) / (m[1].length > 2 ? 100 : 1) : 0;
  return Math.round(base * (rate >= 49 ? 1.5 : 1));
}
