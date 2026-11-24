//! Web pages as sources (`Protocol::Page`): a browser of the server, filmed. Each page source has
//! its own screen (Xvfb, the size of its format), its own sound card (PulseAudio, a sink only it
//! plays into) and its own Chrome, shown full screen on it. An FFmpeg films the screen and the
//! sound; the frames go through this server, which keeps the part of the page chosen (a 16:9 frame,
//! moved and resized at once, without a cut) and gives them, at the format's size, to the encoder,
//! whose MPEG-TS the recorder receives as from any other source.
//!
//! The page is chosen, browsed and reframed from the Streamer and the Extraction: its address, the
//! previous and next pages, a reload; its frame; and piloting it (the mouse and the keyboard sent to
//! it). Chrome is driven through its DevTools protocol, which also gives the page's pictures as
//! they change (a direct view, a tenth of a second behind, for the one who pilots it). The last
//! address and the last frame are kept: the source starts again on them.
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    extract::{Path as UrlPath, State},
    http::{header, HeaderValue, StatusCode},
    response::Response,
    routing::{get, post, put},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{broadcast, mpsc, oneshot, watch},
};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    api::{bad, not_found, require, ApiResult},
    auth::CurrentUser,
    recorder::{pattern::Format, Manager, Protocol, RecorderConfig},
    state::Shared,
};

/// The formats a page may have (progressive: a page has no fields).
pub const FORMATS: [&str; 6] = ["1080p25", "1080p50", "1080p2997", "1080p5994", "720p50", "720p5994"];

/// The part of the page kept, in the window's pixels: its left, its top and its width (its height
/// is 9/16 of it).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub w: f64,
}

/// What is kept of a page between its starts.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
struct Saved {
    url: String,
    crop: Option<Crop>,
}

/// A page source running.
pub struct Page {
    id: String,
    name: String,
    dir: PathBuf,
    w: u32,
    h: u32,
    rate: (u32, u32),
    sound: bool,
    kbps: u32,
    /// A constant rate (CBR), else variable (VBR).
    cbr: bool,
    /// What it was started with (its format, its sound, its bitrate): another, it starts again.
    key: String,
    /// The local port its MPEG-TS goes to (the recorder reads it).
    pub port: u16,
    crop: Mutex<Crop>,
    url: Mutex<String>,
    /// The host and port of the address the admin gave the source: reachable even on the server's
    /// network (that service alone, not the others of its machine).
    home: Option<(String, u16)>,
    /// That address (opened when the last one is no longer allowed).
    given: String,
    /// The hosts already looked at (reachable or not), for a minute.
    hosts: Mutex<HashMap<String, (Instant, bool)>>,
    cdp: Mutex<Option<Arc<Cdp>>>,
    /// The page's pictures as they change (JPEG), for the direct views.
    frames: broadcast::Sender<Bytes>,
    stop: watch::Sender<bool>,
}

pub struct Pages {
    data_dir: PathBuf,
    ffmpeg: String,
    manager: Weak<Manager>,
    running: Mutex<HashMap<String, Arc<Page>>>,
}

static PAGES: OnceLock<Arc<Pages>> = OnceLock::new();

/// Made with the recorders: the pages, and what stops those no longer wanted.
pub fn init(m: &Arc<Manager>) {
    let pages = Arc::new(Pages { data_dir: m.data_dir().to_path_buf(), ffmpeg: m.ffmpeg.clone(), manager: Arc::downgrade(m), running: Mutex::new(HashMap::new()) });
    if PAGES.set(pages.clone()).is_err() {
        return;
    }
    tokio::spawn(async move {
        // A page no longer received for a minute (its source stopped, deleted): its browser closed.
        let mut idle: HashMap<String, i64> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let Some(m) = pages.manager.upgrade() else { return };
            let now = crate::recorder::now_ms();
            let ids: Vec<String> = pages.running.lock().unwrap().keys().cloned().collect();
            for id in ids {
                let wanted = m.get(&id).is_some_and(|r| {
                    let c = r.config();
                    c.protocol == Protocol::Page && (c.enabled || m.wants(&id, now) || r.flowing())
                });
                if wanted {
                    idle.remove(&id);
                } else if now - *idle.entry(id.clone()).or_insert(now) > 60_000 {
                    idle.remove(&id);
                    pages.close(&id);
                }
            }
        }
    });
}

fn pages() -> Option<&'static Arc<Pages>> {
    PAGES.get()
}

/// The page of a source, started if it is not (or started again with its new settings): the port
/// its stream goes to.
pub fn ensure(c: &RecorderConfig) -> Option<u16> {
    let p = pages()?;
    let f = Format::parse(&c.video_mode).filter(|f| !f.interlaced).unwrap_or(Format { width: 1920, height: 1080, interlaced: false, num: 25, den: 1 });
    let sound = c.audio_channels > 0;
    let kbps = ((if c.bitrate > 0.0 { c.bitrate } else { 6.0 }) * 1000.0) as u32;
    let home = host_of(&c.url).filter(|h| h.2).map(|(h, port, _)| (h, port));
    let key = format!("{}x{}@{}/{}:{sound}:{kbps}:{}:{home:?}", f.width, f.height, f.num, f.den, c.cbr);
    let mut running = p.running.lock().unwrap();
    if let Some(page) = running.get(&c.id) {
        if page.key == key && !*page.stop.borrow() {
            return Some(page.port);
        }
        let _ = page.stop.send(true);
    }
    let dir = p.data_dir.join("pages").join(&c.id);
    let _ = std::fs::create_dir_all(&dir);
    let saved: Saved = crate::paths::read_json(&dir.join("state.json")).unwrap_or_default();
    let full = Crop { x: 0.0, y: 0.0, w: f.width as f64 };
    let port = free_port(false)?;
    let page = Arc::new(Page {
        id: c.id.clone(),
        name: c.name.clone(),
        dir,
        w: f.width,
        h: f.height,
        rate: (f.num, f.den),
        sound,
        kbps,
        cbr: c.cbr,
        key,
        port,
        crop: Mutex::new(saved.crop.map(|k| clamp(k, f.width, f.height)).unwrap_or(full)),
        url: Mutex::new(saved.url),
        home,
        given: c.url.trim().to_string(),
        hosts: Mutex::new(HashMap::new()),
        cdp: Mutex::new(None),
        frames: broadcast::channel(4).0,
        stop: watch::channel(false).0,
    });
    running.insert(c.id.clone(), page.clone());
    tokio::spawn(run(p.clone(), page));
    Some(port)
}

/// The port a running page's stream goes to (none: not running).
pub fn port(id: &str) -> Option<u16> {
    pages()?.get(id).map(|p| p.port)
}

impl Pages {
    fn close(&self, id: &str) {
        if let Some(p) = self.running.lock().unwrap().remove(id) {
            let _ = p.stop.send(true);
            tracing::info!("{}: the page's browser closed (the source no longer received)", p.name);
        }
    }

    fn get(&self, id: &str) -> Option<Arc<Page>> {
        self.running.lock().unwrap().get(id).cloned()
    }
}

/// The frame kept inside the window, 16:9, a quarter of its width at least.
fn clamp(c: Crop, w: u32, h: u32) -> Crop {
    let (w, h) = (w as f64, h as f64);
    let cw = c.w.clamp(w / 4.0, w).min(h * 16.0 / 9.0);
    let ch = cw * 9.0 / 16.0;
    Crop { x: c.x.clamp(0.0, w - cw).round(), y: c.y.clamp(0.0, h - ch).round(), w: cw.round() }
}

impl Page {
    fn save(&self) {
        let s = Saved { url: self.url.lock().unwrap().clone(), crop: Some(*self.crop.lock().unwrap()) };
        let _ = crate::paths::write_json(&self.dir.join("state.json"), &s, false);
    }
}

// ---------------------------------------------------------------- the processes

/// Set once the system refused the browser its sandbox.
static NO_SANDBOX: AtomicBool = AtomicBool::new(false);

/// The browser chosen: `SC_CHROME`, else Chrome or Chromium where the system has them.
fn chrome() -> Option<String> {
    if let Ok(p) = std::env::var("SC_CHROME") {
        return Some(p);
    }
    let path = std::env::var_os("PATH")?;
    ["google-chrome-stable", "google-chrome", "chromium", "chromium-browser"]
        .into_iter()
        .find(|p| std::env::split_paths(&path).any(|d| d.join(p).is_file()))
        .map(String::from)
}

use crate::util::free_port;

/// A free X display: none there, or one left by a screen that died (its process gone: its files
/// taken away).
fn free_display() -> u32 {
    (90..400)
        .find(|d| {
            let (lock, sock) = (format!("/tmp/.X{d}-lock"), format!("/tmp/.X11-unix/X{d}"));
            let pid = std::fs::read_to_string(&lock).ok().and_then(|p| p.trim().parse::<u32>().ok());
            match pid {
                Some(p) if Path::new(&format!("/proc/{p}")).exists() => false,
                Some(_) => {
                    let _ = (std::fs::remove_file(&lock), std::fs::remove_file(&sock));
                    true
                }
                None => !Path::new(&sock).exists(),
            }
        })
        .unwrap_or(399)
}

/// The Xorg of the system and its dummy driver, where installed.
const XORG: &str = "/usr/lib/xorg/Xorg";
const DUMMY: &str = "/usr/lib/xorg/modules/drivers/dummy_drv.so";
/// Set once Xorg did not start here: Xvfb then.
static NO_XORG: AtomicBool = AtomicBool::new(false);

/// The page's screen, ready. Xorg's dummy screen refreshes at the page's rate: the browser draws
/// only the pictures filmed (Xvfb's, at 60 a second, draws more than twice too many at 25).
async fn screen(page: &Page, display: u32) -> Option<tokio::process::Child> {
    let (w, h) = (page.w, page.h);
    let socket = format!("/tmp/.X11-unix/X{display}");
    if !NO_XORG.load(Ordering::Relaxed) && Path::new(XORG).is_file() && Path::new(DUMMY).is_file() {
        let conf = page.dir.join("xorg.conf");
        let _ = std::fs::write(&conf, xorg_conf(w, h, page.rate));
        let mut cmd = Command::new(XORG);
        cmd.arg(format!(":{display}")).arg("-config").arg(&conf).arg("-logfile").arg(page.dir.join("xorg.log")).args(["-noreset", "-nolisten", "tcp"]);
        if let Ok(mut c) = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).process_group(0).spawn() {
            if appears(Path::new(&socket), 10).await {
                return Some(c);
            }
            kill_group(&c);
            let _ = c.kill().await;
        }
        tracing::warn!("{}: Xorg does not start here (see {}): Xvfb instead", page.name, page.dir.join("xorg.log").display());
        NO_XORG.store(true, Ordering::Relaxed);
        let _ = (std::fs::remove_file(format!("/tmp/.X{display}-lock")), std::fs::remove_file(&socket));
    }
    let mut c = Command::new("Xvfb").args([format!(":{display}"), "-screen".into(), "0".into(), format!("{w}x{h}x24"), "-nolisten".into(), "tcp".into()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).process_group(0).spawn().ok()?;
    if appears(Path::new(&socket), 10).await {
        return Some(c);
    }
    kill_group(&c);
    let _ = c.kill().await;
    None
}

/// Xorg's dummy screen: the page's size, refreshed at its rate.
fn xorg_conf(w: u32, h: u32, rate: (u32, u32)) -> String {
    let (ht, vt) = (w + 280, h + 45);
    let clock = ht as f64 * vt as f64 * rate.0 as f64 / rate.1.max(1) as f64 / 1e6;
    format!(
        r#"Section "ServerFlags"
  Option "AutoAddDevices" "false"
  Option "AutoAddGPU" "false"
  Option "DontVTSwitch" "true"
EndSection
Section "Device"
  Identifier "dummy"
  Driver "dummy"
  VideoRam 256000
EndSection
Section "Monitor"
  Identifier "page"
  HorizSync 5-300
  VertRefresh 20-100
  Modeline "page" {clock:.3} {w} {} {} {ht} {h} {} {} {vt} +hsync +vsync
EndSection
Section "Screen"
  Identifier "screen"
  Device "dummy"
  Monitor "page"
  DefaultDepth 24
  SubSection "Display"
    Depth 24
    Modes "page"
    Virtual {w} {h}
  EndSubSection
EndSection
"#,
        w + 88,
        w + 132,
        h + 4,
        h + 9,
    )
}

/// Stops a process and everything it started (a browser's many processes).
fn kill_group(c: &tokio::process::Child) {
    if let Some(pid) = c.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

/// Waits for a file to be there (a socket made), `secs` at most.
async fn appears(p: &Path, secs: u64) -> bool {
    for _ in 0..secs * 10 {
        if p.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Keeps a page's processes running until it is stopped (started again together when one stops).
async fn run(pages: Arc<Pages>, page: Arc<Page>) {
    let mut stop = page.stop.subscribe();
    let mut said = String::new();
    loop {
        if *stop.borrow() {
            break;
        }
        let why = run_once(&pages, &page, &mut stop).await;
        let Some(why) = why else { break };
        if why != said {
            tracing::warn!("{}: the page: {why}", page.name);
            said = why;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
            _ = stop.changed() => {}
        }
    }
    *page.cdp.lock().unwrap() = None;
    let mut running = pages.running.lock().unwrap();
    if running.get(&page.id).is_some_and(|p| Arc::ptr_eq(p, &page)) {
        running.remove(&page.id);
    }
}

/// One run of a page's processes: until one stops (why) or the page is stopped (none).
async fn run_once(pages: &Pages, page: &Arc<Page>, stop: &mut watch::Receiver<bool>) -> Option<String> {
    let Some(browser) = chrome() else { return Some("no browser installed (Chrome or Chromium: run the install script again)".into()) };
    // Its sound card's socket in a short folder (a socket's path is 108 characters at most).
    let rt = std::env::temp_dir().join(format!("strata-page-{}", page.id));
    let _ = std::fs::create_dir_all(&rt);
    let display = free_display();
    let (w, h) = (page.w, page.h);
    // Its screen.
    let Some(mut xvfb) = screen(page, display).await else { return Some("its screen (Xorg or Xvfb) does not start (run the install script again)".into()) };
    // Its sound card: a sink only it plays into.
    let socket = rt.join("pulse.sock");
    let _ = std::fs::remove_file(&socket);
    let mut pulse = None;
    if page.sound {
        let load = [format!("module-native-protocol-unix socket={} auth-anonymous=1", socket.display()), "module-null-sink sink_name=out".into(), "module-always-sink".into()];
        let mut cmd = Command::new("pulseaudio");
        cmd.args(["-n", "--daemonize=no", "--exit-idle-time=-1", "--use-pid-file=no", "--disallow-exit"]);
        for l in &load {
            cmd.args(["-L", l]);
        }
        cmd.env("PULSE_RUNTIME_PATH", &rt).env("HOME", &page.dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        match cmd.spawn() {
            Ok(c) => pulse = Some(c),
            Err(e) => return Some(format!("cannot start PulseAudio (run the install script again): {e}")),
        }
        if !appears(&socket, 10).await {
            return Some("its sound card (PulseAudio) does not start".into());
        }
    }
    // Its browser, full screen on it.
    let Some(cdp_port) = free_port(true) else { return Some("no free local port".into()) };
    let url = page.url.lock().unwrap().clone();
    let mut args: Vec<String> = [
        "--kiosk", "--no-first-run", "--no-default-browser-check", "--noerrdialogs", "--disable-infobars", "--disable-session-crashed-bubble", "--hide-crash-restore-bubble",
        "--autoplay-policy=no-user-gesture-required", "--disable-features=Translate,MediaRouter,DialMediaRouteProvider", "--password-store=basic", "--disable-dev-shm-usage", "--window-position=0,0",
        "--disable-background-networking", "--disable-component-update", "--disable-sync", "--disable-default-apps", "--disable-extensions",
    ]
    .map(String::from)
    .to_vec();
    args.push(format!("--window-size={w},{h}"));
    // Its DevTools on the loopback, for this server alone: no page may talk to them (no origin
    // allowed; this server's client sends none).
    args.push(format!("--remote-debugging-port={cdp_port}"));
    args.push(format!("--user-data-dir={}", page.dir.join("profile").display()));
    // As root (a container), or where the system refused it its sandbox, Chrome runs without.
    if unsafe { libc::geteuid() } == 0 || NO_SANDBOX.load(Ordering::Relaxed) {
        args.push("--no-sandbox".into());
    }
    // Blank first: its address opened once its requests are watched (see `reachable`).
    args.push("about:blank".into());
    // The lock a browser killed left on its profile.
    for f in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let _ = std::fs::remove_file(page.dir.join("profile").join(f));
    }
    let mut cmd = Command::new(&browser);
    // What it says, kept beside its profile (the last start's).
    let log = std::fs::File::create(page.dir.join("browser.log")).map(Stdio::from).unwrap_or_else(|_| Stdio::null());
    cmd.process_group(0).args(&args).env("DISPLAY", format!(":{display}")).env("HOME", &page.dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(log).kill_on_drop(true);
    if page.sound {
        cmd.env("PULSE_SERVER", format!("unix:{}", socket.display()));
    }
    let mut chrome = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Some(format!("cannot start {browser}: {e}")),
    };
    // Driven through its DevTools protocol.
    let cdp = match connect(cdp_port, page.clone()).await {
        Ok(c) => c,
        Err(e) => {
            kill_group(&chrome);
            kill_group(&xvfb);
            // Its sandbox refused (user namespaces closed to the service): the next start without.
            let said = std::fs::read_to_string(page.dir.join("browser.log")).unwrap_or_default();
            if said.contains("No usable sandbox") || (said.contains("sandbox") && said.contains("FATAL")) {
                NO_SANDBOX.store(true, Ordering::Relaxed);
                return Some("the browser's sandbox is refused by the system: it starts again without".into());
            }
            return Some(format!("the browser does not answer: {e}"));
        }
    };
    *page.cdp.lock().unwrap() = Some(cdp.clone());
    // The last address opened, else (none, or no longer allowed) the one the admin gave.
    let url = if !url.is_empty() && reachable(page, &url).await.is_ok() { url } else { page.given.clone() };
    if !url.is_empty() {
        if let Err(e) = cdp.call("Page.navigate", json!({ "url": url }), true).await {
            tracing::warn!("{}: its address does not open: {e}", page.name);
        }
    }
    // Filmed: the screen and the sound, raw; the frame kept here; encoded.
    let Some(audio_port) = free_port(true) else { return Some("no free local port".into()) };
    let film = film(pages, page, display, &socket, audio_port);
    let watch_cast = tokio::spawn(cast(page.clone(), cdp.clone()));
    let why = tokio::select! {
        s = xvfb.wait() => Some(format!("its screen stopped: {s:?}")),
        s = async { match pulse.as_mut() { Some(p) => p.wait().await, None => std::future::pending().await } } => Some(format!("its sound card (PulseAudio) stopped: {s:?}")),
        s = chrome.wait() => Some(format!("the browser stopped: {s:?}")),
        r = film => Some(r),
        _ = stop.changed() => None,
    };
    watch_cast.abort();
    *page.cdp.lock().unwrap() = None;
    kill_group(&chrome);
    let _ = chrome.kill().await;
    if let Some(p) = pulse.as_mut() {
        let _ = p.kill().await;
    }
    kill_group(&xvfb);
    let _ = xvfb.kill().await;
    let _ = (std::fs::remove_file(format!("/tmp/.X{display}-lock")), std::fs::remove_file(format!("/tmp/.X11-unix/X{display}")));
    why
}

/// Films the page: FFmpeg grabs the screen (and the sound), its frames are reframed here and
/// given to the encoder. Ends with why.
async fn film(pages: &Pages, page: &Arc<Page>, display: u32, socket: &Path, audio_port: u16) -> String {
    let (w, h) = (page.w as usize, page.h as usize);
    let rate = format!("{}/{}", page.rate.0, page.rate.1);
    let fps = (page.rate.0 as f64 / page.rate.1.max(1) as f64).round().max(1.0) as u32;
    // The encoder: the sound on a local port (or silence), opened first (it waits there for the
    // grabber, which then sends the frames its input waits for), the frames on its input.
    let mut enc = std::process::Command::new(&pages.ffmpeg);
    enc.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
    if page.sound {
        enc.args(["-thread_queue_size", "1024", "-f", "f32le", "-ar", "48000", "-ac", "2", "-i", &format!("tcp://127.0.0.1:{audio_port}?listen=1")]);
    } else {
        enc.args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"]);
    }
    enc.args(["-thread_queue_size", "64", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", &format!("{w}x{h}"), "-framerate", &rate, "-i", "pipe:0"]);
    let k = page.kbps;
    enc.args(["-map", "1:v", "-map", "0:a", "-c:v", "libx264", "-preset", "superfast", "-pix_fmt", "yuv420p"]);
    enc.args(crate::recorder::rate_args(k, page.cbr));
    enc.args(["-g", &(fps * 2).to_string(), "-c:a", "aac", "-b:a", "160k"]);
    enc.args(crate::recorder::muxrate_args(k, 160, page.cbr));
    enc.args(["-f", "mpegts"]);
    enc.arg(format!("udp://127.0.0.1:{}?pkt_size=1316", page.port));
    enc.stdin(Stdio::piped()).stdout(Stdio::null());
    let mut enc = match Proc::start(enc) {
        Ok(c) => c,
        Err(e) => return format!("cannot start {}: {e}", pages.ffmpeg),
    };
    // The grabber: the screen raw on its output, the sound to the encoder.
    let mut grab = std::process::Command::new(&pages.ffmpeg);
    grab.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-thread_queue_size", "512", "-f", "x11grab", "-draw_mouse", "0", "-framerate", &rate, "-video_size", &format!("{w}x{h}"), "-i", &format!(":{display}")]);
    if page.sound {
        grab.args(["-thread_queue_size", "1024", "-f", "pulse", "-server", &format!("unix:{}", socket.display()), "-i", "out.monitor"]);
    }
    grab.args(["-map", "0:v", "-fps_mode", "cfr", "-r", &rate, "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"]);
    if page.sound {
        // Its sound in step with the frames (both from the moment 0, at their own rate).
        grab.args(["-map", "1:a", "-af", "aresample=48000:async=1:first_pts=0", "-f", "f32le", "-ar", "48000", "-ac", "2"]);
        grab.arg(format!("tcp://127.0.0.1:{audio_port}"));
    }
    grab.stdin(Stdio::null()).stdout(Stdio::piped());
    // The encoder listens before the grabber connects.
    if page.sound {
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    // (Both killed and waited for however this ends: dropped with the page too.)
    let mut grab = match Proc::start(grab) {
        Ok(c) => c,
        Err(e) => return format!("cannot start {}: {e}", pages.ffmpeg),
    };
    let (Some(mut input), Some(mut output)) = (grab.child.stdout.take(), enc.child.stdin.take()) else {
        return "cannot read the grabber".into();
    };
    let p = page.clone();
    let (tx, rx) = oneshot::channel();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let end = move |why: &'static str| {
        if let Some(t) = tx.lock().unwrap().take() {
            let _ = t.send(why);
        }
    };
    // The frames read as they come and reframed (a thread: work for the processor), given to the
    // encoder by another: the grabber is never held back (it would hold its sound too, which the
    // encoder waits for first). The encoder a second late: frames left out.
    let (frames, queued) = std::sync::mpsc::sync_channel::<Vec<u8>>(25);
    let end1 = end.clone();
    std::thread::Builder::new()
        .name("page-frames".into())
        .spawn(move || {
            let size = w * h * 3 / 2;
            let mut src = vec![0u8; size];
            loop {
                if input.read_exact(&mut src).is_err() {
                    break end1("the grabber stopped");
                }
                let c = *p.crop.lock().unwrap();
                let mut dst = vec![0u8; size];
                reframe(&src, &mut dst, w, h, c);
                if let Err(std::sync::mpsc::TrySendError::Disconnected(_)) = frames.try_send(dst) {
                    break end1("the encoder stopped");
                }
            }
        })
        .ok();
    std::thread::Builder::new()
        .name("page-encode".into())
        .spawn(move || {
            while let Ok(f) = queued.recv() {
                if output.write_all(&f).is_err() {
                    return end("the encoder stopped");
                }
            }
        })
        .ok();
    let why = rx.await.unwrap_or("the frames stopped");
    let said = [grab.finish(), enc.finish()].into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" / ");
    if said.is_empty() {
        why.to_string()
    } else {
        format!("{why}: {said}")
    }
}

/// A process of the filming (blocking pipes, read by threads): killed and waited for when it
/// goes, however the filming ends; its errors read as it says them (its pipe never full), the
/// last one kept.
struct Proc {
    child: std::process::Child,
    said: Arc<Mutex<String>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Proc {
    fn start(mut cmd: std::process::Command) -> std::io::Result<Proc> {
        let mut child = cmd.stderr(Stdio::piped()).spawn()?;
        let said: Arc<Mutex<String>> = Arc::default();
        let reader = child.stderr.take().map(|err| {
            let said = said.clone();
            std::thread::spawn(move || {
                use std::io::BufRead;
                for l in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                    if !l.trim().is_empty() {
                        *said.lock().unwrap() = l;
                    }
                }
            })
        });
        Ok(Proc { child, said, reader })
    }

    /// Stopped: its last words.
    fn finish(&mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        self.said.lock().unwrap().clone()
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The frame `c` of a picture (YUV 4:2:0, `w`×`h`) at the picture's size (bilinear).
fn reframe(src: &[u8], dst: &mut [u8], w: usize, h: usize, c: Crop) {
    if c.x == 0.0 && c.y == 0.0 && c.w as usize >= w {
        dst.copy_from_slice(src);
        return;
    }
    let (ys, cs) = (w * h, (w / 2) * (h / 2));
    let (sy, rest) = src.split_at(ys);
    let (su, sv) = rest.split_at(cs);
    let (dy, rest) = dst.split_at_mut(ys);
    let (du, dv) = rest.split_at_mut(cs);
    let ch = c.w * 9.0 / 16.0;
    std::thread::scope(|s| {
        // The luma in four bands, the chroma beside.
        let rows = h.div_ceil(4);
        for (i, band) in dy.chunks_mut(rows * w).enumerate() {
            s.spawn(move || plane(sy, band, w, h, i * rows, (c.x, c.y, c.w, ch)));
        }
        let half = (c.x / 2.0, c.y / 2.0, c.w / 2.0, ch / 2.0);
        s.spawn(move || plane(su, du, w / 2, h / 2, 0, half));
        s.spawn(move || plane(sv, dv, w / 2, h / 2, 0, half));
    });
}

/// One plane's rows from `first` (`out` holds them): the rectangle (x, y, w, h) of `src` (`pw`×`ph`)
/// stretched over the whole plane.
fn plane(src: &[u8], out: &mut [u8], pw: usize, ph: usize, first: usize, r: (f64, f64, f64, f64)) {
    let (rx, ry, rw, rh) = r;
    let (sx, sy) = (rw / pw as f64, rh / ph as f64);
    // Each column's source column and weight (in 256ths).
    let cols: Vec<(usize, u32)> = (0..pw)
        .map(|ox| {
            // Inside the rectangle (its edges not mixed with what is beside).
            let x = (rx + (ox as f64 + 0.5) * sx - 0.5).clamp(rx, (rx + rw - 1.0).min((pw - 1) as f64));
            (x as usize, ((x - x.floor()) * 256.0) as u32)
        })
        .collect();
    for (i, row) in out.chunks_mut(pw).enumerate() {
        let oy = first + i;
        let y = (ry + (oy as f64 + 0.5) * sy - 0.5).clamp(ry, (ry + rh - 1.0).min((ph - 1) as f64));
        let (y0, fy) = (y as usize, ((y - y.floor()) * 256.0) as u32);
        let y1 = if fy == 0 { y0 } else { (y0 + 1).min(ph - 1) };
        let (a, b) = (&src[y0 * pw..y0 * pw + pw], &src[y1 * pw..y1 * pw + pw]);
        for (o, &(x0, fx)) in row.iter_mut().zip(&cols) {
            let x1 = if fx == 0 { x0 } else { (x0 + 1).min(pw - 1) };
            let top = a[x0] as u32 * (256 - fx) + a[x1] as u32 * fx;
            let bot = b[x0] as u32 * (256 - fx) + b[x1] as u32 * fx;
            *o = ((top * (256 - fy) + bot * fy) >> 16) as u8;
        }
    }
}

// ---------------------------------------------------------------- the DevTools protocol

/// Chrome driven: commands and their answers, its page's session.
struct Cdp {
    out: mpsc::UnboundedSender<String>,
    next: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    session: Mutex<String>,
    main: Mutex<String>,
    /// Pages it opened on its own (a link to a new tab): their address taken by the main one.
    popups: Mutex<HashSet<String>>,
    casting: AtomicBool,
}

impl Cdp {
    /// A command (of the page, or of the browser), its answer.
    async fn call(&self, method: &str, params: Value, of_page: bool) -> Result<Value, String> {
        self.call_within(method, params, of_page, 10).await
    }

    async fn call_within(&self, method: &str, params: Value, of_page: bool, secs: u64) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if of_page {
            msg["sessionId"] = json!(self.session.lock().unwrap().clone());
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.out.send(msg.to_string()).map_err(|_| "the browser is gone".to_string())?;
        match tokio::time::timeout(Duration::from_secs(secs), rx).await {
            Ok(Ok(r)) => r,
            _ => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("{method}: no answer"))
            }
        }
    }
}

/// Connects to the browser (it takes a moment to start), and to its page.
async fn connect(port: u16, page: Arc<Page>) -> Result<Arc<Cdp>, String> {
    let mut ws_url = None;
    for _ in 0..150 {
        if let Ok(v) = http_json(port, "/json/version").await {
            if let Some(u) = v["webSocketDebuggerUrl"].as_str() {
                ws_url = Some(u.to_string());
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let ws_url = ws_url.ok_or("its DevTools do not answer")?;
    let (ws, _) = tokio_tungstenite::connect_async(&ws_url).await.map_err(|e| e.to_string())?;
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let cdp = Arc::new(Cdp { out: tx, next: AtomicU64::new(1), pending: Mutex::new(HashMap::new()), session: Mutex::new(String::new()), main: Mutex::new(String::new()), popups: Mutex::new(HashSet::new()), casting: Default::default() });
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(Message::text(m)).await.is_err() {
                break;
            }
        }
    });
    let reader = cdp.clone();
    let p = page.clone();
    tokio::spawn(async move {
        while let Some(Ok(m)) = stream.next().await {
            let Message::Text(t) = m else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
            if let Some(id) = v["id"].as_u64() {
                if let Some(tx) = reader.pending.lock().unwrap().remove(&id) {
                    let r = match v.get("error") {
                        Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
                        None => Ok(v["result"].clone()),
                    };
                    let _ = tx.send(r);
                }
            } else if let Some(method) = v["method"].as_str() {
                event(&p, &reader, method, &v["params"]).await;
            }
        }
        // The browser gone: every command waiting fails.
        reader.pending.lock().unwrap().clear();
    });
    // Its page (its tab: a starting browser may show a page of a moment beside it, which answers
    // nothing), followed: its address, its pictures; the tabs it opens.
    let mut found = false;
    for _ in 0..10 {
        let targets = cdp.call("Target.getTargets", json!({}), false).await?;
        let pages: Vec<String> = targets["targetInfos"].as_array().map(|t| t.iter().filter(|x| x["type"] == "page").filter_map(|x| x["targetId"].as_str().map(String::from)).collect()).unwrap_or_default();
        for id in pages.into_iter().rev() {
            let Ok(attached) = cdp.call_within("Target.attachToTarget", json!({ "targetId": id, "flatten": true }), false, 3).await else { continue };
            *cdp.session.lock().unwrap() = attached["sessionId"].as_str().unwrap_or("").to_string();
            if cdp.call_within("Page.enable", json!({}), true, 3).await.is_ok() {
                *cdp.main.lock().unwrap() = id;
                found = true;
                break;
            }
        }
        if found {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if !found {
        return Err("its page does not answer".into());
    }
    // Every request of the page watched: none to the server itself or its network.
    cdp.call("Fetch.enable", json!({ "patterns": [{ "urlPattern": "*" }] }), true).await.map_err(|e| format!("its requests cannot be watched: {e}"))?;
    cdp.call("Target.setDiscoverTargets", json!({ "discover": true }), false).await?;
    if let Ok(r) = cdp.call("Runtime.evaluate", json!({ "expression": "location.href", "returnByValue": true }), true).await {
        if let Some(u) = r["result"]["value"].as_str().filter(|u| u.starts_with("http")) {
            *page.url.lock().unwrap() = u.to_string();
        }
    }
    Ok(cdp)
}

/// What the browser says.
async fn event(page: &Arc<Page>, cdp: &Arc<Cdp>, method: &str, p: &Value) {
    match method {
        // The page's address changed: kept (the source starts again on it).
        "Page.frameNavigated" if p["frame"]["parentId"].is_null() => moved(page, p["frame"]["url"].as_str().unwrap_or("")),
        "Page.navigatedWithinDocument" => moved(page, p["url"].as_str().unwrap_or("")),
        // A request of the page: let through, or refused when it goes to the server's network.
        "Fetch.requestPaused" => {
            let (page, c) = (page.clone(), cdp.clone());
            let id = p["requestId"].clone();
            let url = p["request"]["url"].as_str().unwrap_or("").to_string();
            tokio::spawn(async move {
                let _ = match reachable(&page, &url).await {
                    Ok(()) => c.call("Fetch.continueRequest", json!({ "requestId": id }), true).await,
                    Err(why) => {
                        tracing::warn!("{}: request refused ({why}): {}", page.name, crate::recorder::only_host(&url));
                        c.call("Fetch.failRequest", json!({ "requestId": id, "errorReason": "AccessDenied" }), true).await
                    }
                };
            });
        }
        "Page.screencastFrame" => {
            if let Some(data) = p["data"].as_str().and_then(base64_decode) {
                let _ = page.frames.send(Bytes::from(data));
            }
            let ack = json!({ "sessionId": p["sessionId"] });
            let c = cdp.clone();
            tokio::spawn(async move {
                let _ = c.call("Page.screencastFrameAck", ack, true).await;
            });
        }
        // A tab opened by the page (a link to a new window): its address opened in the page itself.
        "Target.targetCreated" | "Target.targetInfoChanged" => {
            let t = &p["targetInfo"];
            let id = t["targetId"].as_str().unwrap_or("").to_string();
            if t["type"] != "page" || id == *cdp.main.lock().unwrap() {
                return;
            }
            cdp.popups.lock().unwrap().insert(id.clone());
            let url = t["url"].as_str().unwrap_or("").to_string();
            if url.starts_with("http") {
                cdp.popups.lock().unwrap().remove(&id);
                let c = cdp.clone();
                tokio::spawn(async move {
                    let _ = c.call("Target.closeTarget", json!({ "targetId": id }), false).await;
                    let _ = c.call("Page.navigate", json!({ "url": url }), true).await;
                    let _ = c.call("Target.activateTarget", json!({ "targetId": c.main.lock().unwrap().clone() }), false).await;
                });
            }
        }
        _ => {}
    }
}

/// An address's host (lowercase, without brackets), its port, whether it is on the network (http,
/// https, ws, wss).
fn host_of(url: &str) -> Option<(String, u16, bool)> {
    let uri: ureq::http::Uri = url.trim().parse().ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    let port = match scheme.as_str() {
        "http" | "ws" => 80,
        "https" | "wss" => 443,
        _ => return Some((String::new(), 0, false)),
    };
    let host = uri.host()?.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    Some((host, uri.port_u16().unwrap_or(port), true))
}

/// May the page reach this address? The internet and the host the admin gave the source, not the
/// server itself nor its network (its other services, the cloud's metadata, the LAN). Pages' own
/// contents (data:, blob:, about:) need no network; any other kind (file:, chrome:…) is refused.
async fn reachable(page: &Page, url: &str) -> Result<(), String> {
    let low = url.trim_start().to_ascii_lowercase();
    if ["data:", "blob:", "about:"].iter().any(|k| low.starts_with(k)) {
        return Ok(());
    }
    let Some((host, port, net)) = host_of(url) else { return Err("this address is not valid".into()) };
    if !net {
        return Err("only http:// and https:// addresses".into());
    }
    if page.home.as_ref().is_some_and(|(h, p)| *h == host && *p == port) {
        return Ok(());
    }
    let host = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    if let Some((at, ok)) = page.hosts.lock().unwrap().get(&host) {
        if at.elapsed() < Duration::from_secs(60) {
            return if *ok { Ok(()) } else { Err("an address of the server or of its network".into()) };
        }
    }
    let addrs: Vec<_> = tokio::net::lookup_host(host.as_str()).await.map_err(|_| format!("unknown address: {host}"))?.collect();
    let ok = !addrs.is_empty() && !addrs.iter().any(|a| crate::guard::inner_ip(a.ip()));
    page.hosts.lock().unwrap().insert(host, (Instant::now(), ok));
    if ok { Ok(()) } else { Err("an address of the server or of its network".into()) }
}

fn moved(page: &Page, url: &str) {
    if url.starts_with("http") {
        *page.url.lock().unwrap() = url.to_string();
        page.save();
    }
}

/// The page's pictures sent while someone looks at them (started and stopped as they come).
async fn cast(page: Arc<Page>, cdp: Arc<Cdp>) {
    loop {
        let wanted = page.frames.receiver_count() > 0;
        if wanted != cdp.casting.load(Ordering::Relaxed) {
            let r = if wanted {
                cdp.call("Page.startScreencast", json!({ "format": "jpeg", "quality": 70, "maxWidth": page.w, "maxHeight": page.h, "everyNthFrame": 1 }), true).await
            } else {
                cdp.call("Page.stopScreencast", json!({}), true).await
            };
            if r.is_ok() {
                cdp.casting.store(wanted, Ordering::Relaxed);
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// A JSON answer of the browser's HTTP side (its address, its pages): read to its length (it
/// keeps the connection open).
async fn http_json(port: u16, path: &str) -> Result<Value, String> {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| e.to_string())?;
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes()).await.map_err(|e| e.to_string())?;
    let read = async {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = s.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let len = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()));
                if len.is_some_and(|len| body.len() >= len) {
                    break;
                }
            }
        }
        Ok::<Vec<u8>, String>(buf)
    };
    let buf = tokio::time::timeout(Duration::from_secs(3), read).await.map_err(|_| "timeout".to_string())??;
    let text = String::from_utf8_lossy(&buf);
    let body = text.split_once("\r\n\r\n").map(|x| x.1).ok_or("no body")?;
    serde_json::from_str(body).map_err(|e| e.to_string())
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

// ---------------------------------------------------------------- routes

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/pages/{id}", get(read))
        .route("/api/pages/{id}/go", post(go))
        .route("/api/pages/{id}/nav", post(nav))
        .route("/api/pages/{id}/crop", put(set_crop))
        .route("/api/pages/{id}/input", post(input))
        .route("/api/pages/{id}/live", get(live))
}

/// Who may browse, reframe and pilot a page: who runs the lives, or extracts.
fn may(me: &crate::auth::User) -> bool {
    me.can("stream.run") || me.can("extract.cut")
}

fn page_of(s: &Shared, id: &str) -> ApiResult<Arc<Page>> {
    let rec = s.recorders.get(id).ok_or_else(|| not_found("recorder"))?;
    if rec.config().protocol != Protocol::Page {
        return Err(bad("this source is not a web page"));
    }
    pages().and_then(|p| p.get(id)).ok_or_else(|| bad("the page is not open: start the source"))
}

async fn read(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    require(me.can("stream.view") || me.can("extract.view"))?;
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let c = rec.config();
    let f = Format::parse(&c.video_mode).unwrap_or(Format { width: 1920, height: 1080, interlaced: false, num: 25, den: 1 });
    Ok(Json(match pages().and_then(|p| p.get(&id)) {
        Some(p) => json!({ "running": p.cdp.lock().unwrap().is_some(), "url": *p.url.lock().unwrap(), "crop": *p.crop.lock().unwrap(), "width": p.w, "height": p.h }),
        None => {
            let saved: Saved = crate::paths::read_json(&s.data_dir.join("pages").join(&id).join("state.json")).unwrap_or_default();
            json!({ "running": false, "url": saved.url, "crop": saved.crop, "width": f.width, "height": f.height })
        }
    }))
}

#[derive(Deserialize)]
struct Go {
    url: String,
}

/// Opens an address (http or https only: the server's files and the browser's own pages stay
/// out of reach).
async fn go(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(g): Json<Go>) -> ApiResult<Json<Value>> {
    require(may(&me))?;
    let page = page_of(&s, &id)?;
    let mut url = g.url.trim().to_string();
    if !url.contains("://") {
        url = format!("https://{url}");
    }
    let low = url.to_ascii_lowercase();
    if !(low.starts_with("http://") || low.starts_with("https://")) || url.chars().any(char::is_whitespace) {
        return Err(bad("the address is http:// or https://"));
    }
    reachable(&page, &url).await.map_err(bad)?;
    let cdp = page.cdp.lock().unwrap().clone().ok_or_else(|| bad("the page is starting: again in a moment"))?;
    cdp.call("Page.navigate", json!({ "url": url }), true).await.map_err(bad)?;
    tracing::info!("{} opens {} in {}", me.username, crate::recorder::only_host(&url), page.name);
    Ok(Json(json!({ "url": url })))
}

#[derive(Deserialize)]
struct Nav {
    action: String,
}

async fn nav(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(n): Json<Nav>) -> ApiResult<StatusCode> {
    require(may(&me))?;
    let page = page_of(&s, &id)?;
    let cdp = page.cdp.lock().unwrap().clone().ok_or_else(|| bad("the page is starting: again in a moment"))?;
    let r = match n.action.as_str() {
        "back" => cdp.call("Runtime.evaluate", json!({ "expression": "history.back()" }), true).await,
        "forward" => cdp.call("Runtime.evaluate", json!({ "expression": "history.forward()" }), true).await,
        "reload" => cdp.call("Page.reload", json!({}), true).await,
        _ => return Err(bad("back, forward or reload")),
    };
    r.map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_crop(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(c): Json<Crop>) -> ApiResult<Json<Crop>> {
    require(may(&me))?;
    let page = page_of(&s, &id)?;
    if ![c.x, c.y, c.w].iter().all(|v| v.is_finite()) {
        return Err(bad("the frame is not a number"));
    }
    let c = clamp(c, page.w, page.h);
    *page.crop.lock().unwrap() = c;
    page.save();
    tracing::info!("{} reframes {}: {}×{} at {}, {}", me.username, page.name, c.w, (c.w * 9.0 / 16.0).round(), c.x, c.y);
    Ok(Json(c))
}

/// The mouse and the keyboard of the one piloting the page, in the window's pixels.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Input {
    #[serde(rename_all = "camelCase")]
    Mouse {
        /// "down", "up", "move" or "wheel".
        r#type: String,
        x: f64,
        y: f64,
        #[serde(default)]
        button: String,
        #[serde(default)]
        clicks: u32,
        #[serde(default)]
        dx: f64,
        #[serde(default)]
        dy: f64,
        #[serde(default)]
        mods: u32,
    },
    #[serde(rename_all = "camelCase")]
    Key {
        /// "down" or "up".
        r#type: String,
        key: String,
        #[serde(default)]
        code: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        key_code: u32,
        #[serde(default)]
        mods: u32,
    },
}

#[derive(Deserialize)]
struct Inputs {
    events: Vec<Input>,
}

async fn input(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(i): Json<Inputs>) -> ApiResult<StatusCode> {
    require(may(&me))?;
    let page = page_of(&s, &id)?;
    let cdp = page.cdp.lock().unwrap().clone().ok_or_else(|| bad("the page is starting: again in a moment"))?;
    for e in i.events.into_iter().take(200) {
        let (method, params) = match e {
            Input::Mouse { r#type, x, y, button, clicks, dx, dy, mods } => {
                let t = match r#type.as_str() {
                    "down" => "mousePressed",
                    "up" => "mouseReleased",
                    "wheel" => "mouseWheel",
                    _ => "mouseMoved",
                };
                let button = if button.is_empty() { "none" } else { &button };
                ("Input.dispatchMouseEvent", json!({ "type": t, "x": x, "y": y, "button": button, "clickCount": clicks, "deltaX": dx, "deltaY": dy, "modifiers": mods }))
            }
            Input::Key { r#type, key, code, text, key_code, mods } => {
                let t = if r#type == "up" { "keyUp" } else if text.is_empty() { "rawKeyDown" } else { "keyDown" };
                ("Input.dispatchKeyEvent", json!({ "type": t, "key": key, "code": code, "text": text, "unmodifiedText": text, "windowsVirtualKeyCode": key_code, "nativeVirtualKeyCode": key_code, "modifiers": mods }))
            }
        };
        let _ = cdp.call(method, params, true).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The page as it is now, as its pictures change (MJPEG: an image element shows it).
async fn live(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Response> {
    require(me.can("stream.view") || me.can("extract.view"))?;
    let page = page_of(&s, &id)?;
    let rx = page.frames.subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(jpeg) => {
                    let mut part = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len()).into_bytes();
                    part.extend_from_slice(&jpeg);
                    part.extend_from_slice(b"\r\n");
                    return Some((Ok::<Bytes, std::io::Error>(Bytes::from(part)), rx));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    });
    let mut r = Response::new(Body::from_stream(stream));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("multipart/x-mixed-replace; boundary=frame"));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_kept_inside() {
        let c = clamp(Crop { x: 1800.0, y: -5.0, w: 3000.0 }, 1920, 1080);
        assert_eq!(c, Crop { x: 0.0, y: 0.0, w: 1920.0 });
        let c = clamp(Crop { x: 1800.0, y: 900.0, w: 960.0 }, 1920, 1080);
        assert_eq!(c, Crop { x: 960.0, y: 540.0, w: 960.0 });
        assert_eq!(clamp(Crop { x: 0.0, y: 0.0, w: 10.0 }, 1920, 1080).w, 480.0);
    }

    #[test]
    fn xorg_at_the_page_rate() {
        let c = xorg_conf(1920, 1080, (25, 1));
        assert!(c.contains(r#"Modeline "page" 61.875 1920 2008 2052 2200 1080 1084 1089 1125"#), "{c}");
        assert!(xorg_conf(1280, 720, (60000, 1001)).contains(r#"Modeline "page" 71.532 1280"#));
    }

    #[test]
    fn reframed() {
        // A 8×4 picture: its right half (4×2 at 4, 2… a 16:9 of 4 is 2.25 high) stretched.
        let (w, h) = (16usize, 8usize);
        let mut src = vec![0u8; w * h * 3 / 2];
        for y in 0..h {
            for x in 0..w {
                src[y * w + x] = if x >= 8 { 200 } else { 10 };
            }
        }
        let mut dst = vec![0u8; src.len()];
        reframe(&src, &mut dst, w, h, Crop { x: 0.0, y: 0.0, w: 16.0 });
        assert_eq!(dst, src);
        reframe(&src, &mut dst, w, h, Crop { x: 8.0, y: 0.0, w: 8.0 });
        assert!(dst[..w * h].iter().all(|&v| v == 200), "{:?}", &dst[..w]);
        reframe(&src, &mut dst, w, h, Crop { x: 0.0, y: 0.0, w: 8.0 });
        assert!(dst[..w * h].iter().all(|&v| v == 10));
    }
}
