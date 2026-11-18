//! The recorders: one FFmpeg process per source, writing a loop of segments
//! that never stops (the oldest minutes are removed as new ones arrive).
mod media;
pub mod index;
mod run;
pub mod web;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use tokio::sync::watch;

use crate::{
    config::ServerConfig,
    state::{Audience, Events},
};
pub use index::{MediaInfo, Run};
pub use media::routes;

pub const MASKED: &str = "••••••••";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Udp,
    Rtp,
    Rtmp,
    Srt,
    /// A live on a web page (YouTube, Facebook, Instagram, X, LinkedIn…), read with yt-dlp.
    Web,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Listener,
    Caller,
}

/// A recorder's settings (`recorders.json`).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RecorderConfig {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    #[serde(default)]
    pub url: String,
    #[serde(default = "listener")]
    pub mode: Mode,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub path: String,
    #[serde(default = "default_latency")]
    pub latency: u32,
    #[serde(default)]
    pub passphrase: String,
    #[serde(default)]
    pub iface: String,
    /// Web pages: the cookies of a signed-in account (Netscape cookies.txt), for lives that need one.
    #[serde(default)]
    pub cookies: String,
    #[serde(default = "default_loop")]
    pub loop_hours: u32,
    /// Expected bitrate of the source, Mb/s (reserves the loop's space; 0 = unknown).
    #[serde(default)]
    pub bitrate: f64,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn listener() -> Mode {
    Mode::Listener
}
fn default_latency() -> u32 {
    120
}
fn default_loop() -> u32 {
    24
}
fn yes() -> bool {
    true
}

impl RecorderConfig {
    pub fn normalize(&mut self) {
        self.name = self.name.trim().to_string();
        self.url = self.url.trim().to_string();
        self.path = self.path.trim().trim_matches('/').to_string();
        self.iface = self.iface.trim().to_string();
        if self.path.is_empty() && self.protocol == Protocol::Rtmp {
            self.path = "live/stream".into();
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("the name cannot be empty".into());
        }
        if !self.bitrate.is_finite() || !(0.0..=2000.0).contains(&self.bitrate) {
            return Err("the bitrate is 0.1 to 2000 mb/s".into());
        }
        if !(1..=168).contains(&self.loop_hours) {
            return Err("the loop lasts 1 to 168 hours".into());
        }
        let listens = self.listens();
        if listens && self.port == 0 {
            return Err("choose the listening port".into());
        }
        let scheme = match self.protocol {
            Protocol::Udp => "udp://",
            Protocol::Rtp => "rtp://",
            Protocol::Rtmp => "rtmp://",
            Protocol::Srt => "srt://",
            Protocol::Web => "https://",
        };
        if self.protocol == Protocol::Web {
            if !self.url.starts_with("https://") && !self.url.starts_with("http://") {
                return Err("the address of the page starts with https://".into());
            }
        } else if !listens && !self.url.starts_with(scheme) {
            return Err(format!("the address starts with {scheme}"));
        }
        if self.cookies != MASKED && !self.cookies.trim().is_empty() && !self.cookies.contains('\t') {
            return Err("the cookies are a cookies.txt file (netscape format)".into());
        }
        if self.cookies.len() > 1_000_000 {
            return Err("the cookies file is too big".into());
        }
        if self.protocol == Protocol::Srt && !self.passphrase.is_empty() && self.passphrase != MASKED && !(10..=79).contains(&self.passphrase.chars().count()) {
            return Err("the srt passphrase has 10 to 79 characters".into());
        }
        if !self.iface.is_empty() && self.iface.parse::<std::net::IpAddr>().is_err() {
            return Err("the network interface is an ip address".into());
        }
        Ok(())
    }

    /// Waits for the source to connect (rtmp / srt as listener).
    pub fn listens(&self) -> bool {
        matches!(self.protocol, Protocol::Rtmp | Protocol::Srt) && self.mode == Mode::Listener
    }

    /// For clients: the passphrase masked.
    pub fn public(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if !self.passphrase.is_empty() {
            v["passphrase"] = json!(MASKED);
        }
        if !self.cookies.is_empty() {
            v["cookies"] = json!(MASKED);
        }
        if self.protocol == Protocol::Web {
            v["platform"] = json!(web::platform(&self.url));
        }
        v
    }
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Off,
    Starting,
    Recording,
    Nosignal,
    Error,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub id: String,
    pub state: State,
    pub since: i64,
    pub message: String,
    pub media: Option<MediaInfo>,
    pub bitrate: u64,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
    pub bytes: u64,
    /// The loop is shorter than asked: the disk limits removed its oldest part.
    pub space_limited: bool,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// One recorder: its settings, its live state and its loop.
pub struct Rec {
    pub id: String,
    pub cfg: watch::Sender<RecorderConfig>,
    pub status: Mutex<Status>,
    pub runs: RwLock<Vec<Run>>,
    /// Set when the recorder is deleted or the server stops.
    pub stop: watch::Sender<bool>,
    /// Folder of its loop.
    pub dir: Mutex<PathBuf>,
    /// The source has no audio: FFmpeg runs without the audio proxy.
    pub no_audio: std::sync::atomic::AtomicBool,
    /// When the disk limits last removed some of its loop, ms.
    pub space_trimmed: std::sync::atomic::AtomicI64,
    /// Starts a new run now (the loops' folder changed).
    pub new_run: tokio::sync::Notify,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Rec {
    pub fn config(&self) -> RecorderConfig {
        self.cfg.borrow().clone()
    }

    pub fn set_state(&self, state: State, message: impl Into<String>) {
        let mut s = self.status.lock().unwrap();
        let message = message.into();
        if s.state != state {
            s.state = state;
            s.since = now_ms();
        }
        s.message = message;
    }

    /// Refreshes the loop figures of the status.
    pub fn refresh_status(&self) {
        let runs = self.runs.read().unwrap();
        let oldest = runs.iter().filter_map(Run::first_wall).reduce(f64::min).map(|w| w as i64);
        let newest = runs.iter().filter_map(Run::last_wall).reduce(f64::max).map(|w| w as i64);
        let bytes = runs.iter().map(Run::bytes).sum();
        // Bitrate over the last segments of the live run.
        let bitrate = runs.iter().rev().find(|r| r.live).map(|r| {
            let tail: Vec<_> = r.master.iter().rev().take(5).collect();
            let dur: f64 = tail.iter().map(|s| s.t1 - s.t0).sum();
            let b: u64 = tail.iter().map(|s| s.bytes).sum();
            if dur > 0.0 { (b as f64 * 8.0 / dur) as u64 } else { 0 }
        });
        let media = runs.iter().rev().find_map(|r| r.meta.media.clone());
        drop(runs);
        let mut s = self.status.lock().unwrap();
        s.oldest = oldest;
        s.newest = newest;
        s.bytes = bytes;
        s.bitrate = if s.state == State::Recording { bitrate.unwrap_or(0) } else { 0 };
        let cut = self.space_trimmed.load(std::sync::atomic::Ordering::Relaxed);
        let short = matches!((oldest, newest), (Some(a), Some(b)) if ((b - a) as f64) < self.config().loop_hours as f64 * 3_600_000.0 * 0.98);
        s.space_limited = cut > 0 && now_ms() - cut < 15 * 60_000 && short;
        if media.is_some() {
            s.media = media;
        }
    }

    /// The run holding the server time `wall` (ms).
    #[allow(dead_code)]
    pub fn run_at(&self, wall: f64) -> Option<String> {
        let runs = self.runs.read().unwrap();
        runs.iter().find(|r| matches!((r.first_wall(), r.last_wall()), (Some(a), Some(b)) if a <= wall && wall < b)).map(|r| r.meta.id.clone())
    }
}

/// Old segments kept while a clip is being cut from them.
#[derive(Clone)]
pub struct Pin {
    pub id: String,
    pub recorder: String,
    pub run: String,
    pub from: f64,
}

/// How much the loops may take.
#[derive(Clone, Debug, Default)]
pub struct Limits {
    /// Bytes, 0 = only the disk.
    pub loops_max: u64,
    /// The clips: their folder and their limit (bytes, 0 = none).
    pub clips_dir: PathBuf,
    pub clips_max: u64,
}

/// What stays free for the system on a disk of `total` bytes: on the system
/// disk 5 % (10 GB at least), on another disk 1 % (1 GB at least); never more than a quarter.
pub fn system_reserve(path: &Path, total: u64) -> u64 {
    let gb = 1u64 << 30;
    if same_disk(path, Path::new("/")) { (total / 20).max(10 * gb).min(total / 4) } else { (total / 100).max(gb).min(total / 4) }
}

/// Mb/s the browser proxies add to a source.
const PROXY_MBPS: f64 = 1.5;

/// Bytes a full loop takes (proxies and 5 % included), at the expected
/// bitrate or the measured one if higher; None when neither is known.
pub fn loop_need(cfg: &RecorderConfig, measured_bps: u64) -> Option<u64> {
    let mbps = cfg.bitrate.max(measured_bps as f64 / 1e6);
    (mbps > 0.0).then(|| ((mbps + PROXY_MBPS) * 1e6 / 8.0 * 3600.0 * cfg.loop_hours as f64 * 1.05) as u64)
}

/// The two paths are on the same disk.
pub fn same_disk(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let dev = |p: &Path| {
        let mut p = p.to_path_buf();
        loop {
            if let Ok(m) = std::fs::metadata(&p) {
                return Some(m.dev());
            }
            if !p.pop() {
                return None;
            }
        }
    };
    dev(a).is_some() && dev(a) == dev(b)
}

impl Limits {
    pub fn from(cfg: &ServerConfig, data_dir: &Path) -> Self {
        Limits { loops_max: (cfg.loops_max_gb * 1e9) as u64, clips_dir: cfg.clips_dir(data_dir), clips_max: (cfg.clips_max_gb * 1e9) as u64 }
    }
    /// The free space always kept on the disk holding `path` (see `system_reserve`).
    pub fn min_free(&self, path: &Path, total: u64) -> u64 {
        system_reserve(path, total)
    }
}

/// (total, free) bytes of the disk holding `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            return None;
        }
    }
    let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some((st.f_blocks as u64 * st.f_frsize as u64, st.f_bavail as u64 * st.f_frsize as u64))
}

/// Bytes taken by a folder and everything in it.
pub fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

pub struct Manager {
    data_dir: PathBuf,
    recordings_dir: Mutex<PathBuf>,
    /// Every folder that may still hold loops (the current one and former ones).
    dirs: Mutex<Vec<PathBuf>>,
    pub limits: Mutex<Limits>,
    pub events: Events,
    recs: Mutex<Vec<Arc<Rec>>>,
    pub pins: Mutex<Vec<Pin>>,
    pub ffmpeg: String,
    /// Limits the frame grabs running at once.
    pub grabs: tokio::sync::Semaphore,
}

impl Manager {
    pub fn start(data_dir: &Path, cfg: &ServerConfig, events: Events) -> Arc<Self> {
        let m = Arc::new(Manager {
            data_dir: data_dir.to_path_buf(),
            recordings_dir: Mutex::new(cfg.recordings_dir(data_dir)),
            dirs: Mutex::new({
                let mut d: Vec<PathBuf> = crate::paths::read_json(&data_dir.join("loop-dirs.json")).unwrap_or_default();
                if !d.contains(&cfg.recordings_dir(data_dir)) {
                    d.push(cfg.recordings_dir(data_dir));
                }
                d
            }),
            limits: Mutex::new(Limits::from(cfg, data_dir)),
            events,
            recs: Mutex::new(Vec::new()),
            pins: Mutex::new(Vec::new()),
            ffmpeg: std::env::var("SC_FFMPEG").unwrap_or_else(|_| "ffmpeg".into()),
            grabs: tokio::sync::Semaphore::new(4),
        });
        web::keep_updated(data_dir.to_path_buf());
        let list: Vec<RecorderConfig> = crate::paths::read_json(&m.config_path()).unwrap_or_default();
        for c in list {
            m.add(c);
        }
        // Live state for the clients, and the loop trimmed.
        let weak = Arc::downgrade(&m);
        tokio::spawn(async move {
            let mut last = Value::Null;
            let mut tick = 0u64;
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let Some(m) = weak.upgrade() else { return };
                for r in m.recs() {
                    if tick.is_multiple_of(5) {
                        m.trim(&r);
                    }
                }
                if tick.is_multiple_of(5) {
                    let m2 = m.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        m2.enforce_space();
                        m2.forget_old_dirs();
                    })
                    .await;
                }
                for r in m.recs() {
                    r.refresh_status();
                }
                tick += 1;
                let now = m.status_json();
                if now != last {
                    m.events.send(Audience::All, json!({"type": "status", "status": now}));
                    last = now;
                }
            }
        });
        m
    }

    fn config_path(&self) -> PathBuf {
        self.data_dir.join("recorders.json")
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn recordings_dir(&self) -> PathBuf {
        self.recordings_dir.lock().unwrap().clone()
    }

    pub fn set_config(&self, cfg: &ServerConfig) {
        let dir = cfg.recordings_dir(&self.data_dir);
        *self.recordings_dir.lock().unwrap() = dir.clone();
        *self.limits.lock().unwrap() = Limits::from(cfg, &self.data_dir);
        let mut dirs = self.dirs.lock().unwrap();
        if !dirs.contains(&dir) {
            tracing::info!("the loops now go to {}; the former ones stay readable until they roll out", dir.display());
            dirs.push(dir.clone());
            let _ = crate::paths::write_json(&self.data_dir.join("loop-dirs.json"), &*dirs, false);
        }
        drop(dirs);
        // Recording goes on in the new folder at once.
        for r in self.recs() {
            if !r.dir.lock().unwrap().starts_with(&dir) && r.config().enabled {
                r.new_run.notify_one();
            }
        }
    }

    /// Former loop folders that no recording uses any more are forgotten (their empty folders removed).
    fn forget_old_dirs(&self) {
        let current = self.recordings_dir();
        let used: Vec<PathBuf> = self.recs().iter().flat_map(|r| r.runs.read().unwrap().iter().map(|x| x.dir.clone()).collect::<Vec<_>>()).collect();
        let mut dirs = self.dirs.lock().unwrap();
        let before = dirs.len();
        dirs.retain(|d| {
            if *d == current || used.iter().any(|u| u.starts_with(d)) {
                return true;
            }
            for r in self.recs() {
                let _ = std::fs::remove_file(d.join(&r.id).join("thumb.jpg"));
                let _ = std::fs::remove_dir(d.join(&r.id)); // only if empty
            }
            false
        });
        if dirs.len() != before {
            let _ = crate::paths::write_json(&self.data_dir.join("loop-dirs.json"), &*dirs, false);
        }
    }

    /// Bytes of the loops stored on the disk holding `dir`.
    pub fn loops_bytes_under(&self, dir: &Path) -> u64 {
        self.recs().iter().map(|r| r.runs.read().unwrap().iter().filter(|x| same_disk(&x.dir, dir)).map(Run::bytes).sum::<u64>()).sum()
    }

    /// Bytes taken by all the loops.
    pub fn loops_bytes(&self) -> u64 {
        self.recs().iter().map(|r| r.runs.read().unwrap().iter().map(Run::bytes).sum::<u64>()).sum()
    }

    /// Keeps the loops within their limit and the disk from filling up:
    /// the oldest recordings go first, whatever the source.
    pub fn enforce_space(&self) {
        let lim = self.limits.lock().unwrap().clone();
        let current = self.recordings_dir();
        let used = self.loops_bytes();
        let quota_need = if lim.loops_max > 0 { used.saturating_sub(lim.loops_max) } else { 0 };
        let disk_need = disk_space(&current).map(|(total, free)| lim.min_free(&current, total).saturating_sub(free)).unwrap_or(0);
        let need = quota_need.max(disk_need);
        if need == 0 {
            return;
        }
        // A full disk is freed on that disk only (former folders may be elsewhere).
        let disk_only = quota_need < disk_need;
        let recs = self.recs();
        let pins = self.pins.lock().unwrap().clone();
        let mut freed = 0u64;
        let mut doomed: Vec<PathBuf> = Vec::new();
        let mut touched: Vec<(usize, usize)> = Vec::new();
        let mut guards: Vec<_> = recs.iter().map(|r| r.runs.write().unwrap()).collect();
        while freed < need {
            // The oldest segment that may go (not the live edge, not one a clip is being cut from).
            let mut best: Option<(usize, usize, f64)> = None;
            for (ri, runs) in guards.iter().enumerate() {
                for (ui, run) in runs.iter().enumerate() {
                    let Some(s) = run.master.front() else { continue };
                    if run.live && run.master.len() <= 2 {
                        continue;
                    }
                    if disk_only && !run.dir.starts_with(&current) {
                        continue;
                    }
                    if pins.iter().any(|p| p.recorder == recs[ri].id && p.run == run.meta.id && s.t1 > p.from) {
                        continue;
                    }
                    if best.is_none_or(|b| s.wall0 < b.2) {
                        best = Some((ri, ui, s.wall0));
                    }
                }
            }
            let Some((ri, ui, _)) = best else { break };
            let run = &mut guards[ri][ui];
            let Some(s) = run.master.pop_front() else { break };
            freed += s.bytes;
            doomed.push(run.master_path(s.n));
            let start = run.master.front().map(|x| x.t0).unwrap_or(f64::INFINITY);
            for video in [true, false] {
                while let Some(p) = run.track(video).segs.front().cloned() {
                    if p.t1 > start {
                        break;
                    }
                    freed += p.bytes;
                    doomed.push(run.proxy_path(video, p.n));
                    if !video {
                        doomed.push(run.wave_path(p.n));
                        run.audio.waves.remove(&p.n);
                    }
                    run.track_mut(video).segs.pop_front();
                }
            }
            if !touched.contains(&(ri, ui)) {
                touched.push((ri, ui));
            }
            recs[ri].space_trimmed.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
        }
        for &(ri, ui) in &touched {
            if !guards[ri][ui].master.is_empty() {
                index::rewrite_index(&guards[ri][ui]);
            }
        }
        drop(guards);
        if freed > 0 {
            tracing::warn!("disk space: the oldest recordings were removed ({} MB) to stay within the limits", freed >> 20);
        }
        for p in doomed {
            if let Err(e) = std::fs::remove_file(&p) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("{}: {e}", p.display());
                }
            }
        }
    }

    pub fn recs(&self) -> Vec<Arc<Rec>> {
        self.recs.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Rec>> {
        self.recs.lock().unwrap().iter().find(|r| r.id == id).cloned()
    }

    fn save(&self) -> std::io::Result<()> {
        let list: Vec<RecorderConfig> = self.recs().iter().map(|r| r.config()).collect();
        crate::paths::write_json(&self.config_path(), &list, true)
    }

    fn add(self: &Arc<Self>, c: RecorderConfig) -> Arc<Rec> {
        let dir = self.recordings_dir().join(&c.id);
        let mut runs: Vec<Run> = self.dirs.lock().unwrap().iter().flat_map(|d| load_runs(&d.join(&c.id))).collect();
        runs.sort_by_key(|r| r.meta.started);
        let rec = Arc::new(Rec {
            id: c.id.clone(),
            status: Mutex::new(Status { id: c.id.clone(), state: State::Off, since: now_ms(), message: String::new(), media: None, bitrate: 0, oldest: None, newest: None, bytes: 0, space_limited: false }),
            cfg: watch::channel(c).0,
            runs: RwLock::new(runs),
            stop: watch::channel(false).0,
            dir: Mutex::new(dir),
            no_audio: false.into(),
            space_trimmed: 0.into(),
            new_run: tokio::sync::Notify::new(),
            task: Mutex::new(None),
        });
        rec.refresh_status();
        let handle = tokio::spawn(run::supervise(self.clone(), rec.clone()));
        *rec.task.lock().unwrap() = Some(handle);
        self.recs.lock().unwrap().push(rec.clone());
        rec
    }

    /// Space the loops may take: their limit, else what the disk gives them
    /// (other files stay, clips on the same disk keep their limit, a margin stays free).
    pub fn loops_capacity(&self) -> Option<u64> {
        let lim = self.limits.lock().unwrap().clone();
        let dir = self.recordings_dir();
        let (total, free) = disk_space(&dir)?;
        // Loops already on that disk count as space they may reuse.
        let here: u64 = self.recs().iter().map(|r| r.runs.read().unwrap().iter().filter(|x| x.dir.starts_with(&dir)).map(Run::bytes).sum::<u64>()).sum();
        let mut cap = (free + here).saturating_sub(lim.min_free(&dir, total));
        if lim.clips_max > 0 && same_disk(&dir, &lim.clips_dir) {
            cap = cap.saturating_sub(lim.clips_max.saturating_sub(dir_size(&lim.clips_dir)));
        }
        if lim.loops_max > 0 {
            cap = cap.min(lim.loops_max);
        }
        Some(cap)
    }

    /// Bytes reserved by the recorders that record (their full loops).
    pub fn reserved(&self) -> u64 {
        self.recs().iter().filter_map(|r| {
            let c = r.config();
            c.enabled.then(|| loop_need(&c, r.status.lock().unwrap().bitrate)).flatten()
        }).sum()
    }

    /// Refuses a recorder whose full loop does not fit (only when the change asks for more space).
    fn check_space(&self, c: &RecorderConfig) -> Result<(), String> {
        if !c.enabled {
            return Ok(());
        }
        let Some(cap) = self.loops_capacity() else { return Ok(()) };
        let (mut before, mut after, mut known) = (0u64, 0u64, false);
        for r in self.recs() {
            let o = r.config();
            let measured = r.status.lock().unwrap().bitrate;
            let need = if o.enabled { loop_need(&o, measured).unwrap_or(0) } else { 0 };
            before += need;
            if o.id == c.id {
                known = true;
                after += loop_need(c, measured).unwrap_or(0);
            } else {
                after += need;
            }
        }
        if !known {
            after += loop_need(c, 0).unwrap_or(0);
        }
        if after > cap && after > before {
            return Err("not enough disk space for this loop".into());
        }
        Ok(())
    }

    /// Checks a recorder's settings against the others (ports).
    fn check(&self, c: &RecorderConfig) -> Result<(), String> {
        c.validate()?;
        if c.listens() {
            for r in self.recs() {
                let o = r.config();
                if o.id != c.id && o.listens() && o.port == c.port {
                    return Err("this port is used by another recorder".into());
                }
            }
        }
        Ok(())
    }

    pub fn create(self: &Arc<Self>, mut c: RecorderConfig) -> Result<Value, String> {
        c.id = crate::api::new_id();
        c.normalize();
        if c.passphrase == MASKED {
            c.passphrase.clear();
        }
        if c.cookies == MASKED {
            c.cookies.clear();
        }
        if c.bitrate <= 0.0 {
            return Err("give the expected bitrate".into());
        }
        self.check(&c)?;
        self.check_space(&c)?;
        let v = c.public();
        self.add(c);
        // Clients learn the change even if the disk refuses the file.
        let saved = self.save();
        self.list_changed();
        saved.map_err(|e| e.to_string())?;
        Ok(v)
    }

    pub fn update(&self, id: &str, mut c: RecorderConfig) -> Result<Value, String> {
        let rec = self.get(id).ok_or("no such recorder")?;
        let old = rec.config();
        c.id = old.id.clone();
        c.normalize();
        if c.passphrase == MASKED {
            c.passphrase = old.passphrase.clone();
        }
        if c.cookies == MASKED {
            c.cookies = old.cookies.clone();
        }
        self.check(&c)?;
        self.check_space(&c)?;
        let v = c.public();
        if c != old {
            rec.cfg.send_replace(c); // the supervisor restarts FFmpeg if the source changed
            let saved = self.save();
            self.list_changed();
            saved.map_err(|e| e.to_string())?;
        }
        Ok(v)
    }

    pub async fn delete(&self, id: &str) -> Result<(), String> {
        let rec = self.get(id).ok_or("no such recorder")?;
        self.recs.lock().unwrap().retain(|r| r.id != id);
        let saved = self.save();
        self.list_changed();
        let _ = rec.stop.send(true);
        let task = rec.task.lock().unwrap().take();
        if let Some(t) = task {
            let _ = t.await;
        }
        let dirs: Vec<PathBuf> = self.dirs.lock().unwrap().iter().map(|d| d.join(id)).collect();
        let _ = std::fs::remove_file(self.data_dir.join("cookies").join(format!("{id}.txt")));
        let name = rec.config().name;
        let _ = tokio::task::spawn_blocking(move || {
            for dir in dirs {
                match std::fs::remove_dir_all(&dir) {
                    Ok(()) => tracing::info!("{name}: its loop was deleted ({})", dir.display()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => tracing::error!("{name}: its loop could not be deleted ({}): {e}", dir.display()),
                }
            }
        })
        .await;
        saved.map_err(|e| e.to_string())
    }

    /// Stops every FFmpeg cleanly (the last segments are closed).
    pub async fn shutdown(&self) {
        let recs = self.recs();
        for r in &recs {
            let _ = r.stop.send(true);
        }
        for r in recs {
            let task = r.task.lock().unwrap().take();
            if let Some(t) = task {
                let _ = t.await;
            }
        }
    }

    fn list_changed(&self) {
        self.events.send(Audience::All, json!({"type": "recorders", "recorders": self.list_json()}));
    }

    pub fn list_json(&self) -> Value {
        Value::Array(self.recs().iter().map(|r| r.config().public()).collect())
    }

    pub fn status_json(&self) -> Value {
        Value::Array(self.recs().iter().map(|r| serde_json::to_value(&*r.status.lock().unwrap()).unwrap_or_default()).collect())
    }

    /// Removes what is older than the loop (except pinned segments).
    pub fn trim(&self, rec: &Rec) {
        let cutoff = (now_ms() - rec.config().loop_hours as i64 * 3_600_000) as f64;
        let pins: Vec<Pin> = self.pins.lock().unwrap().iter().filter(|p| p.recorder == rec.id).cloned().collect();
        let mut doomed: Vec<PathBuf> = Vec::new();
        let mut runs = rec.runs.write().unwrap();
        for run in runs.iter_mut() {
            let keep_from = pins.iter().filter(|p| p.run == run.meta.id).map(|p| p.from).reduce(f64::min);
            let mut changed = false;
            while let Some(s) = run.master.front() {
                let pinned = keep_from.is_some_and(|f| s.t1 > f);
                if s.wall1() >= cutoff || pinned || (run.live && run.master.len() == 1) {
                    break;
                }
                doomed.push(run.master_path(s.n));
                run.master.pop_front();
                changed = true;
            }
            let start = run.master.front().map(|s| s.t0).unwrap_or(f64::INFINITY);
            for video in [true, false] {
                while let Some(p) = run.track(video).segs.front().cloned() {
                    if p.t1 > start || (run.live && run.track(video).segs.len() == 1) {
                        break;
                    }
                    doomed.push(run.proxy_path(video, p.n));
                    if !video {
                        doomed.push(run.wave_path(p.n));
                        run.audio.waves.remove(&p.n);
                    }
                    run.track_mut(video).segs.pop_front();
                    changed = true;
                }
            }
            if changed && !run.master.is_empty() {
                index::rewrite_index(run);
            }
        }
        runs.retain(|r| {
            if r.master.is_empty() && !r.live {
                doomed.push(r.dir.clone());
                false
            } else {
                true
            }
        });
        drop(runs);
        if !doomed.is_empty() {
            tokio::task::spawn_blocking(move || {
                for p in doomed {
                    let r = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                    if let Err(e) = r {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            tracing::warn!("{}: {e}", p.display());
                        }
                    }
                }
            });
        }
    }

    pub fn pin(&self, p: Pin) {
        self.pins.lock().unwrap().push(p);
    }
    pub fn unpin(&self, id: &str) {
        self.pins.lock().unwrap().retain(|p| p.id != id);
    }
}

/// The finished runs of a recorder's folder, oldest first.
fn load_runs(dir: &Path) -> Vec<Run> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut runs: Vec<Run> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let r = Run::load(&e.path());
            if r.is_none() {
                let _ = std::fs::remove_dir_all(e.path()); // nothing usable in it
            }
            r
        })
        .collect();
    runs.sort_by_key(|r| r.meta.started);
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_needs() {
        let mut c: RecorderConfig = serde_json::from_str(r#"{"name":"a","protocol":"udp","url":"udp://239.1.1.1:5000","loopHours":24,"bitrate":10}"#).unwrap();
        // (10 + 1.5) Mb/s × 24 h × 1.05 ≈ 130 GB
        assert_eq!(loop_need(&c, 0).map(|b| b / 1_000_000_000), Some(130));
        // the measured bitrate wins when higher
        assert!(loop_need(&c, 20_000_000).unwrap() > loop_need(&c, 0).unwrap());
        c.bitrate = 0.0;
        assert_eq!(loop_need(&c, 0), None);
    }

    #[test]
    fn system_reserves() {
        // The system disk: 5 %, 10 GB at least, a quarter at most.
        assert_eq!(system_reserve(Path::new("/"), 1_000_000_000_000), 50_000_000_000);
        assert_eq!(system_reserve(Path::new("/"), 100_000_000_000), 10 << 30);
        assert_eq!(system_reserve(Path::new("/"), 16_000_000_000), 4_000_000_000);
    }
}
