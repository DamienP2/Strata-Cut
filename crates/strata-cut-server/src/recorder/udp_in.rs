//! A witness of a UDP multicast source: the server joins the same group beside FFmpeg (which
//! records it, untouched) and checks the MPEG-TS packets as they arrive on the network card:
//! missing ones (continuity counter), ones sent twice, ones the source marks in error. Its
//! counts, beside FFmpeg's, say where damage comes from: the stream as it arrives, or its reading.
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

/// What the witness counted, per kind.
#[derive(Default)]
pub struct Counts {
    /// Datagrams received.
    pub seen: AtomicU64,
    /// TS packets missing (continuity counter jumps).
    pub missing: AtomicU64,
    /// TS packets sent twice.
    pub twice: AtomicU64,
    /// TS packets the source marks in error (transport error indicator).
    pub marked: AtomicU64,
}

impl Counts {
    /// The counts since the last call (seen, missing, twice, marked).
    pub fn take(&self) -> (u64, u64, u64, u64) {
        (self.seen.swap(0, Ordering::Relaxed), self.missing.swap(0, Ordering::Relaxed), self.twice.swap(0, Ordering::Relaxed), self.marked.swap(0, Ordering::Relaxed))
    }
}

/// Checks the TS packets of each datagram: the last continuity counter of each PID.
pub struct Check {
    last: Vec<i8>,
}

impl Default for Check {
    fn default() -> Self {
        Check { last: vec![-1; 8192] }
    }
}

impl Check {
    /// (missing, twice, marked) in this datagram; nothing for one that is not whole TS packets.
    pub fn datagram(&mut self, dgram: &[u8]) -> (u64, u64, u64) {
        let (mut missing, mut twice, mut marked) = (0, 0, 0);
        if dgram.is_empty() || !dgram.len().is_multiple_of(188) {
            return (0, 0, 0);
        }
        for p in dgram.chunks(188) {
            if p[0] != 0x47 {
                return (missing, twice, marked);
            }
            if p[1] & 0x80 != 0 {
                marked += 1;
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
                    twice += 1;
                } else if cc != expected {
                    missing += ((cc - expected) & 0x0f) as u64;
                }
            }
            self.last[pid] = cc;
        }
        (missing, twice, marked)
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
                let (m, d, e) = check.datagram(&buf[..n]);
                if m > 0 {
                    counts.missing.fetch_add(m, Ordering::Relaxed);
                }
                if d > 0 {
                    counts.twice.fetch_add(d, Ordering::Relaxed);
                }
                if e > 0 {
                    counts.marked.fetch_add(e, Ordering::Relaxed);
                }
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
        let mut c = Check::default();
        let mut dgram = Vec::new();
        // 3, 3 (twice), 4, 7 (5 and 6 missing); another PID; stuffing.
        for p in [packet(0x100, 3, 1), packet(0x100, 3, 1), packet(0x100, 4, 2), packet(0x100, 7, 2), packet(0x101, 9, 0), packet(0x1fff, 0, 0)] {
            dgram.extend_from_slice(&p);
        }
        assert_eq!(c.datagram(&dgram), (2, 1, 0));
        // The counter wraps: 15 then 0 is fine.
        let mut c = Check::default();
        let mut dgram = Vec::new();
        for p in [packet(0x100, 15, 1), packet(0x100, 0, 1)] {
            dgram.extend_from_slice(&p);
        }
        assert_eq!(c.datagram(&dgram), (0, 0, 0));
        // Marked in error by the source.
        let mut p = packet(0x100, 1, 1);
        p[1] |= 0x80;
        assert_eq!(Check::default().datagram(&p), (0, 0, 1));
        assert_eq!(Check::default().datagram(&[1, 2, 3]), (0, 0, 0));
    }
}
