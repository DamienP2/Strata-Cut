//! The recorders: one FFmpeg process per source, writing a loop of segments
//! that never stops (the oldest minutes are removed as new ones arrive).
mod media;
pub mod index;
pub(crate) mod run;
pub use run::AUDIO_SYNC;
pub mod web;
pub mod epg;
pub mod schedule;
pub mod decklink;
pub mod sdk;
pub mod probe;
pub mod hls;
pub mod pattern;
pub mod logo;

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

/// Work that may wait (clips, stills, levels) runs with a lower CPU priority, so that it
/// never slows the recorders' FFmpeg down (a slowed reception loses packets: glitches).
pub fn background(cmd: &mut tokio::process::Command) {
    lower_priority(cmd, 10);
}

/// A child `by` steps nicer than the server (the recorders' receivers stay at the server's).
pub fn lower_priority(cmd: &mut tokio::process::Command, by: i32) {
    // SAFETY: setpriority is async-signal-safe and only touches the child.
    unsafe {
        cmd.pre_exec(move || {
            libc::setpriority(libc::PRIO_PROCESS, 0, by);
            Ok(())
        });
    }
}

/// The same for a std Command.
pub fn background_std(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: as above.
    unsafe {
        cmd.pre_exec(|| {
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            Ok(())
        });
    }
}

/// MPEG timestamps (33 bits at 90 kHz) start again from 0 every 26 h 30 or so.
pub const PTS_WRAP: f64 = 8_589_934_592.0 / 90_000.0;

/// A time read in an MPEG-TS file (its clock starts again from 0 every 26 h 30) put on the turn
/// of `near`, a time kept whole (an RTMP encoder up for days gives large ones).
pub fn same_turn(t: f64, near: f64) -> f64 {
    t + ((near - t) / PTS_WRAP).round() * PTS_WRAP
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Udp,
    Rtp,
    Rtmp,
    Srt,
    /// A YouTube live, read with yt-dlp.
    Web,
    /// An HLS stream: its playlist's address (http(s)://….m3u8), read by FFmpeg.
    Hls,
    /// An input of a Blackmagic DeckLink card (SDI, HDMI…), captured with Blackmagic's SDK.
    Decklink,
    /// A test pattern made here (`pattern`): bars, movement, lip sync, logo, in the format chosen.
    Pattern,
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
    /// DeckLink: the card (its number, from 0), its video input, the video format ("auto":
    /// detected), the audio input and the channels captured (recorded by stereo pairs).
    /// The recording's bitrate is `bitrate`.
    #[serde(default)]
    pub device: u32,
    #[serde(default = "auto")]
    pub connection: String,
    #[serde(default = "auto")]
    pub video_mode: String,
    #[serde(default = "two")]
    pub audio_channels: u32,
    #[serde(default = "auto")]
    pub audio_input: String,
    /// Not recorded during its programmed slots either (Programmation: "Non enregistrée"); with
    /// `enabled` (continuous) false. Still received while a stream sends it somewhere.
    #[serde(default)]
    pub slots_off: bool,
    /// Test pattern: "smpte", "ebu" or "animated" (its format: `video_mode`, its bitrate: `bitrate`).
    #[serde(default = "smpte")]
    pub pattern: String,
    /// HLS: the stream read when the playlist offers several (its playlist's address as written
    /// in it, see `hls`; empty: the best picture).
    #[serde(default)]
    pub variant: String,
}

fn auto() -> String {
    "auto".into()
}
fn smpte() -> String {
    "smpte".into()
}
fn two() -> u32 {
    2
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
        if self.protocol == Protocol::Decklink && self.bitrate == 0.0 {
            self.bitrate = decklink::DEFAULT_BITRATE;
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
            Protocol::Web | Protocol::Hls => "https://",
            Protocol::Decklink => return decklink::validate(self),
            Protocol::Pattern => return pattern::validate(self),
        };
        if self.protocol == Protocol::Hls {
            if !self.url.starts_with("https://") && !self.url.starts_with("http://") {
                return Err("the address of the stream starts with https://".into());
            }
        } else if self.protocol == Protocol::Web {
            if !self.url.starts_with("https://") && !self.url.starts_with("http://") {
                return Err("the address of the page starts with https://".into());
            }
            if web::platform(&self.url) != "youtube" {
                return Err("only youtube lives are recorded".into());
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
        if self.variant.len() > 2000 {
            return Err("the hls stream chosen is not valid".into());
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
    /// Programmed: waiting for its next slot.
    Waiting,
}

/// A reception problem seen in FFmpeg's words.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Glitch {
    /// Packets lost before FFmpeg (network, or a receiver too slow).
    Lost,
    /// A damaged stream (blocks, freezes in the picture).
    Damaged,
}

/// A slot of the programme, for the interface.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SlotInfo {
    pub name: String,
    pub start: i64,
    pub end: i64,
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
    /// The last bitrate measured while recording, bit/s (the loop's space follows it).
    pub measured: u64,
    /// The programme on air now and the next one, from the source's guide.
    pub on_air: Option<epg::Event>,
    pub next: Option<epg::Event>,
    /// Programmed: the slot recording now, and the next one.
    pub slot: Option<SlotInfo>,
    pub next_slot: Option<SlotInfo>,
    /// Reception problems in the last 10 minutes: packets lost, damaged stream.
    pub lost: u32,
    pub damaged: u32,
    /// The server is short of CPU: how far the preview is behind the recording (s), and
    /// whether it had to drop some of the stream in the last 10 minutes (the recording is whole).
    pub preview_lag: u32,
    pub preview_dropped: bool,
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
    /// The programme guide of the source (DVB EIT), when it carries one.
    pub epg: Mutex<epg::Guide>,
    /// Where FFmpeg found the guide in the source (learned at the first start).
    pub epg_found: Mutex<epg::Found>,
    /// FFmpeg could not write the guide of this source: recorded without it.
    pub epg_failed: std::sync::atomic::AtomicBool,
    /// The guide's place in the source comes from an earlier start (it may have changed).
    pub epg_from_disk: std::sync::atomic::AtomicBool,
    /// Reception problems of the last 10 minutes (when, which), and when the log last told them.
    glitches: Mutex<(std::collections::VecDeque<(i64, Glitch)>, i64)>,
    /// Where the last recording of this source ended (source time, s): what a web page gives
    /// again after a cut is not recorded twice.
    pub last_end: Mutex<Option<f64>>,
    /// A DeckLink card without signal on its input (its frames, black, are recorded all the same).
    pub no_signal: std::sync::atomic::AtomicBool,
    /// How far the preview is behind the recording (s); when it last dropped some of the stream.
    pub preview_lag: std::sync::atomic::AtomicU32,
    preview_dropped_at: std::sync::atomic::AtomicI64,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Local UDP ports the stream as received is copied to (the streams to destinations: see
    /// `crate::streams`), each with its key.
    pub taps: Mutex<Vec<(u64, u16)>>,
    /// Streams sending this source somewhere: it is received (and recorded) while there are.
    pub streams: std::sync::atomic::AtomicU32,
    /// Wakes a recorder waiting for its next slot (a stream starts).
    pub wake: tokio::sync::Notify,
}

impl Rec {
    pub fn config(&self) -> RecorderConfig {
        self.cfg.borrow().clone()
    }

    /// Counts a reception problem (the journal tells them every 10 minutes: Manager::report_glitches).
    pub fn glitch(&self, kind: Glitch) {
        let now = now_ms();
        let mut g = self.glitches.lock().unwrap();
        g.0.push_back((now, kind));
        while g.0.front().is_some_and(|(t, _)| now - t > 600_000) {
            g.0.pop_front();
        }
    }

    /// The preview could not keep up and dropped some of the stream (logged once a minute at most).
    pub fn preview_dropped(&self) {
        let now = now_ms();
        let last = self.preview_dropped_at.swap(now, std::sync::atomic::Ordering::Relaxed);
        if now - last >= 60_000 {
            tracing::warn!("{}: the server is too busy, the preview drops some of the stream (the recording is whole)", self.config().name);
        }
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
        if s.bitrate > 0 {
            s.measured = s.bitrate;
        }
        // The guide's programme now and the next one (without their long texts).
        let now = now_ms() as f64;
        let g = self.epg.lock().unwrap();
        let short = |e: &epg::Event| epg::Event { text: String::new(), ..e.clone() };
        s.on_air = g.events.iter().find(|e| e.start <= now && now < e.end).map(short);
        s.next = g.events.iter().find(|e| e.start > now).map(short);
        drop(g);
        let cut = self.space_trimmed.load(std::sync::atomic::Ordering::Relaxed);
        let cfg = self.config();
        // The reception problems of the last 10 minutes (the status is already held: no new lock on it).
        {
            let now = now_ms();
            let g = self.glitches.lock().unwrap();
            let recent = g.0.iter().filter(|(t, _)| now - t < 600_000);
            let (lost, damaged) = recent.fold((0u32, 0u32), |(a, b), (_, k)| if *k == Glitch::Lost { (a + 1, b) } else { (a, b + 1) });
            drop(g);
            s.lost = lost;
            s.damaged = damaged;
            s.preview_lag = self.preview_lag.load(std::sync::atomic::Ordering::Relaxed);
            s.preview_dropped = now - self.preview_dropped_at.load(std::sync::atomic::Ordering::Relaxed) < 600_000;
        }
        let short = matches!((oldest, newest), (Some(a), Some(b)) if ((b - a) as f64) < cfg.loop_hours as f64 * 3_600_000.0 * 0.98);
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

/// A programmed recording starts this long before its slot (the connection takes time).
pub const PREROLL_MS: i64 = 60_000;

/// Bitrate assumed for a source never measured yet, Mb/s.
const DEFAULT_MBPS: f64 = 8.0;

/// Bytes a full loop takes (proxies and 5 % included): at the bitrate measured,
/// else the one given in the settings (former versions), else 8 Mb/s.
pub fn loop_need(cfg: &RecorderConfig, measured_bps: u64) -> Option<u64> {
    let mbps = if measured_bps > 0 { measured_bps as f64 / 1e6 } else if cfg.bitrate > 0.0 { cfg.bitrate } else { DEFAULT_MBPS };
    Some(((mbps + PROXY_MBPS) * 1e6 / 8.0 * 3600.0 * cfg.loop_hours as f64 * 1.05) as u64)
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
    /// The UDP datagrams the system dropped so far (its receive buffers full), at the last report.
    udp_dropped: Mutex<Option<u64>>,
    /// The same for the network cards (their receive rings full).
    nic_dropped: Mutex<Option<u64>>,
    /// The programmed slots of the recorders in schedule mode.
    pub schedules: schedule::Schedules,
    /// The server's time zone (the slots' hours are in it).
    tz: Mutex<jiff::tz::TimeZone>,
    /// recorders.json could not be written (disk full): written again later.
    unsaved: std::sync::atomic::AtomicBool,
}

impl Manager {
    pub fn start(data_dir: &Path, cfg: &ServerConfig, events: Events) -> Arc<Self> {
        let m = Arc::new(Manager {
            udp_dropped: Mutex::new(udp_dropped()),
            nic_dropped: Mutex::new(nic_dropped()),
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
            schedules: schedule::Schedules::load(data_dir.join("schedules.json")),
            tz: Mutex::new(jiff::tz::TimeZone::get(&cfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC)),
            unsaved: false.into(),
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
                        if m2.unsaved.load(std::sync::atomic::Ordering::Relaxed) && m2.save().is_ok() {
                            tracing::info!("the recorders are saved again");
                        }
                    })
                    .await;
                }
                let tz = m.tz();
                let now_t = now_ms();
                for r in m.recs() {
                    r.refresh_status();
                    let info = |o: Option<(schedule::Slot, i64, i64)>| o.map(|(s, a, b)| SlotInfo { name: s.name, start: a, end: b });
                    let (slot, next) = (info(m.schedules.active(&r.id, &tz, now_t)), info(m.schedules.next(&r.id, &tz, now_t)));
                    let mut st = r.status.lock().unwrap();
                    st.slot = slot;
                    st.next_slot = next;
                }
                tick += 1;
                // The reception problems, every 10 minutes.
                if tick.is_multiple_of(300) {
                    m.report_glitches();
                }
                let now = m.status_json();
                if now != last {
                    m.events.send(Audience::All, json!({"type": "status", "status": now}));
                    last = now;
                }
            }
        });
        m
    }

    /// The reception problems of the last 10 minutes, in the journal: recorders damaged at the
    /// same moments on one line (the stream arrives so: the source or the network), the others
    /// one each; and the datagrams the system itself dropped (its buffers full: this server).
    fn report_glitches(&self) {
        let now = now_ms();
        let since = |last: &Mutex<Option<u64>>, n: Option<u64>| {
            let mut last = last.lock().unwrap();
            let d = match (*last, n) {
                (Some(a), Some(b)) => b.saturating_sub(a),
                _ => 0,
            };
            *last = n;
            d
        };
        let dropped = since(&self.udp_dropped, udp_dropped());
        let nic = since(&self.nic_dropped, nic_dropped());
        let recs: Vec<(String, Vec<(i64, Glitch)>)> = self
            .recs()
            .iter()
            .map(|r| (r.config().name, r.glitches.lock().unwrap().0.iter().filter(|(t, _)| now - t < 600_000).copied().collect::<Vec<_>>()))
            .filter(|(_, e)| !e.is_empty())
            .collect();
        for line in glitch_lines(&recs, dropped, nic) {
            tracing::warn!("{line}");
        }
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
        if let Ok(tz) = jiff::tz::TimeZone::get(&cfg.timezone) {
            *self.tz.lock().unwrap() = tz;
            self.schedules.changed.notify_waiters();
        }
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

    /// Writes recorders.json; when the disk refuses (full), it is written again later,
    /// so that a recorder deleted meanwhile does not come back at the next start.
    fn save(&self) -> std::io::Result<()> {
        let list: Vec<RecorderConfig> = self.recs().iter().map(|r| r.config()).collect();
        let r = crate::paths::write_json(&self.config_path(), &list, true);
        self.unsaved.store(r.is_err(), std::sync::atomic::Ordering::Relaxed);
        if let Err(e) = &r {
            tracing::error!("the recorders could not be saved ({e}): tried again every 10 s");
        }
        r
    }

    fn add(self: &Arc<Self>, c: RecorderConfig) -> Arc<Rec> {
        let dir = self.recordings_dir().join(&c.id);
        let mut runs: Vec<Run> = self.dirs.lock().unwrap().iter().flat_map(|d| load_runs(&d.join(&c.id))).collect();
        runs.sort_by_key(|r| r.meta.started);
        let guide = epg::Guide::load(&epg::path(&self.data_dir, &c.id));
        let found: epg::Found = crate::paths::read_json(&epg::found_path(&self.data_dir, &c.id)).unwrap_or_default();
        let found_known = found.stream.is_some();
        let rec = Arc::new(Rec {
            id: c.id.clone(),
            status: Mutex::new(Status { id: c.id.clone(), state: State::Off, since: now_ms(), message: String::new(), media: None, bitrate: 0, oldest: None, newest: None, bytes: 0, space_limited: false, measured: 0, on_air: None, next: None, slot: None, next_slot: None, lost: 0, damaged: 0, preview_lag: 0, preview_dropped: false }),
            cfg: watch::channel(c).0,
            runs: RwLock::new(runs),
            stop: watch::channel(false).0,
            dir: Mutex::new(dir),
            no_audio: false.into(),
            space_trimmed: 0.into(),
            new_run: tokio::sync::Notify::new(),
            epg: Mutex::new(guide),
            epg_found: Mutex::new(found),
            epg_failed: false.into(),
            epg_from_disk: found_known.into(),
            glitches: Mutex::new((std::collections::VecDeque::new(), 0)),
            preview_lag: 0.into(),
            no_signal: false.into(),
            last_end: Mutex::new(None),
            preview_dropped_at: 0.into(),
            task: Mutex::new(None),
            taps: Mutex::new(Vec::new()),
            streams: 0.into(),
            wake: tokio::sync::Notify::new(),
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
            self.counts(&c).then(|| self.need_of(&c, r.status.lock().unwrap().measured)).flatten()
        }).sum()
    }

    pub fn tz(&self) -> jiff::tz::TimeZone {
        self.tz.lock().unwrap().clone()
    }

    /// Bytes a recorder keeps when full: its loop (what its slots record lives in it too).
    pub fn need_of(&self, c: &RecorderConfig, measured: u64) -> Option<u64> {
        loop_need(c, measured)
    }

    /// The recorder takes space: it records, or it has slots programmed.
    pub fn counts(&self, c: &RecorderConfig) -> bool {
        c.enabled || !self.schedules.of(&c.id).iter().all(|s| !s.enabled)
    }

    /// A stopped recorder records during its slots, from a little before (the connection takes time).
    /// Received now: a stream sends it somewhere, or one of its slots (unless it is not recorded).
    pub fn wants(&self, id: &str, now: i64) -> bool {
        if let Some(r) = self.get(id) {
            if r.streams.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                return true;
            }
            if r.config().slots_off {
                return false;
            }
        }
        let tz = self.tz();
        self.schedules.active(id, &tz, now).is_some() || self.schedules.active(id, &tz, now + PREROLL_MS).is_some()
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
            let measured = r.status.lock().unwrap().measured;
            let need = if self.counts(&o) { self.need_of(&o, measured).unwrap_or(0) } else { 0 };
            before += need;
            if o.id == c.id {
                known = true;
                after += self.need_of(c, measured).unwrap_or(0);
            } else {
                after += need;
            }
        }
        if !known {
            after += self.need_of(c, 0).unwrap_or(0);
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
        // One recorder per card input.
        if c.protocol == Protocol::Decklink {
            for r in self.recs() {
                let o = r.config();
                if o.id != c.id && o.protocol == Protocol::Decklink && o.device == c.device {
                    return Err("this card is used by another recorder".into());
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
        let _ = std::fs::remove_file(epg::path(&self.data_dir, id));
        let _ = std::fs::remove_file(epg::found_path(&self.data_dir, id));
        decklink::remove_fifos(&self.data_dir, id);
        logo::remove(&self.data_dir, id);
        self.schedules.delete_recorder(id);
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
        Value::Array(
            self.recs()
                .iter()
                .map(|r| {
                    let mut v = r.config().public();
                    // The logo's version: the interface asks for it again when it changes.
                    v["logo"] = json!(logo::version(&self.data_dir, &r.id));
                    v
                })
                .collect(),
        )
    }

    pub fn status_json(&self) -> Value {
        Value::Array(self.recs().iter().map(|r| serde_json::to_value(&*r.status.lock().unwrap()).unwrap_or_default()).collect())
    }

    /// Removes what is older than the loop (except pinned segments).
    pub fn trim(&self, rec: &Rec) {
        let cutoff = (now_ms() - rec.config().loop_hours as i64 * 3_600_000) as f64;
        self.trim_before(rec, cutoff);
    }

    /// Empties the loop: everything recorded so far goes (what a clip is being cut from stays).
    pub fn clear(&self, rec: &Rec) {
        self.trim_before(rec, f64::INFINITY);
        tracing::warn!("{}: loop emptied", rec.config().name);
        rec.refresh_status();
    }

    /// Removes the segments that end before `cutoff` (server time, ms).
    fn trim_before(&self, rec: &Rec, cutoff: f64) {
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
        // the measured bitrate wins
        assert!(loop_need(&c, 20_000_000).unwrap() > loop_need(&c, 0).unwrap());
        assert!(loop_need(&c, 2_000_000).unwrap() < loop_need(&c, 0).unwrap());
        // nothing known: 8 Mb/s
        c.bitrate = 0.0;
        assert_eq!(loop_need(&c, 0).map(|b| b / 1_000_000_000), Some(107));
    }

    #[test]
    fn system_reserves() {
        // The system disk: 5 %, 10 GB at least, a quarter at most.
        assert_eq!(system_reserve(Path::new("/"), 1_000_000_000_000), 50_000_000_000);
        assert_eq!(system_reserve(Path::new("/"), 100_000_000_000), 10 << 30);
        assert_eq!(system_reserve(Path::new("/"), 16_000_000_000), 4_000_000_000);
    }
}

/// The UDP datagrams the system dropped since it started (its receive buffers full), from
/// /proc/net/snmp ("RcvbufErrors"); None elsewhere.
fn udp_dropped() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/net/snmp").ok()?;
    let mut udp = text.lines().filter(|l| l.starts_with("Udp:"));
    let (names, values) = (udp.next()?, udp.next()?);
    let i = names.split_whitespace().position(|n| n == "RcvbufErrors")?;
    values.split_whitespace().nth(i)?.parse().ok()
}

/// The packets the network cards dropped since the system started, their receive rings full
/// (lost before any program sees them: "missed", "fifo" and "over" errors of each card in
/// /sys/class/net), with those the kernel's backlog dropped; None elsewhere. Not the cards'
/// "dropped": it also counts the frames the system ignores on purpose (the switch's own
/// messages, protocols it does not speak).
fn nic_dropped() -> Option<u64> {
    let cards = nic_overflows_in(Path::new("/sys/class/net"))?;
    // And the kernel's queue behind them (its backlog full: /proc/net/softnet_stat, 2nd column).
    let backlog = std::fs::read_to_string("/proc/net/softnet_stat").map(|t| softnet_dropped_in(&t)).unwrap_or(0);
    Some(cards + backlog)
}

fn softnet_dropped_in(text: &str) -> u64 {
    text.lines().filter_map(|l| l.split_whitespace().nth(1)).filter_map(|x| u64::from_str_radix(x, 16).ok()).sum()
}

/// The ring overflows of the cards under `root` (those with a device: not the loopback, bridges, taps).
fn nic_overflows_in(root: &Path) -> Option<u64> {
    let mut sum = 0;
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let dir = e.path();
        if !dir.join("device").exists() {
            continue;
        }
        for k in ["rx_missed_errors", "rx_fifo_errors", "rx_over_errors"] {
            sum += std::fs::read_to_string(dir.join("statistics").join(k)).ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0);
        }
    }
    Some(sum)
}

/// The journal's lines for the reception problems of the last 10 minutes (by recorder: when and
/// what). Recorders whose problems fall at the same moments (within 2 s, for most of them) are
/// one line: the stream arrives so. `dropped`: the datagrams the system dropped meanwhile;
/// `nic`: the packets the network cards dropped.
fn glitch_lines(recs: &[(String, Vec<(i64, Glitch)>)], dropped: u64, nic: u64) -> Vec<String> {
    let shared = |i: usize| {
        let mine = &recs[i].1;
        let common = mine.iter().filter(|(t, _)| recs.iter().enumerate().any(|(j, (_, o))| j != i && o.iter().any(|(u, _)| (t - u).abs() <= 2000))).count();
        common * 10 >= mine.len() * 6
    };
    let together: Vec<usize> = (0..recs.len()).filter(|&i| shared(i)).collect();
    let mut out = Vec::new();
    let count = |e: &[(i64, Glitch)]| {
        let lost = e.iter().filter(|(_, k)| *k == Glitch::Lost).count();
        (lost, e.len() - lost)
    };
    if together.len() >= 2 {
        let names: Vec<String> = together.iter().map(|&i| format!("{} ({})", recs[i].0, recs[i].1.len())).collect();
        let why = if nic > 0 {
            "the network card of this server dropped packets: this server"
        } else if dropped > 0 {
            "the system's network buffers were full: this server"
        } else {
            "the stream arrives so: the source or the network, not this server"
        };
        out.push(format!("reception problems at the same moments in the last 10 minutes on {}: {why}", names.join(", ")));
    }
    for (i, (name, e)) in recs.iter().enumerate() {
        if together.len() >= 2 && together.contains(&i) {
            continue;
        }
        let (lost, damaged) = count(e);
        out.push(format!("{name}: reception problems in the last 10 minutes: {lost} packets lost by this server, {damaged} damaged on arrival"));
    }
    if nic > 0 {
        out.push(format!("the network cards dropped {nic} packets in the last 10 minutes, before any program read them (their receive rings too small, or the server short of processor): sudo ethtool -G <card> rx <its maximum>, which the installer sets"));
    }
    if dropped > 0 {
        out.push(format!("the system dropped {dropped} UDP packets in the last 10 minutes (its network buffers full: the server short of processor, or the buffers too small)"));
    }
    out
}

#[cfg(test)]
mod glitch_tests {
    use super::*;

    #[test]
    fn together_or_not() {
        let at = |ts: &[i64]| ts.iter().map(|t| (*t, Glitch::Damaged)).collect::<Vec<_>>();
        let recs = vec![("TF1".to_string(), at(&[1000, 60_000, 120_500])), ("TMC".to_string(), at(&[1500, 60_800])), ("TFX".to_string(), at(&[120_000])), ("France 2".to_string(), at(&[300_000]))];
        let l = glitch_lines(&recs, 0, 0);
        assert_eq!(l.len(), 2, "{l:?}");
        assert!(l[0].contains("TF1 (3), TMC (2), TFX (1)") && l[0].contains("not this server"), "{l:?}");
        assert!(l[1].starts_with("France 2:"), "{l:?}");
        let l = glitch_lines(&recs, 40, 0);
        assert!(l[0].contains("this server") && !l[0].contains("not this server") && l.last().unwrap().contains("dropped 40"), "{l:?}");
        assert!(glitch_lines(&[], 0, 0).is_empty());
        let l = glitch_lines(&recs, 0, 12);
        assert!(l[0].contains("network card of this server") && l.iter().any(|x| x.contains("dropped 12 packets")), "{l:?}");
    }

    #[test]
    fn nic_drops() {
        let root = std::env::temp_dir().join(format!("sc-nic-{}", std::process::id()));
        let card = |name: &str, device: bool, missed: &str| {
            let d = root.join(name).join("statistics");
            std::fs::create_dir_all(&d).unwrap();
            if device {
                std::fs::create_dir_all(root.join(name).join("device")).unwrap();
            }
            std::fs::write(d.join("rx_missed_errors"), missed).unwrap();
            std::fs::write(d.join("rx_fifo_errors"), "1\n").unwrap();
            std::fs::write(d.join("rx_dropped"), "939\n").unwrap();
        };
        card("ens18", true, "4\n");
        card("lo", false, "50\n");
        // The ignored frames ("dropped") and the cards without a device are not counted.
        assert_eq!(nic_overflows_in(&root), Some(5));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(softnet_dropped_in("0001a2b3 00000010 00000000\n0001a2b3 00000002 00000001\n"), 18);
    }
}
