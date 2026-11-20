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
            note = format!("encoded again: the source is in {}", video_codec.to_uppercase());
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
    a.push(target(d));
    (a, note)
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
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("destinations.json")
}

static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Streams {
    pub fn start(data_dir: &Path, recorders: Arc<Manager>, events: Events) -> Arc<Streams> {
        Arc::new(Streams { data_dir: data_dir.to_path_buf(), recorders, events, sessions: Mutex::new(HashMap::new()) })
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
                .collect(),
        )
    }

    /// Sends a source to a destination (a destination sends one source at a time).
    pub fn begin(self: &Arc<Self>, dest_id: &str, source: &str, o: &Overrides, who: &str) -> Result<(), String> {
        let dest = self.get(dest_id).ok_or("no such destination")?;
        let rec = self.recorders.get(source).ok_or("no such source")?;
        let d = effective(&dest, o)?;
        {
            let sessions = self.sessions.lock().unwrap();
            if let Some(s) = sessions.get(dest_id) {
                return if s.source == source { Ok(()) } else { Err("this destination already sends another source".into()) };
            }
        }
        let session = Arc::new(Session { dest: dest_id.to_string(), source: source.to_string(), started: now_ms(), stop: watch::channel(false).0, stats: Mutex::new(Stats { state: "connecting", ..Default::default() }) });
        self.sessions.lock().unwrap().insert(dest_id.to_string(), session.clone());
        // The source is received while it is sent (recorded meanwhile, whatever its programme).
        rec.streams.fetch_add(1, Ordering::Relaxed);
        rec.wake.notify_one();
        tracing::info!("{who} sends {} to {}", rec.config().name, dest.name);
        let me = self.clone();
        tokio::spawn(async move {
            me.run(session.clone(), d).await;
            rec.streams.fetch_sub(1, Ordering::Relaxed);
            me.sessions.lock().unwrap().retain(|_, s| !Arc::ptr_eq(s, &session));
            me.changed();
        });
        self.changed();
        Ok(())
    }

    pub fn end(&self, dest_id: &str) -> bool {
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

    /// One FFmpeg after another until stopped: a link that falls is made again (3 s, then up to 30 s).
    async fn run(&self, s: Arc<Session>, d: Destination) {
        let mut stop = s.stop.subscribe();
        let mut failures = 0u32;
        let ffmpeg = self.recorders.ffmpeg.clone();
        loop {
            if *stop.borrow() {
                break;
            }
            let Some(rec) = self.recorders.get(&s.source) else { break };
            let codec = rec.status.lock().unwrap().media.as_ref().map(|m| m.codec.clone()).unwrap_or_default();
            let Some(port) = free_port() else { break };
            let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-nostats", "-loglevel", "warning", "-progress", "pipe:1", "-stats_period", "1", "-thread_queue_size", "4096", "-analyzeduration", "1000000", "-probesize", "3000000", "-f", "mpegts", "-i"].map(String::from).to_vec();
            args.push(format!("udp://127.0.0.1:{port}?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout=15000000"));
            let (out, note) = output_args(&d, &codec);
            args.extend(out);
            let mut cmd = Command::new(&ffmpeg);
            cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    self.set(&s, |st| {
                        st.state = "retrying";
                        st.message = format!("cannot start {ffmpeg}: {e}");
                    });
                    break;
                }
            };
            self.set(&s, |st| {
                st.state = "connecting";
                st.note = note.clone();
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
                        let mut q = last.lock().unwrap();
                        q.push_back(l);
                        if q.len() > 20 {
                            q.pop_front();
                        }
                    }
                });
            }
            let progress = child.stdout.take().map(|o| tokio::io::BufReader::new(o).lines());
            let started = std::time::Instant::now();
            let ended = self.follow(&s, &mut child, progress, &mut stop).await;
            rec.taps.lock().unwrap().retain(|t| t.0 != key);
            if ended {
                crate::recorder::run::stop_within(&mut child, Duration::from_secs(1)).await;
                break;
            }
            // The link fell: why, and again a little later.
            let lines: Vec<String> = last.lock().unwrap().iter().cloned().collect();
            let why = if lines.iter().all(|l| l.trim().is_empty()) { "the link fell: connecting again".to_string() } else { explain(&lines.join("\n"), &hide(&crate::recorder::run::best_line(&lines), &d)) };
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
            let wait = Duration::from_secs((3 * failures as u64).min(30));
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
    ) -> bool {
        let mut size = 0u64;
        let mut last = (0u64, std::time::Instant::now());
        loop {
            tokio::select! {
                _ = stop.changed() => return true,
                _ = child.wait() => return false,
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
fn explain(all: &str, why: &str) -> String {
    let w = all.to_ascii_lowercase();
    if w.contains("connection refused") || w.contains("timed out") || w.contains("no route") || w.contains("failed to resolve") || w.contains("name or service") || w.contains("connection setup failure") {
        "no answer from the server (address or firewall)".into()
    } else if ["badname", "unauthorized", "forbidden", "returned 401", "returned 403", "closed by the server", "connection reset", "broken pipe", "wrong passphrase", "authentication"].iter().any(|k| w.contains(k)) {
        "the server refuses the stream: check the stream key".into()
    } else if w.contains("protocol not found") {
        "this server's ffmpeg cannot send there (rtmps / srt not built in)".into()
    } else if why.is_empty() {
        "the server refused the stream".into()
    } else {
        why.to_string()
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
    require(me.can_extract())?;
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
    require(me.can_extract())?;
    s.streams.test(&id, &o).await.map(Json).map_err(bad)
}

async fn streams(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.can_extract())?;
    Ok(Json(s.streams.json()))
}

async fn start(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<StartBody>) -> ApiResult<Json<Value>> {
    require(me.can_extract())?;
    s.streams.begin(&b.destination, &b.source, &b.overrides, &me.username).map_err(bad)?;
    Ok(Json(s.streams.json()))
}

async fn stop(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(dest): UrlPath<String>) -> ApiResult<Json<Value>> {
    require(me.can_extract())?;
    if s.streams.end(&dest) {
        tracing::info!("{} stops the stream to {dest}", me.username);
    }
    Ok(Json(json!({"ok": true})))
}

/// Every stream of a source ("Tout arrêter").
async fn stop_source(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<StopSource>) -> ApiResult<Json<Value>> {
    require(me.can_extract())?;
    let dests: Vec<String> = s.streams.sessions.lock().unwrap().values().filter(|x| x.source == b.source).map(|x| x.dest.clone()).collect();
    for d in dests {
        s.streams.end(&d);
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
    fn secrets_hidden() {
        let mut d = dest("rtmp", "rtmp://live.example.com/app");
        d.key = "supersecret".into();
        let m = hide("Error opening output rtmp://live.example.com/app/supersecret: I/O error", &d);
        assert!(!m.contains("supersecret"));
        assert!(d.public()["key"] == MASKED && d.for_operator().get("key").is_none());
    }
}
