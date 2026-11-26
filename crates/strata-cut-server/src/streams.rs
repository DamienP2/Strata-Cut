//! Streaming (Streamer page): a source sent live to destinations (Système › Destinations: RTMP,
//! RTMPS or SRT encoders and platforms), each by its own FFmpeg reading the source's programme
//! (the Streamer's image, overlay and sound, `program.rs`) from its relay (`Runner::taps`):
//! starting or stopping one never touches the recording.
//!
//! A destination's secrets (stream key, SRT passphrase) are masked for everyone; an operator
//! sees only its name, its type, its server's host and what the admin hands to them (a key
//! given for each live, the server's address, the bitrate). A key given for a live is kept for
//! that live only: in memory, and in `lives.json` (readable by the service alone) until it stops.
//!
//! The lives under way are written in `lives.json`: after a restart of the server (an update, a
//! reboot, a crash) they start again by themselves. Only a stop asked for (or a live that never
//! started) takes one out.
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
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
    /// "copy" (the programme as it is) or "h264" (encoded again: picture height, bitrates).
    pub encode: String,
    pub height: u32,
    pub video_kbps: u32,
    pub audio_kbps: u32,
    /// Encoded again: a variable rate (VBR: the quality kept, the bitrate a ceiling), else
    /// constant (CBR, what platforms and SRT receivers ask).
    pub vbr: bool,
    /// What the operator may change in the Streamer: the server's address, the video bitrate.
    pub expose_url: bool,
    pub expose_bitrate: bool,
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
        if self.audio_kbps != 0 && !(64..=320).contains(&self.audio_kbps) {
            return Err("the audio bitrate is 64 to 320 kb/s".into());
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
        let saved = d.host().to_ascii_lowercase();
        d.url = o.url.trim().to_string();
        // Another server: the saved secrets are not sent to it (the live gives its own key).
        if d.host().to_ascii_lowercase() != saved {
            d.passphrase.clear();
            if !d.key_live {
                d.key = o.key.trim().to_string();
            }
        }
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

use crate::util::urlencode;

/// The encoding of a destination (`video_codec`: the source's, as received).
fn output_args(d: &Destination) -> Vec<String> {
    let s = crate::util::strs;
    let mut a = s(&["-map", "0:v:0", "-map", "0:a:0?"]);
    // The programme is H.264 and AAC already: sent as it is, or encoded again at the
    // destination's own bitrate and height.
    let encode = d.encode == "h264";
    if encode {
        let k = if d.video_kbps > 0 { d.video_kbps } else { 6000 };
        let scale = if d.height > 0 { format!(",scale=-2:{}", d.height) } else { String::new() };
        a.extend(s(&["-vf", &format!("yadif=deint=interlaced{scale},format=yuv420p"), "-c:v", "libx264", "-preset", "veryfast", "-profile:v", "high"]));
        a.extend(crate::recorder::rate_args(k, !d.vbr));
        a.extend(s(&["-force_key_frames", "expr:gte(t,n_forced*2)"]));
    } else {
        a.extend(s(&["-c:v", "copy"]));
    }
    if d.srt() && !encode {
        a.extend(s(&["-c:a", "copy", "-f", "mpegts"]));
    } else {
        let ak = if d.audio_kbps > 0 { d.audio_kbps } else { 160 };
        a.extend(s(&["-c:a", "aac", "-b:a", &format!("{ak}k"), "-ar", "48000", "-ac", "2"]));
        // SRT encoded again in CBR: the MPEG-TS padded to its rate too.
        if d.srt() {
            a.extend(crate::recorder::muxrate_args(if d.video_kbps > 0 { d.video_kbps } else { 6000 }, ak, !d.vbr));
        }
        a.extend(s(&["-f", if d.srt() { "mpegts" } else { "flv" }]));
    }
    // A server that stops taking what is sent (10 s): FFmpeg ends and the link is made again,
    // instead of waiting forever while the destination receives nothing.
    if !d.srt() {
        a.extend(s(&["-rw_timeout", "10000000"]));
    }
    a.push(target(d));
    a
}

fn free_port() -> Option<u16> {
    crate::util::free_port(false)
}

/// A live under way, as written in `lives.json` (started again after a restart).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Live {
    destination: String,
    source: String,
    #[serde(default)]
    overrides: Overrides,
    #[serde(default)]
    by: String,
    /// Who started it (the account's id): "start and stop their streams" goes by it.
    #[serde(default)]
    by_id: String,
    /// Tells this live from the next one to the same destination.
    token: u64,
}

/// A live to one destination.
struct Session {
    dest: String,
    source: String,
    /// Who started it (account id, name).
    by_id: String,
    by: String,
    started: i64,
    token: u64,
    /// Started again after a restart of the server: it was live, and is made again until stopped.
    resumed: bool,
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
    /// Since when the destination's server no longer takes it, once it was live (server ms):
    /// `DOWN_LIMIT` later, the live is stopped.
    down_since: Option<i64>,
}

/// A live whose destination's server cut the link this long (ms) is stopped: a person sees why,
/// and starts it again when that server is back.
const DOWN_LIMIT: i64 = 120_000;

pub struct Streams {
    data_dir: PathBuf,
    recorders: Arc<Manager>,
    /// The sources' programmes (image, overlay, sound) the lives send.
    pub programs: Arc<crate::program::Programs>,
    events: Events,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Lives that could not start (the destination never took them), by destination: shown
    /// with their reason until another start, a stop, or 10 minutes.
    failed: Mutex<HashMap<String, Failed>>,
    /// The server is stopping: its lives stay in `lives.json`, to start again.
    closing: AtomicBool,
}

struct Failed {
    source: String,
    message: String,
    at: i64,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("destinations.json")
}

fn lives_path(data_dir: &Path) -> PathBuf {
    data_dir.join("lives.json")
}

static LIVES: std::sync::Mutex<()> = std::sync::Mutex::new(());

static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Streams {
    pub fn start(data_dir: &Path, recorders: Arc<Manager>, programs: Arc<crate::program::Programs>, events: Events) -> Arc<Streams> {
        Arc::new(Streams { data_dir: data_dir.to_path_buf(), recorders, programs, events, sessions: Mutex::new(HashMap::new()), failed: Mutex::new(HashMap::new()), closing: AtomicBool::new(false) })
    }

    /// Changes the lives written: `f` gets them, and changes them.
    fn lives(&self, f: impl FnOnce(&mut Vec<Live>)) {
        let _g = LIVES.lock().unwrap();
        let path = lives_path(&self.data_dir);
        let mut all: Vec<Live> = crate::paths::read_json(&path).unwrap_or_default();
        f(&mut all);
        if let Err(e) = crate::paths::write_json(&path, &all, true) {
            tracing::warn!("{}: {e}", path.display());
        }
    }

    /// The lives under way when the server stopped: started again (their destination and their
    /// source still there; the resources' guard does not hold back what already ran).
    pub fn resume(self: &Arc<Self>) {
        let all: Vec<Live> = crate::paths::read_json(&lives_path(&self.data_dir)).unwrap_or_default();
        for l in all {
            if let Err(e) = self.begin_live(&l, true) {
                tracing::warn!("streaming to {}: not started again after the restart: {e}", self.name_of(&l.destination));
                self.lives(|all| all.retain(|x| x.token != l.token));
                self.failed.lock().unwrap().insert(l.destination.clone(), Failed { source: l.source.clone(), message: e, at: now_ms() });
                self.changed();
            }
        }
    }

    pub fn list(&self) -> Vec<Destination> {
        let mut all: Vec<Destination> = crate::paths::read_json(&path(&self.data_dir)).unwrap_or_default();
        all.sort_by_key(|d| d.name.to_lowercase());
        all
    }

    pub(crate) fn get(&self, id: &str) -> Option<Destination> {
        self.list().into_iter().find(|d| d.id == id)
    }

    fn save(&self, all: &[Destination]) -> Result<(), String> {
        // Their keys and passphrases: for the service alone.
        crate::paths::write_json(&path(&self.data_dir), all, true).map_err(|e| e.to_string())
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
        let (height, bits) = {
            let st = rec.status.lock().unwrap();
            (st.media.as_ref().map(|m| m.height).unwrap_or(1080), st.bitrate)
        };
        // The programme is encoded once for all the lives of its source (H.264): counted with the first.
        let again = d.encode == "h264" || !self.programs.is_running(source);
        let kbps = if d.encode != "h264" {
            if bits > 0 { (bits as f64 / 1000.0).clamp(4000.0, 12000.0) } else { 8000.0 }
        } else if again {
            (if d.video_kbps > 0 { d.video_kbps } else { 6000 } + if d.audio_kbps > 0 { d.audio_kbps } else { 160 }) as f64
        } else if bits > 0 {
            bits as f64 / 1000.0
        } else {
            rec.config().bitrate.max(8.0) * 1000.0
        };
        let mut cost = crate::limits::stream(format!("{} → {}", rec.config().name, dest.name), again, d.height, height, kbps);
        // A source not received yet is started by the stream: its receiver and preview count too.
        if !rec.running() {
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
                        "destination": s.dest, "source": s.source, "byId": s.by_id, "by": s.by, "started": s.started, "state": st.state,
                        "message": st.message, "note": st.note, "liveSince": st.live_since,
                        "duration": st.live_since.map(|t| now - t), "bytes": st.bytes, "kbps": st.kbps.round(),
                        "history": st.history.iter().map(|k| k.round()).collect::<Vec<_>>(),
                        "stopAt": st.down_since.map(|t| t + DOWN_LIMIT),
                    })
                })
                .chain({
                    let mut f = self.failed.lock().unwrap();
                    f.retain(|_, x| now - x.at < 600_000);
                    f.iter()
                        .map(|(dest, x)| json!({ "destination": dest, "source": x.source, "started": x.at, "state": "failed", "message": x.message, "note": "", "liveSince": null, "duration": null, "bytes": 0, "kbps": 0, "history": [], "stopAt": null }))
                        .collect::<Vec<_>>()
                })
                .collect(),
        )
    }

    /// Sends a source to a destination (a destination sends one source at a time).
    pub fn begin(self: &Arc<Self>, dest_id: &str, source: &str, o: &Overrides, who: &crate::auth::User) -> Result<(), String> {
        self.begin_live(&Live { destination: dest_id.into(), source: source.into(), overrides: o.clone(), by: who.username.clone(), by_id: who.id.clone(), token: rand_key() }, false)
    }

    /// Who started the live to this destination (account id), if one is under way.
    pub fn started_by(&self, dest_id: &str) -> Option<String> {
        self.sessions.lock().unwrap().get(dest_id).map(|x| x.by_id.clone())
    }

    /// The lives of a source: their destination and who started them.
    pub fn of_source(&self, source: &str) -> Vec<(String, String)> {
        self.sessions.lock().unwrap().values().filter(|x| x.source == source).map(|x| (x.dest.clone(), x.by_id.clone())).collect()
    }

    fn begin_live(self: &Arc<Self>, live: &Live, resumed: bool) -> Result<(), String> {
        let (dest_id, source, o, who) = (live.destination.as_str(), live.source.as_str(), &live.overrides, live.by.as_str());
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
            let session = Arc::new(Session { dest: dest_id.to_string(), source: source.to_string(), by_id: live.by_id.clone(), by: live.by.clone(), started: now_ms(), token: live.token, resumed, planned_kbps, stop: watch::channel(false).0, finished: watch::channel(false).0, stats: Mutex::new(Stats { state: "connecting", ..Default::default() }) });
            sessions.insert(dest_id.to_string(), session.clone());
            (session, previous)
        };
        self.failed.lock().unwrap().remove(dest_id);
        self.lives(|all| {
            all.retain(|x| x.destination != dest_id);
            all.push(live.clone());
        });
        // The source is received while it is sent (recorded meanwhile, whatever its programme).
        rec.streams.fetch_add(1, Ordering::Relaxed);
        rec.wake.notify_one();
        if resumed {
            tracing::info!("streaming to {} again after the restart (started by {who}): {}", dest.name, rec.config().name);
        } else {
            tracing::info!("{who} sends {} to {}", rec.config().name, dest.name);
        }
        let me = self.clone();
        tokio::spawn(async move {
            if let Some(p) = previous {
                let mut done = p.finished.subscribe();
                let _ = tokio::time::timeout(Duration::from_secs(5), done.wait_for(|d| *d)).await;
            }
            me.run(session.clone(), d).await;
            // Stopped, or never started: not to start again. The server stopping: it stays.
            if !me.closing.load(Ordering::Relaxed) {
                me.lives(|all| all.retain(|x| x.token != session.token));
            }
            let _ = session.finished.send(true);
            rec.streams.fetch_sub(1, Ordering::Relaxed);
            me.sessions.lock().unwrap().retain(|_, s| !Arc::ptr_eq(s, &session));
            me.changed();
        });
        self.changed();
        Ok(())
    }

    /// A destination's name (its id when it is gone).
    pub fn name_of(&self, id: &str) -> String {
        self.get(id).map(|d| d.name).unwrap_or_else(|| id.to_string())
    }

    /// Every live of a source stopped (the source deleted, or "Tout arrêter").
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
        self.closing.store(true, Ordering::Relaxed);
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

    /// Waits until the source is received (however long). False: the live was stopped.
    async fn source_ready(&self, s: &Session, rec: &crate::recorder::Rec, stop: &mut watch::Receiver<bool>) -> bool {
        let since = std::time::Instant::now();
        let mut said = "";
        loop {
            if *stop.borrow() {
                return false;
            }
            if rec.flowing() {
                return true;
            }
            // Long waited: said, and waited on (a person stops the live).
            let now = if since.elapsed() > Duration::from_secs(120) { "the source does not answer: waiting for it" } else { "waiting for the source" };
            if said != now {
                said = now;
                self.set(s, |st| st.message = now.into());
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                _ = stop.changed() => {}
            }
        }
    }

    /// Waits until the programme comes (however long, said with why it fails). False: the live
    /// was stopped.
    async fn program_ready(&self, s: &Session, r: &crate::program::Runner, stop: &mut watch::Receiver<bool>) -> bool {
        let since = std::time::Instant::now();
        let mut said = false;
        loop {
            if *stop.borrow() {
                return false;
            }
            // No longer made (it could not be): another is taken.
            if r.ended() {
                return true;
            }
            // Its first picture of the source too: nothing is sent to the destination before (a
            // server that receives a stream without a picture may refuse it, or show black).
            if r.flowing() && r.pictured() {
                self.set(s, |st| {
                    if st.message == "waiting for the source" || st.message.starts_with("the programme is being made") || st.message.starts_with("waiting for the source's first picture") {
                        st.message.clear();
                    }
                });
                return true;
            }
            // The source is here: what is waited for now is the programme (said, with why it failed,
            // else where its making is).
            if !said || since.elapsed() > Duration::from_secs(5) {
                said = true;
                let why = r.why();
                let now = if !why.is_empty() {
                    format!("the programme does not start: {why}")
                } else if r.flowing() {
                    format!("waiting for the source's first picture: {}", r.stage())
                } else if r.epoch() == 0 {
                    format!("the programme is being made: {}", r.stage())
                } else {
                    "the programme is being made: its encoder gives nothing yet".to_string()
                };
                self.set(s, |st| st.message = now);
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(300)) => {}
                _ = stop.changed() => {}
            }
        }
    }

    /// A live that cannot go on, for a reason of the server's own: said, kept with the failed.
    fn give_up(&self, s: &Session, d: &Destination, why: String) {
        tracing::warn!("streaming to {}: {why}", d.name);
        self.failed.lock().unwrap().insert(s.dest.clone(), Failed { source: s.source.clone(), message: why, at: now_ms() });
    }

    /// Said, and 5 s before trying again. False: the live was stopped meanwhile.
    async fn wait_again(&self, s: &Session, d: &Destination, why: String, stop: &mut watch::Receiver<bool>) -> bool {
        tracing::warn!("streaming to {}: {why}", d.name);
        self.set(s, |st| {
            st.state = "retrying";
            st.message = why;
        });
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            _ = stop.changed() => {}
        }
        !*stop.borrow()
    }

    async fn run(&self, s: Arc<Session>, d: Destination) {
        let mut stop = s.stop.subscribe();
        let mut failures = 0u32;
        // Live at least once (or before the restart): a link lost on the way is made again.
        let mut ever_live = s.resumed;
        // After a restart the sources start too: their first seconds, before the programme's.
        if s.resumed {
            let since = std::time::Instant::now();
            while !*stop.borrow() && since.elapsed() < Duration::from_secs(30) && !self.recorders.get(&s.source).is_some_and(|r| r.flowing()) {
                if since.elapsed() > Duration::from_secs(1) {
                    self.set(&s, |st| st.message = "waiting for the source".into());
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                    _ = stop.changed() => {}
                }
            }
        }
        let ffmpeg = self.recorders.ffmpeg.clone();
        // The programme, made while this live reads it.
        let mut runner = self.programs.acquire(&s.source);
        loop {
            if *stop.borrow() {
                break;
            }
            if runner.ended() {
                self.programs.release(&s.source, &runner);
                runner = self.programs.acquire(&s.source);
            }
            let Some(rec) = self.recorders.get(&s.source) else {
                self.give_up(&s, &d, "the source was deleted".into());
                break;
            };
            // The source first, the first time: woken by this live it takes a few seconds (a web
            // page, an encoder that has to call, longer). Then the programme goes on whatever the
            // source does (its last picture, the slate): the live never waits for it again.
            if !ever_live && !self.source_ready(&s, &rec, &mut stop).await {
                break;
            }
            if !self.program_ready(&s, &runner, &mut stop).await {
                break;
            }
            if runner.ended() {
                continue;
            }
            // The programme is H.264, a key frame every 2 s: read 3 s to be sure to find one (its
            // size, without which nothing can be sent).
            let Some(port) = free_port() else {
                if !self.wait_again(&s, &d, "no free local port for the stream: trying again".into(), &mut stop).await {
                    break;
                }
                continue;
            };
            let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-nostats", "-loglevel", "warning", "-progress", "pipe:1", "-stats_period", "1", "-thread_queue_size", "4096", "-analyzeduration", "3000000", "-probesize", "10000000", "-f", "mpegts", "-i"].map(String::from).to_vec();
            args.push(format!("udp://127.0.0.1:{port}?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout=5000000"));
            args.extend(output_args(&d));
            let mut cmd = Command::new(&ffmpeg);
            cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
            let spawned = cmd.spawn();
            if let Ok(c) = &spawned {
                crate::procs::tag(c.id(), format!("streamer:{}", s.source), if output_args(&d).iter().any(|a| a == "copy") { "Live (copy)" } else { "Live (encoded again)" });
            }
            let mut child = match spawned {
                Ok(c) => c,
                Err(e) => {
                    if !self.wait_again(&s, &d, format!("cannot start {ffmpeg}: {e}"), &mut stop).await {
                        break;
                    }
                    continue;
                }
            };
            self.set(&s, |st| {
                st.state = "connecting";
                if st.message == "waiting for the source" {
                    st.message.clear();
                }
            });
            let key = rand_key();
            runner.taps.lock().unwrap().push((key, port));
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
            let epoch = runner.epoch();
            let ended = self.follow(&s, &mut child, progress, &mut stop, &runner).await;
            ever_live |= s.stats.lock().unwrap().live_since.is_some();
            runner.taps.lock().unwrap().retain(|t| t.0 != key);
            if ended {
                crate::recorder::run::stop_within(&mut child, Duration::from_secs(1)).await;
                break;
            }
            // The link fell: why, and again a little later.
            let lines: Vec<String> = last.lock().unwrap().iter().cloned().collect();
            let text = lines.join("\n");
            // Its local port taken meanwhile by another: again at once, nothing to say.
            if text.contains("bind failed") && started.elapsed() < Duration::from_secs(10) {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            let why = if lines.iter().all(|l| l.trim().is_empty()) {
                "the link fell: connecting again".to_string()
            } else if ever_live && stopped_taking(&text) {
                "the server stopped taking the stream: connecting again".to_string()
            } else {
                explain(&text, &hide(&crate::recorder::run::best_line(&lines), &d))
            };
            // Never live: it did not start (its key, its address…): said, and tried again, as long
            // as a person does not stop it.
            if !ever_live {
                tracing::debug!("streaming to {}: FFmpeg said:\n{}", d.name, hide(&text, &d));
            }
            // Live, and the destination's server cut the link (not the programme made again on
            // this side): `DOWN_LIMIT` without it taking the stream again, the live is stopped.
            if ever_live && runner.epoch() == epoch {
                let now = now_ms();
                let since = self.set(&s, |st| *st.down_since.get_or_insert(now));
                if now - since >= DOWN_LIMIT {
                    self.give_up(&s, &d, "the destination's server cut the link for 2 minutes: the live was stopped".into());
                    break;
                }
            }
            if started.elapsed() > Duration::from_secs(60) {
                failures = 0;
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
        self.programs.release(&s.source, &runner);
    }

    /// Reads FFmpeg's progress until it ends (false) or the live is stopped (true).
    async fn follow(
        &self,
        s: &Arc<Session>,
        child: &mut tokio::process::Child,
        mut progress: Option<tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>>,
        stop: &mut watch::Receiver<bool>,
        runner: &crate::program::Runner,
    ) -> bool {
        let mut size = 0u64;
        let mut last = (0u64, std::time::Instant::now());
        // The programme's encoder started again (it stopped): its timestamps begin again, the
        // link is made again with them (a platform takes a new stream, not times going back).
        let epoch = runner.epoch();
        let mut look = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = stop.changed() => return true,
                _ = child.wait() => return false,
                _ = look.tick() => {
                    if runner.epoch() != epoch {
                        let _ = child.kill().await;
                        return false;
                    }
                    // Its server has not taken it again in time: no more trying.
                    if s.stats.lock().unwrap().down_since.is_some_and(|t| now_ms() - t >= DOWN_LIMIT) {
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
                                    // Taken again for a while (not a link that falls again at once).
                                    if st.live_since.is_some_and(|t| now_ms() - t >= 5_000) {
                                        st.down_since = None;
                                    }
                                    // Live, but without the source's picture: why, on its card.
                                    st.message = if runner.pictured() {
                                        String::new()
                                    } else {
                                        let why = runner.why();
                                        if why.is_empty() { "no picture of the source in the programme".into() } else { format!("no picture of the source in the programme: {why}") }
                                    };
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
        let s = crate::util::strs;
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

use crate::util::rand_key;

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
    ["[h264 @", "[hevc @", "[mpeg2video @", "[aac @", "[mp2 @", "[ac3 @", "[eac3 @", "[aac_latm @"].iter().any(|p| l.contains(p))
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
    require(me.can("stream.access"))?;
    let all = s.streams.list();
    Ok(Json(Value::Array(all.iter().filter(|d| crate::groups::may_stream_to(&me, &d.id)).map(|d| if me.is_admin() { d.public() } else { d.for_operator() }).collect())))
}

/// The saved secrets are kept when the form sends them masked, for the same server only: sent
/// elsewhere, they are given again.
fn keep_secrets(d: &mut Destination, old: Option<&Destination>) -> Result<(), String> {
    let same = old.is_some_and(|o| o.protocol == d.protocol && o.host().eq_ignore_ascii_case(&d.host()));
    if d.key == MASKED {
        if !same && old.is_some_and(|o| !o.key.is_empty()) {
            return Err("give the key again for this server".into());
        }
        d.key = old.map(|o| o.key.clone()).unwrap_or_default();
    }
    if d.passphrase == MASKED {
        if !same && old.is_some_and(|o| !o.passphrase.is_empty()) {
            return Err("give the passphrase again for this server".into());
        }
        d.passphrase = old.map(|o| o.passphrase.clone()).unwrap_or_default();
    }
    if d.key_live {
        d.key.clear();
    }
    d.name = d.name.trim().to_string();
    d.url = d.url.trim().to_string();
    d.key = d.key.trim().to_string();
    Ok(())
}

async fn create(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(mut d): Json<Destination>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let _one = WRITE.lock().unwrap();
    keep_secrets(&mut d, None).map_err(bad)?;
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
    keep_secrets(&mut d, Some(&all[i])).map_err(bad)?;
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

/// Tests at the same time, at most (each one encodes a few seconds).
static TESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// May this account stream: at all, and to this destination (its groups say which).
fn may_stream(me: &crate::auth::User, dest: &str) -> bool {
    (me.can("stream.run.own") || me.can("stream.run.all")) && crate::groups::may_stream_to(me, dest)
}

/// May this account stop this live: any, or its own.
fn may_stop(me: &crate::auth::User, by: &str) -> bool {
    me.can("stream.run.all") || (me.can("stream.run.own") && by == me.id)
}

async fn test(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(o): Json<Overrides>) -> ApiResult<Json<Value>> {
    require(may_stream(&me, &id))?;
    let _one = TESTS.try_acquire().map_err(|_| bad("other tests are under way: again in a moment"))?;
    s.streams.test(&id, &o).await.map(Json).map_err(bad)
}

async fn streams(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.can("stream.access"))?;
    Ok(Json(s.streams.json()))
}

async fn start(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<StartBody>) -> ApiResult<Json<Value>> {
    require(may_stream(&me, &b.destination) && crate::groups::sees_source(&me, &b.source))?;
    // A live under way there: replaced only by who may stop it.
    if let Some(by) = s.streams.started_by(&b.destination) {
        require(may_stop(&me, &by))?;
    }
    if let Some(cost) = s.streams.cost(&b.destination, &b.source, &b.overrides).map_err(bad)? {
        crate::limits::check(&s, &cost)?;
    }
    s.streams.begin(&b.destination, &b.source, &b.overrides, &me).map_err(bad)?;
    Ok(Json(s.streams.json()))
}

async fn stop(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(dest): UrlPath<String>) -> ApiResult<Json<Value>> {
    match s.streams.started_by(&dest) {
        Some(by) => require(may_stop(&me, &by))?,
        // A live that could not start: cleared by who may stream.
        None => require(me.can("stream.run.own") || me.can("stream.run.all"))?,
    }
    if s.streams.end(&dest) {
        tracing::info!("{} stops the stream to {dest}", me.username);
    }
    Ok(Json(json!({"ok": true})))
}

/// Every stream of a source ("Tout arrêter") this account may stop.
async fn stop_source(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<StopSource>) -> ApiResult<Json<Value>> {
    require(me.can("stream.run.own") || me.can("stream.run.all"))?;
    for (dest, by) in s.streams.of_source(&b.source) {
        if may_stop(&me, &by) {
            s.streams.end(&dest);
        }
    }
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
        // Handed over: another server never gets the saved key nor the passphrase.
        let mut d = dest("rtmp", "rtmp://live.example.com/app");
        d.key = "saved".into();
        d.expose_url = true;
        let e = effective(&d, &Overrides { url: "rtmp://other.example/app".into(), ..Default::default() }).unwrap();
        assert_eq!(target(&e), "rtmp://other.example/app");
        let e = effective(&d, &Overrides { url: "rtmp://LIVE.example.com/app2".into(), ..Default::default() }).unwrap();
        assert_eq!(target(&e), "rtmp://LIVE.example.com/app2/saved");
        let mut s = dest("srt", "srt://regie.example.net:9000");
        s.passphrase = "p".into();
        s.expose_url = true;
        let e = effective(&s, &Overrides { url: "srt://other.example:9000".into(), ..Default::default() }).unwrap();
        assert!(e.passphrase.is_empty());
    }

    #[test]
    fn encodings() {
        let mut d = dest("rtmp", "rtmp://live.example.com/app");
        let a = output_args(&d);
        assert!(a.windows(2).any(|w| w == ["-c:v", "copy"]) && a.contains(&"flv".to_string()));
        d.encode = "h264".into();
        assert!(output_args(&d).contains(&"libx264".to_string()));
        // SRT as it is: the sound copied too, in MPEG-TS.
        let a = output_args(&dest("srt", "srt://h:1"));
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
