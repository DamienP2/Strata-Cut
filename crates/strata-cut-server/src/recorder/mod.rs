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
pub mod ndi;
mod udp_in;

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

/// The preview's sound tracks besides the first (all the source's, as the Streamer mixes them:
/// each can be heard alone).
pub const PREVIEW_EXTRA: u32 = 31;

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
    /// An NDI source of the network (`url`: its name), received with the NDI library (`ndi`).
    Ndi,
    /// A web page shown by a browser of the server (`crate::pages`): its address, its frame and
    /// its piloting from the Streamer and the Extraction.
    Page,
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
    /// Who pressed REC last (the account's id): "stop only their recordings" goes by it.
    #[serde(default)]
    pub rec_by: String,
    /// Expected bitrate of the source, Mb/s (reserves the loop's space; 0 = unknown).
    #[serde(default)]
    pub bitrate: f64,
    /// For a source the server encodes (a page, a pattern, NDI, DeckLink): a constant rate (CBR),
    /// else a variable one (VBR: the quality kept, the bitrate a ceiling).
    #[serde(default)]
    pub cbr: bool,
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
    /// NDI: addresses where to look for the source when the network does not announce it
    /// (comma separated; empty: the local network).
    #[serde(default)]
    pub host: String,
}

/// Empty settings (to fill a few of them: a preview, a test).
/// x264's rate control: VBR (the quality kept, `kbps` a ceiling) or CBR (exactly `kbps`, with the
/// hypothetical decoder's buffer said in the stream).
pub fn rate_args(kbps: u32, cbr: bool) -> Vec<String> {
    let k = format!("{kbps}k");
    if cbr {
        ["-b:v", &k, "-minrate", &k, "-maxrate", &k, "-bufsize", &k, "-x264-params", "nal-hrd=cbr"].map(String::from).to_vec()
    } else {
        ["-crf", "20", "-maxrate", &k, "-bufsize", &format!("{}k", kbps * 2)].map(String::from).to_vec()
    }
}

/// CBR: the MPEG-TS padded to a constant rate (the video, the sound, the stream's own overhead).
pub fn muxrate_args(video_kbps: u32, audio_kbps: u32, cbr: bool) -> Vec<String> {
    if !cbr {
        return Vec::new();
    }
    let total = ((video_kbps + audio_kbps) as f64 * 1.08) as u32 + 100;
    vec!["-muxrate".into(), format!("{total}k")]
}

pub fn blank_config() -> RecorderConfig {
    RecorderConfig {
        id: String::new(),
        name: String::new(),
        protocol: Protocol::Udp,
        url: String::new(),
        mode: Mode::Caller,
        port: 0,
        path: String::new(),
        latency: default_latency(),
        passphrase: String::new(),
        iface: String::new(),
        cookies: String::new(),
        loop_hours: default_loop(),
        rec_by: String::new(),
        bitrate: 0.0,
        cbr: false,
        enabled: false,
        device: 0,
        connection: auto(),
        video_mode: auto(),
        audio_channels: 2,
        audio_input: auto(),
        slots_off: false,
        pattern: smpte(),
        variant: String::new(),
        host: String::new(),
    }
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
        self.host = self.host.split(',').map(str::trim).filter(|h| !h.is_empty()).collect::<Vec<_>>().join(",");
        if self.path.is_empty() && self.protocol == Protocol::Rtmp {
            self.path = "live/stream".into();
        }
        if self.protocol == Protocol::Decklink && self.bitrate == 0.0 {
            self.bitrate = decklink::DEFAULT_BITRATE;
        }
        if self.protocol == Protocol::Ndi && self.bitrate == 0.0 {
            self.bitrate = ndi::DEFAULT_BITRATE;
        }
        if self.protocol == Protocol::Page && self.bitrate == 0.0 {
            self.bitrate = 6.0;
        }
        // A test pattern is not recorded: made live while the Streamer shows it or sends it.
        if self.protocol == Protocol::Pattern {
            self.enabled = false;
            self.slots_off = true;
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
            Protocol::Ndi => return ndi::validate(self),
            Protocol::Page => {
                if !crate::pages::FORMATS.contains(&self.video_mode.as_str()) {
                    return Err("choose the format of the page".into());
                }
                if !(1.0..=50.0).contains(&self.bitrate) {
                    return Err("the bitrate is 1 to 50 mb/s".into());
                }
                return Ok(());
            }
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

    /// For clients: the passphrase and the cookies masked.
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
    /// The loop alone (without what programmed slots recorded before it, kept longer): its
    /// oldest instant, and its bytes.
    pub loop_from: Option<i64>,
    pub loop_bytes: u64,
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
    /// What the picture and the sound show now (the preview watches them): black, frozen or
    /// silent since (server time, ms).
    pub black_since: Option<i64>,
    pub frozen_since: Option<i64>,
    pub silent_since: Option<i64>,
}

/// What the preview sees in the picture and hears in the sound: since when (server time, ms).
#[derive(Default, Clone, Debug)]
pub struct Content {
    pub black: Option<i64>,
    pub frozen: Option<i64>,
    pub silent: Option<i64>,
    /// Said in the journal already (each once, until it ends).
    pub said: [bool; 3],
}

/// Black, frozen or silent this long (ms): said in the journal (and to the alerts).
pub const CONTENT_SAY_MS: i64 = 10_000;

impl Content {
    /// A line of the preview's FFmpeg: a black picture, a frozen one or silence starting or ending.
    /// True when it was one.
    pub fn read(&mut self, line: &str, now: i64) -> bool {
        let mut set = |slot: usize, on: bool| {
            let v = match slot {
                0 => &mut self.black,
                1 => &mut self.frozen,
                _ => &mut self.silent,
            };
            if on {
                v.get_or_insert(now);
            } else {
                *v = None;
            }
        };
        if line.contains("lavfi.black_start=") {
            set(0, true);
        } else if line.contains("lavfi.black_end=") {
            set(0, false);
        } else if line.contains("freeze_start:") {
            set(1, true);
        } else if line.contains("freeze_end:") {
            set(1, false);
        } else if line.contains("silence_start:") {
            set(2, true);
        } else if line.contains("silence_end:") {
            set(2, false);
        } else {
            return false;
        }
        true
    }
}

/// `scheme://user:pass@host:port/path?query` → `scheme://host:port`; a name (NDI, DeckLink) as it is.
pub fn only_host(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_string() };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    format!("{scheme}://{host}")
}

pub use crate::util::now_ms;

/// A test pattern is made while the Streamer asked for it within this (it asks every 10 s).
pub const DEMAND_MS: i64 = 30_000;
/// What a test pattern keeps: the Streamer's player reads only the last seconds.
const PATTERN_KEEP_MS: i64 = 120_000;

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
    /// What its programmed slots recorded (server times, ms), kept longer than its loop (as the
    /// last trim saw them).
    pub kept: Mutex<Vec<(f64, f64)>>,
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
    /// What the witness of a multicast source saw on the network (since the last report).
    pub net: Arc<udp_in::Counts>,
    /// Damaged packets FFmpeg read, of picture and of sound (since the last report).
    pub damage: [std::sync::atomic::AtomicU64; 2],
    /// A source given as UDP that sends RTP: read as RTP.
    pub rtp: std::sync::atomic::AtomicBool,
    /// Where the last recording of this source ended (source time, s): what a web page gives
    /// again after a cut is not recorded twice.
    pub last_end: Mutex<Option<f64>>,
    /// A DeckLink card without signal on its input (its frames, black, are recorded all the same).
    pub no_signal: std::sync::atomic::AtomicBool,
    /// How far the preview is behind the recording (s); when it last dropped some of the stream.
    pub preview_lag: std::sync::atomic::AtomicU32,
    preview_dropped_at: std::sync::atomic::AtomicI64,
    /// When the relay last passed the stream on (server time, ms): a live sees the source lost
    /// at once, before its recorder notices.
    pub last_packet: std::sync::atomic::AtomicI64,
    /// When the source last sent sound (an audio packet, server time, ms; 0: never).
    pub last_sound: std::sync::atomic::AtomicI64,
    /// Sound tracks the preview makes besides the first (0 to 3: the player's choice of track),
    /// and a run started again when the source turns out to have more.
    pub audio_extra: std::sync::atomic::AtomicU32,
    /// The input's streams that came without anything to describe them (an audio track that
    /// sends nothing): the next run leaves them out, they would stop the recording.
    pub empty_streams: Mutex<Vec<u32>>,
    pub restart_run: tokio::sync::Notify,
    /// The picture black, frozen, the sound silent: since (server time, ms), and whether the
    /// journal already said so.
    pub content: Mutex<Content>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Local UDP ports the stream as received is copied to (the streams to destinations: see
    /// `crate::streams`), each with its key.
    pub taps: Mutex<Vec<(u64, u16)>>,
    /// Streams sending this source somewhere: it is received (and recorded) while there are.
    pub streams: std::sync::atomic::AtomicU32,
    /// Wakes a recorder waiting for its next slot (a stream starts).
    pub wake: tokio::sync::Notify,
    /// A test pattern: when the Streamer last asked for it (server ms).
    pub demand: std::sync::atomic::AtomicI64,
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
    /// Its state now.
    pub fn state(&self) -> State {
        self.status.lock().unwrap().state
    }

    /// Received: recording, or starting to.
    pub fn running(&self) -> bool {
        matches!(self.state(), State::Recording | State::Starting)
    }

    /// The stream comes now: the source records and its relay passed packets in the last 2 s.
    pub fn flowing(&self) -> bool {
        self.state() == State::Recording && now_ms() - self.last_packet.load(std::sync::atomic::Ordering::Relaxed) < 2000
    }

    /// A picture black or frozen, a sound silent for 10 s: said in the journal, and when it ends.
    pub fn say_content(&self) {
        let now = now_ms();
        let name = self.config().name;
        let mut c = self.content.lock().unwrap();
        let what = ["black picture", "frozen picture", "silence"];
        let since = [c.black, c.frozen, c.silent];
        for k in 0..3 {
            match since[k] {
                Some(t) if !c.said[k] && now - t >= CONTENT_SAY_MS => {
                    c.said[k] = true;
                    tracing::warn!("{name}: {} for {} s", what[k], (now - t) / 1000);
                }
                None if c.said[k] => {
                    c.said[k] = false;
                    tracing::info!("{name}: {} ended", what[k]);
                }
                _ => {}
            }
        }
    }

    pub fn refresh_status(&self) {
        let runs = self.runs.read().unwrap();
        let oldest = runs.iter().filter_map(Run::first_wall).reduce(f64::min).map(|w| w as i64);
        let newest = runs.iter().filter_map(Run::last_wall).reduce(f64::max).map(|w| w as i64);
        let bytes: u64 = runs.iter().map(Run::bytes).sum();
        // The loop alone: what programmed slots recorded before it stays apart (proxies counted
        // in the same share as the originals).
        let cutoff = (now_ms() - self.config().loop_hours as i64 * 3_600_000) as f64;
        let kept = self.kept.lock().unwrap().clone();
        let beyond = |seg: &index::Seg| seg.wall1() < cutoff && kept_in(&kept, seg);
        let segs = || runs.iter().flat_map(|r| r.master.iter());
        let loop_from = segs().filter(|x| !beyond(x)).map(|x| x.wall0).reduce(f64::min).map(|w| w as i64);
        let (all, apart) = segs().fold((0u64, 0u64), |(a, b), x| (a + x.bytes, b + if beyond(x) { x.bytes } else { 0 }));
        let loop_bytes = if all > 0 { bytes - (bytes as f64 * apart as f64 / all as f64) as u64 } else { bytes };
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
        s.loop_from = loop_from;
        s.loop_bytes = loop_bytes;
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
            let c = self.content.lock().unwrap();
            let live = s.state == State::Recording;
            s.black_since = c.black.filter(|_| live);
            s.frozen_since = c.frozen.filter(|_| live);
            s.silent_since = c.silent.filter(|_| live);
        }
        let short = matches!((loop_from, newest), (Some(a), Some(b)) if ((b - a) as f64) < cfg.loop_hours as f64 * 3_600_000.0 * 0.98);
        s.space_limited = cut > 0 && now_ms() - cut < 15 * 60_000 && short;
        if media.is_some() {
            s.media = media;
        }
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

/// The segment meets one of the spans (server times, ms).
fn kept_in(spans: &[(f64, f64)], s: &index::Seg) -> bool {
    spans.iter().any(|&(a, b)| s.wall0 < b && s.wall1() > a)
}

/// Removes a run's proxy fragments (and their waves) that no longer meet any of its original
/// segments (a live run keeps those past its last one: its live edge). The bytes freed.
fn prune_proxies(run: &mut Run, doomed: &mut Vec<PathBuf>) -> u64 {
    let spans: Vec<(f64, f64)> = run.master.iter().map(|s| (s.t0, s.t1)).collect();
    // A live run: those ahead of its last segment stay (all, none yet: its next one coming).
    let end = spans.last().map_or(f64::NEG_INFINITY, |s| s.1);
    let meets = |t0: f64, t1: f64| {
        let i = spans.partition_point(|s| s.1 <= t0);
        i < spans.len() && spans[i].0 < t1
    };
    let mut freed = 0;
    for video in [true, false] {
        let segs = std::mem::take(&mut run.track_mut(video).segs);
        let n = segs.len();
        for (i, p) in segs.into_iter().enumerate() {
            if (run.live && (p.t1 > end || i + 1 == n)) || meets(p.t0, p.t1) {
                run.track_mut(video).segs.push_back(p);
                continue;
            }
            freed += p.bytes;
            doomed.push(run.proxy_path(video, p.n));
            if !video {
                doomed.push(run.wave_path(p.n));
                run.audio.waves.remove(&p.n);
                // The other sound tracks' fragment of the same number.
                doomed.extend((1..=PREVIEW_EXTRA).map(|k| run.dir.join(format!("audio{k}")).join(format!("p{:08}.m4s", p.n))).filter(|f| f.exists()));
            }
        }
    }
    freed
}

/// How much the loops may take.
#[derive(Clone, Debug, Default)]
pub struct Limits {
    /// Bytes, 0 = only the disk.
    pub loops_max: u64,
    /// The clips: their folder and their limit (bytes, 0 = none).
    pub clips_dir: PathBuf,
    pub clips_max: u64,
    /// What a programmed slot recorded stays this many days after it (0 = always).
    pub programmed_keep_days: u32,
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
        Limits { loops_max: (cfg.loops_max_gb * 1e9) as u64, clips_dir: cfg.clips_dir(data_dir), clips_max: (cfg.clips_max_gb * 1e9) as u64, programmed_keep_days: cfg.programmed_keep_days }
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
    /// What each network card received since the last report: its name → bytes at the last
    /// sample, then the 2-second samples' sum and number, and the highest (Mb/s).
    nic_load: Mutex<std::collections::BTreeMap<String, NicLoad>>,
    /// The programmed slots of the recorders in schedule mode.
    pub schedules: schedule::Schedules,
    /// The server's time zone (the slots' hours are in it).
    tz: Mutex<jiff::tz::TimeZone>,
    /// recorders.json could not be written (disk full): written again later.
    unsaved: std::sync::atomic::AtomicBool,
    /// What each recorder's programmed slots recorded (server times, ms): kept longer than the
    /// loop (`Limits::programmed_keep_days`).
    programmed: Mutex<std::collections::HashMap<String, Vec<(f64, f64)>>>,
}

impl Manager {
    pub fn start(data_dir: &Path, cfg: &ServerConfig, events: Events) -> Arc<Self> {
        let m = Arc::new(Manager {
            udp_dropped: Mutex::new(udp_dropped()),
            nic_dropped: Mutex::new(nic_dropped()),
            nic_load: Mutex::new(Default::default()),
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
            programmed: Mutex::new(crate::paths::read_json(&data_dir.join("programmed.json")).unwrap_or_default()),
        });
        web::keep_updated(data_dir.to_path_buf());
        let list: Vec<RecorderConfig> = crate::paths::read_json(&m.config_path()).unwrap_or_default();
        for mut c in list {
            c.normalize();
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
                    r.say_content();
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
                    // Never recorded: its slots record nothing, none is shown as on or to come.
                    let (slot, next) = if r.config().slots_off { (None, None) } else { (info(m.schedules.active(&r.id, &tz, now_t)), info(m.schedules.next(&r.id, &tz, now_t))) };
                    // A slot recording: what it records is kept longer than the loop.
                    if let Some(s) = &slot {
                        if r.state() == State::Recording && r.config().protocol != Protocol::Pattern {
                            m.keep_programmed(&r.id, (s.start - PREROLL_MS) as f64, s.end as f64);
                        }
                    }
                    let mut st = r.status.lock().unwrap();
                    st.slot = slot;
                    st.next_slot = next;
                }
                m.sample_nic_load(2.0);
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
        crate::pages::init(&m);
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
        // What the cards received meanwhile (said when something went wrong): a card close to its
        // speed loses what comes on top, before this server sees it.
        let load = std::mem::take(&mut *self.nic_load.lock().unwrap());
        let mut lines = glitch_lines(&recs, dropped, nic);
        if !lines.is_empty() || self.recs().iter().any(|r| r.net.seen.load(std::sync::atomic::Ordering::Relaxed) > 0 && r.net.missing.load(std::sync::atomic::Ordering::Relaxed) > 0) {
            lines.extend(load.iter().filter(|(_, l)| l.samples > 0).map(|(card, l)| nic_load_line(card, l)));
        }
        // The last bytes stay: the next ten minutes go on from them.
        *self.nic_load.lock().unwrap() = load.into_iter().map(|(k, l)| (k, NicLoad { last: l.last, speed: l.speed, now_mbps: l.now_mbps, ..Default::default() })).collect();
        for line in lines {
            tracing::warn!("{line}");
        }
        // In detail for each source: what FFmpeg read damaged (picture, sound), and what the
        // witness of a multicast source saw arrive on the network card.
        for r in self.recs() {
            let name = r.config().name;
            let take = |a: &std::sync::atomic::AtomicU64| a.swap(0, std::sync::atomic::Ordering::Relaxed);
            let (video, audio) = (take(&r.damage[0]), take(&r.damage[1]));
            if video + audio > 0 {
                tracing::warn!("{name}: damaged in the last 10 minutes: {video} packets of picture, {audio} of sound");
            }
            let (seen, net) = r.net.take();
            if seen == 0 {
                continue;
            }
            let rtp = r.rtp.load(std::sync::atomic::Ordering::Relaxed);
            let damaged = video + audio;
            for line in witness_lines(&name, seen, net, rtp, damaged) {
                tracing::warn!("{line}");
            }
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
        // Recording goes on in the new folder at once: every source received now, whatever the
        // reason (all the time, a programmed recording, a stream).
        for r in self.recs() {
            let received = r.running() || r.state() == State::Nosignal;
            if !r.dir.lock().unwrap().starts_with(&dir) && received {
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
        let kept: Vec<Vec<(f64, f64)>> = recs.iter().map(|r| self.programmed(&r.id)).collect();
        let mut freed = 0u64;
        let mut doomed: Vec<PathBuf> = Vec::new();
        let mut touched: Vec<(usize, usize)> = Vec::new();
        let mut guards: Vec<_> = recs.iter().map(|r| r.runs.write().unwrap()).collect();
        // The programmed recordings go last.
        let mut programmed_too = false;
        while freed < need {
            // The oldest segment that may go (not the live edge, not one a clip is being cut from).
            let mut best: Option<(usize, usize, usize, f64)> = None;
            for (ri, runs) in guards.iter().enumerate() {
                for (ui, run) in runs.iter().enumerate() {
                    if disk_only && !run.dir.starts_with(&current) {
                        continue;
                    }
                    let pin = pins.iter().filter(|p| p.recorder == recs[ri].id && p.run == run.meta.id).map(|p| p.from).reduce(f64::min);
                    let may = if run.live { run.master.len().saturating_sub(2) } else { run.master.len() };
                    let Some(i) = run.master.iter().take(may).position(|s| !pin.is_some_and(|f| s.t1 > f) && (programmed_too || !kept_in(&kept[ri], s))) else { continue };
                    let w = run.master[i].wall0;
                    if best.is_none_or(|b| w < b.3) {
                        best = Some((ri, ui, i, w));
                    }
                }
            }
            let Some((ri, ui, i, _)) = best else {
                if programmed_too {
                    break;
                }
                programmed_too = true;
                continue;
            };
            let run = &mut guards[ri][ui];
            let Some(s) = run.master.remove(i) else { break };
            freed += s.bytes;
            doomed.push(run.master_path(s.n));
            freed += prune_proxies(run, &mut doomed);
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

    /// The Streamer shows a test pattern: it is made (and kept made a little while).
    pub fn demand(&self, id: &str) -> Result<(), String> {
        let r = self.get(id).ok_or("no such recorder")?;
        if r.config().protocol != Protocol::Pattern {
            return Ok(());
        }
        let was = now_ms() - r.demand.swap(now_ms(), std::sync::atomic::Ordering::Relaxed) >= DEMAND_MS;
        if was {
            r.wake.notify_one();
        }
        Ok(())
    }

    /// The recorders in this order (those not named keep theirs, after).
    pub fn reorder(&self, ids: &[String]) -> Result<(), String> {
        {
            let mut recs = self.recs.lock().unwrap();
            recs.sort_by_key(|r| ids.iter().position(|i| *i == r.id).unwrap_or(usize::MAX));
        }
        let saved = self.save();
        self.list_changed();
        saved.map_err(|e| e.to_string())
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
        // The source's sound tracks, as its newest recording had them (no start again for them).
        let extra_audio = runs.iter().rev().find_map(|r| r.meta.media.as_ref()).map(|m| m.audio_tracks.saturating_sub(1).min(PREVIEW_EXTRA)).unwrap_or(0);
        let rec = Arc::new(Rec {
            id: c.id.clone(),
            status: Mutex::new(Status { id: c.id.clone(), state: State::Off, since: now_ms(), message: String::new(), media: None, bitrate: 0, oldest: None, newest: None, bytes: 0, loop_from: None, loop_bytes: 0, space_limited: false, measured: 0, on_air: None, next: None, slot: None, next_slot: None, lost: 0, damaged: 0, preview_lag: 0, preview_dropped: false, black_since: None, frozen_since: None, silent_since: None }),
            cfg: watch::channel(c).0,
            runs: RwLock::new(runs),
            stop: watch::channel(false).0,
            dir: Mutex::new(dir),
            no_audio: false.into(),
            space_trimmed: 0.into(),
            kept: Mutex::new(Vec::new()),
            new_run: tokio::sync::Notify::new(),
            epg: Mutex::new(guide),
            epg_found: Mutex::new(found),
            epg_failed: false.into(),
            epg_from_disk: found_known.into(),
            glitches: Mutex::new((std::collections::VecDeque::new(), 0)),
            net: Default::default(),
            damage: Default::default(),
            rtp: false.into(),
            preview_lag: 0.into(),
            content: Mutex::new(Content::default()),
            last_packet: 0.into(),
            last_sound: 0.into(),
            audio_extra: extra_audio.into(),
            empty_streams: Mutex::new(Vec::new()),
            restart_run: tokio::sync::Notify::new(),
            no_signal: false.into(),
            last_end: Mutex::new(None),
            preview_dropped_at: 0.into(),
            task: Mutex::new(None),
            taps: Mutex::new(Vec::new()),
            streams: 0.into(),
            wake: tokio::sync::Notify::new(),
            demand: 0.into(),
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

    /// The recorder takes space: it records all the time, or it has slots programmed (a source
    /// never recorded takes none, whatever its slots).
    pub fn counts(&self, c: &RecorderConfig) -> bool {
        c.protocol != Protocol::Pattern && (c.enabled || (!c.slots_off && self.schedules.of(&c.id).iter().any(|s| s.enabled)))
    }

    /// A stopped recorder records during its slots, from a little before (the connection takes time).
    /// Received now: a stream sends it somewhere, or one of its slots (unless it is not recorded).
    pub fn wants(&self, id: &str, now: i64) -> bool {
        if let Some(r) = self.get(id) {
            if r.streams.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                return true;
            }
            // A test pattern: while the Streamer shows it.
            if r.config().protocol == Protocol::Pattern {
                return now - r.demand.load(std::sync::atomic::Ordering::Relaxed) < DEMAND_MS;
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
        // Recording all the time, or by its slots: its loop must fit (a longer one too).
        if !self.counts(c) {
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
        self.events.send(Audience::Admins, json!({"type": "recorders", "recorders": self.list_json(true)}));
        self.events.send(Audience::NotAdmins, json!({"type": "recorders", "recorders": self.list_json(false)}));
    }

    /// The sources, for an admin; for the others, their addresses without what may be secret
    /// (a stream key in the path, a token in the query, a user and password, the path an encoder
    /// publishes to).
    pub fn list_json(&self, admin: bool) -> Value {
        Value::Array(
            self.recs()
                .iter()
                .map(|r| {
                    let mut v = r.config().public();
                    if !admin {
                        v["url"] = json!(only_host(&r.config().url));
                        // The path an encoder publishes to (RTMP listener): its stream key.
                        v["path"] = json!("");
                    }
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

    /// Removes what is older than the loop (except pinned segments); a test pattern keeps only
    /// what the Streamer's player needs.
    pub fn trim(&self, rec: &Rec) {
        let c = rec.config();
        let keep = if c.protocol == Protocol::Pattern { PATTERN_KEEP_MS } else { c.loop_hours as i64 * 3_600_000 };
        let cutoff = (now_ms() - keep) as f64;
        let kept = self.programmed(&rec.id);
        self.trim_before(rec, cutoff, &kept);
        *rec.kept.lock().unwrap() = kept;
    }

    /// Empties the loop: everything recorded so far goes, what its programmed slots recorded too
    /// (what a clip is being cut from stays).
    pub fn clear(&self, rec: &Rec) {
        {
            let mut all = self.programmed.lock().unwrap();
            if all.remove(&rec.id).is_some() {
                let _ = crate::paths::write_json(&self.data_dir.join("programmed.json"), &*all, false);
            }
        }
        self.trim_before(rec, f64::INFINITY, &[]);
        tracing::warn!("{}: loop emptied", rec.config().name);
        rec.refresh_status();
    }

    /// Notes a programmed slot's recording (server times, ms): what the loop holds of it stays
    /// longer than the loop.
    pub fn keep_programmed(&self, id: &str, from: f64, to: f64) {
        let mut all = self.programmed.lock().unwrap();
        let spans = all.entry(id.to_string()).or_default();
        if spans.iter().any(|&(a, b)| a <= from && to <= b) {
            return;
        }
        // One span a slot: the same slot's again, else a new one.
        match spans.iter_mut().find(|(a, b)| from <= *b && *a <= to) {
            Some(s) => *s = (s.0.min(from), s.1.max(to)),
            None => spans.push((from, to)),
        }
        let _ = crate::paths::write_json(&self.data_dir.join("programmed.json"), &*all, false);
    }

    /// A recorder's programmed recordings still kept (those past their time are forgotten).
    pub fn programmed(&self, id: &str) -> Vec<(f64, f64)> {
        let days = self.limits.lock().unwrap().programmed_keep_days;
        let mut all = self.programmed.lock().unwrap();
        let Some(spans) = all.get_mut(id) else { return Vec::new() };
        if days > 0 {
            let since = (now_ms() - days as i64 * 86_400_000) as f64;
            let before = spans.len();
            spans.retain(|&(_, b)| b >= since);
            if spans.len() != before {
                let _ = crate::paths::write_json(&self.data_dir.join("programmed.json"), &*all, false);
                return all.get(id).cloned().unwrap_or_default();
            }
        }
        spans.clone()
    }

    /// A recorder's programmed recordings still held: their bytes (the original segments), and
    /// how many.
    pub fn programmed_held(&self, rec: &Rec) -> (u64, usize) {
        let kept = self.programmed(&rec.id);
        if kept.is_empty() {
            return (0, 0);
        }
        let runs = rec.runs.read().unwrap();
        let segs: Vec<&index::Seg> = runs.iter().flat_map(|r| r.master.iter()).collect();
        let bytes = segs.iter().filter(|s| kept_in(&kept, s)).map(|s| s.bytes).sum();
        let count = kept.iter().filter(|&&span| segs.iter().any(|s| kept_in(&[span], s))).count();
        (bytes, count)
    }

    /// Removes the segments that end before `cutoff` (server time, ms), but those a programmed
    /// slot recorded (`kept`).
    fn trim_before(&self, rec: &Rec, cutoff: f64, kept: &[(f64, f64)]) {
        let pins: Vec<Pin> = self.pins.lock().unwrap().iter().filter(|p| p.recorder == rec.id).cloned().collect();
        let mut doomed: Vec<PathBuf> = Vec::new();
        let mut runs = rec.runs.write().unwrap();
        for run in runs.iter_mut() {
            let keep_from = pins.iter().filter(|p| p.run == run.meta.id).map(|p| p.from).reduce(f64::min);
            let (live, n) = (run.live, run.master.len());
            let mut changed = false;
            for (i, s) in std::mem::take(&mut run.master).into_iter().enumerate() {
                let pinned = keep_from.is_some_and(|f| s.t1 > f);
                if s.wall1() >= cutoff || pinned || (live && i + 1 == n) || kept_in(kept, &s) {
                    run.master.push_back(s);
                } else {
                    doomed.push(run.master_path(s.n));
                    changed = true;
                }
            }
            if changed {
                prune_proxies(run, &mut doomed);
                if !run.master.is_empty() {
                    index::rewrite_index(run);
                }
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
    fn programmed_parts_stay() {
        use index::{PSeg, RunMeta, Seg};
        // Ten 6-s segments from t 0 (server time 0), proxies of 2 s, the run finished.
        let mut run = Run::new(PathBuf::from("/nonexistent"), RunMeta::default());
        run.live = false;
        for i in 0..10u32 {
            let t = i as f64 * 6.0;
            run.master.push_back(Seg { n: i, t0: t, t1: t + 6.0, wall0: t * 1000.0, bytes: 10 });
        }
        for i in 0..30u32 {
            let t = i as f64 * 2.0;
            run.video.segs.push_back(PSeg { n: i, t0: t, t1: t + 2.0, bytes: 1 });
        }
        // A slot recorded 18 s to 30 s: its segments stay, the others go.
        let kept = [(18_000.0, 30_000.0)];
        run.master.retain(|s| kept_in(&kept, s));
        assert_eq!(run.master.iter().map(|s| s.n).collect::<Vec<_>>(), vec![3, 4]);
        let mut doomed = Vec::new();
        let freed = prune_proxies(&mut run, &mut doomed);
        // Its proxies stay (18 s to 30 s), the others go.
        assert_eq!(run.video.segs.iter().map(|p| p.n).collect::<Vec<_>>(), vec![9, 10, 11, 12, 13, 14]);
        assert_eq!(freed, 24);
        // A live run keeps what is past its last segment.
        run.live = true;
        run.video.segs.push_back(PSeg { n: 40, t0: 80.0, t1: 82.0, bytes: 1 });
        prune_proxies(&mut run, &mut doomed);
        assert_eq!(run.video.segs.back().map(|p| p.n), Some(40));
    }

    #[test]
    fn system_reserves() {
        // The system disk: 5 %, 10 GB at least, a quarter at most.
        assert_eq!(system_reserve(Path::new("/"), 1_000_000_000_000), 50_000_000_000);
        assert_eq!(system_reserve(Path::new("/"), 100_000_000_000), 10 << 30);
        assert_eq!(system_reserve(Path::new("/"), 16_000_000_000), 4_000_000_000);
    }
}

/// What the witness of a multicast source says of ten minutes: what arrived on the network card
/// (`seen` datagrams), beside what FFmpeg read damaged.
fn witness_lines(name: &str, seen: u64, net: udp_in::Seen, rtp: bool, damaged: u64) -> Vec<String> {
    let mut out = Vec::new();
    if rtp {
        // Read as RTP: a datagram that comes late is put back in its place; one missing, or sent
        // twice (FFmpeg reads it twice), damages the picture.
        if net.reordered > 0 {
            out.push(format!("{name}: RTP, in the last 10 minutes: {} datagrams came out of order: put back in order by this server (read as plain UDP, each would have damaged the picture)", net.reordered));
        }
        if net.missing + net.twice + net.marked > 0 {
            out.push(format!("{name}: RTP, as it arrives on the network card, in the last 10 minutes: {} datagrams never came, {} came twice, {} packets marked in error by the source: the source or the network, not this server", net.missing, net.twice, net.marked));
        }
    } else if net.missing + net.twice + net.marked > 0 {
        let moments = if net.events > 0 { format!(" (in {} moments, up to {} at once)", net.events, net.worst) } else { String::new() };
        out.push(format!("{name}: as it arrives on the network card, in the last 10 minutes: {} packets missing{moments}, {} sent twice, {} marked in error by the source: the source or the network, not this server", net.missing, net.twice, net.marked));
    }
    if out.is_empty() && damaged > 0 {
        out.push(format!("{name}: the stream arrives whole on the network card ({seen} datagrams checked in the last 10 minutes): the damage comes from its reading on this server"));
    }
    out
}

/// What a network card received over ten minutes.
#[derive(Default, Clone)]
struct NicLoad {
    /// Its received bytes at the last sample.
    last: u64,
    sum_mbps: f64,
    samples: u32,
    peak_mbps: f64,
    /// Its speed (Mb/s), as the system says (0: unknown).
    speed: u64,
    /// Over the last 2 seconds, Mb/s.
    now_mbps: f64,
}

impl Manager {
    /// Each card's received bytes now: the rate since the last sample (`secs` ago).
    fn sample_nic_load(&self, secs: f64) {
        let Ok(dir) = std::fs::read_dir("/sys/class/net") else { return };
        let mut load = self.nic_load.lock().unwrap();
        for e in dir.flatten() {
            let path = e.path();
            if !path.join("device").exists() {
                continue;
            }
            let read = |k: &str| std::fs::read_to_string(path.join(k)).ok().and_then(|v| v.trim().parse::<i64>().ok());
            let Some(bytes) = read("statistics/rx_bytes") else { continue };
            let card = e.file_name().to_string_lossy().into_owned();
            let l = load.entry(card).or_default();
            l.speed = read("speed").filter(|s| *s > 0).unwrap_or(0) as u64;
            let bytes = bytes as u64;
            if l.last > 0 && bytes >= l.last {
                let mbps = (bytes - l.last) as f64 * 8.0 / 1e6 / secs;
                l.sum_mbps += mbps;
                l.samples += 1;
                l.peak_mbps = l.peak_mbps.max(mbps);
                l.now_mbps = mbps;
            }
            l.last = bytes;
        }
    }
}

impl Manager {
    /// Each network card now: its name, what it receives (Mb/s) and its speed (Mb/s, 0: unknown).
    pub fn nics_now(&self) -> Vec<(String, f64, u64)> {
        self.nic_load.lock().unwrap().iter().map(|(k, l)| (k.clone(), l.now_mbps, l.speed)).collect()
    }
}

/// A card's load over ten minutes, and whether it came close to its speed.
fn nic_load_line(card: &str, l: &NicLoad) -> String {
    let avg = l.sum_mbps / l.samples.max(1) as f64;
    let speed = if l.speed > 0 { format!(" ({} Mb/s)", l.speed) } else { String::new() };
    let near = if l.speed > 0 && l.peak_mbps >= l.speed as f64 * 0.7 { ": close to its speed: what comes on top is lost before this server" } else { "" };
    format!("network card {card}{speed}: received in the last 10 minutes {avg:.0} Mb/s on average, up to {:.0} Mb/s over 2 s{near}", l.peak_mbps)
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
    fn addresses_without_secrets() {
        assert_eq!(only_host("rtmp://live.example.com/app/secretkey"), "rtmp://live.example.com");
        assert_eq!(only_host("https://u:p@cdn.example.com:8443/live.m3u8?token=x"), "https://cdn.example.com:8443");
        assert_eq!(only_host("udp://@239.1.1.1:5000"), "udp://239.1.1.1:5000");
        assert_eq!(only_host("MACHINE (Caméra 1)"), "MACHINE (Caméra 1)");
    }

    #[test]
    fn nic_load() {
        let l = NicLoad { last: 1, sum_mbps: 3000.0, samples: 10, peak_mbps: 960.0, speed: 1000, now_mbps: 0.0 };
        assert_eq!(nic_load_line("eno1", &l), "network card eno1 (1000 Mb/s): received in the last 10 minutes 300 Mb/s on average, up to 960 Mb/s over 2 s: close to its speed: what comes on top is lost before this server");
        let quiet = NicLoad { peak_mbps: 200.0, ..l };
        assert!(!nic_load_line("eno1", &quiet).contains("close"));
    }

    #[test]
    fn witness() {
        let seen = |missing, twice, marked, reordered| udp_in::Seen { missing, twice, marked, reordered, ..Default::default() };
        assert!(witness_lines("TF1", 1000, seen(0, 0, 0, 0), false, 0).is_empty());
        assert!(witness_lines("TF1", 1000, seen(0, 0, 0, 0), false, 4)[0].contains("arrives whole"));
        assert!(witness_lines("TF1", 1000, seen(3, 1, 0, 0), false, 4)[0].contains("3 packets missing, 1 sent twice"));
        let burst = udp_in::Seen { missing: 479, events: 61, worst: 14, ..Default::default() };
        assert!(witness_lines("TF1", 1000, burst, false, 40)[0].contains("479 packets missing (in 61 moments, up to 14 at once)"));
        let l = witness_lines("TF1", 1000, seen(2, 1, 0, 5), true, 2);
        assert!(l[0].contains("5 datagrams came out of order") && l[1].contains("2 datagrams never came, 1 came twice"), "{l:?}");
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
