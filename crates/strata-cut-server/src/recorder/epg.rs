//! The programme guide (DVB EIT) carried by an MPEG-TS source.
//!
//! FFmpeg shows the EIT as a data stream ("Data: epg") but does not keep it in the
//! recorded segments: the recorder sends it to FFmpeg's output (`-f data pipe:1`), and the
//! sections are decoded here. Events are kept per recorder (in `<data>/epg/<id>.json`)
//! for the loop's length, and shown on the Extraction timeline.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A programme of the guide. Times are UTC milliseconds (as the server's clock).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Event {
    pub id: u16,
    pub start: f64,
    pub end: f64,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

/// What FFmpeg says of the source: the EPG stream to map, and the service of the video.
/// Kept in `<data>/epg/<id>.stream.json`: the next start maps it at once (no restart).
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub stream: Option<String>,
    pub service: Option<u16>,
}

/// Reads FFmpeg's description of the input ("Program 3", "Stream #0:2[0x12]: Data: epg").
#[derive(Default)]
pub struct InfoReader {
    program: Option<u16>,
    pub found: Found,
    in_input: bool,
}

impl InfoReader {
    /// Returns true when the EPG stream was just found.
    pub fn line(&mut self, l: &str) -> bool {
        let t = l.trim_start();
        if t.starts_with("Input #0") {
            self.in_input = true;
            return false;
        }
        if t.starts_with("Output #") || t.starts_with("Stream mapping") {
            self.in_input = false;
            return false;
        }
        if !self.in_input {
            return false;
        }
        if let Some(n) = t.strip_prefix("Program ") {
            self.program = n.trim().parse().ok();
        } else if let Some(rest) = t.strip_prefix("Stream #0:") {
            let index: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if rest.contains(": Video:") && self.found.service.is_none() {
                self.found.service = self.program;
            }
            if rest.contains(": Data: epg") && self.found.stream.is_none() && !index.is_empty() {
                self.found.stream = Some(format!("0:{index}"));
                return true;
            }
        }
        false
    }
}

/// Cuts FFmpeg's raw data output into sections (each says its length).
#[derive(Default)]
pub struct Sections {
    buf: Vec<u8>,
}

impl Sections {
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let mut i = 0;
        while self.buf.len() >= i + 3 {
            let table = self.buf[i];
            // Not the start of a section (stuffing, or lost alignment): one byte further.
            if table == 0xFF || table < 0x4E && table != 0x42 && table != 0x46 && table != 0x70 && table != 0x73 && table != 0xFC {
                i += 1;
                continue;
            }
            let len = 3 + (((self.buf[i + 1] & 0x0F) as usize) << 8 | self.buf[i + 2] as usize);
            if self.buf.len() < i + len {
                break;
            }
            out.push(self.buf[i..i + len].to_vec());
            i += len;
        }
        self.buf.drain(..i);
        if self.buf.len() > 1 << 16 {
            self.buf.clear();
        }
        out
    }
}

/// The events of an EIT section (present/following or schedule), with its service.
pub fn parse_eit(s: &[u8]) -> Option<(u16, bool, Vec<Event>)> {
    let table = *s.first()?;
    if !(0x4E..=0x6F).contains(&table) || s.len() < 18 {
        return None;
    }
    // Only the multiplex received (not "other" ones: 0x4F, 0x60-0x6F).
    if table == 0x4F || table >= 0x60 {
        return None;
    }
    let len = 3 + (((s[1] & 0x0F) as usize) << 8 | s[2] as usize);
    if len > s.len() || len < 18 || crc32(&s[..len]) != 0 {
        return None;
    }
    let service = u16::from_be_bytes([s[3], s[4]]);
    let end = len - 4;
    let mut i = 14;
    let mut events = Vec::new();
    while i + 12 <= end {
        let id = u16::from_be_bytes([s[i], s[i + 1]]);
        let start = mjd_time(&s[i + 2..i + 7]);
        let dur = bcd_secs(&s[i + 7..i + 10]);
        let dlen = (((s[i + 10] & 0x0F) as usize) << 8) | s[i + 11] as usize;
        let d0 = i + 12;
        let d1 = (d0 + dlen).min(end);
        let (mut name, mut text, mut ext) = (String::new(), String::new(), String::new());
        let mut j = d0;
        while j + 2 <= d1 {
            let tag = s[j];
            let l = s[j + 1] as usize;
            let body = &s[(j + 2).min(d1)..(j + 2 + l).min(d1)];
            match tag {
                // short_event_descriptor: language, name, text
                0x4D if body.len() >= 4 => {
                    let nl = body[3] as usize;
                    if 4 + nl < body.len() {
                        name = dvb_text(&body[4..4 + nl]);
                        let tl = body[4 + nl] as usize;
                        text = dvb_text(&body[(5 + nl).min(body.len())..(5 + nl + tl).min(body.len())]);
                    } else {
                        name = dvb_text(&body[4..(4 + nl).min(body.len())]);
                    }
                }
                // extended_event_descriptor: more text
                0x4E if body.len() >= 6 => {
                    let items = body[4] as usize;
                    let k = 5 + items;
                    if k < body.len() {
                        let tl = body[k] as usize;
                        ext.push_str(&dvb_text(&body[(k + 1).min(body.len())..(k + 1 + tl).min(body.len())]));
                    }
                }
                _ => {}
            }
            j += 2 + l;
        }
        if text.is_empty() {
            text = ext;
        }
        if let (Some(start), Some(dur)) = (start, dur) {
            let name = name.trim().to_string();
            if !name.is_empty() && dur > 0 {
                events.push(Event { id, start, end: start + dur as f64 * 1000.0, name, text: text.trim().chars().take(600).collect() });
            }
        }
        i = d0 + dlen;
    }
    Some((service, table == 0x4E, events))
}

/// "Modified Julian Date + hh:mm:ss in BCD" (UTC) → milliseconds since 1970.
fn mjd_time(b: &[u8]) -> Option<f64> {
    if b.iter().all(|&x| x == 0xFF) {
        return None;
    }
    let mjd = u16::from_be_bytes([b[0], b[1]]) as f64;
    let secs = bcd_secs(&b[2..5])? as f64;
    Some(((mjd - 40587.0) * 86400.0 + secs) * 1000.0)
}

fn bcd_secs(b: &[u8]) -> Option<u32> {
    let d = |x: u8| -> Option<u32> {
        let (h, l) = ((x >> 4) as u32, (x & 0x0F) as u32);
        (h < 10 && l < 10).then_some(h * 10 + l)
    };
    let (h, m, s) = (d(b[0])?, d(b[1])?, d(b[2])?);
    (m < 60 && s < 60).then_some(h * 3600 + m * 60 + s)
}

/// MPEG-2 CRC32 (0 over a whole section when it is intact).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

/// A DVB string (EN 300 468 annex A) as text.
pub fn dvb_text(b: &[u8]) -> String {
    let Some(&first) = b.first() else { return String::new() };
    let (body, enc) = match first {
        0x15 => (&b[1..], Enc::Utf8),
        0x11 => (&b[1..], Enc::Ucs2),
        0x10 if b.len() >= 3 => (&b[3..], Enc::Latin),
        0x01..=0x0B => (&b[1..], Enc::Latin),
        0x00..=0x1F => (&b[1..], Enc::Latin),
        // No table given: ISO 6937 says the norm (accent before its letter), but many
        // channels send Latin-1: 6937 only when an accent byte comes before a letter.
        _ if b.windows(2).any(|w| (0xC1..=0xCF).contains(&w[0]) && w[1].is_ascii_alphabetic()) => (b, Enc::Iso6937),
        _ if b.iter().any(|&c| c >= 0xA0) => (b, Enc::Latin),
        _ => (b, Enc::Iso6937),
    };
    let raw = match enc {
        Enc::Utf8 => String::from_utf8_lossy(body).into_owned(),
        Enc::Ucs2 => body.as_chunks::<2>().0.iter().filter_map(|c| char::from_u32(u16::from_be_bytes(*c) as u32)).collect(),
        Enc::Latin => body.iter().map(|&c| c as char).collect(),
        Enc::Iso6937 => iso6937(body),
    };
    // Control codes: emphasis on/off (0x86/0x87) dropped, line break (0x8A) as a space.
    raw.chars().filter_map(|c| match c as u32 { 0x8A => Some(' '), 0x80..=0x9F => None, _ => Some(c) }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

enum Enc {
    Utf8,
    Ucs2,
    Latin,
    Iso6937,
}

/// ISO/IEC 6937 (the DVB default): accents come before their letter.
fn iso6937(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if (0xC1..=0xCF).contains(&c) && i + 1 < b.len() {
            let base = b[i + 1] as char;
            let mark = match c {
                0xC1 => '\u{300}', // grave
                0xC2 => '\u{301}', // acute
                0xC3 => '\u{302}', // circumflex
                0xC4 => '\u{303}', // tilde
                0xC5 => '\u{304}', // macron
                0xC8 => '\u{308}', // diaeresis
                0xCA => '\u{30A}', // ring
                0xCB => '\u{327}', // cedilla
                _ => '\0',
            };
            out.push_str(&compose(base, mark));
            i += 2;
            continue;
        }
        out.push(match c {
            0xA4 => '$',
            0xA6 => '#',
            0xA9 => '‘',
            0xAA => '“',
            0xB9 => '’',
            0xBA => '”',
            0xD0 => '―',
            0xE1 => 'Æ',
            0xE9 => 'Œ',
            0xF1 => 'æ',
            0xF9 => 'œ',
            0xFB => 'ß',
            _ => c as char,
        });
        i += 1;
    }
    out
}

/// A letter and its accent as one character when it exists (é, è, ê, à, ç…).
fn compose(base: char, mark: char) -> String {
    const TABLE: &[(char, char, char)] = &[
        ('a', '\u{300}', 'à'), ('e', '\u{300}', 'è'), ('i', '\u{300}', 'ì'), ('o', '\u{300}', 'ò'), ('u', '\u{300}', 'ù'),
        ('A', '\u{300}', 'À'), ('E', '\u{300}', 'È'), ('U', '\u{300}', 'Ù'),
        ('a', '\u{301}', 'á'), ('e', '\u{301}', 'é'), ('i', '\u{301}', 'í'), ('o', '\u{301}', 'ó'), ('u', '\u{301}', 'ú'), ('E', '\u{301}', 'É'),
        ('a', '\u{302}', 'â'), ('e', '\u{302}', 'ê'), ('i', '\u{302}', 'î'), ('o', '\u{302}', 'ô'), ('u', '\u{302}', 'û'),
        ('A', '\u{302}', 'Â'), ('E', '\u{302}', 'Ê'), ('I', '\u{302}', 'Î'), ('O', '\u{302}', 'Ô'), ('U', '\u{302}', 'Û'),
        ('a', '\u{308}', 'ä'), ('e', '\u{308}', 'ë'), ('i', '\u{308}', 'ï'), ('o', '\u{308}', 'ö'), ('u', '\u{308}', 'ü'), ('y', '\u{308}', 'ÿ'),
        ('E', '\u{308}', 'Ë'), ('I', '\u{308}', 'Ï'), ('O', '\u{308}', 'Ö'), ('U', '\u{308}', 'Ü'),
        ('c', '\u{327}', 'ç'), ('C', '\u{327}', 'Ç'), ('n', '\u{303}', 'ñ'), ('N', '\u{303}', 'Ñ'), ('a', '\u{30A}', 'å'),
    ];
    if let Some(&(_, _, c)) = TABLE.iter().find(|(b, m, _)| *b == base && *m == mark) {
        return c.to_string();
    }
    if mark == '\0' {
        return base.to_string();
    }
    format!("{base}{mark}")
}

/// A recorder's guide: events by start, the newest information winning.
#[derive(Default)]
pub struct Guide {
    pub events: Vec<Event>,
    pub changed: bool,
}

impl Guide {
    pub fn load(path: &Path) -> Guide {
        Guide { events: crate::paths::read_json(path).unwrap_or_default(), changed: false }
    }

    /// Adds the events of a section; an event that overlaps another one replaces it.
    pub fn add(&mut self, new: Vec<Event>) {
        for e in new {
            if let Some(old) = self.events.iter().find(|o| o.id == e.id && o.start == e.start) {
                if *old == e {
                    continue;
                }
            }
            self.events.retain(|o| o.id == e.id && o.start == e.start || o.end <= e.start || o.start >= e.end);
            self.events.retain(|o| !(o.id == e.id && o.start == e.start));
            self.events.push(e);
            self.changed = true;
        }
        self.events.sort_by(|a, b| a.start.total_cmp(&b.start));
    }

    /// Keeps the loop's length behind and two days ahead.
    pub fn trim(&mut self, now: f64, behind_hours: u32) {
        let lo = now - (behind_hours as f64 + 1.0) * 3_600_000.0;
        let hi = now + 48.0 * 3_600_000.0;
        let n = self.events.len();
        self.events.retain(|e| e.end >= lo && e.start <= hi);
        if self.events.len() != n {
            self.changed = true;
        }
    }

    pub fn between(&self, from: f64, to: f64) -> Vec<Event> {
        self.events.iter().filter(|e| e.end > from && e.start < to).cloned().collect()
    }
}

pub fn found_path(data_dir: &Path, id: &str) -> PathBuf {
    data_dir.join("epg").join(format!("{id}.stream.json"))
}

pub fn path(data_dir: &Path, id: &str) -> PathBuf {
    data_dir.join("epg").join(format!("{id}.json"))
}

/// Sections → events of the recorded service (when known, else of the first service seen).
#[derive(Default)]
pub struct Decoder {
    pub service: Option<u16>,
}

impl Decoder {
    pub fn section(&mut self, s: &[u8]) -> Vec<Event> {
        let Some((service, _pf, events)) = parse_eit(s) else { return Vec::new() };
        let want = *self.service.get_or_insert(service);
        if service == want {
            events
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// An EIT section with one event (for the tests and the test source).
    #[allow(clippy::too_many_arguments)]
    pub fn section(table: u8, service: u16, id: u16, start_mjd: u16, hms: [u8; 3], dur: [u8; 3], name: &[u8], text: &[u8]) -> Vec<u8> {
        let mut desc = vec![0x4D, 0, b'f', b'r', b'e', name.len() as u8];
        desc.extend_from_slice(name);
        desc.push(text.len() as u8);
        desc.extend_from_slice(text);
        desc[1] = (desc.len() - 2) as u8;
        let mut ev = Vec::new();
        ev.extend_from_slice(&id.to_be_bytes());
        ev.extend_from_slice(&start_mjd.to_be_bytes());
        ev.extend_from_slice(&hms);
        ev.extend_from_slice(&dur);
        ev.push(0x80 | ((desc.len() >> 8) as u8 & 0x0F));
        ev.push(desc.len() as u8);
        ev.extend_from_slice(&desc);
        let mut s = vec![table, 0, 0];
        s.extend_from_slice(&service.to_be_bytes());
        s.extend_from_slice(&[0xC1, 0, 0, 0, 1, 0, 1, 0, table]);
        s.extend_from_slice(&ev);
        let len = s.len() - 3 + 4;
        s[1] = 0xF0 | ((len >> 8) as u8 & 0x0F);
        s[2] = len as u8;
        let crc = crc32(&s);
        s.extend_from_slice(&crc.to_be_bytes());
        s
    }

    #[test]
    fn eit() {
        // 2026-11-18 = MJD 61362; 20:45:00 for 1 h 30.
        let s = section(0x4E, 3, 77, 61362, [0x20, 0x45, 0x00], [0x01, 0x30, 0x00], b"Le journal", b"\x15Les infos de la soir\xc3\xa9e");
        let (service, pf, ev) = parse_eit(&s).unwrap();
        assert_eq!((service, pf), (3, true));
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].name, "Le journal");
        assert_eq!(ev[0].text, "Les infos de la soirée");
        let start = ((61362.0 - 40587.0) * 86400.0 + 20.0 * 3600.0 + 45.0 * 60.0) * 1000.0;
        assert_eq!(ev[0].start, start);
        assert_eq!(ev[0].end - ev[0].start, 5_400_000.0);
        // A damaged section is ignored.
        let mut bad = s.clone();
        bad[20] ^= 1;
        assert!(parse_eit(&bad).is_none());
        // Sections cut anywhere in FFmpeg's output come back whole.
        let mut sec = Sections::default();
        let mut all = s.clone();
        all.extend_from_slice(&s);
        let (a, b) = all.split_at(25);
        assert_eq!(sec.push(a).len(), 0);
        assert_eq!(sec.push(b).len(), 2);
    }

    #[test]
    fn texts() {
        assert_eq!(dvb_text(b"\xc2et\xc2e \x8ad\xc1ej\xcbc"), "été dèjç");
        assert_eq!(dvb_text(b"\x05Caf\xe9"), "Café");
        assert_eq!(dvb_text(b"L'\xe9t\xe9"), "L'été");
        assert_eq!(dvb_text(b"\x15Th\xc3\xa9\xc3\xa2tre"), "Théâtre");
    }

    #[test]
    fn guide() {
        let e = |id, s: f64, d: f64, n: &str| Event { id, start: s, end: s + d, name: n.into(), text: String::new() };
        let mut g = Guide::default();
        g.add(vec![e(1, 0.0, 100.0, "A"), e(2, 100.0, 100.0, "B")]);
        g.add(vec![e(1, 0.0, 100.0, "A")]);
        assert_eq!(g.events.len(), 2);
        // B moved: the old B is replaced, a new C overlapping it removes it.
        g.add(vec![e(3, 90.0, 50.0, "C")]);
        assert_eq!(g.events.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["C"]);
        let mut info = InfoReader::default();
        for l in ["Input #0, mpegts, from 'udp://x':", "  Program 3 ", "    Stream #0:0[0x100]: Video: h264", "    Stream #0:1[0x101]: Audio: aac", "  Stream #0:2[0x12]: Data: epg"] {
            info.line(l);
        }
        assert_eq!(info.found, Found { stream: Some("0:2".into()), service: Some(3) });
    }
}
