//! Streamer: the programme of a source — what its lives send. The source, or an image; a PNG
//! overlay placed anywhere over both; passed together (« Passer ») at once or by a fade (its length
//! chosen), an overlay moved coming over its old place by the same fade; the sound of the tracks
//! chosen, at their level, on the channels chosen (none under the image); the source gone, its
//! last picture (what goes out never stops). Its settings are the source's, shared by all its
//! lives (`program.json`).
//!
//! Made from the first live that takes it to the last: the source's decoder, the mixer (a frame
//! synchronizer: `crate::mixer`) and the encoder (`crate::engine`), to a relay the lives read.
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
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
    process::Command,
    sync::{mpsc, watch},
};

use crate::{
    api::{bad, not_found, require, ApiResult},
    auth::CurrentUser,
    engine::{rand_key, Ends, Untap, Watch},
    meter::Meters,
    mixer::{Inputs, Mixer, Pics, Rate, TRACKS},
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
    /// What is passed (the source or the image, the overlay) passes by a fade, else at once.
    pub fade_on: bool,
    /// The fade's length, s.
    pub fade_secs: f64,
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
}

impl Default for Program {
    fn default() -> Self {
        Program {
            image: None,
            image_on: false,
            fade_on: true,
            fade_secs: 1.0,
            overlay: None,
            overlay_on: false,
            x: 0.79,
            y: 0.05,
            size: 18.0,
            opacity: 100.0,
            tracks: Vec::new(),
            channels: "lr".into(),
            master: 0.0,
        }
    }
}

impl Program {
    fn normalize(&mut self) -> Result<(), String> {
        if !matches!(self.channels.as_str(), "lr" | "l" | "r" | "mono") {
            return Err("the channels are lr, l, r or mono".into());
        }
        let ok = |v: f64| v.is_finite();
        if ![self.x, self.y, self.size, self.opacity, self.master, self.fade_secs].into_iter().all(ok) || self.tracks.iter().any(|t| !ok(t.gain)) {
            return Err("a value is not a number".into());
        }
        self.master = self.master.clamp(-40.0, 12.0);
        self.fade_secs = self.fade_secs.clamp(0.2, 5.0);
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

    pub(crate) fn overlay_look(&self) -> (Option<&String>, u64, u64, u64) {
        (self.overlay.as_ref(), (self.x * 10_000.0) as u64, (self.y * 10_000.0) as u64, (self.size * 100.0) as u64)
    }
}

/// What the programme is made of: its size and rate, its tracks, its bitrate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Shape {
    pub w: u32,
    pub h: u32,
    /// Its rate, exactly the source's (29.97: 30000/1001).
    pub rate: Rate,
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
            rate: m.filter(|m| m.fps_den > 0 && (10.0..=61.0).contains(&m.fps())).map_or(Rate { num: 25, den: 1 }, |m| Rate { num: m.fps_num, den: m.fps_den }),
            tracks: m.map(|m| m.audio_tracks as usize).unwrap_or(1).min(TRACKS),
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
    /// What was passed (the image, the overlay): by a fade of this many seconds (0: cut).
    pub fade: f64,
}

/// A programme being made.
pub struct Runner {
    /// The lives reading it (as `Rec::taps`).
    pub taps: Mutex<Vec<(u64, u16)>>,
    pub(crate) last_packet: AtomicI64,
    users: AtomicUsize,
    tx: mpsc::UnboundedSender<Change>,
    stop: watch::Sender<bool>,
    /// Why its decoder or its encoder stopped last (said to the lives).
    pub(crate) failed: Mutex<String>,
    /// A picture of the source went in (before: the live waits for it).
    pictured: Arc<AtomicBool>,
    /// Its encoder's runs: one started again begins its timestamps again, the lives connect again.
    pub(crate) epoch: AtomicU64,
    /// Where its making is (said to the lives while they wait for it).
    pub(crate) stage: Mutex<&'static str>,
    /// It is no longer made (it could not be): the lives take another.
    ended: AtomicBool,
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
    /// Where its making is.
    pub fn stage(&self) -> &'static str {
        *self.stage.lock().unwrap()
    }
    pub(crate) fn at_stage(&self, s: &'static str) {
        *self.stage.lock().unwrap() = s;
    }
    /// The run of its encoder.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }
    /// It is no longer made: a live takes another (`Programs::acquire`).
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }
}

/// The end of a programme's making, however it ends: it is no longer the source's.
struct Ended(Arc<Programs>, String, Arc<Runner>);
impl Drop for Ended {
    fn drop(&mut self) {
        self.2.ended.store(true, Ordering::Relaxed);
        let mut running = self.0.running.lock().unwrap();
        if running.get(&self.1).is_some_and(|x| Arc::ptr_eq(x, &self.2)) {
            running.remove(&self.1);
        }
    }
}

pub struct Programs {
    data_dir: PathBuf,
    recorders: Arc<Manager>,
    events: Events,
    all: Mutex<BTreeMap<String, Program>>,
    running: Mutex<HashMap<String, Arc<Runner>>>,
    /// The levels of the sources' sound, for the Streamer's meters.
    pub meters: Arc<Meters>,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("program.json")
}

impl Programs {
    pub fn start(data_dir: &Path, recorders: Arc<Manager>, events: Events) -> Arc<Programs> {
        let all = crate::paths::read_json(&path(data_dir)).unwrap_or_default();
        let meters = Meters::new(recorders.clone());
        Arc::new(Programs { data_dir: data_dir.to_path_buf(), recorders, events, all: Mutex::new(all), running: Mutex::new(HashMap::new()), meters })
    }

    pub fn get(&self, source: &str) -> Program {
        self.all.lock().unwrap().get(source).cloned().unwrap_or_default()
    }

    pub fn is_running(&self, source: &str) -> bool {
        self.running.lock().unwrap().contains_key(source)
    }

    /// Saves a programme, and gives it to its mixer if it runs.
    fn put(&self, source: &str, p: Program, fade: f64) -> Result<(), String> {
        let all = {
            let mut all = self.all.lock().unwrap();
            all.insert(source.to_string(), p.clone());
            all.clone()
        };
        crate::paths::write_json(&path(&self.data_dir), &all, false).map_err(|e| e.to_string())?;
        if let Some(r) = self.running.lock().unwrap().get(source) {
            let _ = r.tx.send(Change { program: p, fade });
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
        let r = Arc::new(Runner { taps: Mutex::new(Vec::new()), last_packet: AtomicI64::new(0), users: AtomicUsize::new(1), failed: Mutex::new(String::new()), pictured: Default::default(), epoch: AtomicU64::new(0), stage: Mutex::new("waiting for the source's description"), ended: AtomicBool::new(false), tx, stop: watch::channel(false).0 });
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
    /// decoder as long as the source comes.
    async fn run(self: Arc<Self>, source: String, r: Arc<Runner>, mut rx: mpsc::UnboundedReceiver<Change>) {
        let _ended = Ended(self.clone(), source.clone(), r.clone());
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
        r.at_stage("making its pictures");
        let mut p = self.get(&source);
        let pics = Arc::new(Mutex::new(Pics { image: Arc::new(self.image(&ffmpeg, &p, &sh, &name).await), overlay: Arc::new(self.overlay(&ffmpeg, &p, &sh).await), overlay_was: Arc::new(Vec::new()) }));
        // The delay a picture is played after its coming: a source in segments (HLS, the web) comes
        // a segment at a time.
        let delay = if crate::engine::in_segments(&rec) { 10 } else { 1 };
        let inputs = Arc::new(Inputs::new(sh.rate, delay));
        let sink = Arc::new(Mutex::new(None));
        let done = Arc::new(AtomicBool::new(false));
        let _end = Ends(done.clone());
        let (changes, crx) = std::sync::mpsc::channel();
        if let Err(e) = (Mixer { shape: sh, program: p.clone(), inputs: inputs.clone(), pics: pics.clone(), changes: crx, sink: sink.clone(), done: done.clone(), pictured: r.pictured.clone() }).spawn() {
            *r.failed.lock().unwrap() = e.to_string();
            return;
        }
        // The source's tracks, read as they pass (which send something).
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
        tokio::spawn(crate::engine::encode(ffmpeg.clone(), r.clone(), sh, sink, r.stop.subscribe()));
        tokio::spawn(crate::engine::decode(self.recorders.clone(), source.clone(), sh, inputs.clone(), watch, r.clone(), r.stop.subscribe()));
        // Its first picture of the source: said in the journal; none after a while: said, with
        // where the making is.
        // Still none after a minute while the source comes: made again (the lives take the new
        // one), as stopping and starting the live again does.
        {
            let (r, name, mut stop, rec) = (r.clone(), name.clone(), r.stop.subscribe(), rec.clone());
            tokio::spawn(async move {
                let since = Instant::now();
                let mut warned = false;
                loop {
                    if r.pictured() {
                        tracing::info!("programme of {name}: the source's first picture ({:.1} s)", since.elapsed().as_secs_f64());
                        return;
                    }
                    if !warned && since.elapsed() > Duration::from_secs(40) {
                        warned = true;
                        tracing::warn!("programme of {name}: no picture of the source after 40 s ({}; {} pictures decoded; the programme {})", r.stage(), inputs.came(), if r.flowing() { "comes out" } else { "does not come out" });
                    }
                    let comes = rec.state() == crate::recorder::State::Recording && now_ms() - rec.last_packet.load(Ordering::Relaxed) < 15_000;
                    if since.elapsed() > Duration::from_secs(60) && comes {
                        tracing::warn!("programme of {name}: no picture of the source after 60 s while it comes ({}; {} pictures decoded): made again", r.stage(), inputs.came());
                        let _ = r.stop.send(true);
                        return;
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                        _ = stop.changed() => return,
                    }
                }
            });
        }
        tracing::info!("programme of {name}: made ({}x{}, {}/{} fps, {} kb/s)", sh.w, sh.h, sh.rate.num, sh.rate.den, sh.kbps);
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
                        let ovl = Arc::new(self.overlay(&ffmpeg, &c.program, &sh).await);
                        // The one on air kept: the new one comes over it by the fade.
                        let mut pics = pics.lock().unwrap();
                        pics.overlay_was = std::mem::replace(&mut pics.overlay, ovl);
                    }
                    p = c.program.clone();
                    let _ = changes.send(c);
                }
            }
            if *stop.borrow() {
                break;
            }
        }
        tracing::info!("programme of {name}: stopped");
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
    // Each track's language and kind (as the stream says them).
    let (langs, kinds) = rec.status.lock().unwrap().media.as_ref().map(|m| {
        let at = |v: &Vec<String>, k: usize| v.get(k).cloned().unwrap_or_default();
        ((0..m.audio_tracks as usize).map(|k| at(&m.audio_langs, k)).collect::<Vec<_>>(), (0..m.audio_tracks as usize).map(|k| at(&m.audio_kinds, k)).collect::<Vec<_>>())
    }).unwrap_or_default();
    Ok(Json(json!({ "program": programs.get(&source), "tracks": langs, "kinds": kinds, "running": programs.is_running(&source) })))
}

/// The settings (not what is on air, nor where the overlay is: « Passer » says that).
async fn write(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(source): UrlPath<String>, Json(mut p): Json<Program>) -> ApiResult<Json<Value>> {
    require(me.can("stream.run"))?;
    s.recorders.get(&source).ok_or_else(|| not_found("recorder"))?;
    p.normalize().map_err(bad)?;
    let programs = &s.streams.programs;
    let now = programs.get(&source);
    p.image_on = now.image_on;
    p.overlay_on = now.overlay_on;
    (p.x, p.y, p.size) = (now.x, now.y, now.size);
    programs.put(&source, p.clone(), 0.0).map_err(bad)?;
    Ok(Json(json!(p)))
}

#[derive(Deserialize)]
struct Take {
    /// The image on air (else the source).
    image: bool,
    /// The overlay as it was made ready: on air or not, where, how big.
    overlay: Option<OverlayAt>,
}

#[derive(Deserialize)]
struct OverlayAt {
    on: bool,
    x: f64,
    y: f64,
    size: f64,
}

async fn take(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(source): UrlPath<String>, Json(t): Json<Take>) -> ApiResult<Json<Value>> {
    require(me.can("stream.run"))?;
    let rec = s.recorders.get(&source).ok_or_else(|| not_found("recorder"))?;
    let programs = &s.streams.programs;
    let was = programs.get(&source);
    let mut p = was.clone();
    p.image_on = t.image;
    if let Some(o) = t.overlay {
        p.overlay_on = o.on && p.overlay.is_some();
        (p.x, p.y, p.size) = (o.x, o.y, o.size);
        p.normalize().map_err(bad)?;
    }
    if p != was {
        let secs = if p.fade_on { p.fade_secs } else { 0.0 };
        programs.put(&source, p.clone(), secs).map_err(bad)?;
        tracing::info!("{} passes {}{} on air for {}", me.username, if t.image { "the image" } else { "the source" }, if p.overlay_on { " and the overlay" } else { "" }, rec.config().name);
    }
    Ok(Json(json!(p)))
}

#[derive(Deserialize)]
struct At {
    /// The timestamp (the source's, ms) of the frame the page shows; none: the newest measured.
    t: Option<i64>,
}

/// Each sound track's peaks, each of its channels (dB, -90: silence; none yet: none), of the frame
/// the page shows (it plays some seconds behind the source): by that frame's own timestamp.
async fn levels(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(source): UrlPath<String>, axum::extract::Query(q): axum::extract::Query<At>) -> ApiResult<Json<Value>> {
    require(me.can("stream.view") || me.is_admin())?;
    let rec = s.recorders.get(&source).ok_or_else(|| not_found("recorder"))?;
    let n = rec.status.lock().unwrap().media.as_ref().map_or(0, |m| m.audio_tracks as usize);
    Ok(Json(json!({ "tracks": s.streams.programs.meters.read(&source, n, q.t) })))
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
    let secs = if p.fade_on { p.fade_secs } else { 0.0 };
    programs.put(&source, p.clone(), secs).map_err(bad)?;
    Ok(Json(json!(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(tracks: usize) -> Shape {
        Shape { w: 1920, h: 1080, rate: Rate { num: 25, den: 1 }, tracks, kbps: 8000 }
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
    fn overlay_places() {
        let p = Program { size: 20.0, x: 0.5, y: 0.25, ..Default::default() };
        let f = overlay_filter(&p, &shape(1));
        assert!(f.contains("scale=w=384:h=1080") && f.contains("pad=1920:1080:min(960\\,ow-iw):min(270\\,oh-ih):color=black@0"), "{f}");
    }

    #[test]
    fn plain_pictures() {
        let sh = Shape { w: 4, h: 2, rate: Rate { num: 25, den: 1 }, tracks: 0, kbps: 1 };
        assert_eq!(plain(&sh, false), vec![16, 16, 16, 16, 16, 16, 16, 16, 128, 128, 128, 128, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(plain(&sh, true).len(), sh.frame_bytes());
    }
}
