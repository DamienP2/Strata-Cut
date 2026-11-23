//! MPEG-TS read as it passes: the audio tracks of a stream (its PAT and its PMT, in the order
//! FFmpeg numbers them) and when each last sent something.

/// The audio tracks of an MPEG-TS, in order: their PID and their last packet (ms).
#[derive(Default)]
pub struct Seen {
    pmt: Option<u16>,
    /// Its PMT was read (the tracks below are the source's).
    read: bool,
    tracks: Vec<(u16, i64)>,
    /// The PMT being put together (it may take more than one packet: many tracks).
    section: Vec<u8>,
}

impl Seen {
    /// The tracks that sent something in the last `ms` (their order among the audio tracks).
    pub fn alive(&self, now: i64, ms: i64) -> Vec<usize> {
        self.tracks.iter().enumerate().filter(|(_, t)| now - t.1 < ms).map(|(k, _)| k).collect()
    }

    /// Its PMT was read: its tracks are known.
    pub fn known(&self) -> bool {
        self.read
    }

    /// When an audio track last sent something (0: never).
    pub fn newest(&self) -> i64 {
        self.tracks.iter().map(|t| t.1).max().unwrap_or(0)
    }

    /// One 188-byte packet.
    pub fn packet(&mut self, p: &[u8], now: i64) {
        if p.len() < 188 || p[0] != 0x47 {
            return;
        }
        let pid = ((u16::from(p[1]) & 0x1f) << 8) | u16::from(p[2]);
        if let Some(t) = self.tracks.iter_mut().find(|t| t.0 == pid) {
            t.1 = now;
            return;
        }
        let start = p[1] & 0x40 != 0;
        if pid != 0 && Some(pid) != self.pmt {
            return;
        }
        // Its payload: after the adaptation field (and, where a section starts, the pointer field).
        let afc = (p[3] >> 4) & 3;
        let mut i = 4;
        if afc & 2 != 0 {
            i += 1 + p[4] as usize;
        }
        if afc & 1 == 0 || i >= 188 {
            return;
        }
        if pid == 0 {
            if !start {
                return;
            }
            i += 1 + p[i] as usize;
            if let Some(pmt) = p.get(i..).and_then(pat_pmt) {
                self.pmt = Some(pmt);
            }
            return;
        }
        if start {
            i += 1 + p[i] as usize;
            self.section = p.get(i..).unwrap_or(&[]).to_vec();
        } else if !self.section.is_empty() {
            self.section.extend_from_slice(&p[i..]);
        }
        let s = &self.section;
        let whole = s.len() >= 3 && s.len() >= 3 + (((usize::from(s[1]) & 0x0f) << 8) | usize::from(s[2]));
        if !whole {
            return;
        }
        let section = std::mem::take(&mut self.section);
        if let Some(pids) = pmt_audio(&section) {
            self.read = true;
            let old = std::mem::take(&mut self.tracks);
            self.tracks = pids.into_iter().map(|pid| old.iter().find(|t| t.0 == pid).copied().unwrap_or((pid, 0))).collect();
        }
    }
}

/// The PMT's PID, from the PAT (its first programme).
fn pat_pmt(s: &[u8]) -> Option<u16> {
    if s.len() < 8 || s[0] != 0 {
        return None;
    }
    let len = (((usize::from(s[1]) & 0x0f) << 8) | usize::from(s[2])).min(s.len().saturating_sub(3));
    let mut i = 8;
    while i + 4 <= 3 + len.saturating_sub(4) {
        let prog = (u16::from(s[i]) << 8) | u16::from(s[i + 1]);
        if prog != 0 {
            return Some(((u16::from(s[i + 2]) & 0x1f) << 8) | u16::from(s[i + 3]));
        }
        i += 4;
    }
    None
}

/// The audio PIDs of a PMT, in order (as FFmpeg numbers the tracks).
fn pmt_audio(s: &[u8]) -> Option<Vec<u16>> {
    if s.len() < 12 || s[0] != 2 {
        return None;
    }
    let len = ((usize::from(s[1]) & 0x0f) << 8) | usize::from(s[2]);
    let end = (3 + len).checked_sub(4)?.min(s.len());
    let info = ((usize::from(s[10]) & 0x0f) << 8) | usize::from(s[11]);
    let mut i = 12 + info;
    let mut out = Vec::new();
    while i + 5 <= end {
        let kind = s[i];
        let pid = ((u16::from(s[i + 1]) & 0x1f) << 8) | u16::from(s[i + 2]);
        let n = ((usize::from(s[i + 3]) & 0x0f) << 8) | usize::from(s[i + 4]);
        let desc = s.get(i + 5..(i + 5 + n).min(end)).unwrap_or(&[]);
        if is_audio(kind, desc) {
            out.push(pid);
        }
        i += 5 + n;
    }
    Some(out)
}

/// An audio stream type (MPEG, AAC, AC-3, E-AC-3, DTS…), or private data said to be audio by its
/// descriptors (AC-3, E-AC-3, DTS, SMPTE 302M, Opus).
fn is_audio(kind: u8, desc: &[u8]) -> bool {
    match kind {
        0x03 | 0x04 | 0x0f | 0x11 | 0x1c | 0x81 | 0x82 | 0x83 | 0x84 | 0x85 | 0x86 | 0x87 => true,
        0x06 => {
            let mut i = 0;
            while i + 2 <= desc.len() {
                let (tag, n) = (desc[i], desc[i + 1] as usize);
                let body = desc.get(i + 2..i + 2 + n).unwrap_or(&[]);
                match tag {
                    0x6a | 0x7a | 0x7b => return true,
                    0x05 if [&b"AC-3"[..], b"EAC3", b"BSSD", b"Opus", b"DTS1", b"DTS2", b"DTS3"].iter().any(|r| body.starts_with(r)) => return true,
                    _ => {}
                }
                i += 2 + n;
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TS packet starting a section (PSI) on `pid`.
    fn psi(pid: u16, section: &[u8]) -> Vec<u8> {
        let mut p = vec![0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10, 0];
        p.extend_from_slice(section);
        p.resize(188, 0xff);
        p
    }

    #[test]
    fn tracks_read() {
        // PAT: programme 1 on PID 0x100.
        let pat = [0x00, 0xb0, 13, 0, 1, 0xc1, 0, 0, 0, 1, 0xe1, 0x00, 0, 0, 0, 0];
        // PMT: video 0x101 (H.264), MP2 0x102, teletext 0x103, AC-3 in private data 0x104, AAC 0x105.
        let mut pmt = vec![0x02, 0xb0, 0, 0, 1, 0xc1, 0, 0, 0xe1, 0x01, 0xf0, 0];
        pmt.extend([0x1b, 0xe1, 0x01, 0xf0, 0]);
        pmt.extend([0x03, 0xe1, 0x02, 0xf0, 0]);
        pmt.extend([0x06, 0xe1, 0x03, 0xf0, 2, 0x56, 0]);
        pmt.extend([0x06, 0xe1, 0x04, 0xf0, 2, 0x6a, 0]);
        pmt.extend([0x0f, 0xe1, 0x05, 0xf0, 0]);
        pmt[2] = (pmt.len() - 3 + 4) as u8;
        pmt.extend([0, 0, 0, 0]);
        // Over two packets (many tracks): put together.
        let mut s = Seen::default();
        s.packet(&psi(0, &pat), 0);
        // Its payload's end: the start of the section only (the rest in the next packet).
        let mut a = vec![0x47, 0x41, 0x00, 0x30, (188 - 5 - 1 - 10) as u8, 0];
        a.resize(188 - 11, 0xff);
        a.push(0);
        a.extend_from_slice(&pmt[..10]);
        let mut b = vec![0x47, 0x01, 0x00, 0x11];
        b.extend_from_slice(&pmt[10..]);
        b.resize(188, 0xff);
        s.packet(&a, 0);
        assert!(!s.read);
        s.packet(&b, 0);
        assert_eq!(s.tracks.iter().map(|t| t.0).collect::<Vec<_>>(), vec![0x102, 0x104, 0x105]);
        let mut s = Seen::default();
        s.packet(&psi(0, &pat), 0);
        assert_eq!(s.pmt, Some(0x100));
        s.packet(&psi(0x100, &pmt), 0);
        assert_eq!(s.tracks.iter().map(|t| t.0).collect::<Vec<_>>(), vec![0x102, 0x104, 0x105]);
        let es = |pid: u16| {
            let mut p = vec![0x47, (pid >> 8) as u8, pid as u8, 0x10];
            p.resize(188, 0);
            p
        };
        for t in (0..=6000).step_by(100) {
            s.packet(&es(0x102), t);
            if t <= 2000 {
                s.packet(&es(0x104), t);
            }
        }
        s.packet(&es(0x105), 5000);
        s.packet(&es(0x105), 6000);
        assert_eq!(s.newest(), 6000);
        // Track 2 silent since 2 s: not alive.
        assert_eq!(s.alive(6000, 2000), vec![0, 2]);
        assert!(s.known());
    }
}
