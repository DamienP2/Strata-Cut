//! The recorders: one FFmpeg process per source, writing a loop of segments
//! that never stops (the oldest minutes are removed as new ones arrive).
mod media;
pub mod index;
mod run;

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
    #[serde(default = "default_loop")]
    pub loop_hours: u32,
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
        };
        if !listens && !self.url.starts_with(scheme) {
            return Err(format!("the address starts with {scheme}"));
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

pub struct Manager {
    data_dir: PathBuf,
    recordings_dir: Mutex<PathBuf>,
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
            events,
            recs: Mutex::new(Vec::new()),
            pins: Mutex::new(Vec::new()),
            ffmpeg: std::env::var("SC_FFMPEG").unwrap_or_else(|_| "ffmpeg".into()),
            grabs: tokio::sync::Semaphore::new(4),
        });
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
                    if tick % 5 == 0 {
                        m.trim(&r);
                    }
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

    pub fn recordings_dir(&self) -> PathBuf {
        self.recordings_dir.lock().unwrap().clone()
    }

    pub fn set_config(&self, cfg: &ServerConfig) {
        *self.recordings_dir.lock().unwrap() = cfg.recordings_dir(&self.data_dir);
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
        let runs = load_runs(&dir);
        let rec = Arc::new(Rec {
            id: c.id.clone(),
            status: Mutex::new(Status { id: c.id.clone(), state: State::Off, since: now_ms(), message: String::new(), media: None, bitrate: 0, oldest: None, newest: None, bytes: 0 }),
            cfg: watch::channel(c).0,
            runs: RwLock::new(runs),
            stop: watch::channel(false).0,
            dir: Mutex::new(dir),
            no_audio: false.into(),
            task: Mutex::new(None),
        });
        rec.refresh_status();
        let handle = tokio::spawn(run::supervise(self.clone(), rec.clone()));
        *rec.task.lock().unwrap() = Some(handle);
        self.recs.lock().unwrap().push(rec.clone());
        rec
    }

    /// Checks a recorder's settings against the others (ports).
    fn check(&self, c: &RecorderConfig) -> Result<(), String> {
        c.validate()?;
        if c.listens() {
            for r in self.recs() {
                let o = r.config();
                if o.id != c.id && o.listens() && o.port == c.port {
                    return Err(format!("port {} is used by {}", c.port, o.name));
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
        self.check(&c)?;
        let v = c.public();
        self.add(c);
        self.save().map_err(|e| e.to_string())?;
        self.list_changed();
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
        self.check(&c)?;
        let v = c.public();
        if c != old {
            rec.cfg.send_replace(c); // the supervisor restarts FFmpeg
            self.save().map_err(|e| e.to_string())?;
            self.list_changed();
        }
        Ok(v)
    }

    pub async fn delete(&self, id: &str) -> Result<(), String> {
        let rec = self.get(id).ok_or("no such recorder")?;
        self.recs.lock().unwrap().retain(|r| r.id != id);
        self.save().map_err(|e| e.to_string())?;
        self.list_changed();
        let _ = rec.stop.send(true);
        let task = rec.task.lock().unwrap().take();
        if let Some(t) = task {
            let _ = t.await;
        }
        let dir = rec.dir.lock().unwrap().clone();
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await;
        Ok(())
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
                while let Some(p) = run.track(video).segs.front() {
                    if p.t1 > start || (run.live && run.track(video).segs.len() == 1) {
                        break;
                    }
                    doomed.push(run.proxy_path(video, p.n));
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
