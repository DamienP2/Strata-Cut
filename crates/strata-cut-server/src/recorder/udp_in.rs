//! A UDP source received by the server itself, then given to FFmpeg on its standard input.
//!
//! MPEG-TS allows a packet to be sent twice (same PID, same continuity counter, same payload:
//! ISO/IEC 13818-1, 2.4.3.3); the receiver must drop the second. FFmpeg 6 keeps it: the
//! picture's data then holds a piece twice and decodes as a band of blocks (what VLC, which
//! drops it, never shows). Here the duplicates are dropped before FFmpeg, and counted.
//! A large receive buffer and a queue in memory keep the source's bursts while FFmpeg writes.
use std::{
    io::Write,
    net::{Ipv4Addr, UdpSocket},
    os::fd::{AsRawFd, FromRawFd},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

use super::{Glitch, Rec};

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

    fn multicast(&self) -> bool {
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

/// Drops the TS packets sent twice. Keeps, for each PID, the last packet with a payload.
pub struct Dedup {
    last: Vec<Option<(u8, [u8; 184])>>,
}

impl Default for Dedup {
    fn default() -> Self {
        Dedup { last: vec![None; 8192] }
    }
}

impl Dedup {
    /// Copies `dgram` into `out` without its duplicate packets; how many were dropped. A
    /// datagram that is not whole TS packets goes through as it is.
    pub fn filter(&mut self, dgram: &[u8], out: &mut Vec<u8>) -> u32 {
        if dgram.is_empty() || !dgram.len().is_multiple_of(188) || dgram.chunks(188).any(|p| p[0] != 0x47) {
            out.extend_from_slice(dgram);
            return 0;
        }
        let mut dropped = 0;
        for p in dgram.chunks(188) {
            let pid = (((p[1] & 0x1f) as usize) << 8) | p[2] as usize;
            let afc = (p[3] >> 4) & 3;
            let cc = p[3] & 0x0f;
            let has_payload = afc & 1 == 1;
            if pid == 0x1fff || !has_payload {
                out.extend_from_slice(p);
                continue;
            }
            // The payload, after the adaptation field (a duplicate may carry another PCR in it).
            let start = if afc == 3 { 5 + p[4] as usize } else { 4 };
            let mut body = [0u8; 184];
            if start <= 188 {
                body[..188 - start].copy_from_slice(&p[start..]);
            }
            let dup = matches!(&self.last[pid], Some((c, b)) if *c == cc && *b == body);
            if dup {
                dropped += 1;
                // Only one repeat is allowed: a third one is a new packet (a counter that wrapped).
                self.last[pid] = None;
                continue;
            }
            self.last[pid] = Some((cc, body));
            out.extend_from_slice(p);
        }
        dropped
    }
}

/// The receiving: stops when dropped (the run ends).
pub struct UdpIn {
    stop: Arc<AtomicBool>,
    got: Arc<AtomicBool>,
}

impl UdpIn {
    /// Whether anything came from the source.
    pub fn received(&self) -> bool {
        self.got.load(Ordering::Relaxed)
    }
}

impl Drop for UdpIn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Nothing received this long: the input ends (as FFmpeg's own timeout did), the run with it.
const SILENCE: Duration = Duration::from_secs(5);
/// Datagrams held while FFmpeg writes (about 85 MB, a minute at 10 Mb/s).
const QUEUE: usize = 65_536;

/// Receives on `sock` into `pipe` (FFmpeg's standard input). The duplicates dropped are added
/// to `rec`'s count; what the queue could not hold counts as lost by this server.
pub fn start(sock: UdpSocket, pipe: std::io::PipeWriter, rec: Arc<Rec>) -> UdpIn {
    let stop = Arc::new(AtomicBool::new(false));
    let got = Arc::new(AtomicBool::new(false));
    // A larger pipe: FFmpeg reads in bursts.
    // SAFETY: fcntl on an open pipe's fd, with an int.
    unsafe {
        libc::fcntl(pipe.as_raw_fd(), libc::F_SETPIPE_SZ, 1 << 20);
    }
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(QUEUE);
    let done = stop.clone();
    let writer = std::thread::Builder::new().name("udp-in-write".into()).spawn(move || {
        let mut pipe = pipe;
        while let Ok(buf) = rx.recv() {
            if pipe.write_all(&buf).is_err() {
                break; // FFmpeg ended
            }
        }
        done.store(true, Ordering::Relaxed);
    });
    if let Err(e) = writer {
        tracing::error!("cannot start the reception of a recorder: {e}");
        stop.store(true, Ordering::Relaxed);
        return UdpIn { stop, got };
    }
    let done = stop.clone();
    let got2 = got.clone();
    let spawned = std::thread::Builder::new().name("udp-in".into()).spawn(move || {
        let mut buf = vec![0u8; 65_536];
        let mut dedup = Dedup::default();
        let mut last = Instant::now();
        let mut full_logged = Instant::now() - Duration::from_secs(60);
        while !done.load(Ordering::Relaxed) {
            let n = match sock.recv(&mut buf) {
                Ok(n) => n,
                Err(_) => {
                    if last.elapsed() > SILENCE {
                        break; // the source is silent: FFmpeg's input ends
                    }
                    continue;
                }
            };
            last = Instant::now();
            got2.store(true, Ordering::Relaxed);
            let mut out = Vec::with_capacity(n);
            let dropped = dedup.filter(&buf[..n], &mut out);
            if dropped > 0 {
                rec.dups.fetch_add(dropped as u64, Ordering::Relaxed);
            }
            match tx.try_send(out) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    // FFmpeg a minute behind: what comes is lost (said once a minute at most).
                    rec.glitch(Glitch::Lost);
                    if full_logged.elapsed() > Duration::from_secs(60) {
                        full_logged = Instant::now();
                        tracing::warn!("{}: the recording does not keep up: packets lost by this server", rec.config().name);
                    }
                }
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
        }
        // Dropping the sender ends the writer once the queue is written: FFmpeg reads the end.
    });
    if let Err(e) = spawned {
        tracing::error!("cannot start the reception of a recorder: {e}");
        stop.store(true, Ordering::Relaxed);
    }
    UdpIn { stop, got }
}

/// Counts of the duplicates dropped, for the journal.
pub fn take_dups(rec: &Rec) -> u64 {
    rec.dups.swap(0, Ordering::Relaxed)
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
    fn duplicates() {
        let mut d = Dedup::default();
        let mut out = Vec::new();
        let a = packet(0x100, 3, 1);
        let b = packet(0x100, 4, 2);
        let mut dgram = Vec::new();
        for p in [a, a, b, packet(0x101, 3, 1)] {
            dgram.extend_from_slice(&p);
        }
        assert_eq!(d.filter(&dgram, &mut out), 1);
        assert_eq!(out.len(), 3 * 188);
        // Same counter, other payload: not a duplicate (a packet lost in between, 16 later).
        out.clear();
        let mut dgram = Vec::new();
        dgram.extend_from_slice(&packet(0x100, 4, 9));
        assert_eq!(d.filter(&dgram, &mut out), 0);
        // Not TS: through as it is.
        out.clear();
        assert_eq!(d.filter(&[1, 2, 3], &mut out), 0);
        assert_eq!(out, vec![1, 2, 3]);
    }
}
