//! Streaming (Streamer page): a source sent live to destinations (Système › Destinations: RTMP,
//! RTMPS or SRT encoders and platforms), each by its own FFmpeg reading the stream as received
//! from the source's relay (`Rec::taps`): starting or stopping one never touches the recording.
//!
//! A destination's secrets (stream key, SRT passphrase) are masked for everyone; an operator
//! sees only its name, its type, its server's host and what the admin hands to them (a key
//! given for each live, the server's address, the bitrate). A key given for a live is kept in
//! memory only, for that live.
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::{Path as UrlPath, State},
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{io::AsyncBufReadExt, process::Command, sync::watch};

use crate::{
    api::{bad, not_found, require, ApiResult},
    auth::CurrentUser,
    recorder::{now_ms, Manager, MASKED},
    state::{Audience, Events, Shared},
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Destination {
    pub id: String,
    pub name: String,
    /// "rtmp", "rtmps" or "srt".
    pub protocol: String,
    /// The server: rtmp(s)://host/app, or srt://host:port.
    pub url: String,
    /// RTMP: the stream key (secret), unless given for each live.
    pub key: String,
    /// The key is given by the operator for each live (a platform gives one per live).
    pub key_live: bool,
    /// SRT: latency (ms) and passphrase (secret).
    pub latency: u32,
    pub passphrase: String,
    /// "copy" (the source as it is) or "h264" (encoded again: picture height, bitrates).
    pub encode: String,
    pub height: u32,
    pub video_kbps: u32,
    pub audio_kbps: u32,
    /// What the operator may change in the Streamer: the server's address, the video bitrate.
    pub expose_url: bool,
    pub expose_bitrate: bool,
    /// The source lost during a live: a still picture (and silence) sent until it comes back, so
    /// that the platform keeps the live open.
    #[serde(default = "yes")]
    pub slate: bool,
}

fn yes() -> bool {
    true
}

impl Destination {
    fn srt(&self) -> bool {
        self.protocol == "srt"
    }
    /// The server's host, all an operator sees of its address.
    fn host(&self) -> String {
        let rest = self.url.split_once("://").map(|(_, r)| r).unwrap_or(&self.url);
        rest.split(['/', '?']).next().unwrap_or("").to_string()
    }
    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("the name cannot be empty".into());
        }
        let scheme = match self.protocol.as_str() {
            "rtmp" => "rtmp://",
            "rtmps" => "rtmps://",
            "srt" => "srt://",
            _ => return Err("the destination is rtmp, rtmps or srt".into()),
        };
        if !self.url.starts_with(scheme) || self.host().is_empty() {
            return Err(format!("the address starts with {scheme}"));
        }
        if self.srt() && !self.passphrase.is_empty() && self.passphrase != MASKED && !(10..=79).contains(&self.passphrase.chars().count()) {
            return Err("the srt passphrase has 10 to 79 characters".into());
        }
        if !matches!(self.encode.as_str(), "copy" | "h264") {
            return Err("the encoding is copy or h264".into());
        }
        if self.encode == "h264" && !(300..=50_000).contains(&self.video_kbps) {
            return Err("the video bitrate is 0.3 to 50 mb/s".into());
        }
        Ok(())
    }
    /// For an admin: everything but the secrets.
    fn public(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if !self.key.is_empty() {
            v["key"] = json!(MASKED);
        }
        if !self.passphrase.is_empty() {
            v["passphrase"] = json!(MASKED);
        }
        v["host"] = json!(self.host());
        v
    }
    /// For an operator: what the Streamer shows, and what the admin handed over.
    fn for_operator(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "protocol": self.protocol, "host": self.host(),
            "keyLive": self.key_live, "exposeUrl": self.expose_url, "exposeBitrate": self.expose_bitrate,
            "encode": self.encode, "height": self.height,
            "url": if self.expose_url { json!(self.url) } else { Value::Null },
            "videoKbps": if self.expose_bitrate || self.encode == "h264" { json!(self.video_kbps) } else { Value::Null },
        })
    }
}

/// What a live may change from the destination's settings (the operator's, when handed over).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Overrides {
    pub key: String,
    pub url: String,
    pub video_kbps: u32,
}

/// The destination as it is sent to: its settings with what this live changed.
fn effective(d: &Destination, o: &Overrides) -> Result<Destination, String> {
    let mut d = d.clone();
    if d.key_live {
        if o.key.trim().is_empty() && !d.srt() {
            return Err("give the stream key of this live".into());
        }
        d.key = o.key.trim().to_string();
    }
    if d.expose_url && !o.url.trim().is_empty() {
        d.url = o.url.trim().to_string();
    }
    if d.expose_bitrate && o.video_kbps > 0 {
        d.video_kbps = o.video_kbps.clamp(300, 50_000);
        if d.encode == "copy" {
            d.encode = "h264".into();
        }
    }
    d.validate()?;
    Ok(d)
}

/// The address FFmpeg sends to (with the key; never written in the journal).
fn target(d: &Destination) -> String {
    if d.srt() {
        let mut extra = format!("mode=caller&latency={}&pkt_size=1316", d.latency.max(20) as u64 * 1000);
        if !d.passphrase.is_empty() {
            extra.push_str(&format!("&passphrase={}", urlencode(&d.passphrase)));
        }
        let sep = if d.url.contains('?') { '&' } else { '?' };
        format!("{}{sep}{extra}", d.url)
    } else if d.key.is_empty() {
        d.url.clone()
    } else {
        format!("{}/{}", d.url.trim_end_matches('/'), d.key)
    }
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// The encoding of a destination (`video_codec`: the source's, as received).
fn output_args(d: &Destination, video_codec: &str) -> (Vec<String>, String) {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let mut a = s(&["-map", "0:v:0", "-map", "0:a:0?"]);
    // RTMP carries H.264 and AAC only: a source in another codec is encoded again.
    let must = !d.srt() && !video_codec.is_empty() && video_codec != "h264";
    let mut note = String::new();
    if d.encode == "h264" || must {
        if must && d.encode == "copy" {
            note = "encoded again: rtmp does not carry this source's video".into();
        }
        let k = if d.video_kbps > 0 { d.video_kbps } else { 6000 };
        let scale = if d.height > 0 { format!(",scale=-2:{}", d.height) } else { String::new() };
        a.extend(s(&["-vf", &format!("yadif=deint=interlaced{scale},format=yuv420p"), "-c:v", "libx264", "-preset", "veryfast", "-profile:v", "high"]));
        a.extend(s(&["-b:v", &format!("{k}k"), "-maxrate", &format!("{k}k"), "-bufsize", &format!("{}k", k * 2), "-force_key_frames", "expr:gte(t,n_forced*2)"]));
    } else {
        a.extend(s(&["-c:v", "copy"]));
    }
    if d.srt() && d.encode == "copy" {
        a.extend(s(&["-c:a", "copy", "-f", "mpegts"]));
    } else {
        let ak = if d.audio_kbps > 0 { d.audio_kbps } else { 160 };
        a.extend(s(&["-c:a", "aac", "-b:a", &format!("{ak}k"), "-ar", "48000", "-ac", "2"]));
        a.extend(s(&["-f", if d.srt() { "mpegts" } else { "flv" }]));
    }
    // A server that stops taking what is sent (10 s): FFmpeg ends and the link is made again,
    // instead of waiting forever while the destination receives nothing.
    if !d.srt() {
        a.extend(s(&["-rw_timeout", "10000000"]));
    }
    a.push(target(d));
    (a, note)
}

/// The picture sent while the source is lost: the source's size and rate (the platform sees the
/// same stream), a dark ground, the source's name and that the live comes back; silence.
fn slate_args(d: &Destination, text: &std::path::Path, (w, h, fps): (u32, u32, u32)) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let (w, h) = if d.height > 0 && d.encode == "h264" { ((d.height * 16 / 9 + 1) & !1, d.height) } else { (w, h) };
    let size = (h / 16).max(16);
    let file = text.to_string_lossy().replace('\\', "/").replace(':', "\\\\:").replace('\'', "");
    let mut a = s(&["-hide_banner", "-nostdin", "-nostats", "-loglevel", "warning", "-re", "-f", "lavfi", "-i"]);
    a.push(format!("color=c=0x0b0e16:s={w}x{h}:r={fps}"));
    a.extend(s(&["-re", "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-map", "0:v", "-map", "1:a", "-vf"]));
    a.push(format!("drawtext=textfile='{file}':fontcolor=white:fontsize={size}:line_spacing={}:x=(w-tw)/2:y=(h-th)/2,format=yuv420p", size / 2));
    let k = if d.video_kbps > 0 { d.video_kbps.min(4000) } else { 2500 };
    a.extend(s(&["-c:v", "libx264", "-preset", "veryfast", "-tune", "stillimage", "-b:v", &format!("{k}k"), "-maxrate", &format!("{k}k"), "-bufsize", &format!("{}k", k * 2), "-g", &(fps * 2).to_string()]));
    let ak = if d.audio_kbps > 0 { d.audio_kbps } else { 128 };
    a.extend(s(&["-c:a", "aac", "-b:a", &format!("{ak}k"), "-ar", "48000", "-ac", "2", "-f", if d.srt() { "mpegts" } else { "flv" }]));
    if !d.srt() {
        a.extend(s(&["-rw_timeout", "10000000"]));
    }
    a.push(target(d));
    a
}

fn free_port() -> Option<u16> {
    std::net::UdpSocket::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|a| a.port())
}

/// A live to one destination.
struct Session {
    dest: String,
    source: String,
    started: i64,
    stop: watch::Sender<bool>,
    /// What it is expected to send (kb/s), counted by the guard until it really sends.
    planned_kbps: f64,
    /// True once its last FFmpeg has ended (a live started again waits for it: the destination
    /// takes one link per stream key).
    finished: watch::Sender<bool>,
    stats: Mutex<Stats>,
}

#[derive(Default)]
struct Stats {
    /// "connecting", "live" or "retrying".
    state: &'static str,
    message: String,
    note: String,
    /// Since when it is live (server ms), bytes sent, kb/s now and its last 10 minutes (one a second).
    live_since: Option<i64>,
    bytes: u64,
    kbps: f64,
    history: VecDeque<f32>,
}

pub struct Streams {
    data_dir: PathBuf,
    recorders: Arc<Manager>,
    events: Events,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Lives that could not start (the destination never took them), by destination: shown
    /// with their reason until another start, a stop, or 10 minutes.
    failed: Mutex<HashMap<String, Failed>>,
}

struct Failed {
    source: String,
    message: String,
    at: i64,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("destinations.json")
}

static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Streams {
    pub fn start(data_dir: &Path, recorders: Arc<Manager>, events: Events) -> Arc<Streams> {
        Arc::new(Streams { data_dir: data_dir.to_path_buf(), recorders, events, sessions: Mutex::new(HashMap::new()), failed: Mutex::new(HashMap::new()) })
    }

    pub fn list(&self) -> Vec<Destination> {
        let mut all: Vec<Destination> = crate::paths::read_json(&path(&self.data_dir)).unwrap_or_default();
        all.sort_by_key(|d| d.name.to_lowercase());
        all
    }

    fn get(&self, id: &str) -> Option<Destination> {
        self.list().into_iter().find(|d| d.id == id)
    }

    fn save(&self, all: &[Destination]) -> Result<(), String> {
        crate::paths::write_json(&path(&self.data_dir), all, false).map_err(|e| e.to_string())
    }

    fn changed(&self) {
        self.events.send(Audience::All, json!({"type": "changed", "what": "streams"}));
    }

    /// What the streams send out now, kb/s.
    pub fn out_kbps(&self) -> f64 {
        // A live still connecting sends nothing yet: what it will send is counted, so that several
        // started at once cannot all pass the limit.
        self.sessions
            .lock()
            .unwrap()
            .values()
            .map(|s| {
                let st = s.stats.lock().unwrap();
                if st.kbps > 0.0 || st.state == "live" { st.kbps } else { s.planned_kbps }
            })
            .sum::<f64>()
            // An empty sum is -0.0: shown "-0".
            .max(0.0)
    }

    /// What sending this source to this destination would take (the resources' guard); None
    /// when it already does.
    fn cost(&self, dest_id: &str, source: &str, o: &Overrides) -> Result<Option<crate::limits::Cost>, String> {
        if self.sessions.lock().unwrap().get(dest_id).is_some_and(|s| s.source == source) {
            return Ok(None);
        }
        let dest = self.get(dest_id).ok_or("no such destination")?;
        let rec = self.recorders.get(source).ok_or("no such source")?;
        let d = effective(&dest, o)?;
        let (codec, height, bits) = {
            let st = rec.status.lock().unwrap();
            (st.media.as_ref().map(|m| m.codec.clone()).unwrap_or_default(), st.media.as_ref().map(|m| m.height).unwrap_or(1080), st.bitrate)
        };
        let again = d.encode == "h264" || (!d.srt() && !codec.is_empty() && codec != "h264");
        let kbps = if again {
            (if d.video_kbps > 0 { d.video_kbps } else { 6000 } + if d.audio_kbps > 0 { d.audio_kbps } else { 160 }) as f64
        } else if bits > 0 {
            bits as f64 / 1000.0
        } else {
            rec.config().bitrate.max(8.0) * 1000.0
        };
        let mut cost = crate::limits::stream(format!("{} → {}", rec.config().name, dest.name), again, d.height, height, kbps);
        // A source not received yet is started by the stream: its receiver and preview count too.
        if !matches!(rec.status.lock().unwrap().state, crate::recorder::State::Recording | crate::recorder::State::Starting) {
            cost.cores += crate::limits::recording(&rec.config()).cores;
        }
        Ok(Some(cost))
    }

    /// The lives under way.
    pub fn json(&self) -> Value {
        let sessions: Vec<Arc<Session>> = self.sessions.lock().unwrap().values().cloned().collect();
        let now = now_ms();
        Value::Array(
            sessions
                .iter()
                .map(|s| {
                    let st = s.stats.lock().unwrap();
                    json!({
                        "destination": s.dest, "source": s.source, "started": s.started, "state": st.state,
                        "message": st.message, "note": st.note, "liveSince": st.live_since,
                        "duration": st.live_since.map(|t| now - t), "bytes": st.bytes, "kbps": st.kbps.round(),
                        "history": st.history.iter().map(|k| k.round()).collect::<Vec<_>>(),
                    })
                })
                .chain({
                    let mut f = self.failed.lock().unwrap();
                    f.retain(|_, x| now - x.at < 600_000);
                    f.iter()
                        .map(|(dest, x)| json!({ "destination": dest, "source": x.source, "started": x.at, "state": "failed", "message": x.message, "note": "", "liveSince": null, "duration": null, "bytes": 0, "kbps": 0, "history": [] }))
                        .collect::<Vec<_>>()
                })
                .collect(),
        )
    }

    /// Sends a source to a destination (a destination sends one source at a time).
    pub fn begin(self: &Arc<Self>, dest_id: &str, source: &str, o: &Overrides, who: &str) -> Result<(), String> {
        let dest = self.get(dest_id).ok_or("no such destination")?;
        let rec = self.recorders.get(source).ok_or("no such source")?;
        let d = effective(&dest, o)?;
        let planned_kbps = self.cost(dest_id, source, o).ok().flatten().map(|c| c.kbps).unwrap_or(0.0);
        // Looked at and taken in one go: two starts at once make one live. A live being stopped
        // (its FFmpeg ends within a second) does not count: the new one starts after it.
        let (session, previous) = {
            let mut sessions = self.sessions.lock().unwrap();
            let previous = match sessions.get(dest_id) {
                Some(s) if !*s.stop.borrow() => {
                    return if s.source == source { Ok(()) } else { Err("this destination already sends another source".into()) };
                }
                Some(s) => Some(s.clone()),
                None => None,
            };
            let session = Arc::new(Session { dest: dest_id.to_string(), source: source.to_string(), started: now_ms(), planned_kbps, stop: watch::channel(false).0, finished: watch::channel(false).0, stats: Mutex::new(Stats { state: "connecting", ..Default::default() }) });
            sessions.insert(dest_id.to_string(), session.clone());
            (session, previous)
        };
        self.failed.lock().unwrap().remove(dest_id);
        // The source is received while it is sent (recorded meanwhile, whatever its programme).
        rec.streams.fetch_add(1, Ordering::Relaxed);
        rec.wake.notify_one();
        tracing::info!("{who} sends {} to {}", rec.config().name, dest.name);
        let me = self.clone();
        tokio::spawn(async move {
            if let Some(p) = previous {
                let mut done = p.finished.subscribe();
                let _ = tokio::time::timeout(Duration::from_secs(5), done.wait_for(|d| *d)).await;
            }
            me.run(session.clone(), d).await;
            let _ = session.finished.send(true);
            rec.streams.fetch_sub(1, Ordering::Relaxed);
            me.sessions.lock().unwrap().retain(|_, s| !Arc::ptr_eq(s, &session));
            me.changed();
        });
        self.changed();
        Ok(())
    }

    /// Every live of a source stopped (the source deleted, or "Tout arrêter").
    /// A destination's name (its id when it is gone).
    pub fn name_of(&self, id: &str) -> String {
        self.get(id).map(|d| d.name).unwrap_or_else(|| id.to_string())
    }

    pub fn end_source(&self, source: &str) {
        let dests: Vec<String> = self.sessions.lock().unwrap().values().filter(|x| x.source == source).map(|x| x.dest.clone()).collect();
        for d in dests {
            self.end(&d);
        }
    }

    pub fn end(&self, dest_id: &str) -> bool {
        if self.failed.lock().unwrap().remove(dest_id).is_some() {
            self.changed();
        }
        match self.sessions.lock().unwrap().get(dest_id) {
            Some(s) => {
                let _ = s.stop.send(true);
                true
            }
            None => false,
        }
    }

    pub async fn shutdown(&self) {
        let all: Vec<Arc<Session>> = self.sessions.lock().unwrap().values().cloned().collect();
        for s in &all {
            let _ = s.stop.send(true);
        }
        for _ in 0..50 {
            if self.sessions.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// One FFmpeg after another until stopped. A live that never started (the destination did
    /// not take it: its key, its address) ends there, with why; a live lost on the way is made
    /// again until it is stopped (2 s, then every 5 s).
    /// Waits until the source is received. False: the live was stopped, or the source did not
    /// come within `limit` (then said, with the failed).
    async fn source_ready(&self, s: &Session, d: &Destination, rec: &crate::recorder::Rec, stop: &mut watch::Receiver<bool>, limit: Option<Duration>) -> bool {
        let since = std::time::Instant::now();
        let mut said = false;
        loop {
            if *stop.borrow() {
                return false;
            }
            if rec.flowing() {
                return true;
            }
            if limit.is_some_and(|l| since.elapsed() > l) {
                self.give_up(s, d, "the source does not answer".into());
                return false;
            }
            if !said {
                said = true;
                self.set(s, |st| {
                    st.message = "waiting for the source".into();
                });
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                _ = stop.changed() => {}
            }
        }
    }

    /// Sends the slate until the source is received again (true) or the live is stopped (false).
    async fn slate(&self, s: &Session, d: &Destination, rec: &crate::recorder::Rec, stop: &mut watch::Receiver<bool>) -> bool {
        let (size, name) = {
            let st = rec.status.lock().unwrap();
            let m = st.media.as_ref();
            let fps = m.map(|m| m.fps().round() as u32).filter(|f| (10..=60).contains(f)).unwrap_or(25);
            ((m.map(|m| m.width).filter(|w| *w > 0).unwrap_or(1920), m.map(|m| m.height).filter(|h| *h > 0).unwrap_or(1080), fps), rec.config().name)
        };
        let text = self.data_dir.join(format!("slate-{}.txt", s.dest));
        let _ = std::fs::write(&text, format!("{name}\nLe direct reprend dans un instant"));
        self.set(s, |st| {
            st.state = "slate";
            st.message = "the source is no longer received: slate sent".into();
            st.live_since = None;
            st.kbps = 0.0;
        });
        tracing::warn!("streaming to {}: the source is no longer received, a slate is sent", d.name);
        loop {
            let mut cmd = Command::new(&self.recorders.ffmpeg);
            cmd.args(slate_args(d, &text, size)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
            let mut child = cmd.spawn().ok();
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                    _ = stop.changed() => {}
                }
                if *stop.borrow() {
                    if let Some(c) = child.as_mut() {
                        let _ = c.kill().await;
                    }
                    return false;
                }
                if rec.flowing() {
                    if let Some(c) = child.as_mut() {
                        let _ = c.kill().await;
                    }
                    tracing::info!("streaming to {}: the source is back, the live goes on", d.name);
                    self.set(s, |st| st.message.clear());
                    return true;
                }
                // The slate's link fell (the server, the network): made again.
                if child.as_mut().is_none_or(|c| c.try_wait().ok().flatten().is_some()) {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    break;
                }
            }
        }
    }

    /// A live that cannot go on, for a reason of the server's own: said, kept with the failed.
    fn give_up(&self, s: &Session, d: &Destination, why: String) {
        tracing::warn!("streaming to {}: {why}", d.name);
        self.failed.lock().unwrap().insert(s.dest.clone(), Failed { source: s.source.clone(), message: why, at: now_ms() });
    }

    async fn run(&self, s: Arc<Session>, d: Destination) {
        let mut stop = s.stop.subscribe();
        let mut failures = 0u32;
        // Live at least once: a link lost on the way is made again.
        let mut ever_live = false;
        let ffmpeg = self.recorders.ffmpeg.clone();
        loop {
            if *stop.borrow() {
                break;
            }
            let Some(rec) = self.recorders.get(&s.source) else {
                self.give_up(&s, &d, "the source was deleted".into());
                break;
            };
            // Lost during a live: the slate goes out until the source comes back (or the live stops).
            if ever_live && d.slate && !rec.flowing() {
                if !self.slate(&s, &d, &rec, &mut stop).await {
                    break;
                }
                continue;
            }
            // The source first: woken by this live it takes a few seconds (a web page, an encoder
            // that has to call, longer). Lost during a live, the live waits for it as long as it
            // is not stopped; never live, two minutes.
            if !self.source_ready(&s, &d, &rec, &mut stop, if ever_live { None } else { Some(Duration::from_secs(120)) }).await {
                break;
            }
            let codec = rec.status.lock().unwrap().media.as_ref().map(|m| m.codec.clone()).unwrap_or_default();
            let Some(port) = free_port() else {
                self.give_up(&s, &d, "no free local port for the stream".into());
                break;
            };
            let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-nostats", "-loglevel", "warning", "-progress", "pipe:1", "-stats_period", "1", "-thread_queue_size", "4096", "-analyzeduration", "1000000", "-probesize", "3000000", "-f", "mpegts", "-i"].map(String::from).to_vec();
            args.push(format!("udp://127.0.0.1:{port}?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout=5000000"));
            let (out, note) = output_args(&d, &codec);
            args.extend(out);
            let mut cmd = Command::new(&ffmpeg);
            cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    self.give_up(&s, &d, format!("cannot start {ffmpeg}: {e}"));
                    break;
                }
            };
            self.set(&s, |st| {
                st.state = "connecting";
                st.note = note.clone();
                if st.message == "waiting for the source" {
                    st.message.clear();
                }
            });
            let key = rand_key();
            rec.taps.lock().unwrap().push((key, port));
            // Its progress (bytes sent each second) and its last words.
            let last: Arc<Mutex<VecDeque<String>>> = Default::default();
            if let Some(err) = child.stderr.take() {
                let last = last.clone();
                tokio::spawn(async move {
                    let mut lines = tokio::io::BufReader::new(err).lines();
                    while let Ok(Some(l)) = lines.next_line().await {
                        // The decoder's complaints about the source (a damaged picture) say nothing
                        // of the link, and came so many that its own error was pushed out.
                        if decoder_noise(&l) {
                            continue;
                        }
                        let mut q = last.lock().unwrap();
                        q.push_back(l);
                        if q.len() > 40 {
                            q.pop_front();
                        }
                    }
                });
            }
            let progress = child.stdout.take().map(|o| tokio::io::BufReader::new(o).lines());
            let started = std::time::Instant::now();
            let ended = self.follow(&s, &mut child, progress, &mut stop, &rec, d.slate).await;
            ever_live |= s.stats.lock().unwrap().live_since.is_some();
            rec.taps.lock().unwrap().retain(|t| t.0 != key);
            if ended {
                crate::recorder::run::stop_within(&mut child, Duration::from_secs(1)).await;
                break;
            }
            // The link fell: why, and again a little later.
            let lines: Vec<String> = last.lock().unwrap().iter().cloned().collect();
            let text = lines.join("\n");
            let source_lost = !rec.flowing();
            let why = if source_lost {
                // Nothing came from the source: it is not the destination's doing.
                "the source is no longer received: waiting for it".to_string()
            } else if lines.iter().all(|l| l.trim().is_empty()) {
                "the link fell: connecting again".to_string()
            } else if ever_live && stopped_taking(&text) {
                "the server stopped taking the stream: connecting again".to_string()
            } else {
                explain(&text, &hide(&crate::recorder::run::best_line(&lines), &d))
            };
            // Never live: it did not start, there is nothing to make again.
            if !ever_live {
                let why = why.replace(": connecting again", "").replace(": trying again", "").replace(": waiting for it", "");
                tracing::warn!("streaming to {}: could not start: {why}", d.name);
                self.failed.lock().unwrap().insert(s.dest.clone(), Failed { source: s.source.clone(), message: why, at: now_ms() });
                break;
            }
            if started.elapsed() > Duration::from_secs(60) {
                failures = 0;
            }
            // The source lost, with a slate: it goes out at once (the loop's next turn).
            if source_lost && d.slate {
                continue;
            }
            failures += 1;
            tracing::warn!("streaming to {}: {why}", d.name);
            self.set(&s, |st| {
                st.state = "retrying";
                st.message = why.clone();
                st.live_since = None;
                st.kbps = 0.0;
            });
            // Again soon: while the link is down the destination receives nothing (its player
            // plays its last seconds over and over). 2 s, then 5 s at most.
            let wait = Duration::from_secs(if failures <= 1 { 2 } else { 5 });
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = stop.changed() => {}
            }
        }
    }

    /// Reads FFmpeg's progress until it ends (false) or the live is stopped (true).
    async fn follow(
        &self,
        s: &Arc<Session>,
        child: &mut tokio::process::Child,
        mut progress: Option<tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>>,
        stop: &mut watch::Receiver<bool>,
        rec: &crate::recorder::Rec,
        slate: bool,
    ) -> bool {
        let mut size = 0u64;
        let mut last = (0u64, std::time::Instant::now());
        let mut look = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = stop.changed() => return true,
                _ = child.wait() => return false,
                // Live, with a slate: the source stopped coming, the slate goes out now rather
                // than when FFmpeg gives up waiting for it.
                _ = look.tick() => {
                    if slate && s.stats.lock().unwrap().live_since.is_some() && !rec.flowing() {
                        let _ = child.kill().await;
                        return false;
                    }
                }
                line = async { match progress.as_mut() { Some(p) => p.next_line().await, None => std::future::pending().await } } => {
                    let Ok(Some(line)) = line else { progress = None; continue };
                    if let Some(v) = line.strip_prefix("total_size=") {
                        size = v.trim().parse().unwrap_or(size);
                    }
                    if line.starts_with("progress=") {
                        let now = std::time::Instant::now();
                        let secs = now.duration_since(last.1).as_secs_f64();
                        if secs >= 0.5 {
                            // The first bytes (what was waiting when the link opened) are not a bitrate.
                            let kbps = if last.0 == 0 { 0.0 } else { (size.saturating_sub(last.0)) as f64 * 8.0 / 1000.0 / secs };
                            last = (size, now);
                            let first = self.set(s, |st| {
                                let first = st.live_since.is_none() && size > 0;
                                if first {
                                    st.live_since = Some(now_ms());
                                }
                                if size > 0 {
                                    st.state = "live";
                                    st.message.clear();
                                }
                                st.bytes = size;
                                st.kbps = kbps;
                                st.history.push_back(kbps as f32);
                                while st.history.len() > 600 {
                                    st.history.pop_front();
                                }
                                first
                            });
                            if first {
                                self.changed();
                            }
                        }
                    }
                }
            }
        }
    }

    fn set<T>(&self, s: &Session, f: impl FnOnce(&mut Stats) -> T) -> T {
        f(&mut s.stats.lock().unwrap())
    }

    /// Tries a destination: two seconds of black and silence sent with its key (SRT: the link and
    /// its passphrase). Ok with how long it took, else why.
    pub async fn test(&self, dest_id: &str, o: &Overrides) -> Result<Value, String> {
        let dest = self.get(dest_id).ok_or("no such destination")?;
        let mut o = o.clone();
        if dest.key_live && o.key.trim().is_empty() && !dest.srt() {
            o.key = "test".into(); // no key yet: the server is tried (it refuses the key)
        }
        let d = effective(&dest, &o)?;
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
        let mut args = s(&["-hide_banner", "-nostdin", "-nostats", "-loglevel", "error", "-re", "-f", "lavfi", "-i", "color=c=black:s=640x360:r=25", "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-t", "2"]);
        args.extend(s(&["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-g", "25", "-c:a", "aac", "-b:a", "64k", "-f", if d.srt() { "mpegts" } else { "flv" }]));
        args.push(target(&d));
        let started = std::time::Instant::now();
        let mut cmd = Command::new(&self.recorders.ffmpeg);
        cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
        let out = tokio::time::timeout(Duration::from_secs(20), cmd.output()).await.map_err(|_| "no answer from the server (address or firewall)".to_string())?.map_err(|e| e.to_string())?;
        let ms = started.elapsed().as_millis() as u64;
        if out.status.success() {
            let tried_key = !(dest.key_live && o.key == "test");
            return Ok(json!({"ok": true, "ms": ms.saturating_sub(2000), "keyChecked": tried_key || dest.srt()}));
        }
        let text = String::from_utf8_lossy(&out.stderr).to_string();
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        Err(explain(&text, &hide(&crate::recorder::run::best_line(&lines), &d)))
    }
}

/// A short random key (the taps of a source).
fn rand_key() -> u64 {
    uuid::Uuid::new_v4().as_u128() as u64
}

/// FFmpeg's message without the secrets (its address holds the key).
fn hide(msg: &str, d: &Destination) -> String {
    let mut m = msg.to_string();
    for secret in [&d.key, &d.passphrase] {
        if secret.len() >= 3 {
            m = m.replace(secret.as_str(), "••••");
        }
    }
    let target = target(d);
    m.replace(&target, &d.host()).chars().take(200).collect()
}

/// The usual reasons, said plainly (from all FFmpeg said; `why`: its most telling line).
/// A line of the decoder about the source's pictures or sound (a damaged packet), not about the link.
fn decoder_noise(line: &str) -> bool {
    let l = line.trim_start();
    ["[h264 @", "[hevc @", "[mpeg2video @", "[aac @", "[mp2 @", "[ac3 @", "[eac3 @", "[aac_latm @"].iter().any(|p| l.starts_with(p))
        || ["Consider increasing the value for the 'analyzeduration'", "Increasing reorder buffer", "corrupt input packet", "Packet corrupt", "PES packet size mismatch", "Non-monotonic DTS", "changing to", "Last message repeated"].iter().any(|k| l.contains(k))
}

/// A live that was going: a write that timed out (no "connection … failed": the link was made)
/// is the server no longer taking the stream, not an address or a firewall.
fn stopped_taking(all: &str) -> bool {
    let w = all.to_ascii_lowercase();
    (w.contains("timed out") || w.contains("timeout")) && !(w.contains("connection to") && w.contains("failed")) && !w.contains("connection refused")
}

fn explain(all: &str, why: &str) -> String {
    let w = all.to_ascii_lowercase();
    let link_failed = w.contains("connection to") && w.contains("failed");
    if link_failed || w.contains("connection refused") || w.contains("timed out") || w.contains("no route") || w.contains("failed to resolve") || w.contains("name or service") || w.contains("connection setup failure") {
        "no answer from the server (address or firewall)".into()
    } else if w.contains("badname") || w.contains("already publishing") || w.contains("already in use") {
        "the server still holds the previous link (same stream key): trying again".into()
    } else if ["unauthorized", "forbidden", "returned 401", "returned 403", "wrong passphrase", "authentication"].iter().any(|k| w.contains(k)) {
        "the server refuses the stream: check the stream key".into()
    } else if ["closed by the server", "connection reset", "broken pipe", "end of file"].iter().any(|k| w.contains(k)) {
        "the server closed the link: connecting again".into()
    } else if w.contains("timed out") || w.contains("timeout") {
        "the server stopped taking the stream: connecting again".into()
    } else if w.contains("protocol not found") {
        "this server's ffmpeg cannot send there (rtmps / srt not built in)".into()
    } else if why.is_empty() {
        "the server refused the stream".into()
    } else {
        // FFmpeg's line without its "[srt @ 0x…]".
        why.split_once("] ").filter(|_| why.starts_with('[')).map(|(_, r)| r).unwrap_or(why).to_string()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartBody {
    destination: String,
    source: String,
    #[serde(flatten)]
    overrides: Overrides,
}

#[derive(Deserialize)]
struct StopSource {
    source: String,
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/destinations", get(list).post(create))
        .route("/api/destinations/{id}", put(update).delete(remove))
        .route("/api/destinations/{id}/test", post(test))
        .route("/api/streams", get(streams).post(start))
        .route("/api/streams/{destination}", delete(stop))
        .route("/api/streams/stop", post(stop_source))
}

/// Admins: the destinations (their secrets masked). Operators: what the Streamer shows.
async fn list(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.can("stream.view"))?;
    let all = s.streams.list();
    Ok(Json(Value::Array(all.iter().map(|d| if me.is_admin() { d.public() } else { d.for_operator() }).collect())))
}

/// The saved secrets are kept when the form sends them masked.
fn keep_secrets(d: &mut Destination, old: Option<&Destination>) {
    if d.key == MASKED {
        d.key = old.map(|o| o.key.clone()).unwrap_or_default();
    }
    if d.passphrase == MASKED {
        d.passphrase = old.map(|o| o.passphrase.clone()).unwrap_or_default();
    }
    if d.key_live {
        d.key.clear();
    }
    d.name = d.name.trim().to_string();
    d.url = d.url.trim().to_string();
    d.key = d.key.trim().to_string();
}

async fn create(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(mut d): Json<Destination>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let _one = WRITE.lock().unwrap();
    keep_secrets(&mut d, None);
    d.validate().map_err(bad)?;
    let mut all = s.streams.list();
    d.id = crate::api::new_id();
    all.push(d.clone());
    s.streams.save(&all).map_err(bad)?;
    tracing::info!("{} created the destination {}", me.username, d.name);
    s.changed("destinations");
    Ok(Json(d.public()))
}

async fn update(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(mut d): Json<Destination>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let _one = WRITE.lock().unwrap();
    let mut all = s.streams.list();
    let i = all.iter().position(|x| x.id == id).ok_or_else(|| not_found("destination"))?;
    keep_secrets(&mut d, Some(&all[i]));
    d.id = id;
    d.validate().map_err(bad)?;
    all[i] = d.clone();
    s.streams.save(&all).map_err(bad)?;
    s.changed("destinations");
    Ok(Json(d.public()))
}

async fn remove(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let _one = WRITE.lock().unwrap();
    let mut all = s.streams.list();
    let before = all.len();
    all.retain(|d| d.id != id);
    if all.len() == before {
        return Err(not_found("destination"));
    }
    s.streams.end(&id);
    s.streams.save(&all).map_err(bad)?;
    s.changed("destinations");
    Ok(Json(json!({"ok": true})))
}

async fn test(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(o): Json<Overrides>) -> ApiResult<Json<Value>> {
    require(me.can("stream.run"))?;
    s.streams.test(&id, &o).await.map(Json).map_err(bad)
}

async fn streams(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.can("stream.view") || me.is_admin())?;
    Ok(Json(s.streams.json()))
}

async fn start(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<StartBody>) -> ApiResult<Json<Value>> {
    require(me.can("stream.run"))?;
    if let Some(cost) = s.streams.cost(&b.destination, &b.source, &b.overrides).map_err(bad)? {
        crate::limits::check(&s, &cost)?;
    }
    s.streams.begin(&b.destination, &b.source, &b.overrides, &me.username).map_err(bad)?;
    Ok(Json(s.streams.json()))
}

async fn stop(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(dest): UrlPath<String>) -> ApiResult<Json<Value>> {
    require(me.can("stream.run"))?;
    if s.streams.end(&dest) {
        tracing::info!("{} stops the stream to {dest}", me.username);
    }
    Ok(Json(json!({"ok": true})))
}

/// Every stream of a source ("Tout arrêter").
async fn stop_source(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<StopSource>) -> ApiResult<Json<Value>> {
    require(me.can("stream.run"))?;
    s.streams.end_source(&b.source);
    Ok(Json(json!({"ok": true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(protocol: &str, url: &str) -> Destination {
        Destination { id: "d".into(), name: "D".into(), protocol: protocol.into(), url: url.into(), encode: "copy".into(), latency: 500, ..Default::default() }
    }

    #[test]
    fn targets() {
        let mut d = dest("rtmps", "rtmps://a.example.com/live2/");
        d.key = "abcd-1234".into();
        assert_eq!(target(&d), "rtmps://a.example.com/live2/abcd-1234");
        assert_eq!(d.host(), "a.example.com");
        let mut s = dest("srt", "srt://regie.example.net:9000");
        s.passphrase = "secret pass&1".into();
        assert_eq!(target(&s), "srt://regie.example.net:9000?mode=caller&latency=500000&pkt_size=1316&passphrase=secret%20pass%261");
        assert!(dest("rtmp", "srt://x").validate().is_err());
    }

    #[test]
    fn a_key_for_each_live() {
        let mut d = dest("rtmp", "rtmp://live.example.com/app");
        d.key_live = true;
        assert!(effective(&d, &Overrides::default()).is_err());
        let e = effective(&d, &Overrides { key: " k1 ".into(), ..Default::default() }).unwrap();
        assert_eq!(target(&e), "rtmp://live.example.com/app/k1");
        // Not handed over: the address stays the admin's.
        let e = effective(&d, &Overrides { key: "k".into(), url: "rtmp://other/app".into(), ..Default::default() }).unwrap();
        assert_eq!(e.url, "rtmp://live.example.com/app");
    }

    #[test]
    fn encodings() {
        let d = dest("rtmp", "rtmp://live.example.com/app");
        let (a, note) = output_args(&d, "h264");
        assert!(a.windows(2).any(|w| w == ["-c:v", "copy"]) && a.contains(&"flv".to_string()) && note.is_empty());
        // MPEG-2 cannot go by RTMP: encoded again, and said.
        let (a, note) = output_args(&d, "mpeg2video");
        assert!(a.contains(&"libx264".to_string()) && !note.is_empty());
        // SRT carries anything as it is.
        let (a, _) = output_args(&dest("srt", "srt://h:1"), "mpeg2video");
        assert!(a.windows(2).any(|w| w == ["-c:a", "copy"]) && a.contains(&"mpegts".to_string()));
    }

    #[test]
    fn reasons() {
        // A port holding "401" is not a refused key.
        assert_eq!(explain("[tcp @ 0x1] Connection to tcp://127.0.0.1:19401?tcp_nodelay=0 failed: Connection refused", ""), "no answer from the server (address or firewall)");
        assert_eq!(explain("[srt @ 0x5] Connection to 127.0.0.1:19501 failed: Input/output error", ""), "no answer from the server (address or firewall)");
        assert_eq!(explain("Error in the pull function. NetStream.Publish.BadName", ""), "the server still holds the previous link (same stream key): trying again");
        assert_eq!(explain("[rtmp @ 0x1] Server error: Unauthorized", ""), "the server refuses the stream: check the stream key");
        assert_eq!(explain("av_interleaved_write_frame(): Broken pipe", ""), "the server closed the link: connecting again");
        assert!(stopped_taking("[rtmp @ 0x1] Error writing frame: Connection timed out"));
        assert!(!stopped_taking("[tcp @ 0x1] Connection to tcp://127.0.0.1:1935 failed: Connection timed out"));
        assert!(decoder_noise("[h264 @ 0x55] mmco: unref short failure") && decoder_noise("[mpegts @ 0x1] Packet corrupt (stream = 0, dts = 1).") && !decoder_noise("[rtmp @ 0x1] Server error: x"));
        assert_eq!(explain("x", "[flv @ 0x2] Something odd"), "Something odd");
    }

    #[test]
    fn secrets_hidden() {
        let mut d = dest("rtmp", "rtmp://live.example.com/app");
        d.key = "supersecret".into();
        let m = hide("Error opening output rtmp://live.example.com/app/supersecret: I/O error", &d);
        assert!(!m.contains("supersecret"));
        assert!(d.public()["key"] == MASKED && d.for_operator().get("key").is_none());
    }
}
