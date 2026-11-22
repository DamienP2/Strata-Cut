//! Streamer: the programme of a source — what its lives send. The source, or an image passed at
//! once or by a fade (1 s); a PNG overlay placed anywhere over both; the sound of the tracks
//! chosen, at their level, on the channels chosen (none under the image); while the source is
//! lost, the slate. Its settings are the source's, shared by all its lives (`program.json`).
//!
//! Made like a mixer, from the first live that takes it to the last: decoders read the source's
//! relay (its picture, each of its sound tracks: each on its own, a track that sends nothing holds
//! nothing back) and give raw pictures and sound to the mixer (`crate::mixer`), which makes a
//! picture every frame and its sound on its own clock, whatever comes in; an encoder makes H.264
//! and AAC of it, in MPEG-TS, to a relay the lives read. The decoders come and go with the source;
//! the mixer, the encoder and the lives do not stop.
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io::Write,
    net::{Ipv4Addr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
        mpsc::{channel, Receiver, RecvTimeoutError},
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
    io::{AsyncBufReadExt, AsyncReadExt},
    process::Command,
    sync::{mpsc, watch},
};

use crate::{
    api::{bad, not_found, require, ApiResult},
    auth::CurrentUser,
    mixer::{Inputs, Mixer, Pics, Sink, TRACKS},
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
    /// While the source is lost, the slate goes out (else its last picture).
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

    pub(crate) fn track(&self, k: usize) -> Track {
        self.tracks.get(k).cloned().unwrap_or(Track { on: k == 0, gain: 0.0 })
    }

    /// The overlay's opacity on air (0 when it is not).
    pub(crate) fn overlay_alpha(&self) -> f64 {
        if self.overlay_on && self.overlay.is_some() { self.opacity / 100.0 } else { 0.0 }
    }

    fn overlay_look(&self) -> (Option<&String>, u64, u64, u64) {
        (self.overlay.as_ref(), (self.x * 10_000.0) as u64, (self.y * 10_000.0) as u64, (self.size * 100.0) as u64)
    }
}

/// What the programme is made of: its size and rate, its tracks, its bitrate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Shape {
    pub w: u32,
    pub h: u32,
    pub fps: u32,
    pub tracks: usize,
    pub kbps: u32,
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
    pub fn frame_bytes(&self) -> usize {
        (self.w * self.h) as usize * 5 / 2
    }
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
pub(crate) fn plain(sh: &Shape, opaque: bool) -> Vec<u8> {
    let n = (sh.w * sh.h) as usize;
    let mut v = Vec::with_capacity(sh.frame_bytes());
    v.resize(n, if opaque { 18 } else { 16 });
    v.resize(n + n / 2, 128);
    v.resize(n * 5 / 2, if opaque { 255 } else { 0 });
    v
}

// ---------------------------------------------------------------- the programmes

/// A change for a running programme, and how it is shown.
pub(crate) struct Change {
    pub program: Program,
    /// The image passed: by a fade of this many seconds (0: cut).
    pub image_fade: f64,
}

/// A programme being made.
pub struct Runner {
    /// The lives reading it (as `Rec::taps`).
    pub taps: Mutex<Vec<(u64, u16)>>,
    last_packet: AtomicI64,
    users: AtomicUsize,
    tx: mpsc::UnboundedSender<Change>,
    stop: watch::Sender<bool>,
    /// Why its encoder stopped last (said to the lives waiting for it).
    failed: Mutex<String>,
    /// A picture of the source went in (before: the live waits for it).
    pictured: Arc<AtomicBool>,
    /// Its encoder's runs: one started again begins its timestamps again, the lives connect again.
    epoch: AtomicU64,
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
    /// The source's picture is in it.
    pub fn pictured(&self) -> bool {
        self.pictured.load(Ordering::Relaxed)
    }
    /// The run of its encoder.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
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

    pub fn is_running(&self, source: &str) -> bool {
        self.running.lock().unwrap().contains_key(source)
    }

    /// Saves a programme, and gives it to its mixer if it runs.
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
        let r = Arc::new(Runner { taps: Mutex::new(Vec::new()), last_packet: AtomicI64::new(0), users: AtomicUsize::new(1), failed: Mutex::new(String::new()), pictured: Default::default(), epoch: AtomicU64::new(0), tx, stop: watch::channel(false).0 });
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

    /// Made while it is wanted: the mixer and the encoder from the first live to the last, the
    /// decoders as long as the source comes (started again when it comes back).
    async fn run(self: Arc<Self>, source: String, r: Arc<Runner>, mut rx: mpsc::UnboundedReceiver<Change>) {
        let mut stop = r.stop.subscribe();
        let ffmpeg = self.recorders.ffmpeg.clone();
        // Its size and rate are the source's: a moment to know them when it was not received yet.
        let since = Instant::now();
        loop {
            if *stop.borrow() {
                return;
            }
            let Some(rec) = self.recorders.get(&source) else { return };
            if (rec.flowing() && rec.status.lock().unwrap().media.is_some()) || since.elapsed() > Duration::from_secs(20) {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(300)) => {}
                _ = stop.changed() => {}
            }
        }
        let Some(rec) = self.recorders.get(&source) else { return };
        let (sh, name) = (Shape::of(&rec), rec.config().name);
        let mut p = self.get(&source);
        let mut slate_file = crate::slates::image_for(&self.data_dir, &source);
        let pics = Arc::new(Mutex::new(Pics {
            image: Arc::new(self.image(&ffmpeg, &p, &sh, &name).await),
            overlay: Arc::new(self.overlay(&ffmpeg, &p, &sh).await),
            slate: Arc::new(self.picture(&ffmpeg, slate_file.as_deref(), &sh, &name).await),
        }));
        let inputs = Arc::new(Inputs::default());
        let sink = Arc::new(Mutex::new(None));
        let done = Arc::new(AtomicBool::new(false));
        let _end = Ended(done.clone());
        let (changes, crx) = std::sync::mpsc::channel();
        if let Err(e) = (Mixer { shape: sh, program: p.clone(), inputs: inputs.clone(), pics: pics.clone(), changes: crx, sink: sink.clone(), done: done.clone(), pictured: r.pictured.clone() }).spawn() {
            *r.failed.lock().unwrap() = e.to_string();
            return;
        }
        // The source's sound tracks, read as they pass (which send something).
        let watch = match Watch::open() {
            Ok(w) => Arc::new(w),
            Err(e) => {
                *r.failed.lock().unwrap() = e;
                return;
            }
        };
        let key = rand_key();
        rec.taps.lock().unwrap().push((key, watch.port));
        let _untap = Untap(rec.clone(), key);
        tokio::spawn(self.clone().encode(r.clone(), sh, sink, r.stop.subscribe()));
        tokio::spawn(self.clone().decode(source.clone(), sh, None, inputs.clone(), watch.clone(), r.stop.subscribe()));
        for k in 0..TRACKS {
            tokio::spawn(self.clone().decode(source.clone(), sh, Some(k), inputs.clone(), watch.clone(), r.stop.subscribe()));
        }
        tracing::info!("programme of {name}: made ({}x{}, {} fps, {} kb/s)", sh.w, sh.h, sh.fps, sh.kbps);
        let mut look = tokio::time::interval(Duration::from_secs(3));
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                c = rx.recv() => {
                    let Some(c) = c else { break };
                    if c.program.image != p.image {
                        let img = self.image(&ffmpeg, &c.program, &sh, &name).await;
                        pics.lock().unwrap().image = Arc::new(img);
                    }
                    if c.program.overlay_look() != p.overlay_look() {
                        let ovl = self.overlay(&ffmpeg, &c.program, &sh).await;
                        pics.lock().unwrap().overlay = Arc::new(ovl);
                    }
                    p = c.program.clone();
                    let _ = changes.send(c);
                }
                // The source's slate (chosen in the settings): made again when it changes.
                _ = look.tick() => {
                    let now = crate::slates::image_for(&self.data_dir, &source);
                    if now != slate_file {
                        slate_file = now;
                        let s = self.picture(&ffmpeg, slate_file.as_deref(), &sh, &name).await;
                        pics.lock().unwrap().slate = Arc::new(s);
                    }
                }
            }
            if *stop.borrow() {
                break;
            }
        }
        tracing::info!("programme of {name}: stopped");
    }

    /// A decoder of the source: its picture (`track` None), at the programme's size and rate, or
    /// one of its sound tracks, to the mixer. Started again while the programme is wanted, when
    /// the source comes (a track: when it sends something).
    async fn decode(self: Arc<Self>, source: String, sh: Shape, track: Option<usize>, inputs: Arc<Inputs>, watch: Arc<Watch>, mut stop: watch::Receiver<bool>) {
        let ffmpeg = self.recorders.ffmpeg.clone();
        let mut said = String::new();
        loop {
            if *stop.borrow() {
                return;
            }
            let ready = |rec: &Arc<Rec>| rec.flowing() && track.is_none_or(|k| watch.alive(k));
            if let Some(rec) = self.recorders.get(&source).filter(ready) {
                let (args, bytes) = match track {
                    None => (picture_args(&sh), FrameBytes::Fixed(sh.frame_bytes() * 3 / 5)),
                    Some(k) => (sound_args(k), FrameBytes::PerSample(8)),
                };
                let why = decoding(&ffmpeg, &rec, args, bytes, &mut stop, |pts, b| match track {
                    None => inputs.picture(pts, b),
                    Some(k) => inputs.sound(k, pts, &b.as_chunks::<4>().0.iter().map(|x| f32::from_le_bytes(*x)).collect::<Vec<_>>()),
                })
                .await;
                // Said once (a source that comes and goes would fill the journal).
                if !why.is_empty() && why != said {
                    tracing::debug!("programme of {}: decoder {track:?}: {why}", rec.config().name);
                    said = why;
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = stop.changed() => {}
            }
        }
    }

    /// The encoder, made again if it stops, until the programme ends.
    async fn encode(self: Arc<Self>, r: Arc<Runner>, sh: Shape, sink: Arc<Mutex<Option<Sink>>>, mut stop: watch::Receiver<bool>) {
        loop {
            if *stop.borrow() {
                return;
            }
            if let Err(e) = self.encode_once(&r, &sh, &sink, &mut stop).await {
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

    async fn encode_once(&self, r: &Arc<Runner>, sh: &Shape, sink: &Arc<Mutex<Option<Sink>>>, stop: &mut watch::Receiver<bool>) -> Result<(), String> {
        let listen = || TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string());
        let (lv, la) = (listen()?, listen()?);
        let port = |l: &TcpListener| l.local_addr().map(|a| a.port()).map_err(|e| e.to_string());
        let (vp, ap) = (port(&lv)?, port(&la)?);
        let out = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        let op = out.local_addr().map_err(|e| e.to_string())?.port();
        let done = Arc::new(AtomicBool::new(false));
        let _end = Ended(done.clone());
        let (vtx, vrx) = channel();
        let (atx, arx) = channel();
        let queued = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
        write_to(lv, vrx, queued[0].clone(), done.clone());
        write_to(la, arx, queued[1].clone(), done.clone());
        relay(out, r.clone(), done.clone());
        // A second waiting at most (the encoder starting, the server busy).
        *sink.lock().unwrap() = Some(Sink { video: vtx, audio: atx, queued, most: sh.fps.max(1) as usize });
        r.epoch.fetch_add(1, Ordering::Relaxed);
        let mut cmd = Command::new(&self.recorders.ffmpeg);
        cmd.args(encoder_args(sh, vp, ap, op)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("cannot start {}: {e}", self.recorders.ffmpeg))?;
        let last = last_words(&mut child);
        tokio::select! {
            status = child.wait() => {
                let why = last.lock().unwrap().last().cloned().unwrap_or_else(|| format!("{status:?}"));
                Err(format!("the encoder stopped: {why}"))
            }
            _ = stop.changed() => {
                let _ = child.kill().await;
                Ok(())
            }
        }
    }

    /// The image, at the programme's size: the one chosen, else the source's name on a dark picture.
    async fn image(&self, ffmpeg: &str, p: &Program, sh: &Shape, name: &str) -> Vec<u8> {
        let file = p.image.as_deref().and_then(|id| crate::slates::file_of(&self.data_dir, id));
        self.picture(ffmpeg, file.as_deref(), sh, name).await
    }

    /// A picture at the programme's size: a file, else the source's name on a dark picture.
    async fn picture(&self, ffmpeg: &str, file: Option<&Path>, sh: &Shape, name: &str) -> Vec<u8> {
        let (w, h) = (sh.w, sh.h);
        let made = match file {
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

/// The source's relay as a decoder reads it (`{input}` in the arguments).
const INPUT: &str = "{input}";

/// The source's timestamps kept (the picture's and the sound's, of the same clock), and each
/// frame said with its own (`showinfo`).
fn decoder_head() -> Vec<String> {
    ["-hide_banner", "-nostdin", "-nostats", "-loglevel", "info", "-copyts", "-thread_queue_size", "4096", "-analyzeduration", "1000000", "-probesize", "3000000", "-f", "mpegts", "-i", INPUT].map(String::from).to_vec()
}

/// The source's picture: deinterlaced, at the programme's size (its shape kept, bands added) and
/// rate, raw.
fn picture_args(sh: &Shape) -> Vec<String> {
    let (w, h, fps) = (sh.w, sh.h, sh.fps);
    let mut a = decoder_head();
    a.extend(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"].map(String::from));
    a.push(format!("bwdif=mode=send_frame:deint=interlaced,scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps},format=yuv420p,showinfo=checksum=0"));
    a.extend(["-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"].map(String::from));
    a
}

/// One of the source's sound tracks: 48 kHz stereo, raw.
fn sound_args(k: usize) -> Vec<String> {
    let mut a = decoder_head();
    a.extend(["-map".to_string(), format!("0:a:{k}")]);
    a.extend(["-vn", "-sn", "-dn", "-af", "aresample=48000,aformat=sample_fmts=flt:channel_layouts=stereo,ashowinfo", "-f", "f32le", "-ar", "48000", "-ac", "2", "pipe:1"].map(String::from));
    a
}

/// The encoder: the mixer's pictures and sound (raw, over TCP: their timestamps are their count,
/// so they stay together), H.264 and AAC in MPEG-TS to the programme's relay.
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

/// The size of a decoder's frames: a picture's, or so many bytes a sample.
#[derive(Clone, Copy)]
enum FrameBytes {
    Fixed(usize),
    PerSample(usize),
}

/// A frame said by `showinfo` or `ashowinfo`: its timestamp (s) and its samples (sound).
fn frame_said(line: &str) -> Option<(Option<f64>, Option<usize>)> {
    if !line.contains("showinfo") || !line.contains(" n:") {
        return None;
    }
    let field = |k: &str| line.split_once(k).map(|x| x.1.split_whitespace().next().unwrap_or(""));
    Some((field("pts_time:").and_then(|v| v.parse().ok()), field("nb_samples:").and_then(|v| v.parse().ok())))
}

/// One run of a decoder: it reads the source's relay (a tap of its own), and gives each frame it
/// makes to `take` with its timestamp, until it ends (its last words) or the programme stops.
async fn decoding(ffmpeg: &str, rec: &Arc<Rec>, args: Vec<String>, bytes: FrameBytes, stop: &mut watch::Receiver<bool>, mut take: impl FnMut(f64, Vec<u8>)) -> String {
    let port = match free_udp() {
        Ok(p) => p,
        Err(e) => return e,
    };
    let input = format!("udp://127.0.0.1:{port}?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout=5000000");
    let args: Vec<String> = args.into_iter().map(|a| if a == INPUT { input.clone() } else { a }).collect();
    let key = rand_key();
    rec.taps.lock().unwrap().push((key, port));
    let _untap = Untap(rec.clone(), key);
    let mut cmd = Command::new(ffmpeg);
    cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("cannot start {ffmpeg}: {e}"),
    };
    let (Some(mut out), Some(err)) = (child.stdout.take(), child.stderr.take()) else { return "no output".into() };
    let mut lines = tokio::io::BufReader::new(err).lines();
    let mut last: VecDeque<String> = VecDeque::new();
    // Each frame is said (its timestamp) before it is written: said, then read.
    let mut pts = 0.0;
    loop {
        let line = tokio::select! {
            l = lines.next_line() => match l {
                Ok(Some(l)) => l,
                _ => break,
            },
            _ = stop.changed() => {
                let _ = child.kill().await;
                return String::new();
            }
        };
        let Some((t, samples)) = frame_said(&line) else {
            last.push_back(line);
            if last.len() > 20 {
                last.pop_front();
            }
            continue;
        };
        let n = match bytes {
            FrameBytes::Fixed(n) => n,
            FrameBytes::PerSample(k) => samples.unwrap_or(0) * k,
        };
        let mut buf = vec![0u8; n];
        if out.read_exact(&mut buf).await.is_err() {
            break;
        }
        // Without one (rare): just after the one before.
        pts = t.unwrap_or(pts + 0.04);
        take(pts, buf);
    }
    let _ = child.kill().await;
    // Its error, not its descriptions (the input, the mapping).
    last.into_iter().rev().find(|l| !l.starts_with(' ') && !l.starts_with("Input #") && !l.starts_with("Output #") && !l.starts_with("Stream mapping") && !l.starts_with("Press")).unwrap_or_default()
}

/// A child's last lines on its error output (20 at most).
fn last_words(child: &mut tokio::process::Child) -> Arc<Mutex<Vec<String>>> {
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
    last
}

/// Writes what the mixer makes to the encoder that connects (the first that does), counting
/// down what waits. Ends when the encoder is gone (it is made again) or the run ends.
fn write_to(listener: TcpListener, rx: Receiver<Vec<u8>>, queued: Arc<AtomicUsize>, done: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new().name("program-write".into()).spawn(move || {
        let _ = listener.set_nonblocking(true);
        let mut stream = loop {
            if done.load(Ordering::Relaxed) {
                return;
            }
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => return,
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
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) if !done.load(Ordering::Relaxed) => {}
                Err(_) => return,
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

/// Ends the threads of a run (writing to the encoder, relaying what it makes).
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

/// The source's relay, read to know its sound tracks and which send something (a decoder of a
/// track that sends nothing is not started: it would wait for it).
struct Watch {
    port: u16,
    seen: Arc<Mutex<Seen>>,
    done: Arc<AtomicBool>,
}

impl Watch {
    fn open() -> Result<Watch, String> {
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

    /// Track `k` of the source sent something in the last 2 s.
    fn alive(&self, k: usize) -> bool {
        self.seen.lock().unwrap().tracks.get(k).is_some_and(|t| now_ms() - t.1 < 2000)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

/// The audio tracks of the source's MPEG-TS, in order: their PID, their last packet and the start
/// of the packets coming without a break (ms).
#[derive(Default)]
pub(crate) struct Seen {
    pmt: Option<u16>,
    /// Its PMT was read (the tracks below are the source's).
    read: bool,
    tracks: Vec<(u16, i64, i64)>,
}

impl Seen {
    /// When an audio track last sent something (0: never).
    pub(crate) fn newest(&self) -> i64 {
        self.tracks.iter().map(|t| t.1).max().unwrap_or(0)
    }

    /// One 188-byte packet.
    pub(crate) fn packet(&mut self, p: &[u8], now: i64) {
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
            self.read = true;
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
        assert_eq!(s.newest(), 6000);
        let w = Watch { port: 0, seen: Arc::new(Mutex::new(s)), done: Default::default() };
        // Track 1 silent since 2 s: not alive; the others are (as of now: their last packet
        // is of the test's clock, so none is, a moment later).
        let alive = |w: &Watch, k: usize| w.seen.lock().unwrap().tracks.get(k).is_some_and(|t| 6000 - t.1 < 2000);
        assert!(alive(&w, 0) && !alive(&w, 1) && alive(&w, 2) && !alive(&w, 3));
        assert!(!w.alive(0));
    }

    #[test]
    fn decoders_and_encoder() {
        let sh = shape(2);
        assert_eq!(
            frame_said("[Parsed_showinfo_2 @ 0x5] n:   0 pts:     37 pts_time:1.48    duration:      1 fmt:yuv420p"),
            Some((Some(1.48), None))
        );
        assert_eq!(frame_said("[Parsed_ashowinfo_2 @ 0x5] n:0 pts:70559 pts_time:1.46998 fmt:flt channels:2 rate:48000 nb_samples:1152 checksum:86"), Some((Some(1.46998), Some(1152))));
        assert_eq!(frame_said("[Parsed_showinfo_2 @ 0x5] config in time_base: 1/25"), None);
        let v = picture_args(&sh);
        assert!(v.contains(&INPUT.to_string()) && v.windows(2).any(|w| w == ["-map", "0:v:0"]));
        assert!(v.iter().any(|x| x.starts_with("bwdif") && x.contains("scale=1920:1080") && x.contains("fps=25")));
        assert!(v.ends_with(&["-f".into(), "rawvideo".into(), "-pix_fmt".into(), "yuv420p".into(), "pipe:1".into()]));
        let a = sound_args(1);
        assert!(a.windows(2).any(|w| w == ["-map", "0:a:1"]) && a.contains(&"f32le".to_string()));
        let e = encoder_args(&sh, 1, 2, 3);
        assert!(e.contains(&"tcp://127.0.0.1:1".to_string()) && e.contains(&"tcp://127.0.0.1:2".to_string()));
        assert!(e.contains(&"udp://127.0.0.1:3?pkt_size=1316".to_string()) && e.windows(2).any(|w| w == ["-b:v", "8000k"]));
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
}
