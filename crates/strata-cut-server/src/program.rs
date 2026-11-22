//! Streamer: the programme of a source — what its lives send when they take the programme rather
//! (all of them). The source, or an image passed at once or by a fade (1 s); a PNG overlay
//! placed anywhere over both; the sound of the tracks chosen, at their level, on the channels chosen (none under
//! the image). Its settings are the source's, shared by all its lives (`program.json`).
//!
//! One FFmpeg a source, started by the first live that takes the programme and stopped after the
//! last: it reads the source's relay, and the image and the overlay as raw pictures the server
//! sends it (TCP: a new image or overlay goes in without stopping it). The switches reach its
//! filters through ZMQ, frame by frame. What it makes (H.264 and AAC in MPEG-TS) goes to a relay
//! the lives read, as they read the source's.
use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    net::{Ipv4Addr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use axum::{
    extract::{Path as UrlPath, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::Command,
    sync::{mpsc, watch},
};

use crate::{
    api::{bad, not_found, require, ApiResult},
    auth::CurrentUser,
    recorder::{now_ms, Manager, Rec},
    state::{Audience, Events, Shared},
};

/// A sound track of the source in the programme.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Track {
    pub on: bool,
    /// dB.
    pub gain: f64,
}

impl Default for Track {
    fn default() -> Self {
        Track { on: true, gain: 0.0 }
    }
}

/// A source's programme.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Program {
    /// The image (one of the Streamer's pictures), or none: the source's name on a dark picture.
    pub image: Option<String>,
    /// The image is on air (rather than the source).
    pub image_on: bool,
    /// The image and the source pass by a fade (1 s), else at once.
    pub fade_on: bool,
    /// The overlay (a PNG of the Streamer's overlays), and whether it is on air.
    pub overlay: Option<String>,
    pub overlay_on: bool,
    /// Its top left corner, a share of the picture's width and height (0 to 1).
    pub x: f64,
    pub y: f64,
    /// Its width, % of the picture's; its opacity, %.
    pub size: f64,
    pub opacity: f64,
    /// The source's tracks, in order (a track not said: the first on, the others off).
    pub tracks: Vec<Track>,
    /// "lr" (as they are), "l" or "r" (that one on both), "mono".
    pub channels: String,
    /// The level of what goes out, dB (after the tracks' own).
    pub master: f64,
    /// While the source is lost, its lives send the slate (else they wait for it).
    pub slate: bool,
}

impl Default for Program {
    fn default() -> Self {
        Program {
            image: None,
            image_on: false,
            fade_on: true,
            overlay: None,
            overlay_on: false,
            x: 0.79,
            y: 0.05,
            size: 18.0,
            opacity: 100.0,
            tracks: Vec::new(),
            channels: "lr".into(),
            master: 0.0,
            slate: true,
        }
    }
}

impl Program {
    fn normalize(&mut self) -> Result<(), String> {
        if !matches!(self.channels.as_str(), "lr" | "l" | "r" | "mono") {
            return Err("the channels are lr, l, r or mono".into());
        }
        let ok = |v: f64| v.is_finite();
        if ![self.x, self.y, self.size, self.opacity, self.master].into_iter().all(ok) || self.tracks.iter().any(|t| !ok(t.gain)) {
            return Err("a value is not a number".into());
        }
        self.master = self.master.clamp(-40.0, 12.0);
        self.x = self.x.clamp(0.0, 1.0);
        self.y = self.y.clamp(0.0, 1.0);
        self.size = self.size.clamp(2.0, 100.0);
        self.opacity = self.opacity.clamp(0.0, 100.0);
        self.tracks.truncate(8);
        for t in &mut self.tracks {
            t.gain = t.gain.clamp(-40.0, 12.0);
        }
        Ok(())
    }

    fn track(&self, k: usize) -> Track {
        self.tracks.get(k).cloned().unwrap_or(Track { on: k == 0, gain: 0.0 })
    }

    /// The tracks on, among the source's `n`: the only ones mixed (a track off that sends nothing,
    /// or no longer, would hold the whole programme back).
    fn on(&self, n: usize) -> Vec<usize> {
        (0..n).filter(|&k| self.track(k).on).collect()
    }

    /// The weight of each track mixed, as amix takes them ("1|0.5").
    fn weights(&self, mixed: &[usize]) -> String {
        mixed.iter().map(|&k| trim(10f64.powf(self.track(k).gain / 20.0))).collect::<Vec<_>>().join("|")
    }

    /// The left and right outputs, each as a mix of the left and right inputs.
    fn channel_weights(&self) -> (&'static str, &'static str) {
        match self.channels.as_str() {
            "l" => ("1|0", "1|0"),
            "r" => ("0|1", "0|1"),
            "mono" => ("0.5|0.5", "0.5|0.5"),
            _ => ("1|0", "0|1"),
        }
    }

    /// The master level, as a factor.
    fn master_gain(&self) -> String {
        trim(10f64.powf(self.master / 20.0))
    }

    /// The overlay's opacity on air (0 when it is not).
    fn overlay_alpha(&self) -> f64 {
        if self.overlay_on && self.overlay.is_some() { self.opacity / 100.0 } else { 0.0 }
    }

    fn overlay_look(&self) -> (Option<&String>, u64, u64, u64) {
        (self.overlay.as_ref(), (self.x * 10_000.0) as u64, (self.y * 10_000.0) as u64, (self.size * 100.0) as u64)
    }
}

/// What goes out never passes -1 dBFS, whatever the levels set (a limiter, not a level that moves).
const LIMIT: &str = "0.891";

/// A number short ("0.5", "1", "0.251189").
fn trim(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.to_string() }
}

/// What the programme is made of: its size and rate, its tracks, its bitrate.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shape {
    w: u32,
    h: u32,
    fps: u32,
    tracks: usize,
    kbps: u32,
}

impl Shape {
    fn of(rec: &Rec) -> Shape {
        let st = rec.status.lock().unwrap();
        let m = st.media.as_ref();
        let even = |v: u32| v & !1;
        Shape {
            w: m.map(|m| even(m.width)).filter(|w| (64..=4096).contains(w)).unwrap_or(1920),
            h: m.map(|m| even(m.height)).filter(|h| (64..=2160).contains(h)).unwrap_or(1080),
            fps: m.map(|m| m.fps().round() as u32).filter(|f| (10..=60).contains(f)).unwrap_or(25),
            tracks: m.map(|m| m.audio_tracks as usize).unwrap_or(1).min(8),
            kbps: if st.bitrate > 0 { ((st.bitrate / 1000) as u32).clamp(4000, 12000) } else { 8000 },
        }
    }
    /// The bytes of one raw picture (YUVA 4:2:0).
    fn frame_bytes(&self) -> usize {
        (self.w * self.h) as usize * 5 / 2
    }
}

/// The ports of one run of the programme's FFmpeg.
struct Ports {
    tap: u16,
    image: u16,
    overlay: u16,
    zmq: u16,
    out: u16,
}

/// The programme's FFmpeg: the source (0), the image (1) and the overlay (2) as raw pictures,
/// silence (3) when no track is mixed (none on, none sending anything, or the source has none).
fn ffmpeg_args(p: &Program, sh: &Shape, ports: &Ports, on: &[usize]) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let (w, h, fps) = (sh.w, sh.h, sh.fps);
    let mut a = s(&["-hide_banner", "-nostdin", "-nostats", "-loglevel", "error"]);
    a.extend(s(&["-thread_queue_size", "4096", "-analyzeduration", "1000000", "-probesize", "3000000", "-f", "mpegts", "-i"]));
    a.push(format!("udp://127.0.0.1:{}?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout=5000000", ports.tap));
    for port in [ports.image, ports.overlay] {
        a.extend(s(&["-thread_queue_size", "2", "-f", "rawvideo", "-pix_fmt", "yuva420p", "-s", &format!("{w}x{h}"), "-framerate", "5", "-i"]));
        a.push(format!("tcp://127.0.0.1:{port}"));
    }
    if on.is_empty() {
        a.extend(s(&["-re", "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"]));
    }
    let image = if p.image_on { 1.0 } else { 0.0 };
    let (ei, eo) = (u8::from(p.image_on), u8::from(p.overlay_alpha() > 0.0));
    let mut g = format!(
        "[0:v:0]bwdif=mode=send_frame:deint=interlaced,scale={w}:{h},setsar=1,format=yuv420p,zmq=bind_address=tcp\\\\://127.0.0.1\\\\:{zmq}[src];\
         [1:v]fps={fps},lutyuv@img=a=val*{ki}:enable={ei}[img];\
         [2:v]fps={fps},lutyuv@ovl=a=val*{ko}:enable={eo}[ovl];\
         [src][img]overlay@img=eof_action=repeat:format=yuv420:enable={ei}[mix];\
         [mix][ovl]overlay@ovl=eof_action=repeat:format=yuv420:enable={eo},fps={fps}[v];",
        zmq = ports.zmq,
        ki = trim(image),
        ko = trim(p.overlay_alpha()),
    );
    let volume = trim(1.0 - image);
    if on.is_empty() {
        g.push_str(&format!("[3:a]volume@pgm={volume}[a]"));
    } else {
        for &k in on {
            g.push_str(&format!("[0:a:{k}]aresample=48000:async=1,aformat=sample_fmts=fltp:channel_layouts=stereo[t{k}];"));
        }
        let inputs: String = on.iter().map(|k| format!("[t{k}]")).collect();
        let (cl, cr) = p.channel_weights();
        g.push_str(&format!(
            "{inputs}amix@trk=inputs={n}:weights={wt}:normalize=0:dropout_transition=0,channelsplit=channel_layout=stereo[L][R];\
             [L]asplit[L1][L2];[R]asplit[R1][R2];\
             [L1][R1]amix@cl=inputs=2:weights={cl}:normalize=0[OL];\
             [L2][R2]amix@cr=inputs=2:weights={cr}:normalize=0[OR];\
             [OL][OR]join=inputs=2:channel_layout=stereo,volume@pgm={volume},volume@mst={mst},alimiter=limit={LIMIT}:attack=5:release=50:level=0[a]",
            n = on.len(),
            mst = p.master_gain(),
            wt = p.weights(on),
        ));
    }
    a.extend(s(&["-filter_complex", &g, "-map", "[v]", "-map", "[a]"]));
    let k = sh.kbps;
    a.extend(s(&["-c:v", "libx264", "-preset", "veryfast", "-profile:v", "high", "-pix_fmt", "yuv420p"]));
    a.extend(s(&["-b:v", &format!("{k}k"), "-maxrate", &format!("{k}k"), "-bufsize", &format!("{}k", k * 2), "-g", &(fps * 2).to_string(), "-sc_threshold", "0"]));
    a.extend(s(&["-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2", "-f", "mpegts"]));
    a.push(format!("udp://127.0.0.1:{}?pkt_size=1316", ports.out));
    a
}

/// The filter that places the overlay on a transparent picture of the programme's size.
fn overlay_filter(p: &Program, sh: &Shape) -> String {
    let (w, h) = (sh.w, sh.h);
    let bw = ((w as f64 * p.size / 100.0).round() as u32).clamp(2, w) & !1;
    // Where it was put, kept inside the picture.
    let x = format!("min({}\\,ow-iw)", (w as f64 * p.x).round() as u32);
    let y = format!("min({}\\,oh-ih)", (h as f64 * p.y).round() as u32);
    format!("scale=w={bw}:h={h}:force_original_aspect_ratio=decrease,format=rgba,pad={w}:{h}:{x}:{y}:color=black@0,format=yuva420p")
}

/// A raw picture made by FFmpeg (one frame, YUVA 4:2:0), or none.
async fn render(ffmpeg: &str, input: &[String], filter: &str, sh: &Shape) -> Option<Vec<u8>> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"]).args(input).args(["-vf", filter, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuva420p", "-"]);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(20), cmd.output()).await.ok()?.ok()?;
    if out.stdout.len() == sh.frame_bytes() {
        Some(out.stdout)
    } else {
        tracing::warn!("programme: a picture was not made: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or(""));
        None
    }
}

/// A plain picture: transparent (the overlay without one), or the dark ground.
fn plain(sh: &Shape, opaque: bool) -> Vec<u8> {
    let n = (sh.w * sh.h) as usize;
    let mut v = Vec::with_capacity(sh.frame_bytes());
    v.resize(n, if opaque { 18 } else { 16 });
    v.resize(n + n / 2, 128);
    v.resize(n * 5 / 2, if opaque { 255 } else { 0 });
    v
}

// ---------------------------------------------------------------- ZMQ (ZMTP 3, a REQ socket)

/// The commands to the programme's filters: FFmpeg's `zmq` filter (a REP socket).
struct Zmq {
    s: TcpStream,
}

fn zmq_frame(body: &[u8], flags: u8) -> Vec<u8> {
    let mut f = Vec::with_capacity(body.len() + 9);
    if body.len() > 255 {
        f.push(flags | 2);
        f.extend((body.len() as u64).to_be_bytes());
    } else {
        f.push(flags);
        f.push(body.len() as u8);
    }
    f.extend(body);
    f
}

impl Zmq {
    async fn connect(port: u16) -> std::io::Result<Zmq> {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
        let mut greeting = vec![0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0x7F, 3, 0];
        greeting.extend(b"NULL");
        greeting.resize(64, 0);
        s.write_all(&greeting).await?;
        let mut theirs = [0u8; 64];
        s.read_exact(&mut theirs).await?;
        let mut ready = b"\x05READY\x0bSocket-Type".to_vec();
        ready.extend(3u32.to_be_bytes());
        ready.extend(b"REQ");
        s.write_all(&zmq_frame(&ready, 4)).await?;
        let mut z = Zmq { s };
        z.read_frame().await?;
        Ok(z)
    }

    async fn read_frame(&mut self) -> std::io::Result<(u8, Vec<u8>)> {
        let flags = self.s.read_u8().await?;
        let n = if flags & 2 != 0 { self.s.read_u64().await? as usize } else { self.s.read_u8().await? as usize };
        if n > 1 << 20 {
            return Err(std::io::Error::other("zmq: frame too large"));
        }
        let mut b = vec![0u8; n];
        self.s.read_exact(&mut b).await?;
        Ok((flags, b))
    }

    /// "target command argument" → FFmpeg's answer ("0 Success").
    async fn send(&mut self, msg: &str) -> std::io::Result<String> {
        let mut out = zmq_frame(b"", 1);
        out.extend(zmq_frame(msg.as_bytes(), 0));
        let ask = async {
            self.s.write_all(&out).await?;
            let mut last = Vec::new();
            loop {
                let (flags, body) = self.read_frame().await?;
                if flags & 4 == 0 {
                    last = body;
                }
                if flags & 1 == 0 {
                    return Ok::<_, std::io::Error>(String::from_utf8_lossy(&last).into_owned());
                }
            }
        };
        // Answered at the next frame: the source stopped, FFmpeg waits; this does not.
        tokio::time::timeout(Duration::from_secs(3), ask).await.map_err(|_| std::io::Error::other("zmq: no answer"))?
    }
}

// ---------------------------------------------------------------- the programmes

/// A change for a running programme, and how it is shown.
struct Change {
    program: Program,
    /// The image passed: by a fade of this many seconds (0: cut).
    image_fade: f64,
}

/// A programme being made.
pub struct Runner {
    /// The lives reading it (as `Rec::taps`).
    pub taps: Mutex<Vec<(u64, u16)>>,
    last_packet: AtomicI64,
    users: AtomicUsize,
    tx: mpsc::UnboundedSender<Change>,
    stop: watch::Sender<bool>,
    /// Why its FFmpeg stopped last (said to the lives waiting for it).
    failed: Mutex<String>,
}

impl Runner {
    /// Why the programme could not be made, the last time (empty: it was not refused).
    pub fn why(&self) -> String {
        self.failed.lock().unwrap().clone()
    }
    /// It comes now (packets in the last 2 s).
    pub fn flowing(&self) -> bool {
        now_ms() - self.last_packet.load(Ordering::Relaxed) < 2000
    }
}

pub struct Programs {
    data_dir: PathBuf,
    recorders: Arc<Manager>,
    events: Events,
    all: Mutex<BTreeMap<String, Program>>,
    running: Mutex<HashMap<String, Arc<Runner>>>,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("program.json")
}

impl Programs {
    pub fn start(data_dir: &Path, recorders: Arc<Manager>, events: Events) -> Arc<Programs> {
        let all = crate::paths::read_json(&path(data_dir)).unwrap_or_default();
        Arc::new(Programs { data_dir: data_dir.to_path_buf(), recorders, events, all: Mutex::new(all), running: Mutex::new(HashMap::new()) })
    }

    pub fn get(&self, source: &str) -> Program {
        self.all.lock().unwrap().get(source).cloned().unwrap_or_default()
    }

    /// While the source is lost, its lives send the slate.
    pub fn slate(&self, source: &str) -> bool {
        self.get(source).slate
    }

    pub fn is_running(&self, source: &str) -> bool {
        self.running.lock().unwrap().contains_key(source)
    }

    /// Saves a programme, and gives it to its FFmpeg if it runs.
    fn put(&self, source: &str, p: Program, image_fade: f64) -> Result<(), String> {
        let all = {
            let mut all = self.all.lock().unwrap();
            all.insert(source.to_string(), p.clone());
            all.clone()
        };
        crate::paths::write_json(&path(&self.data_dir), &all, false).map_err(|e| e.to_string())?;
        if let Some(r) = self.running.lock().unwrap().get(source) {
            let _ = r.tx.send(Change { program: p, image_fade });
        }
        self.events.send(Audience::All, json!({"type": "changed", "what": "program"}));
        Ok(())
    }

    /// The programme of a source, made while a live reads it (`release` after).
    pub fn acquire(self: &Arc<Self>, source: &str) -> Arc<Runner> {
        let mut running = self.running.lock().unwrap();
        if let Some(r) = running.get(source) {
            r.users.fetch_add(1, Ordering::Relaxed);
            return r.clone();
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let r = Arc::new(Runner { taps: Mutex::new(Vec::new()), last_packet: AtomicI64::new(0), users: AtomicUsize::new(1), failed: Mutex::new(String::new()), tx, stop: watch::channel(false).0 });
        running.insert(source.to_string(), r.clone());
        let (me, source, runner) = (self.clone(), source.to_string(), r.clone());
        tokio::spawn(async move { me.run(source, runner, rx).await });
        r
    }

    pub fn release(&self, source: &str, r: &Arc<Runner>) {
        let mut running = self.running.lock().unwrap();
        if r.users.fetch_sub(1, Ordering::Relaxed) == 1 {
            let _ = r.stop.send(true);
            if running.get(source).is_some_and(|x| Arc::ptr_eq(x, r)) {
                running.remove(source);
            }
        }
    }

    /// Made again and again while it is wanted (the source lost: when it comes back).
    async fn run(self: Arc<Self>, source: String, r: Arc<Runner>, mut rx: mpsc::UnboundedReceiver<Change>) {
        let mut stop = r.stop.subscribe();
        let inlet = match Inlet::open() {
            Ok(i) => i,
            Err(e) => {
                *r.failed.lock().unwrap() = e;
                return;
            }
        };
        // The tracks mixed by the last run (None: none yet).
        let mut was: Option<Vec<usize>> = None;
        loop {
            if *stop.borrow() {
                break;
            }
            let Some(rec) = self.recorders.get(&source) else { break };
            if rec.flowing() {
                let made = self.once(&rec, &r, &inlet, &mut was, &mut rx, &mut stop).await;
                if *stop.borrow() {
                    break;
                }
                match made {
                    // Made again at once (other tracks on).
                    Ok(()) => continue,
                    Err(e) => {
                        was = None;
                        tracing::warn!("programme of {}: {e}", rec.config().name);
                        *r.failed.lock().unwrap() = e;
                    }
                }
            }
            // The changes meanwhile are in the settings: the next run starts with them.
            while rx.try_recv().is_ok() {}
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = stop.changed() => {}
            }
        }
    }

    /// One run of the programme's FFmpeg; Ok: to be made again at once (other tracks to mix), or
    /// stopped.
    async fn once(&self, rec: &Arc<Rec>, r: &Arc<Runner>, inlet: &Inlet, was: &mut Option<Vec<usize>>, rx: &mut mpsc::UnboundedReceiver<Change>, stop: &mut watch::Receiver<bool>) -> Result<(), String> {
        let ffmpeg = self.recorders.ffmpeg.clone();
        let sh = Shape::of(rec);
        let mut p = self.get(&rec.id);
        let name = rec.config().name;
        let image = Arc::new(Mutex::new(Arc::new(self.image(&ffmpeg, &p, &sh, &name).await)));
        let overlay = Arc::new(Mutex::new(Arc::new(self.overlay(&ffmpeg, &p, &sh).await)));
        let done = Arc::new(AtomicBool::new(false));
        let _end = Ended(done.clone());
        let listen = || TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string());
        let (li, lo) = (listen()?, listen()?);
        let out = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        let key = rand_key();
        rec.taps.lock().unwrap().push((key, inlet.port));
        let _untap = Untap(rec.clone(), key);
        // The first time, a moment to read the source's tracks (which send something).
        if was.is_none() {
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(2500) {
                let now = now_ms();
                if inlet.known() && (t.elapsed() > Duration::from_millis(1500) || inlet.mixed(&p.on(sh.tracks), sh.tracks, None, now) == p.on(sh.tracks)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let mixed = inlet.mixed(&p.on(sh.tracks), sh.tracks, was.as_deref(), now_ms());
        if was.as_ref().is_some_and(|w| *w != mixed) || was.is_none() && mixed != p.on(sh.tracks) {
            tracing::warn!("programme of {name}: tracks mixed {mixed:?} (on: {:?}; the others send nothing)", p.on(sh.tracks));
        }
        *was = Some(mixed.clone());
        let ports = Ports {
            tap: free_udp()?,
            image: li.local_addr().map_err(|e| e.to_string())?.port(),
            overlay: lo.local_addr().map_err(|e| e.to_string())?.port(),
            zmq: listen()?.local_addr().map_err(|e| e.to_string())?.port(),
            out: out.local_addr().map_err(|e| e.to_string())?.port(),
        };
        feed(li, image.clone(), done.clone());
        feed(lo, overlay.clone(), done.clone());
        relay(out, r.clone(), done.clone());
        inlet.to.store(ports.tap, Ordering::Relaxed);
        let _off = Off(inlet.to.clone());
        let mut cmd = Command::new(&ffmpeg);
        cmd.args(ffmpeg_args(&p, &sh, &ports, &mixed)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("cannot start {ffmpeg}: {e}"))?;
        let last: Arc<Mutex<Vec<String>>> = Default::default();
        if let Some(err) = child.stderr.take() {
            let last = last.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let mut v = last.lock().unwrap();
                    v.push(l);
                    let n = v.len();
                    if n > 20 {
                        v.drain(..n - 20);
                    }
                }
            });
        }
        tracing::info!("programme of {name}: made ({}x{}, {} fps, {} kb/s)", sh.w, sh.h, sh.fps, sh.kbps);
        let mut zmq: Option<Zmq> = None;
        let started = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut watch = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                status = child.wait() => {
                    let why = last.lock().unwrap().last().cloned().unwrap_or_else(|| format!("{status:?}"));
                    return Err(format!("FFmpeg stopped: {why}"));
                }
                _ = stop.changed() => {
                    let _ = child.kill().await;
                    return Ok(());
                }
                // A track mixed that no longer sends anything, or one on that sends again: another
                // mix (the source lost: what comes next is the source's matter).
                _ = watch.tick() => {
                    if rec.flowing() && inlet.mixed(&p.on(sh.tracks), sh.tracks, Some(&mixed), now_ms()) != mixed {
                        let _ = child.kill().await;
                        return Ok(());
                    }
                }
                _ = tick.tick(), if zmq.is_none() => {
                    // Its filters listen once the source is read (a few seconds).
                    if let Ok(z) = Zmq::connect(ports.zmq).await {
                        zmq = Some(z);
                    } else if started.elapsed() > Duration::from_secs(30) {
                        let _ = child.kill().await;
                        return Err("its filters do not answer".into());
                    }
                }
                change = rx.recv(), if zmq.is_some() => {
                    let Some(c) = change else { continue };
                    let Some(z) = zmq.as_mut() else { continue };
                    let next = c.program;
                    // Other tracks on: another mix, the programme is made again (a moment).
                    if inlet.mixed(&next.on(sh.tracks), sh.tracks, Some(&mixed), now_ms()) != mixed {
                        tracing::info!("programme of {name}: other tracks on, made again");
                        let _ = child.kill().await;
                        return Ok(());
                    }
                    if next.image != p.image {
                        *image.lock().unwrap() = Arc::new(self.image(&ffmpeg, &next, &sh, &name).await);
                    }
                    if next.overlay_look() != p.overlay_look() {
                        *overlay.lock().unwrap() = Arc::new(self.overlay(&ffmpeg, &next, &sh).await);
                    }
                    if let Err(e) = apply(z, &p, &next, &mixed, c.image_fade).await {
                        tracing::warn!("programme of {name}: {e}");
                        zmq = None;
                    }
                    p = next;
                }
            }
        }
    }

    /// The image, at the programme's size: the one chosen, else the source's name on a dark picture.
    async fn image(&self, ffmpeg: &str, p: &Program, sh: &Shape, name: &str) -> Vec<u8> {
        let (w, h) = (sh.w, sh.h);
        let made = match p.image.as_deref().and_then(|id| crate::slates::file_of(&self.data_dir, id)) {
            Some(file) => {
                let input = vec!["-i".to_string(), file.to_string_lossy().into_owned()];
                render(ffmpeg, &input, &format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=0x0b0e16,setsar=1"), sh).await
            }
            None => {
                let text = self.data_dir.join(format!("program-{}.txt", rand_key()));
                let _ = std::fs::write(&text, format!("{name}\nLe direct reprend dans un instant"));
                let file = text.to_string_lossy().replace('\\', "/").replace(':', "\\\\:").replace('\'', "");
                let size = (h / 16).max(16);
                let input = ["-f", "lavfi", "-i", &format!("color=c=0x0b0e16:s={w}x{h}")].map(String::from).to_vec();
                let made = render(ffmpeg, &input, &format!("drawtext=textfile='{file}':fontcolor=white:fontsize={size}:line_spacing={}:x=(w-tw)/2:y=(h-th)/2", size / 2), sh).await;
                let _ = std::fs::remove_file(&text);
                made
            }
        };
        made.unwrap_or_else(|| plain(sh, true))
    }

    /// The overlay placed on a transparent picture of the programme's size.
    async fn overlay(&self, ffmpeg: &str, p: &Program, sh: &Shape) -> Vec<u8> {
        let made = match p.overlay.as_deref().and_then(|id| crate::slates::file_of(&self.data_dir, id)) {
            Some(file) => render(ffmpeg, &["-i".to_string(), file.to_string_lossy().into_owned()], &overlay_filter(p, sh), sh).await,
            None => None,
        };
        made.unwrap_or_else(|| plain(sh, false))
    }
}

/// Sends what changed to the running filters: the image by a fade (or a cut), the overlay by a
/// short fade, the sound at once.
async fn apply(z: &mut Zmq, was: &Program, next: &Program, mixed: &[usize], image_fade: f64) -> std::io::Result<()> {
    if !mixed.is_empty() {
        if next.weights(mixed) != was.weights(mixed) {
            z.send(&format!("amix@trk weights {}", next.weights(mixed))).await?;
        }
        if next.master != was.master {
            z.send(&format!("volume@mst volume {}", next.master_gain())).await?;
        }
        if next.channels != was.channels {
            let (cl, cr) = next.channel_weights();
            z.send(&format!("amix@cl weights {cl}")).await?;
            z.send(&format!("amix@cr weights {cr}")).await?;
        }
    }
    let (o0, o1) = (was.overlay_alpha(), next.overlay_alpha());
    if o0 != o1 {
        fade(z, "ovl", o0, o1, if was.overlay_on != next.overlay_on { 0.3 } else { 0.0 }, false).await?;
    }
    if was.image_on != next.image_on {
        let (a, b) = if next.image_on { (0.0, 1.0) } else { (1.0, 0.0) };
        fade(z, "img", a, b, image_fade, true).await?;
    }
    Ok(())
}

/// A picture's opacity from `a` to `b` in `secs` (0: at once), one step a frame or so; with the
/// image, the sound goes the other way (none under the image).
async fn fade(z: &mut Zmq, which: &str, a: f64, b: f64, secs: f64, sound: bool) -> std::io::Result<()> {
    if b > 0.0 {
        z.send(&format!("lutyuv@{which} enable 1")).await?;
        z.send(&format!("overlay@{which} enable 1")).await?;
    }
    let start = Instant::now();
    loop {
        let x = if secs <= 0.0 { 1.0 } else { (start.elapsed().as_secs_f64() / secs).min(1.0) };
        let k = a + (b - a) * x;
        z.send(&format!("lutyuv@{which} a val*{}", trim(k))).await?;
        if sound {
            z.send(&format!("volume@pgm volume {}", trim(1.0 - k))).await?;
        }
        if x >= 1.0 {
            break;
        }
    }
    if b <= 0.0 {
        z.send(&format!("overlay@{which} enable 0")).await?;
        z.send(&format!("lutyuv@{which} enable 0")).await?;
    }
    Ok(())
}

/// Sends a raw picture over and over to the FFmpeg that connects (it takes them as it needs
/// them); the picture may change meanwhile.
fn feed(listener: TcpListener, slot: Arc<Mutex<Arc<Vec<u8>>>>, done: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new().name("program-feed".into()).spawn(move || {
        let _ = listener.set_nonblocking(true);
        let mut stream = loop {
            if done.load(Ordering::Relaxed) {
                return;
            }
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => return,
            }
        };
        let _ = stream.set_nonblocking(false);
        while !done.load(Ordering::Relaxed) {
            let frame = slot.lock().unwrap().clone();
            // FFmpeg gone: the writes fail.
            if stream.write_all(&frame).is_err() {
                return;
            }
        }
    });
}

/// Copies the programme to the lives that read it.
fn relay(sock: UdpSocket, r: Arc<Runner>, done: Arc<AtomicBool>) {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = std::thread::Builder::new().name("program-relay".into()).spawn(move || {
        let mut buf = vec![0u8; 65536];
        let mut taps: Vec<u16> = Vec::new();
        let mut packets = 0u32;
        while !done.load(Ordering::Relaxed) {
            let Ok(n) = sock.recv(&mut buf) else { continue };
            if packets.is_multiple_of(64) {
                taps = r.taps.lock().unwrap().iter().map(|t| t.1).collect();
                r.last_packet.store(now_ms(), Ordering::Relaxed);
            }
            packets = packets.wrapping_add(1);
            for &port in &taps {
                let _ = sock.send_to(&buf[..n], (Ipv4Addr::LOCALHOST, port));
            }
        }
    });
}

/// Stops passing the source on, after a run.
struct Off(Arc<AtomicU16>);
impl Drop for Off {
    fn drop(&mut self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

/// Ends the feeding and relay threads of a run.
struct Ended(Arc<AtomicBool>);
impl Drop for Ended {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Takes the programme's tap off the source's relay.
struct Untap(Arc<Rec>, u64);
impl Drop for Untap {
    fn drop(&mut self) {
        self.0.taps.lock().unwrap().retain(|t| t.0 != self.1);
    }
}

/// The source on its way to the programme: read, to know when each of its audio tracks last
/// sent something (a track that sends nothing holds the whole mix back: it is left out while it
/// is silent), and passed on to the programme's FFmpeg.
struct Inlet {
    port: u16,
    /// Where it is passed on (0: nowhere, between two runs).
    to: Arc<AtomicU16>,
    seen: Arc<Mutex<Seen>>,
    done: Arc<AtomicBool>,
}

/// The audio tracks of the source's MPEG-TS, in order: their PID, their last packet and the start
/// of the packets coming without a break (ms).
#[derive(Default)]
struct Seen {
    pmt: Option<u16>,
    tracks: Vec<(u16, i64, i64)>,
}

impl Seen {
    /// One 188-byte packet.
    fn packet(&mut self, p: &[u8], now: i64) {
        if p.len() < 188 || p[0] != 0x47 {
            return;
        }
        let pid = ((u16::from(p[1]) & 0x1f) << 8) | u16::from(p[2]);
        if let Some(t) = self.tracks.iter_mut().find(|t| t.0 == pid) {
            // More than a second without: a break.
            if now - t.1 > 1000 {
                t.2 = now;
            }
            t.1 = now;
            return;
        }
        let start = p[1] & 0x40 != 0;
        if !start || (pid != 0 && Some(pid) != self.pmt) {
            return;
        }
        // Its payload: after the adaptation field, the pointer field, then the section.
        let afc = (p[3] >> 4) & 3;
        let mut i = 4;
        if afc & 2 != 0 {
            i += 1 + p[4] as usize;
        }
        if afc & 1 == 0 || i >= 188 {
            return;
        }
        i += 1 + p[i] as usize;
        let Some(sec) = p.get(i..) else { return };
        if pid == 0 {
            if let Some(pmt) = pat_pmt(sec) {
                self.pmt = Some(pmt);
            }
        } else if let Some(pids) = pmt_audio(sec) {
            let old = std::mem::take(&mut self.tracks);
            self.tracks = pids.into_iter().map(|pid| old.iter().find(|t| t.0 == pid).copied().unwrap_or((pid, 0, 0))).collect();
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

impl Inlet {
    fn open() -> Result<Inlet, String> {
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        let port = sock.local_addr().map_err(|e| e.to_string())?.port();
        let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
        let (to, seen, done) = (Arc::new(AtomicU16::new(0)), Arc::new(Mutex::new(Seen::default())), Arc::new(AtomicBool::new(false)));
        let (to2, seen2, done2) = (to.clone(), seen.clone(), done.clone());
        std::thread::Builder::new()
            .name("program-inlet".into())
            .spawn(move || {
                let mut buf = vec![0u8; 65536];
                while !done2.load(Ordering::Relaxed) {
                    let Ok(n) = sock.recv(&mut buf) else { continue };
                    {
                        let now = now_ms();
                        let mut s = seen2.lock().unwrap();
                        for p in buf[..n].chunks(188) {
                            s.packet(p, now);
                        }
                    }
                    let to = to2.load(Ordering::Relaxed);
                    if to != 0 {
                        let _ = sock.send_to(&buf[..n], (Ipv4Addr::LOCALHOST, to));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Inlet { port, to, seen, done })
    }

    /// The tracks to mix among those on, `was` the ones mixed until now (None: the first time):
    /// a track is left out after 3 s without anything, taken back after 5 s coming again. All of
    /// those on when the source's tracks are not known (not as many as said).
    fn mixed(&self, on: &[usize], n: usize, was: Option<&[usize]>, now: i64) -> Vec<usize> {
        let s = self.seen.lock().unwrap();
        if s.tracks.len() != n {
            return on.to_vec();
        }
        on.iter()
            .copied()
            .filter(|&k| {
                let (_, last, since) = s.tracks[k];
                match was {
                    Some(was) if !was.contains(&k) => now - last < 1000 && now - since >= 5000,
                    _ => now - last < 3000,
                }
            })
            .collect()
    }

    /// The source's tracks were read (its PMT), or not.
    fn known(&self) -> bool {
        let s = self.seen.lock().unwrap();
        s.pmt.is_some() && !s.tracks.is_empty()
    }
}

impl Drop for Inlet {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

fn free_udp() -> Result<u16, String> {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).and_then(|s| s.local_addr()).map(|a| a.port()).map_err(|e| e.to_string())
}

fn rand_key() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(now_ms());
    h.finish()
}

// ---------------------------------------------------------------- routes

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/program/{source}", get(read).put(write))
        .route("/api/program/{source}/take", post(take))
        .route("/api/program/{source}/overlay", post(overlay_on))
        .route("/api/program/{source}/levels", get(levels))
}

async fn read(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(source): UrlPath<String>) -> ApiResult<Json<Value>> {
    require(me.can("stream.view") || me.is_admin())?;
    let rec = s.recorders.get(&source).ok_or_else(|| not_found("recorder"))?;
    let programs = &s.streams.programs;
    let langs = rec.status.lock().unwrap().media.as_ref().map(|m| (0..m.audio_tracks as usize).map(|k| m.audio_langs.get(k).cloned().unwrap_or_default()).collect::<Vec<_>>()).unwrap_or_default();
    Ok(Json(json!({ "program": programs.get(&source), "tracks": langs, "running": programs.is_running(&source) })))
}

/// The settings (not what is on air: CUT, AUTO and the overlay's button say that).
async fn write(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(source): UrlPath<String>, Json(mut p): Json<Program>) -> ApiResult<Json<Value>> {
    require(me.can("stream.run"))?;
    s.recorders.get(&source).ok_or_else(|| not_found("recorder"))?;
    p.normalize().map_err(bad)?;
    let programs = &s.streams.programs;
    let now = programs.get(&source);
    p.image_on = now.image_on;
    p.overlay_on = now.overlay_on;
    programs.put(&source, p.clone(), 0.0).map_err(bad)?;
    Ok(Json(json!(p)))
}

#[derive(Deserialize)]
struct Take {
    /// The image on air (else the source).
    image: bool,
}

/// The length of the fade between the source and the image, s.
const FADE_SECS: f64 = 1.0;

async fn take(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(source): UrlPath<String>, Json(t): Json<Take>) -> ApiResult<Json<Value>> {
    require(me.can("stream.run"))?;
    let rec = s.recorders.get(&source).ok_or_else(|| not_found("recorder"))?;
    let programs = &s.streams.programs;
    let mut p = programs.get(&source);
    if p.image_on != t.image {
        p.image_on = t.image;
        let secs = if p.fade_on { FADE_SECS } else { 0.0 };
        programs.put(&source, p.clone(), secs).map_err(bad)?;
        tracing::info!("{} puts {} on air for {}", me.username, if t.image { "the image" } else { "the source" }, rec.config().name);
    }
    Ok(Json(json!(p)))
}

#[derive(Deserialize)]
struct At {
    /// The moment (server ms) the page shows; now if not said.
    at: Option<i64>,
}

/// Each sound track's peaks, left and right (dB, -90: silence), of a moment: the one the page's
/// picture shows (it plays some seconds behind the source).
async fn levels(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(source): UrlPath<String>, axum::extract::Query(q): axum::extract::Query<At>) -> ApiResult<Json<Value>> {
    require(me.can("stream.view") || me.is_admin())?;
    let rec = s.recorders.get(&source).ok_or_else(|| not_found("recorder"))?;
    let n = rec.status.lock().unwrap().media.as_ref().map(|m| m.audio_tracks as usize).unwrap_or(0).min(4);
    let lv = rec.levels.lock().unwrap();
    // Not said: the newest measured.
    let at = q.at.or_else(|| lv.newest_at()).unwrap_or_else(now_ms);
    Ok(Json(json!({ "tracks": (0..n).map(|k| lv.read(k, at)).collect::<Vec<_>>() })))
}

#[derive(Deserialize)]
struct On {
    on: bool,
}

async fn overlay_on(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(source): UrlPath<String>, Json(b): Json<On>) -> ApiResult<Json<Value>> {
    require(me.can("stream.run"))?;
    s.recorders.get(&source).ok_or_else(|| not_found("recorder"))?;
    let programs = &s.streams.programs;
    let mut p = programs.get(&source);
    p.overlay_on = b.on;
    programs.put(&source, p.clone(), 0.0).map_err(bad)?;
    Ok(Json(json!(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(tracks: usize) -> Shape {
        Shape { w: 1920, h: 1080, fps: 25, tracks, kbps: 8000 }
    }
    fn ports() -> Ports {
        Ports { tap: 1, image: 2, overlay: 3, zmq: 4, out: 5 }
    }

    #[test]
    fn mixes() {
        let mut p = Program::default();
        assert_eq!(p.on(3), vec![0]);
        p.tracks = vec![Track { on: false, gain: 0.0 }, Track { on: true, gain: -6.0 }, Track { on: true, gain: 0.0 }];
        assert_eq!((p.weights(&p.on(3)), p.on(3)), ("0.501187|1".to_string(), vec![1, 2]));
        assert_eq!(p.weights(&[2]), "1");
        p.channels = "mono".into();
        assert_eq!(p.channel_weights(), ("0.5|0.5", "0.5|0.5"));
        assert_eq!(trim(1.0), "1");
        assert_eq!(trim(0.0), "0");
        assert_eq!(trim(0.25), "0.25");
    }

    #[test]
    fn normalized() {
        let mut p = Program { x: 3.0, y: -1.0, size: 500.0, channels: "lr".into(), ..Default::default() };
        p.normalize().unwrap();
        assert_eq!((p.x, p.y, p.size), (1.0, 0.0, 100.0));
        p.channels = "x".into();
        assert!(p.normalize().is_err());
        p.channels = "l".into();
        p.size = f64::NAN;
        assert!(p.normalize().is_err());
    }

    #[test]
    fn args() {
        let mut p = Program { image_on: true, ..Default::default() };
        p.tracks = vec![Track { on: true, gain: 0.0 }, Track { on: true, gain: -6.0 }];
        let a = ffmpeg_args(&p, &shape(3), &ports(), &p.on(3));
        let g = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(g.contains("zmq=bind_address=tcp\\\\://127.0.0.1\\\\:4"), "{g}");
        assert!(g.contains("lutyuv@img=a=val*1:enable=1"));
        assert!(g.contains("[0:a:1]aresample"));
        assert!(g.contains("amix@trk=inputs=2:weights=1|0.501187") && !g.contains("[0:a:2]"), "{g}");
        assert!(g.contains("volume@pgm=0,volume@mst=1,alimiter=limit=0.891"));
        assert!(a.contains(&"tcp://127.0.0.1:2".to_string()) && a.contains(&"udp://127.0.0.1:5?pkt_size=1316".to_string()));
        // No sound, or no track mixed: silence.
        for (p, n) in [(Program::default(), 0), (p, 2)] {
            let a = ffmpeg_args(&p, &shape(n), &ports(), &[]);
            assert!(a.contains(&"anullsrc=r=48000:cl=stereo".to_string()));
            let g = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
            assert!(g.contains("[3:a]volume@pgm=") && !g.contains("[0:a:"), "{g}");
        }
    }

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
        let inlet = Inlet { port: 0, to: Default::default(), seen: Arc::new(Mutex::new(s)), done: Default::default() };
        let all = [0, 1, 2];
        // Track 1 silent since 2 s: left out; track 2, back for 1 s only: not yet.
        assert_eq!(inlet.mixed(&all, 3, None, 6000), vec![0, 2]);
        assert_eq!(inlet.mixed(&all, 3, Some(&[0]), 6000), vec![0]);
        assert_eq!(inlet.mixed(&all, 3, Some(&[0, 1, 2]), 6000), vec![0, 2]);
        {
            let mut s = inlet.seen.lock().unwrap();
            for t in (6100..=11000).step_by(100) {
                s.packet(&es(0x102), t);
                s.packet(&es(0x105), t);
            }
        }
        assert_eq!(inlet.mixed(&all, 3, Some(&[0]), 11000), vec![0, 2]);
        // Not as many tracks as said: all of those on.
        assert_eq!(inlet.mixed(&all, 4, Some(&[0]), 6000), vec![0, 1, 2]);
    }

    #[test]
    fn overlay_places() {
        let p = Program { size: 20.0, x: 0.5, y: 0.25, ..Default::default() };
        let f = overlay_filter(&p, &shape(1));
        assert!(f.contains("scale=w=384:h=1080") && f.contains("pad=1920:1080:min(960\\,ow-iw):min(270\\,oh-ih):color=black@0"), "{f}");
    }

    #[test]
    fn plain_pictures() {
        let sh = Shape { w: 4, h: 2, fps: 25, tracks: 0, kbps: 1 };
        assert_eq!(plain(&sh, false), vec![16, 16, 16, 16, 16, 16, 16, 16, 128, 128, 128, 128, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(plain(&sh, true).len(), sh.frame_bytes());
    }

    #[test]
    fn zmq_frames() {
        assert_eq!(zmq_frame(b"ab", 1), vec![1, 2, b'a', b'b']);
        let long = vec![0u8; 300];
        let f = zmq_frame(&long, 0);
        assert_eq!(f[0], 2);
        assert_eq!(&f[1..9], &300u64.to_be_bytes());
    }
}
