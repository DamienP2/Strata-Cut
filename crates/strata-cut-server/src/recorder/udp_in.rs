//! What a UDP source sends, seen by the server itself beside FFmpeg (which records it, untouched).
//!
//! Its first datagram says how it carries its MPEG-TS: alone, or in RTP. FFmpeg reads RTP given
//! as "udp://" without its numbers: a datagram that comes out of order, or twice, damages the
//! picture (blocks), where a player that follows the numbers shows nothing. An RTP source is
//! therefore read as RTP (see `run`).
//!
//! A witness then joins the group of a multicast source and checks what arrives on the network
//! card: packets missing, sent twice, out of order, marked in error by the source. Its counts,
//! beside FFmpeg's, say where damage comes from: the stream as it arrives, or its reading.
use std::{
    net::{Ipv4Addr, UdpSocket},
    os::fd::FromRawFd,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};


/// What the source's address asks: the group (or unicast address), the port, the card.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub addr: Ipv4Addr,
    pub port: u16,
    pub iface: Ipv4Addr,
}

impl Target {
    /// `udp://[@]a.b.c.d:port[?options]`; None for what the server leaves to FFmpeg (a name,
    /// IPv6, options it does not know, such as the sources of a source-specific multicast).
    pub fn parse(url: &str, iface: &str) -> Option<Target> {
        let rest = url.strip_prefix("udp://")?;
        let (hostport, query) = rest.split_once('?').unwrap_or((rest, ""));
        let hostport = hostport.trim_start_matches('@').trim_end_matches('/');
        let (host, port) = hostport.rsplit_once(':')?;
        let port: u16 = port.parse().ok().filter(|p| *p > 0)?;
        let addr: Ipv4Addr = if host.is_empty() { Ipv4Addr::UNSPECIFIED } else { host.parse().ok()? };
        let mut card: Ipv4Addr = if iface.is_empty() { Ipv4Addr::UNSPECIFIED } else { iface.parse().ok()? };
        for kv in query.split('&').filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            match k {
                "localaddr" => card = v.parse().ok()?,
                // FFmpeg's own buffers and waits: the server has its own.
                "fifo_size" | "buffer_size" | "overrun_nonfatal" | "timeout" | "pkt_size" | "reuse" | "reuse_socket" => {}
                _ => return None,
            }
        }
        Some(Target { addr, port, iface: card })
    }

    pub fn multicast(&self) -> bool {
        self.addr.is_multicast()
    }
}

/// The socket, joined to its group; an error in FFmpeg's words (what the recorder explains).
pub fn open(t: &Target) -> Result<UdpSocket, String> {
    // SAFETY: a new socket, its options set with pointers to ints and their sizes; the fd is
    // owned by the UdpSocket made from it (closed with it).
    let sock = unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(format!("udp: {}", std::io::Error::last_os_error()));
        }
        let sock = UdpSocket::from_raw_fd(fd);
        let on: libc::c_int = 1;
        let int = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // A group can be received by several programs (another recorder, VLC); a unicast port not.
        if t.multicast() {
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, (&on as *const libc::c_int).cast(), int);
        }
        // 32 MB: about 25 s at 10 Mb/s. Forced when allowed (root), else what the system gives.
        let size: libc::c_int = 32 << 20;
        if libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUFFORCE, (&size as *const libc::c_int).cast(), int) != 0 {
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, (&size as *const libc::c_int).cast(), int);
        }
        // Only this group's packets, even when others are joined on the same port.
        let off: libc::c_int = 0;
        libc::setsockopt(fd, libc::IPPROTO_IP, libc::IP_MULTICAST_ALL, (&off as *const libc::c_int).cast(), int);
        // Bound to the group itself (as FFmpeg): another group on the same port does not come in.
        let bind_to = if t.multicast() { t.addr } else { Ipv4Addr::UNSPECIFIED };
        let sa = libc::sockaddr_in {
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: t.port.to_be(),
            sin_addr: libc::in_addr { s_addr: u32::from(bind_to).to_be() },
            sin_zero: [0; 8],
        };
        if libc::bind(fd, (&sa as *const libc::sockaddr_in).cast(), std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t) != 0 {
            return Err(format!("udp: bind failed: {}", std::io::Error::last_os_error()));
        }
        sock
    };
    if t.multicast() {
        sock.join_multicast_v4(&t.addr, &t.iface).map_err(|e| format!("udp: setsockopt(IP_ADD_MEMBERSHIP): {e}"))?;
    }
    sock.set_read_timeout(Some(Duration::from_millis(200))).map_err(|e| format!("udp: {e}"))?;
    Ok(sock)
}

/// How a UDP source carries its MPEG-TS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// MPEG-TS packets alone in each datagram.
    Ts,
    /// RTP (a numbered header before the MPEG-TS packets): a receiver can put the datagrams
    /// back in order and drop those sent twice.
    Rtp,
    Other,
}

/// The RTP header's length when `dgram` is RTP carrying MPEG-TS (payload type 33), and its number.
fn rtp(dgram: &[u8]) -> Option<(usize, u16)> {
    if dgram.len() < 12 + 188 || dgram[0] >> 6 != 2 || dgram[1] & 0x7f != 33 {
        return None;
    }
    let mut head = 12 + 4 * (dgram[0] & 0x0f) as usize;
    if dgram[0] & 0x10 != 0 {
        // A header extension: its length in 32-bit words, after its 16-bit profile.
        let words = u16::from_be_bytes([*dgram.get(head + 2)?, *dgram.get(head + 3)?]) as usize;
        head += 4 + 4 * words;
    }
    let payload = dgram.get(head..)?;
    (!payload.is_empty() && payload.len().is_multiple_of(188) && payload[0] == 0x47).then(|| (head, u16::from_be_bytes([dgram[2], dgram[3]])))
}

pub fn classify(dgram: &[u8]) -> Kind {
    if rtp(dgram).is_some() {
        Kind::Rtp
    } else if !dgram.is_empty() && dgram.len().is_multiple_of(188) && dgram[0] == 0x47 {
        Kind::Ts
    } else {
        Kind::Other
    }
}

/// What a source sends, from its first datagram (waited for `wait` at most); None: nothing
/// came. The socket is closed before FFmpeg opens its own.
pub fn sniff(t: &Target, wait: Duration) -> Option<Kind> {
    let sock = open(t).ok()?;
    let until = std::time::Instant::now() + wait;
    let mut buf = vec![0u8; 65_536];
    while std::time::Instant::now() < until {
        if let Ok(n) = sock.recv(&mut buf) {
            return Some(classify(&buf[..n]));
        }
    }
    None
}

/// What the witness counted, per kind.
#[derive(Default)]
pub struct Counts {
    /// Datagrams received.
    pub seen: AtomicU64,
    /// Packets missing (MPEG-TS: continuity counter jumps; RTP: numbers that never came).
    pub missing: AtomicU64,
    /// Packets sent twice.
    pub twice: AtomicU64,
    /// TS packets the source marks in error (transport error indicator).
    pub marked: AtomicU64,
    /// RTP datagrams that came after a later one (out of order).
    pub reordered: AtomicU64,
}

/// A count of what a datagram (or ten minutes) showed.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Seen {
    pub missing: u64,
    pub twice: u64,
    pub marked: u64,
    pub reordered: u64,
}

impl Counts {
    /// The datagrams and what they showed since the last call.
    pub fn take(&self) -> (u64, Seen) {
        let t = |a: &AtomicU64| a.swap(0, Ordering::Relaxed);
        (t(&self.seen), Seen { missing: t(&self.missing), twice: t(&self.twice), marked: t(&self.marked), reordered: t(&self.reordered) })
    }

    fn add(&self, s: Seen) {
        for (a, n) in [(&self.missing, s.missing), (&self.twice, s.twice), (&self.marked, s.marked), (&self.reordered, s.reordered)] {
            if n > 0 {
                a.fetch_add(n, Ordering::Relaxed);
            }
        }
    }
}

/// Checks each datagram: RTP by its numbers, plain MPEG-TS by the continuity counter of each PID.
pub struct Check {
    last: Vec<i8>,
    /// RTP: the highest number seen, and the last numbers received (to tell one sent twice
    /// from one that comes late).
    highest: Option<u16>,
    recent: std::collections::VecDeque<u16>,
    /// RTP numbers skipped and not come since (each is missing once it is 500 numbers old).
    gaps: std::collections::VecDeque<u16>,
}

impl Default for Check {
    fn default() -> Self {
        Check { last: vec![-1; 8192], highest: None, recent: Default::default(), gaps: Default::default() }
    }
}

/// How far a late RTP datagram may come after its turn before it counts as missing.
const RTP_WINDOW: u16 = 500;

impl Check {
    /// What this datagram shows; nothing for one that is neither RTP nor whole TS packets.
    pub fn datagram(&mut self, dgram: &[u8]) -> Seen {
        let mut seen = Seen::default();
        let ts = match rtp(dgram) {
            Some((head, n)) => {
                self.rtp_number(n, &mut seen);
                &dgram[head..]
            }
            None if dgram.is_empty() || !dgram.len().is_multiple_of(188) => return seen,
            None => dgram,
        };
        let plain = self.highest.is_none();
        for p in ts.chunks(188) {
            if p[0] != 0x47 {
                break;
            }
            if p[1] & 0x80 != 0 {
                seen.marked += 1;
            }
            // RTP: its numbers say what is missing or twice (the counters would say it again).
            if !plain {
                continue;
            }
            let pid = (((p[1] & 0x1f) as usize) << 8) | p[2] as usize;
            let afc = (p[3] >> 4) & 3;
            let cc = (p[3] & 0x0f) as i8;
            if pid == 0x1fff || afc & 1 == 0 {
                continue; // stuffing, or no payload: the counter does not move
            }
            // A discontinuity the source announces: the counter starts again.
            let announced = afc == 3 && p[4] > 0 && p[5] & 0x80 != 0;
            let last = self.last[pid];
            if last >= 0 && !announced {
                let expected = (last + 1) & 0x0f;
                if cc == last {
                    seen.twice += 1;
                } else if cc != expected {
                    seen.missing += ((cc - expected) & 0x0f) as u64;
                }
            }
            self.last[pid] = cc;
        }
        seen
    }

    fn rtp_number(&mut self, n: u16, seen: &mut Seen) {
        let Some(high) = self.highest else {
            self.highest = Some(n);
            self.recent.push_back(n);
            return;
        };
        let ahead = n.wrapping_sub(high) as i16;
        if ahead > 0 {
            // The numbers skipped may still come (late); a jump too large is the source starting again.
            if (ahead as u16) < RTP_WINDOW {
                for k in 1..ahead as u16 {
                    self.gaps.push_back(high.wrapping_add(k));
                }
            } else {
                self.gaps.clear();
            }
            self.highest = Some(n);
        } else if let Some(i) = self.gaps.iter().position(|g| *g == n) {
            self.gaps.remove(i);
            seen.reordered += 1;
        } else if self.recent.contains(&n) {
            seen.twice += 1;
        }
        self.recent.push_back(n);
        if self.recent.len() > RTP_WINDOW as usize {
            self.recent.pop_front();
        }
        // Skipped numbers too old to come now: missing.
        let high = self.highest.unwrap_or(n);
        while self.gaps.front().is_some_and(|g| high.wrapping_sub(*g) >= RTP_WINDOW) {
            self.gaps.pop_front();
            seen.missing += 1;
        }
    }
}

/// The witness: stops when dropped (the run ends).
pub struct Witness {
    stop: Arc<AtomicBool>,
}

impl Drop for Witness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Joins the group of a multicast source beside FFmpeg and counts into `counts`.
pub fn watch(t: &Target, counts: Arc<Counts>) -> Result<Witness, String> {
    if !t.multicast() {
        return Err("udp: not a multicast".into());
    }
    let sock = open(t)?;
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    std::thread::Builder::new()
        .name("udp-witness".into())
        .spawn(move || {
            let mut buf = vec![0u8; 65_536];
            let mut check = Check::default();
            while !done.load(Ordering::Relaxed) {
                let Ok(n) = sock.recv(&mut buf) else { continue };
                counts.seen.fetch_add(1, Ordering::Relaxed);
                counts.add(check.datagram(&buf[..n]));
            }
        })
        .map_err(|e| format!("udp: {e}"))?;
    Ok(Witness { stop })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(pid: u16, cc: u8, fill: u8) -> [u8; 188] {
        let mut p = [fill; 188];
        p[0] = 0x47;
        p[1] = (pid >> 8) as u8 & 0x1f;
        p[2] = pid as u8;
        p[3] = 0x10 | (cc & 0x0f);
        p
    }

    #[test]
    fn targets() {
        assert_eq!(Target::parse("udp://@239.1.1.1:1234", ""), Some(Target { addr: Ipv4Addr::new(239, 1, 1, 1), port: 1234, iface: Ipv4Addr::UNSPECIFIED }));
        assert_eq!(Target::parse("udp://239.1.1.1:1234?localaddr=10.0.0.2&fifo_size=1000", ""), Some(Target { addr: Ipv4Addr::new(239, 1, 1, 1), port: 1234, iface: Ipv4Addr::new(10, 0, 0, 2) }));
        assert_eq!(Target::parse("udp://239.1.1.1:1234", "10.0.0.3").map(|t| t.iface), Some(Ipv4Addr::new(10, 0, 0, 3)));
        assert_eq!(Target::parse("udp://@:5000", "").map(|t| (t.addr, t.port)), Some((Ipv4Addr::UNSPECIFIED, 5000)));
        // Left to FFmpeg: a source-specific multicast, a name, a missing port.
        assert_eq!(Target::parse("udp://232.1.1.1:1234?sources=10.1.1.1", ""), None);
        assert_eq!(Target::parse("udp://tv.local:1234", ""), None);
        assert_eq!(Target::parse("udp://239.1.1.1", ""), None);
    }

    #[test]
    fn checks() {
        let seen = |missing, twice, marked| Seen { missing, twice, marked, reordered: 0 };
        let mut c = Check::default();
        let mut dgram = Vec::new();
        // 3, 3 (twice), 4, 7 (5 and 6 missing); another PID; stuffing.
        for p in [packet(0x100, 3, 1), packet(0x100, 3, 1), packet(0x100, 4, 2), packet(0x100, 7, 2), packet(0x101, 9, 0), packet(0x1fff, 0, 0)] {
            dgram.extend_from_slice(&p);
        }
        assert_eq!(c.datagram(&dgram), seen(2, 1, 0));
        // The counter wraps: 15 then 0 is fine.
        let mut c = Check::default();
        let mut dgram = Vec::new();
        for p in [packet(0x100, 15, 1), packet(0x100, 0, 1)] {
            dgram.extend_from_slice(&p);
        }
        assert_eq!(c.datagram(&dgram), seen(0, 0, 0));
        // Marked in error by the source.
        let mut p = packet(0x100, 1, 1);
        p[1] |= 0x80;
        assert_eq!(Check::default().datagram(&p), seen(0, 0, 1));
        assert_eq!(Check::default().datagram(&[1, 2, 3]), seen(0, 0, 0));
    }

    fn rtp_dgram(n: u16) -> Vec<u8> {
        let mut d = vec![0x80, 33, (n >> 8) as u8, n as u8, 0, 0, 0, 0, 1, 2, 3, 4];
        d.extend_from_slice(&packet(0x100, (n & 15) as u8, 1));
        d
    }

    #[test]
    fn rtp_numbers() {
        assert_eq!(classify(&rtp_dgram(7)), Kind::Rtp);
        assert_eq!(classify(&packet(0x100, 1, 1)), Kind::Ts);
        assert_eq!(classify(&[0x80, 33, 0, 1]), Kind::Other);
        // 65534, 65535, 1 (0 skipped), 0 (late), 1 again (twice), 2, 4 (3 never comes).
        let mut c = Check::default();
        let mut total = Seen::default();
        let mut feed = |c: &mut Check, n: u16| {
            let s = c.datagram(&rtp_dgram(n));
            total.missing += s.missing;
            total.twice += s.twice;
            total.reordered += s.reordered;
        };
        for n in [65534u16, 65535, 1, 0, 1, 2, 4] {
            feed(&mut c, n);
        }
        // 3 is missing once it is 500 numbers old.
        for n in 5..600u16 {
            feed(&mut c, n);
        }
        assert_eq!(total, Seen { missing: 1, twice: 1, marked: 0, reordered: 1 });
    }
}
