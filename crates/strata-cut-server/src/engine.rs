//! Streamer: the processes around a programme's mixer (`crate::mixer`). The source's decoder
//! (FFmpeg: the source's relay to raw pictures and sound, in step), the encoder (FFmpeg: what the
//! mixer makes to H.264 and AAC in MPEG-TS) and the relay the lives read. The decoder comes and
//! goes with the source; the encoder, made again only if it stops, and the lives do not.
use std::{
    io::Write,
    net::{Ipv4Addr, TcpListener, UdpSocket},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{channel, Receiver, RecvTimeoutError},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt},
    process::Command,
    sync::watch,
};

use crate::{
    mixer::{Inputs, Sink, TRACKS},
    program::{Runner, Shape},
    recorder::{now_ms, Manager, Rec},
    ts::Seen,
};

// ---------------------------------------------------------------- the decoder

/// The source's decoder: deinterlaced pictures at the programme's size (its shape kept, bands
/// added) and rate, on its output; the sound of each track given, 48 kHz stereo, to its port (a
/// track it cannot find is left out, not an end). Pictures made at an even rate from the moment 0
/// (`fps`: a hole filled), the sound alike (`aresample`): the `k`-th picture and the `k`-th frame
/// of sound are of the same moment, whatever the source does. Its timestamps jumping (a splice,
/// an encoder started again) by more than a second: taken as going on (else the jump would be
/// filled: a picture held for as long). It reads the source as it comes; the mixer full, it waits
/// (its output not read).
fn decoder_args(sh: &Shape, input: &str, tracks: &[(usize, u16)]) -> Vec<String> {
    let (w, h, fps) = (sh.w, sh.h, sh.fps);
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let mut a = s(&["-hide_banner", "-nostdin", "-nostats", "-loglevel", "error", "-dts_delta_threshold", "1", "-thread_queue_size", "4096", "-analyzeduration", "1000000", "-probesize", "3000000", "-f", "mpegts", "-i", input]);
    a.extend(s(&["-map", "0:v:0", "-vf"]));
    a.push(format!("bwdif=mode=send_frame:deint=interlaced,scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps}:start_time=0,format=yuv420p"));
    a.extend(s(&["-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"]));
    for &(k, port) in tracks {
        a.extend(s(&["-map", &format!("0:a:{k}?"), "-af", "aresample=48000:async=1:first_pts=0,aformat=sample_fmts=flt:channel_layouts=stereo", "-f", "f32le", "-ar", "48000", "-ac", "2"]));
        a.push(format!("tcp://127.0.0.1:{port}"));
    }
    a
}

/// The source's decoder, while the programme is wanted: started when the source comes, again when
/// it stops (the source lost). All the source's tracks in it: one that sends nothing, or no
/// longer, is silence (it holds nothing back).
pub async fn decode(recorders: Arc<Manager>, source: String, sh: Shape, inputs: Arc<Inputs>, watch: Arc<Watch>, r: Arc<Runner>, mut stop: watch::Receiver<bool>) {
    let mut said = String::new();
    loop {
        if *stop.borrow() {
            return;
        }
        if let Some(rec) = recorders.get(&source).filter(|r| r.flowing()) {
            // How many tracks it has (its PMT, a moment to read it; else as it was described).
            let mut tracks = None;
            for _ in 0..30 {
                tracks = watch.tracks();
                if tracks.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let tracks = tracks.unwrap_or(sh.tracks).min(TRACKS);
            let Some(why) = decode_once(&recorders.ffmpeg, &rec, &sh, tracks, &inputs, &mut stop).await else { return };
            // Said once (a source that comes and goes would fill the journal); to the lives too.
            if why != said {
                tracing::warn!("programme of {}: the decoder stopped: {why}", rec.config().name);
                *r.failed.lock().unwrap() = format!("the decoder stopped: {why}");
                said = why;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            _ = stop.changed() => {}
        }
    }
}

/// One run of the decoder, with the source's first `tracks` tracks: until it ends (why), or the
/// programme stops (none).
async fn decode_once(ffmpeg: &str, rec: &Arc<Rec>, sh: &Shape, tracks: usize, inputs: &Arc<Inputs>, stop: &mut watch::Receiver<bool>) -> Option<String> {
    let mut listeners = Vec::new();
    for k in 0..tracks {
        match tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await {
            Ok(l) => listeners.push((k, l)),
            Err(e) => return Some(e.to_string()),
        }
    }
    let ports: Vec<(usize, u16)> = listeners.iter().map(|(k, l)| (*k, l.local_addr().map(|a| a.port()).unwrap_or(0))).collect();
    let port = match free_udp() {
        Ok(p) => p,
        Err(e) => return Some(e),
    };
    // 5 s without anything: it ends (the source lost), started again when it comes back.
    let input = format!("udp://127.0.0.1:{port}?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout=5000000");
    let key = rand_key();
    rec.taps.lock().unwrap().push((key, port));
    let _untap = Untap(rec.clone(), key);
    let mut cmd = Command::new(ffmpeg);
    cmd.args(decoder_args(sh, &input, &ports)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Some(format!("cannot start {ffmpeg}: {e}")),
    };
    inputs.restart();
    let last = last_words(&mut child);
    // Its pictures, and each track's sound: read as they come.
    let mut readers = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let (inputs, size) = (inputs.clone(), sh.frame_bytes() * 3 / 5);
        readers.push(tokio::spawn(async move {
            loop {
                // The mixer full: the decoder waits (all of it: the sound stays in step).
                while inputs.full() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                let mut f = vec![0u8; size];
                if out.read_exact(&mut f).await.is_err() {
                    return;
                }
                inputs.picture(f);
            }
        }));
    }
    for (k, l) in listeners {
        let inputs = inputs.clone();
        readers.push(tokio::spawn(async move {
            let Ok(Ok((mut s, _))) = tokio::time::timeout(Duration::from_secs(20), l.accept()).await else { return };
            let mut buf = vec![0u8; 3840];
            while s.read_exact(&mut buf).await.is_ok() {
                inputs.sound(k, &buf.as_chunks::<4>().0.iter().map(|x| f32::from_le_bytes(*x)).collect::<Vec<_>>());
            }
        }));
    }
    let end = tokio::select! {
        status = child.wait() => Some(last.lock().unwrap().last().cloned().unwrap_or_else(|| format!("{status:?}"))),
        _ = stop.changed() => None,
    };
    let _ = child.kill().await;
    readers.iter().for_each(|t| t.abort());
    end
}

/// The source's relay, read to know how many sound tracks it has.
pub struct Watch {
    pub port: u16,
    seen: Arc<Mutex<Seen>>,
    done: Arc<AtomicBool>,
}

impl Watch {
    pub fn open() -> Result<Watch, String> {
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        let port = sock.local_addr().map_err(|e| e.to_string())?.port();
        let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
        let (seen, done) = (Arc::new(Mutex::new(Seen::default())), Arc::new(AtomicBool::new(false)));
        let (seen2, done2) = (seen.clone(), done.clone());
        std::thread::Builder::new()
            .name("program-watch".into())
            .spawn(move || {
                let mut buf = vec![0u8; 65536];
                while !done2.load(Ordering::Relaxed) {
                    let Ok(n) = sock.recv(&mut buf) else { continue };
                    let now = now_ms();
                    let mut s = seen2.lock().unwrap();
                    for p in buf[..n].chunks(188) {
                        s.packet(p, now);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Watch { port, seen, done })
    }

    /// Its sound tracks, once its PMT was read.
    fn tracks(&self) -> Option<usize> {
        let s = self.seen.lock().unwrap();
        s.known().then(|| s.count())
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------- the encoder

/// The encoder: the mixer's pictures and sound (raw, over TCP: their timestamps are their count,
/// so they stay in step), H.264 and AAC in MPEG-TS to the programme's relay.
fn encoder_args(sh: &Shape, video: u16, audio: u16, out: u16) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let (w, h, fps, k) = (sh.w, sh.h, sh.fps, sh.kbps);
    let mut a = s(&["-hide_banner", "-nostdin", "-nostats", "-loglevel", "error"]);
    a.extend(s(&["-thread_queue_size", "64", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", &format!("{w}x{h}"), "-framerate", &fps.to_string(), "-i"]));
    a.push(format!("tcp://127.0.0.1:{video}"));
    a.extend(s(&["-thread_queue_size", "256", "-f", "f32le", "-ar", "48000", "-ac", "2", "-i"]));
    a.push(format!("tcp://127.0.0.1:{audio}"));
    a.extend(s(&["-map", "0:v", "-map", "1:a", "-c:v", "libx264", "-preset", "veryfast", "-profile:v", "high", "-pix_fmt", "yuv420p"]));
    a.extend(s(&["-b:v", &format!("{k}k"), "-maxrate", &format!("{k}k"), "-bufsize", &format!("{}k", k * 2), "-g", &(fps * 2).to_string(), "-sc_threshold", "0"]));
    a.extend(s(&["-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2", "-f", "mpegts"]));
    a.push(format!("udp://127.0.0.1:{out}?pkt_size=1316"));
    a
}

/// The encoder, made again if it stops, until the programme ends.
pub async fn encode(ffmpeg: String, r: Arc<Runner>, sh: Shape, sink: Arc<Mutex<Option<Sink>>>, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        if let Err(e) = encode_once(&ffmpeg, &r, &sh, &sink, &mut stop).await {
            tracing::warn!("programme: {e}");
            *r.failed.lock().unwrap() = e;
        }
        *sink.lock().unwrap() = None;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            _ = stop.changed() => {}
        }
    }
}

async fn encode_once(ffmpeg: &str, r: &Arc<Runner>, sh: &Shape, sink: &Arc<Mutex<Option<Sink>>>, stop: &mut watch::Receiver<bool>) -> Result<(), String> {
    let listen = || TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string());
    let (lv, la) = (listen()?, listen()?);
    let port = |l: &TcpListener| l.local_addr().map(|a| a.port()).map_err(|e| e.to_string());
    let (vp, ap) = (port(&lv)?, port(&la)?);
    let out = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    let op = out.local_addr().map_err(|e| e.to_string())?.port();
    let done = Arc::new(AtomicBool::new(false));
    let _end = Ends(done.clone());
    let (vtx, vrx) = channel();
    let (atx, arx) = channel();
    let queued = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let dead = Arc::new(AtomicBool::new(false));
    write_to(lv, vrx, queued[0].clone(), done.clone(), dead.clone());
    write_to(la, arx, queued[1].clone(), done.clone(), dead.clone());
    relay(out, r.clone(), done.clone());
    // A second waiting at most (the encoder starting, the server busy).
    *sink.lock().unwrap() = Some(Sink { video: vtx, audio: atx, queued, most: sh.fps.max(1) as usize, dead: dead.clone() });
    // Its timestamps begin again: the lives connect again.
    r.epoch.fetch_add(1, Ordering::Relaxed);
    let mut cmd = Command::new(ffmpeg);
    cmd.args(encoder_args(sh, vp, ap, op)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("cannot start {ffmpeg}: {e}"))?;
    let last = last_words(&mut child);
    let mut look = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            status = child.wait() => {
                let why = last.lock().unwrap().last().cloned().unwrap_or_else(|| format!("{status:?}"));
                return Err(format!("the encoder stopped: {why}"));
            }
            _ = stop.changed() => {
                let _ = child.kill().await;
                return Ok(());
            }
            // One of its inputs broke (nothing would be sent to it any more): made again.
            _ = look.tick() => {
                if dead.load(Ordering::Relaxed) {
                    let _ = child.kill().await;
                    return Err("the encoder's input broke: made again".into());
                }
            }
        }
    }
}

/// Writes what the mixer makes to the encoder that connects (the first that does), counting
/// down what waits. Ends when the run ends; when the encoder is gone, or broke the link, `dead`.
fn write_to(listener: TcpListener, rx: Receiver<Vec<u8>>, queued: Arc<AtomicUsize>, done: Arc<AtomicBool>, dead: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new().name("program-write".into()).spawn(move || {
        let _ = listener.set_nonblocking(true);
        let mut stream = loop {
            if done.load(Ordering::Relaxed) {
                return;
            }
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    dead.store(true, Ordering::Relaxed);
                    return;
                }
            }
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_nodelay(true);
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(b) => {
                    let ok = stream.write_all(&b).is_ok();
                    queued.fetch_sub(1, Ordering::Relaxed);
                    if !ok {
                        if !done.load(Ordering::Relaxed) {
                            dead.store(true, Ordering::Relaxed);
                        }
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) if !done.load(Ordering::Relaxed) => {}
                Err(_) => return,
            }
        }
    });
}

/// Copies the programme to the lives that read it (looked at again every 100 ms: by time, a still
/// picture makes few packets).
fn relay(sock: UdpSocket, r: Arc<Runner>, done: Arc<AtomicBool>) {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = std::thread::Builder::new().name("program-relay".into()).spawn(move || {
        let mut buf = vec![0u8; 65536];
        let mut taps: Vec<u16> = Vec::new();
        let mut looked: Option<Instant> = None;
        while !done.load(Ordering::Relaxed) {
            let Ok(n) = sock.recv(&mut buf) else { continue };
            if looked.is_none_or(|t| t.elapsed() >= Duration::from_millis(100)) {
                looked = Some(Instant::now());
                taps = r.taps.lock().unwrap().iter().map(|t| t.1).collect();
                r.last_packet.store(now_ms(), Ordering::Relaxed);
            }
            for &port in &taps {
                let _ = sock.send_to(&buf[..n], (Ipv4Addr::LOCALHOST, port));
            }
        }
    });
}

// ---------------------------------------------------------------- helpers

/// A child's last lines on its error output (20 at most; bytes in another charset kept).
fn last_words(child: &mut tokio::process::Child) -> Arc<Mutex<Vec<String>>> {
    let last: Arc<Mutex<Vec<String>>> = Default::default();
    if let Some(err) = child.stderr.take() {
        let last = last.clone();
        tokio::spawn(async move {
            let mut err = tokio::io::BufReader::new(err);
            let mut raw = Vec::new();
            while err.read_until(b'\n', &mut raw).await.is_ok_and(|n| n > 0) {
                let mut v = last.lock().unwrap();
                v.push(String::from_utf8_lossy(&raw).trim_end().to_string());
                raw.clear();
                let n = v.len();
                if n > 20 {
                    v.drain(..n - 20);
                }
            }
        });
    }
    last
}

/// Ends the threads of a run (writing to the encoder, relaying what it makes).
pub struct Ends(pub Arc<AtomicBool>);
impl Drop for Ends {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Takes a tap off the source's relay.
pub struct Untap(pub Arc<Rec>, pub u64);
impl Drop for Untap {
    fn drop(&mut self) {
        self.0.taps.lock().unwrap().retain(|t| t.0 != self.1);
    }
}

fn free_udp() -> Result<u16, String> {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).and_then(|s| s.local_addr()).map(|a| a.port()).map_err(|e| e.to_string())
}

pub fn rand_key() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(now_ms());
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_and_encoder() {
        let sh = Shape { w: 1920, h: 1080, fps: 25, tracks: 2, kbps: 8000 };
        let d = decoder_args(&sh, "udp://in", &[(0, 7001), (2, 7002)]);
        assert!(!d.contains(&"-re".to_string()) && d.contains(&"udp://in".to_string()));
        assert!(d.windows(2).any(|w| w == ["-dts_delta_threshold", "1"]));
        assert!(d.iter().any(|x| x.starts_with("bwdif") && x.contains("scale=1920:1080") && x.contains("fps=25:start_time=0")));
        assert!(d.windows(2).any(|w| w == ["-map", "0:a:2?"]) && d.contains(&"tcp://127.0.0.1:7002".to_string()));
        assert!(d.iter().any(|x| x.starts_with("aresample=48000:async=1:first_pts=0")));
        let e = encoder_args(&sh, 1, 2, 3);
        assert!(e.contains(&"tcp://127.0.0.1:1".to_string()) && e.contains(&"tcp://127.0.0.1:2".to_string()));
        assert!(e.contains(&"udp://127.0.0.1:3?pkt_size=1316".to_string()) && e.windows(2).any(|w| w == ["-b:v", "8000k"]));
    }
}
