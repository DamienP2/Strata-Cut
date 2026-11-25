//! Sending the clips (Système › Serveurs de fichiers): the places clips are put — a NAS's share
//! (SMB 2/3, by `smbclient`), SFTP, FTP or FTPS, WebDAV (by `curl`), a folder of the server (a NAS
//! mounted there) — each in a sub-folder made of the clip's source, date, kind, maker.
//!
//! A clip goes there when asked (the Extraits page; chosen when extracting), or by itself (every
//! new clip, or those of some sources). The sends wait in a queue, two at a time, and start again
//! after a restart; a failed one says why, and is tried again when asked.
//!
//! The passwords are masked for everyone (and never on a command line: in a file for the service
//! alone, the time of a send); a saved one goes only to the server it was given for.
use axum::{
    extract::{Path as UrlPath, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{watch, Notify},
};

use crate::{
    api::{bad, not_found, require, ApiResult},
    auth::{CurrentUser, User},
    clips::{Clip, Clips},
    recorder::MASKED,
    state::{Audience, Events, Shared},
    util::{now_ms, urlencode},
};

/// Sends under way at the same time.
const AT_ONCE: usize = 2;
/// Records kept (the sends done and failed, for the Extraits page).
const KEPT: usize = 2000;
pub const KINDS: [&str; 5] = ["smb", "sftp", "ftp", "webdav", "dir"];

/// A place clips are sent to.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Target {
    pub id: String,
    pub name: String,
    /// "smb", "sftp", "ftp", "webdav" or "dir" (a folder of the server).
    pub kind: String,
    pub host: String,
    /// 0: the kind's own.
    pub port: u16,
    /// SMB: the share.
    pub share: String,
    /// The folder there (in the share, on the server); "dir": the server's folder.
    pub path: String,
    pub user: String,
    pub password: String,
    /// FTP: "explicit" (FTPS), "implicit" (FTPS) or "none"; WebDAV: "tls" (https) or "none".
    pub tls: String,
    /// The sub-folder, of `{source}`, `{date}`, `{kind}`, `{user}`.
    pub folder: String,
    /// A file of that name there: "rename", "replace" or "skip".
    pub exists: String,
    /// Every new clip sent (of `sources`; none listed: of all).
    pub auto: bool,
    pub sources: Vec<String>,
    /// Who sends there when they want: "extract" (who extracts), "groups", "admins".
    pub who: String,
    pub groups: Vec<String>,
}

impl Default for Target {
    fn default() -> Self {
        Target {
            id: String::new(),
            name: String::new(),
            kind: "smb".into(),
            host: String::new(),
            port: 0,
            share: String::new(),
            path: String::new(),
            user: String::new(),
            password: String::new(),
            tls: "explicit".into(),
            folder: "{source}/{date}".into(),
            exists: "rename".into(),
            auto: false,
            sources: Vec::new(),
            who: "extract".into(),
            groups: Vec::new(),
        }
    }
}

impl Target {
    fn normalize(&mut self) {
        for v in [&mut self.name, &mut self.host, &mut self.share, &mut self.path, &mut self.user, &mut self.folder] {
            *v = v.trim().to_string();
        }
        self.share = self.share.trim_matches(['/', '\\']).to_string();
        if !matches!(self.exists.as_str(), "rename" | "replace" | "skip") {
            self.exists = "rename".into();
        }
        if !matches!(self.who.as_str(), "extract" | "groups" | "admins") {
            self.who = "extract".into();
        }
        let tls_ok = match self.kind.as_str() {
            "ftp" => matches!(self.tls.as_str(), "explicit" | "implicit" | "none"),
            "webdav" => matches!(self.tls.as_str(), "tls" | "none"),
            _ => true,
        };
        if !tls_ok {
            self.tls = if self.kind == "ftp" { "explicit".into() } else { "tls".into() };
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("the name cannot be empty".into());
        }
        if !KINDS.contains(&self.kind.as_str()) {
            return Err("unknown kind of place".into());
        }
        let parts = |p: &str| p.split(['/', '\\']).any(|s| s == "..");
        if parts(&self.path) || parts(&self.folder) || parts(&self.share) {
            return Err("no « .. » in the folders".into());
        }
        if self.kind == "dir" {
            if !self.path.starts_with('/') {
                return Err("the server's folder is a full path (/mnt/…)".into());
            }
        } else {
            if self.host.is_empty() || self.host.starts_with('-') || self.host.chars().any(|c| c.is_whitespace() || "/\\@?#".contains(c)) {
                return Err("the server's name or address is not valid".into());
            }
            if self.kind == "smb" && self.share.is_empty() {
                return Err("give the share".into());
            }
        }
        if [&self.name, &self.user, &self.path, &self.share, &self.folder].iter().any(|v| v.chars().any(char::is_control)) {
            return Err("no control characters".into());
        }
        Ok(())
    }

    fn default_port(&self) -> u16 {
        match (self.kind.as_str(), self.tls.as_str()) {
            ("smb", _) => 445,
            ("sftp", _) => 22,
            ("ftp", "implicit") => 990,
            ("ftp", _) => 21,
            ("webdav", "none") => 80,
            ("webdav", _) => 443,
            _ => 0,
        }
    }

    fn port(&self) -> u16 {
        if self.port > 0 { self.port } else { self.default_port() }
    }

    /// Where it is, as said to people (no password).
    pub fn address(&self) -> String {
        let port = if self.port > 0 && self.port != self.default_port() { format!(":{}", self.port) } else { String::new() };
        let path = self.path.trim_matches('/');
        let path = if path.is_empty() { String::new() } else { format!("/{path}") };
        match self.kind.as_str() {
            "dir" => self.path.clone(),
            "smb" => format!("smb://{}{port}/{}{path}", self.host, self.share),
            "ftp" => format!("{}://{}{port}{path}", if self.tls == "none" { "ftp" } else { "ftps" }, self.host),
            "webdav" => format!("{}://{}{port}{path}", if self.tls == "none" { "http" } else { "https" }, self.host),
            k => format!("{k}://{}{port}{path}", self.host),
        }
    }

    /// The folder clips go under on the server (its sub-folder added to it): shown in the extraction window.
    fn base(&self) -> String {
        let path = self.path.trim_matches(['/', '\\']);
        match self.kind.as_str() {
            "dir" => self.path.trim_end_matches(['/', '\\']).to_string(),
            "smb" => [self.share.trim_matches(['/', '\\']), path].iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join("/"),
            _ => format!("/{path}"),
        }
    }

    /// For the admins: the password masked.
    fn public(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if !self.password.is_empty() {
            v["password"] = json!(MASKED);
        }
        v["address"] = json!(self.address());
        v["base"] = json!(self.base());
        v
    }

    /// May this account send there (when it wants)? It must be allowed to send at all.
    fn usable_by(&self, me: &User, groups: &[String]) -> bool {
        me.is_admin()
            || me.can("extract.send")
                && match self.who.as_str() {
                "groups" => self.groups.iter().any(|g| groups.contains(g)),
                "extract" => me.can("extract.cut"),
                _ => false,
            }
    }

    /// The same server and account (a saved password may go there).
    fn same_place(&self, o: &Target) -> bool {
        (self.kind.as_str(), self.host.to_lowercase(), self.port(), self.user.as_str()) == (o.kind.as_str(), o.host.to_lowercase(), o.port(), o.user.as_str())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SendState {
    Queued,
    Running,
    Done,
    Error,
}

/// A clip sent (or to send) to a place.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Send {
    pub id: String,
    pub clip: String,
    pub clip_name: String,
    pub target: String,
    pub state: SendState,
    /// 0 to 1; none: not known (SMB).
    pub progress: Option<f64>,
    /// Bytes a second, while it goes.
    pub speed: u64,
    pub message: String,
    /// Where it went (its folder and name there).
    pub remote: String,
    /// Who asked (empty: sent by itself).
    pub by: String,
    pub at: i64,
    pub ended: Option<i64>,
}

pub struct Sends {
    data_dir: PathBuf,
    events: Events,
    clips: Arc<Clips>,
    targets: Mutex<Vec<Target>>,
    list: Mutex<Vec<Send>>,
    /// The sends going: how to stop each.
    stops: Mutex<HashMap<String, watch::Sender<bool>>>,
    timezone: Mutex<String>,
    wake: Notify,
    told: Mutex<Option<Instant>>,
}

fn targets_path(dir: &Path) -> PathBuf {
    dir.join("sends.json")
}
fn list_path(dir: &Path) -> PathBuf {
    dir.join("sends-log.json")
}

impl Sends {
    pub fn start(data_dir: &Path, events: Events, clips: Arc<Clips>) -> Arc<Self> {
        let targets: Vec<Target> = crate::paths::read_json(&targets_path(data_dir)).unwrap_or_default();
        let mut list: Vec<Send> = crate::paths::read_json(&list_path(data_dir)).unwrap_or_default();
        // Going when the server stopped: again from the start.
        for s in list.iter_mut().filter(|s| s.state == SendState::Running) {
            s.state = SendState::Queued;
            s.progress = Some(0.0);
        }
        let s = Arc::new(Sends {
            data_dir: data_dir.to_path_buf(),
            events,
            clips,
            targets: Mutex::new(targets),
            list: Mutex::new(list),
            stops: Mutex::new(HashMap::new()),
            timezone: Mutex::new(String::new()),
            wake: Notify::new(),
            told: Mutex::new(None),
        });
        tokio::spawn(dispatch(s.clone()));
        s
    }

    pub fn set_config(&self, cfg: &crate::config::ServerConfig) {
        *self.timezone.lock().unwrap() = cfg.timezone.clone();
    }

    fn target(&self, id: &str) -> Option<Target> {
        self.targets.lock().unwrap().iter().find(|t| t.id == id).cloned()
    }

    fn save_targets(&self, all: &[Target]) -> Result<(), String> {
        crate::paths::write_json(&targets_path(&self.data_dir), all, true).map_err(|e| e.to_string())
    }

    fn save_list(&self) {
        let list = self.list.lock().unwrap().clone();
        if let Err(e) = crate::paths::write_json(&list_path(&self.data_dir), &list, false) {
            tracing::error!("sends-log.json: {e}");
        }
    }

    /// Said to the pages (the progress at most every second).
    fn changed(&self, now: bool) {
        let mut told = self.told.lock().unwrap();
        if now || told.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            *told = Some(Instant::now());
            self.events.send(Audience::All, json!({"type": "changed", "what": "sends"}));
        }
    }

    fn set(&self, id: &str, f: impl FnOnce(&mut Send)) {
        if let Some(s) = self.list.lock().unwrap().iter_mut().find(|s| s.id == id) {
            f(s);
        }
    }

    /// Queues a clip for a place (one already waiting or going there: that one).
    pub fn queue(&self, clip: &Clip, target: &Target, by: &str) -> Send {
        let tz = self.timezone.lock().unwrap().clone();
        let remote = remote_path(target, clip, &tz);
        let mut list = self.list.lock().unwrap();
        if let Some(s) = list.iter().find(|s| s.clip == clip.id && s.target == target.id && matches!(s.state, SendState::Queued | SendState::Running)) {
            return s.clone();
        }
        // The older record of that clip there: replaced.
        list.retain(|s| !(s.clip == clip.id && s.target == target.id));
        let s = Send { id: crate::api::new_id(), clip: clip.id.clone(), clip_name: clip.name.clone(), target: target.id.clone(), state: SendState::Queued, progress: Some(0.0), speed: 0, message: String::new(), remote, by: by.to_string(), at: now_ms(), ended: None };
        list.push(s.clone());
        if list.len() > KEPT {
            let over = list.len() - KEPT;
            let mut n = 0;
            list.retain(|x| {
                let old = n < over && matches!(x.state, SendState::Done | SendState::Error);
                if old {
                    n += 1;
                }
                !old
            });
        }
        drop(list);
        self.save_list();
        self.wake.notify_one();
        self.changed(true);
        s
    }

    /// A new clip: to the places that take every new clip (of its source), and to those its
    /// maker chose when extracting (that they may use).
    pub fn clip_made(&self, clip: &Clip, chosen: &[String], maker: Option<&User>, groups: &[String]) {
        let targets = self.targets.lock().unwrap().clone();
        for t in &targets {
            let auto = t.auto && (t.sources.is_empty() || t.sources.contains(&clip.recorder));
            let asked = chosen.contains(&t.id) && maker.is_some_and(|m| t.usable_by(m, groups));
            if auto || asked {
                self.queue(clip, t, if asked { maker.map(|m| m.id.as_str()).unwrap_or("") } else { "" });
            }
        }
    }

    /// A clip deleted: its records go.
    pub fn clip_gone(&self, id: &str) {
        let stopping: Vec<String> = self.list.lock().unwrap().iter().filter(|s| s.clip == id).map(|s| s.id.clone()).collect();
        for s in &stopping {
            if let Some(tx) = self.stops.lock().unwrap().get(s) {
                let _ = tx.send(true);
            }
        }
        self.list.lock().unwrap().retain(|s| s.clip != id);
        self.save_list();
        self.changed(true);
    }
}

// ---------------------------------------------------------------- the queue

async fn dispatch(s: Arc<Sends>) {
    loop {
        let next = {
            let going = s.list.lock().unwrap().iter().filter(|x| x.state == SendState::Running).count();
            if going >= AT_ONCE {
                None
            } else {
                let mut list = s.list.lock().unwrap();
                list.iter_mut().find(|x| x.state == SendState::Queued).map(|x| {
                    x.state = SendState::Running;
                    x.progress = Some(0.0);
                    x.message.clear();
                    x.clone()
                })
            }
        };
        let Some(job) = next else {
            tokio::select! { _ = s.wake.notified() => {}, _ = tokio::time::sleep(Duration::from_secs(10)) => {} }
            continue;
        };
        s.changed(true);
        let (tx, rx) = watch::channel(false);
        s.stops.lock().unwrap().insert(job.id.clone(), tx);
        let s2 = s.clone();
        tokio::spawn(async move {
            let result = run(&s2, &job, rx).await;
            s2.stops.lock().unwrap().remove(&job.id);
            let ok = result.is_ok();
            let said = match &result {
                Ok(m) => m.clone(),
                Err(e) => e.clone(),
            };
            match &result {
                Ok(_) => tracing::info!("clip {} sent to {}: {}", job.clip_name, s2.target(&job.target).map(|t| t.name).unwrap_or_default(), job.remote),
                Err(e) => tracing::warn!("clip {} not sent to {}: {e}", job.clip_name, s2.target(&job.target).map(|t| t.name).unwrap_or_default()),
            }
            s2.set(&job.id, |x| {
                x.state = if ok { SendState::Done } else { SendState::Error };
                x.progress = Some(if ok { 1.0 } else { x.progress.unwrap_or(0.0) });
                x.speed = 0;
                x.message = said.clone();
                x.ended = Some(now_ms());
            });
            s2.save_list();
            s2.changed(true);
            s2.wake.notify_one();
        });
    }
}

/// One send: Ok(what to say, "" or "already there"), or why it failed.
async fn run(s: &Arc<Sends>, job: &Send, stop: watch::Receiver<bool>) -> Result<String, String> {
    let target = s.target(&job.target).ok_or("this place no longer exists")?;
    let clip = s.clips.get(&job.clip).ok_or("this clip no longer exists")?;
    let size = tokio::fs::metadata(&clip.file).await.map_err(|_| "the clip's file is gone".to_string())?.len();
    let (dir, name) = job.remote.rsplit_once('/').map(|(d, n)| (d.to_string(), n.to_string())).unwrap_or((String::new(), job.remote.clone()));
    let id = job.id.clone();
    let s2 = s.clone();
    let progress = move |done: u64, speed: u64| {
        let p = if size > 0 { (done as f64 / size as f64).min(0.99) } else { 0.0 };
        s2.set(&id, |x| {
            x.progress = Some(p);
            x.speed = speed;
        });
        s2.changed(false);
    };
    let t = Transfer { target: &target, temp: s.data_dir.join("sends"), stop };
    let name = match t.exists(&dir, &name).await? {
        false => name,
        true => match target.exists.as_str() {
            "skip" => return Ok("already there: not sent".into()),
            "replace" => name,
            _ => t.free_name(&dir, &name).await?,
        },
    };
    if target.kind == "smb" {
        s.set(&job.id, |x| x.progress = None);
    }
    t.put(&clip.file, &dir, &name, size, &progress).await?;
    if name != job.remote.rsplit('/').next().unwrap_or("") {
        let full = if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") };
        s.set(&job.id, |x| x.remote = full);
    }
    Ok(String::new())
}

/// The folder and name of a clip there: the place's sub-folder, the clip's file name.
fn remote_path(t: &Target, clip: &Clip, tz: &str) -> String {
    let ext = clip.file.extension().and_then(|e| e.to_str()).unwrap_or_else(|| clip.kind.ext());
    let name = crate::clips::file_name(&clip.name, ext);
    let zone = jiff::tz::TimeZone::get(tz).unwrap_or_else(|_| jiff::tz::TimeZone::system());
    let date = jiff::Timestamp::from_millisecond(clip.created_at).map(|t| t.to_zoned(zone).strftime("%Y-%m-%d").to_string()).unwrap_or_default();
    let kind = match clip.kind {
        crate::clips::Kind::Video => "Vidéo",
        crate::clips::Kind::Audio => "Son",
        crate::clips::Kind::Image => "Image",
    };
    let sub = expand(&t.folder, &[("{source}", &clip.recorder_name), ("{date}", &date), ("{kind}", kind), ("{user}", &clip.created_by)]);
    let mut parts: Vec<String> = t.path.split(['/', '\\']).chain(sub.iter().map(String::as_str)).filter(|p| !p.is_empty() && *p != ".").map(String::from).collect();
    if t.kind == "dir" {
        // The server's folder itself is not part of what is made there.
        parts = sub;
    }
    parts.push(name);
    parts.join("/")
}

/// The sub-folder's parts, each token replaced by its value made safe for a file name.
fn expand(pattern: &str, values: &[(&str, &str)]) -> Vec<String> {
    pattern
        .split(['/', '\\'])
        .map(|part| {
            let mut p = part.to_string();
            for (k, v) in values {
                p = p.replace(k, &safe(v));
            }
            safe(&p)
        })
        .filter(|p| !p.is_empty() && p != "." && p != "..")
        .collect()
}

fn safe(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '-' } else { c }).collect();
    t.trim().trim_matches('.').trim().to_string()
}

// ---------------------------------------------------------------- the protocols

struct Transfer<'a> {
    target: &'a Target,
    /// Where the password files and the test files are made (the service's alone).
    temp: PathBuf,
    stop: watch::Receiver<bool>,
}

/// What a tool says it failed of, in plain words.
fn explain(kind: &str, code: Option<i32>, said: &str) -> String {
    let low = said.to_lowercase();
    let by_code = match (kind, code) {
        (_, Some(6)) => Some("the server's name is unknown"),
        (_, Some(7)) => Some("no answer from the server (address, port or firewall)"),
        (_, Some(28)) => Some("the server stopped answering"),
        (_, Some(67)) => Some("the server refuses the user or the password"),
        (_, Some(9)) => Some("access to the folder refused"),
        (_, Some(25)) => Some("the server refuses the file (rights, or no space)"),
        ("sftp", Some(60)) => Some("the server's key changed: is it the right server? (Tester learns it again)"),
        (_, Some(35)) | (_, Some(60)) => Some("the server's certificate is not valid"),
        (_, Some(51)) | (_, Some(83)) => Some("the server's key changed: is it the right server?"),
        (_, Some(60..=99)) if low.contains("known") => Some("the server's key changed: is it the right server?"),
        _ => None,
    };
    if let Some(r) = by_code {
        return r.into();
    }
    if low.contains("logon_failure") || low.contains("access denied") || low.contains("login denied") || low.contains("401") {
        return "the server refuses the user or the password".into();
    }
    if low.contains("bad_network_name") {
        return "this share does not exist on the server".into();
    }
    if low.contains("access_denied") || low.contains("403") || low.contains("permission denied") {
        return "access to the folder refused".into();
    }
    if low.contains("disk_full") || low.contains("507") || low.contains("no space") {
        return "the server has no space left".into();
    }
    if low.contains("connection refused") || low.contains("connection_refused") || low.contains("host_unreachable") || low.contains("timed out") || low.contains("io_timeout") {
        return "no answer from the server (address, port or firewall)".into();
    }
    let line = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.is_empty() { "the send failed".into() } else { line.chars().take(160).collect() }
}

impl Transfer<'_> {
    fn url(&self, dir: &str, name: &str) -> String {
        let t = self.target;
        let scheme = match (t.kind.as_str(), t.tls.as_str()) {
            ("ftp", "implicit") => "ftps",
            ("ftp", _) => "ftp",
            ("webdav", "none") => "http",
            ("webdav", _) => "https",
            (k, _) => k,
        };
        let host = if t.host.contains(':') { format!("[{}]", t.host) } else { t.host.clone() };
        let enc = |p: &str| p.split('/').filter(|x| !x.is_empty()).map(urlencode).collect::<Vec<_>>().join("/");
        let mut path = enc(dir);
        if t.kind == "sftp" && !t.path.starts_with('/') {
            // A folder without « / »: in the user's home.
            path = format!("~/{path}");
        }
        let mut url = format!("{scheme}://{host}:{}/{path}", t.port());
        if !url.ends_with('/') {
            url.push('/');
        }
        url.push_str(&urlencode(name));
        url
    }

    /// A file for the service alone, removed when dropped (a password, given to a tool).
    async fn secret_file(&self, text: &str) -> Result<TempFile, String> {
        tokio::fs::create_dir_all(&self.temp).await.map_err(|e| e.to_string())?;
        let path = self.temp.join(format!(".{}", crate::api::new_id()));
        crate::paths::write_atomic(&path, text.as_bytes(), true).map_err(|e| e.to_string())?;
        Ok(TempFile(path))
    }

    /// curl, its user and password in a file (never on its command line): (exit code, what it said).
    async fn curl(&self, args: &[String], on_progress: Option<&(dyn Fn(u64, u64) + Sync)>, size: u64) -> Result<(Option<i32>, String), String> {
        let t = self.target;
        let quote = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
        let mut conf = format!("user = \"{}:{}\"\n", quote(&t.user), quote(&t.password));
        if t.user.is_empty() {
            conf = if t.kind == "ftp" { "user = \"anonymous:\"\n".into() } else { String::new() };
        }
        let file = self.secret_file(&conf).await?;
        let home = self.temp.join("home");
        let _ = tokio::fs::create_dir_all(home.join(".ssh")).await;
        if t.kind == "sftp" {
            self.know_host(false).await?;
        }
        let mut cmd = Command::new("curl");
        cmd.env("HOME", &home)
            .arg("--config")
            .arg(&file.0)
            .args(["--connect-timeout", "15", "--speed-limit", "1", "--speed-time", "60", "--fail"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if t.kind == "ftp" && t.tls == "explicit" {
            cmd.arg("--ssl-reqd");
        }
        if t.kind == "ftp" && t.tls != "none" {
            // FTPS servers of a LAN have their own certificate, most often.
            cmd.arg("--insecure");
        }
        crate::recorder::background(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("cannot start curl: {e}"))?;
        let mut err = child.stderr.take().ok_or("curl")?;
        let mut stop = self.stop.clone();
        let mut said = Vec::new();
        let mut buf = [0u8; 4096];
        let mut last = Instant::now();
        loop {
            tokio::select! {
                n = err.read(&mut buf) => {
                    let n = n.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    said.extend_from_slice(&buf[..n]);
                    if let Some(f) = on_progress {
                        if last.elapsed() > Duration::from_millis(500) {
                            last = Instant::now();
                            if let Some((done, speed)) = meter(&said, size) {
                                f(done, speed);
                            }
                        }
                    }
                    if said.len() > 64_000 {
                        said.drain(..said.len() - 16_000);
                    }
                }
                _ = stop.changed() => {
                    let _ = child.kill().await;
                    return Err("cancelled".into());
                }
            }
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&said).to_string();
        Ok((status.code(), text))
    }

    /// SFTP: the server's key, learnt the first time (then each time checked by curl: another
    /// key, another server, refused). `again`: forgotten first (a test, by an admin).
    async fn know_host(&self, again: bool) -> Result<(), String> {
        let t = self.target;
        let file = self.temp.join("home").join(".ssh").join("known_hosts");
        let key = if t.port() == 22 { t.host.to_lowercase() } else { format!("[{}]:{}", t.host.to_lowercase(), t.port()) };
        let known = tokio::fs::read_to_string(&file).await.unwrap_or_default();
        let mine = |l: &str| l.split_whitespace().next().is_some_and(|h| h.eq_ignore_ascii_case(&key));
        if !again && known.lines().any(mine) {
            return Ok(());
        }
        let out = Command::new("ssh-keyscan").args(["-T", "10", "-p", &t.port().to_string(), "--", &t.host]).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).output().await.map_err(|e| format!("cannot start ssh-keyscan: {e}"))?;
        let found: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).map(String::from).collect();
        if found.is_empty() {
            return Err("no answer from the server (address, port or firewall)".into());
        }
        let mut lines: Vec<String> = known.lines().filter(|l| !mine(l)).map(String::from).collect();
        lines.extend(found);
        crate::paths::write_atomic(&file, (lines.join("\n") + "\n").as_bytes(), true).map_err(|e| e.to_string())
    }

    /// smbclient on the share, its user and password in a file: (ok, what it said).
    async fn smb(&self, dir: &str, command: &str) -> Result<(bool, String), String> {
        let t = self.target;
        let auth = self.secret_file(&format!("username = {}\npassword = {}\n", t.user, t.password)).await?;
        let mut cmd = Command::new("smbclient");
        cmd.arg(format!("//{}/{}", t.host, t.share)).arg("-A").arg(&auth.0).args(["-p", &t.port().to_string(), "-m", "SMB3", "-t", "30"]);
        if !dir.is_empty() {
            cmd.arg("-D").arg(dir.replace('/', "\\"));
        }
        cmd.arg("-c").arg(command).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        crate::recorder::background(&mut cmd);
        let child = cmd.spawn().map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "smbclient is not installed on the server".to_string() } else { format!("cannot start smbclient: {e}") })?;
        let mut stop = self.stop.clone();
        let out = tokio::select! {
            o = child.wait_with_output() => o.map_err(|e| e.to_string())?,
            _ = stop.changed() => return Err("cancelled".into()),
        };
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let failed = text.contains("NT_STATUS_") && !text.contains("NT_STATUS_OBJECT_NAME_COLLISION");
        Ok((out.status.success() && !failed, text))
    }

    /// The folders in a folder there (`dir`: from the share, the login's folder, the root; the
    /// server's folder itself for « dir »): None when it is not there.
    async fn list(&self, dir: &str) -> Result<Option<Vec<String>>, String> {
        let t = self.target;
        let mut names: Vec<String> = match t.kind.as_str() {
            "dir" => {
                let mut rd = match tokio::fs::read_dir(dir).await {
                    Ok(rd) => rd,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(format!("cannot read this folder: {e}")),
                };
                let mut out = Vec::new();
                while let Ok(Some(e)) = rd.next_entry().await {
                    if e.file_type().await.is_ok_and(|f| f.is_dir()) {
                        out.push(e.file_name().to_string_lossy().into_owned());
                    }
                }
                out
            }
            "smb" => {
                let (ok, said) = self.smb(dir, "ls").await?;
                if said.contains("NT_STATUS_OBJECT_NAME_NOT_FOUND") || said.contains("NT_STATUS_OBJECT_PATH_NOT_FOUND") || said.contains("NT_STATUS_NOT_A_DIRECTORY") {
                    return Ok(None);
                }
                if !ok {
                    return Err(explain("smb", None, &said));
                }
                // « name   D   0  Mon Nov 24 10:00:00 2026 »: the attributes after the name.
                said.lines()
                    .filter_map(|l| {
                        let l = l.trim_end();
                        if !l.chars().any(|c| c.is_ascii_digit()) {
                            return None;
                        }
                        let parts: Vec<&str> = l.split("  ").map(str::trim).filter(|x| !x.is_empty()).collect();
                        (parts.len() >= 3 && parts[1].contains('D')).then(|| parts[0].to_string())
                    })
                    .collect()
            }
            "webdav" => {
                let url = self.url(dir, "");
                let (code, said) = self.curl_out(&["-sS".into(), "-X".into(), "PROPFIND".into(), "-H".into(), "Depth: 1".into(), url.clone()]).await?;
                if code == Some(22) && said.contains("404") {
                    return Ok(None);
                }
                if code != Some(0) {
                    return Err(explain("webdav", code, &said));
                }
                // Each answer: its address, and whether it is a collection (a folder).
                let own = url.split_once("://").and_then(|(_, r)| r.split_once('/')).map(|(_, p)| format!("/{p}")).unwrap_or_default();
                said.split("response>")
                    .filter(|r| r.contains("collection"))
                    .filter_map(|r| {
                        let a = r.find("href>")? + 5;
                        let b = r[a..].find('<')? + a;
                        let href = r[a..b].trim().trim_end_matches('/');
                        let path = href.split_once("://").and_then(|(_, r)| r.split_once('/')).map(|(_, p)| format!("/{p}")).unwrap_or_else(|| href.to_string());
                        (path.trim_end_matches('/') != own.trim_end_matches('/')).then(|| decode(path.rsplit('/').next().unwrap_or("")))
                    })
                    .filter(|n| !n.is_empty())
                    .collect()
            }
            kind => {
                // FTP and SFTP: the folder's listing, « ls -l » style (a « d » first: a folder).
                let (code, said) = self.curl_out(&["-sS".into(), self.url(dir, "")]).await?;
                if matches!(code, Some(9) | Some(78)) {
                    return Ok(None);
                }
                if code != Some(0) {
                    return Err(explain(kind, code, &said));
                }
                said.lines()
                    .filter(|l| l.starts_with('d'))
                    .filter_map(|l| l.split_whitespace().nth(8).map(|_| l.split_whitespace().skip(8).collect::<Vec<_>>().join(" ")))
                    .collect()
            }
        };
        names.retain(|n| n != "." && n != ".." && !n.starts_with('.'));
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        Ok(Some(names))
    }

    /// curl as `curl`, what it writes (not its progress) answered with its exit code.
    async fn curl_out(&self, args: &[String]) -> Result<(Option<i32>, String), String> {
        let t = self.target;
        let quote = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
        let conf = if t.user.is_empty() { if t.kind == "ftp" { "user = \"anonymous:\"\n".to_string() } else { String::new() } } else { format!("user = \"{}:{}\"\n", quote(&t.user), quote(&t.password)) };
        let file = self.secret_file(&conf).await?;
        let home = self.temp.join("home");
        let _ = tokio::fs::create_dir_all(home.join(".ssh")).await;
        if t.kind == "sftp" {
            self.know_host(false).await?;
        }
        let mut cmd = Command::new("curl");
        cmd.env("HOME", &home).arg("--config").arg(&file.0).args(["--connect-timeout", "10", "--max-time", "20", "--fail"]).args(args).stdin(Stdio::null()).kill_on_drop(true);
        if t.kind == "ftp" && t.tls == "explicit" {
            cmd.arg("--ssl-reqd");
        }
        if t.kind == "ftp" && t.tls != "none" {
            cmd.arg("--insecure");
        }
        let out = cmd.output().await.map_err(|e| format!("cannot start curl: {e}"))?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.code(), text))
    }

    /// Is a file of that name there?
    async fn exists(&self, dir: &str, name: &str) -> Result<bool, String> {
        let t = self.target;
        match t.kind.as_str() {
            "dir" => Ok(tokio::fs::try_exists(Path::new(&t.path).join(dir).join(name)).await.unwrap_or(false)),
            "smb" => {
                let (_, said) = self.smb("", &format!("allinfo \"{}\"", smb_path(dir, name))).await?;
                if said.contains("NT_STATUS_OBJECT_NAME_NOT_FOUND") || said.contains("NT_STATUS_OBJECT_PATH_NOT_FOUND") || said.contains("NT_STATUS_NO_SUCH_FILE") {
                    Ok(false)
                } else if said.contains("NT_STATUS_") {
                    Err(explain("smb", None, &said))
                } else {
                    Ok(true)
                }
            }
            kind => {
                let (code, said) = self.curl(&["--head".into(), "-sS".into(), self.url(dir, name)], None, 0).await?;
                match code {
                    Some(0) => Ok(true),
                    // Not found (FTP 78, SFTP 78, HTTP 404: --fail 22).
                    Some(78) | Some(19) => Ok(false),
                    Some(22) if said.contains("404") || said.contains("409") => Ok(false),
                    Some(9) if kind == "ftp" => Ok(false),
                    c => Err(explain(kind, c, &said)),
                }
            }
        }
    }

    /// A name not taken there: « name (2).ext », « name (3).ext »…
    async fn free_name(&self, dir: &str, name: &str) -> Result<String, String> {
        let (stem, ext) = name.rsplit_once('.').map(|(s, e)| (s.to_string(), format!(".{e}"))).unwrap_or((name.to_string(), String::new()));
        for n in 2..100 {
            let candidate = format!("{stem} ({n}){ext}");
            if !self.exists(dir, &candidate).await? {
                return Ok(candidate);
            }
        }
        Err("too many files of that name there".into())
    }

    /// Sends the file (the folders made on the way).
    async fn put(&self, file: &Path, dir: &str, name: &str, size: u64, progress: &(dyn Fn(u64, u64) + Sync)) -> Result<(), String> {
        let t = self.target;
        match t.kind.as_str() {
            "dir" => copy(file, &Path::new(&t.path).join(dir), name, size, progress, self.stop.clone()).await,
            "smb" => {
                let mut made = String::new();
                for part in dir.split('/').filter(|p| !p.is_empty()) {
                    made = if made.is_empty() { part.to_string() } else { format!("{made}\\{part}") };
                    let (ok, said) = self.smb("", &format!("mkdir \"{made}\"")).await?;
                    if !ok && !said.contains("COLLISION") {
                        return Err(explain("smb", None, &said));
                    }
                }
                let local = file.to_string_lossy().replace('"', "");
                let (ok, said) = self.smb(dir, &format!("put \"{local}\" \"{name}\"")).await?;
                if ok { Ok(()) } else { Err(explain("smb", None, &said)) }
            }
            kind => {
                if kind == "webdav" {
                    self.mkcol(dir).await?;
                }
                let mut args = vec!["-T".to_string(), file.to_string_lossy().into_owned(), self.url(dir, name)];
                if kind != "webdav" {
                    args.insert(0, "--ftp-create-dirs".into());
                }
                let (code, said) = self.curl(&args, Some(progress), size).await?;
                if code == Some(0) { Ok(()) } else { Err(explain(kind, code, &said)) }
            }
        }
    }

    /// WebDAV: each folder made (one already there answers « not allowed »: fine).
    async fn mkcol(&self, dir: &str) -> Result<(), String> {
        let mut made = String::new();
        for part in dir.split('/').filter(|p| !p.is_empty()) {
            made = if made.is_empty() { part.to_string() } else { format!("{made}/{part}") };
            let url = self.url(&made, "");
            let (code, said) = self.curl(&["-sS".into(), "-X".into(), "MKCOL".into(), url], None, 0).await?;
            if code != Some(0) && !(code == Some(22) && (said.contains("405") || said.contains("301"))) {
                return Err(explain("webdav", code, &said));
            }
        }
        Ok(())
    }

    /// Writes a small file in the place's folder, then removes it: Ok(ms).
    async fn test(&self) -> Result<u64, String> {
        let t = self.target;
        let started = Instant::now();
        let name = format!(".strata-test-{}.txt", crate::api::new_id());
        let local = self.temp.join(&name);
        tokio::fs::create_dir_all(&self.temp).await.map_err(|e| e.to_string())?;
        tokio::fs::write(&local, b"Strata LiveCut: test\n").await.map_err(|e| e.to_string())?;
        let _gone = TempFile(local.clone());
        let none = |_: u64, _: u64| {};
        let base = t.path.trim_matches('/');
        if t.kind == "sftp" {
            self.know_host(true).await?;
        }
        match t.kind.as_str() {
            "dir" => {
                let dir = Path::new(&t.path);
                if !dir.is_dir() {
                    return Err("this folder does not exist on the server".into());
                }
                tokio::fs::write(dir.join(&name), b"test").await.map_err(|e| format!("cannot write there: {e}"))?;
                let _ = tokio::fs::remove_file(dir.join(&name)).await;
            }
            "smb" => {
                let (ok, said) = self.smb(base, &format!("put \"{}\" \"{name}\"; del \"{name}\"", local.to_string_lossy())).await?;
                if !ok {
                    return Err(explain("smb", None, &said));
                }
            }
            kind => {
                self.put(&local, base, &name, 21, &none).await?;
                let args: Vec<String> = match kind {
                    "webdav" => vec!["-sS".into(), "-X".into(), "DELETE".into(), self.url(base, &name)],
                    // (Sent before curl goes into the folder: from the login's folder.)
                    "ftp" => vec!["-sS".into(), "-Q".into(), format!("DELE {}{name}", if base.is_empty() { String::new() } else { format!("{base}/") }), self.url(base, "")],
                    // SFTP: from the root, or from the user's home.
                    _ => vec!["-sS".into(), "-Q".into(), format!("rm \"{}{name}\"", if base.is_empty() { String::new() } else { format!("{}{base}/", if t.path.starts_with('/') { "/" } else { "" }) }), self.url(base, "")],
                };
                let _ = self.curl(&args, None, 0).await?;
            }
        }
        Ok(started.elapsed().as_millis() as u64)
    }
}

/// « %20 » and the like, back to their characters.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn smb_path(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{}\\{name}", dir.replace('/', "\\")) }
}

/// curl's progress meter: (bytes sent, bytes a second), from its last line.
fn meter(said: &[u8], size: u64) -> Option<(u64, u64)> {
    let text = String::from_utf8_lossy(said);
    let line = text.rsplit(['\r', '\n']).find(|l| l.trim().chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    // % Total % Received % Xferd Average-Dload Speed-Upload Time… Current
    let pct: f64 = cols.first()?.parse().ok()?;
    let speed = cols.get(7).map(|s| bytes_of(s)).unwrap_or(0);
    Some(((size as f64 * pct / 100.0) as u64, speed))
}

/// « 38.2M », « 512k », « 1200 » → bytes.
fn bytes_of(s: &str) -> u64 {
    let (num, mul) = match s.chars().last() {
        Some('k') => (&s[..s.len() - 1], 1024.0),
        Some('M') => (&s[..s.len() - 1], 1024.0 * 1024.0),
        Some('G') => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (s, 1.0),
    };
    num.parse::<f64>().map(|v| (v * mul) as u64).unwrap_or(0)
}

/// A copy into a folder of the server: as « .name.part », then named (never half a file there).
async fn copy(file: &Path, dir: &Path, name: &str, size: u64, progress: &(dyn Fn(u64, u64) + Sync), mut stop: watch::Receiver<bool>) -> Result<(), String> {
    tokio::fs::create_dir_all(dir).await.map_err(|e| format!("cannot make the folder there: {e}"))?;
    let part = dir.join(format!(".{name}.part"));
    let mut from = tokio::fs::File::open(file).await.map_err(|e| e.to_string())?;
    let mut to = tokio::fs::File::create(&part).await.map_err(|e| format!("cannot write there: {e}"))?;
    let mut buf = vec![0u8; 1 << 20];
    let (mut done, started, mut told) = (0u64, Instant::now(), Instant::now());
    loop {
        if *stop.borrow_and_update() {
            drop(to);
            let _ = tokio::fs::remove_file(&part).await;
            return Err("cancelled".into());
        }
        let n = from.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if let Err(e) = to.write_all(&buf[..n]).await {
            drop(to);
            let _ = tokio::fs::remove_file(&part).await;
            return Err(if e.raw_os_error() == Some(28) { "the server has no space left".into() } else { format!("cannot write there: {e}") });
        }
        done += n as u64;
        if told.elapsed() > Duration::from_millis(500) {
            told = Instant::now();
            let secs = started.elapsed().as_secs_f64().max(0.001);
            progress(done.min(size), (done as f64 / secs) as u64);
        }
    }
    to.flush().await.map_err(|e| e.to_string())?;
    to.sync_all().await.map_err(|e| e.to_string())?;
    drop(to);
    tokio::fs::rename(&part, dir.join(name)).await.map_err(|e| e.to_string())
}

/// A file removed when it goes.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---------------------------------------------------------------- routes

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/send-targets", get(list_targets).post(create_target))
        .route("/api/send-targets/test", post(test_target))
        .route("/api/send-targets/browse", post(browse))
        .route("/api/send-targets/{id}", put(update_target).delete(delete_target))
        .route("/api/sends", get(list_sends).post(send))
        .route("/api/sends/{id}/retry", post(retry))
        .route("/api/sends/{id}", axum::routing::delete(dismiss))
}

/// For an admin: every place (passwords masked), with its last result; for the others: those
/// they may send to, and those clips go to by themselves (their name, kind, where).
async fn list_targets(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> Json<Value> {
    let all = s.sends.targets.lock().unwrap().clone();
    // Each place's last send ended: (when, ok, what it said).
    let mut last: HashMap<String, (i64, bool, String)> = HashMap::new();
    for x in s.sends.list.lock().unwrap().iter() {
        if let Some(at) = x.ended {
            if last.get(&x.target).is_none_or(|l| l.0 < at) {
                last.insert(x.target.clone(), (at, x.state == SendState::Done, x.message.clone()));
            }
        }
    }
    if me.is_admin() {
        return Json(Value::Array(
            all.iter()
                .map(|t| {
                    let mut v = t.public();
                    v["usable"] = json!(true);
                    if let Some((at, ok, said)) = last.get(&t.id) {
                        v["last"] = json!({ "at": at, "ok": ok, "message": said });
                    }
                    v
                })
                .collect(),
        ));
    }
    let groups = crate::groups::of(&s, &me.id);
    // Those it may send to, and those clips go to by themselves (shown, not chosen).
    Json(Value::Array(
        all.iter()
            .filter(|t| t.auto || t.usable_by(&me, &groups))
            .map(|t| json!({ "id": t.id, "name": t.name, "kind": t.kind, "folder": t.folder, "base": t.base(), "auto": t.auto, "sources": t.sources, "usable": t.usable_by(&me, &groups) }))
            .collect(),
    ))
}

/// The saved password kept when the form sends it masked, for the same server and account only.
fn keep_password(t: &mut Target, old: Option<&Target>) -> Result<(), String> {
    if t.password == MASKED {
        match old {
            Some(o) if o.same_place(t) => t.password = o.password.clone(),
            Some(o) if !o.password.is_empty() => return Err("give the password again for this server".into()),
            _ => t.password.clear(),
        }
    }
    Ok(())
}

async fn create_target(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(mut t): Json<Target>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    t.normalize();
    keep_password(&mut t, None).map_err(bad)?;
    t.validate().map_err(bad)?;
    t.id = crate::api::new_id();
    let mut all = s.sends.targets.lock().unwrap().clone();
    all.push(t.clone());
    s.sends.save_targets(&all).map_err(bad)?;
    *s.sends.targets.lock().unwrap() = all;
    tracing::info!("{} adds the clips' place {} ({})", me.username, t.name, t.address());
    s.sends.changed(true);
    Ok(Json(t.public()))
}

async fn update_target(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(mut t): Json<Target>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let mut all = s.sends.targets.lock().unwrap().clone();
    let i = all.iter().position(|x| x.id == id).ok_or_else(|| not_found("place"))?;
    t.normalize();
    keep_password(&mut t, Some(&all[i])).map_err(bad)?;
    t.validate().map_err(bad)?;
    t.id = id;
    all[i] = t.clone();
    s.sends.save_targets(&all).map_err(bad)?;
    *s.sends.targets.lock().unwrap() = all;
    s.sends.changed(true);
    Ok(Json(t.public()))
}

async fn delete_target(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    let mut all = s.sends.targets.lock().unwrap().clone();
    let before = all.len();
    all.retain(|t| t.id != id);
    if all.len() == before {
        return Err(not_found("place"));
    }
    s.sends.save_targets(&all).map_err(bad)?;
    *s.sends.targets.lock().unwrap() = all;
    // Its sends waiting or going: stopped; its records: gone.
    for (sid, tx) in s.sends.stops.lock().unwrap().iter() {
        if s.sends.list.lock().unwrap().iter().any(|x| x.id == *sid && x.target == id) {
            let _ = tx.send(true);
        }
    }
    s.sends.list.lock().unwrap().retain(|x| x.target != id);
    s.sends.save_list();
    s.sends.changed(true);
    Ok(StatusCode::NO_CONTENT)
}

/// Tests a place as in the form (its saved password when masked, for the same server).
async fn test_target(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(mut t): Json<Target>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    static TESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let _one = TESTS.try_acquire().map_err(|_| bad("other tests are under way: again in a moment"))?;
    t.normalize();
    let old = s.sends.target(&t.id);
    keep_password(&mut t, old.as_ref()).map_err(bad)?;
    t.validate().map_err(bad)?;
    let (_tx, rx) = watch::channel(false);
    let tr = Transfer { target: &t, temp: s.data_dir.join("sends"), stop: rx };
    let r = tokio::time::timeout(Duration::from_secs(60), tr.test()).await.map_err(|_| bad("no answer from the server (address, port or firewall)"))?;
    let ms = r.map_err(bad)?;
    Ok(Json(json!({ "ok": true, "ms": ms })))
}

#[derive(Deserialize)]
struct Browse {
    target: Target,
    /// The folder to look in (as the place's folder is written); none: the place's own.
    #[serde(default)]
    path: Option<String>,
}

/// A folder there, as in the form: whether it is there (else it is made at the first send), and
/// the folders in it (to choose one).
async fn browse(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<Browse>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let mut t = b.target;
    t.normalize();
    let old = s.sends.target(&t.id);
    keep_password(&mut t, old.as_ref()).map_err(bad)?;
    let path = b.path.unwrap_or_else(|| t.path.clone());
    if path.split(['/', '\\']).any(|p| p == "..") {
        return Err(bad("no « .. » in the folders"));
    }
    t.path = path.clone();
    t.validate().map_err(bad)?;
    let (_tx, rx) = watch::channel(false);
    let tr = Transfer { target: &t, temp: s.data_dir.join("sends"), stop: rx };
    let dir = if t.kind == "dir" { path.clone() } else { path.trim_matches('/').to_string() };
    let r = tokio::time::timeout(Duration::from_secs(30), tr.list(&dir)).await.map_err(|_| bad("no answer from the server (address, port or firewall)"))?;
    match r.map_err(bad)? {
        Some(folders) => Ok(Json(json!({ "exists": true, "path": path, "folders": folders }))),
        None => Ok(Json(json!({ "exists": false, "path": path, "folders": [] }))),
    }
}

/// The sends of the clips this account sees (and whether each is its own).
async fn list_sends(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> Json<Value> {
    if !me.can("clips.view") {
        return Json(Value::Array(Vec::new()));
    }
    let groups = crate::groups::of(&s, &me.id);
    let seen: std::collections::HashSet<String> = s.clips.visible_ids(&me, &groups);
    let names: HashMap<String, String> = s.sends.targets.lock().unwrap().iter().map(|t| (t.id.clone(), t.name.clone())).collect();
    let mine_clips: std::collections::HashSet<String> = s.clips.owned_ids(&me);
    let list = s.sends.list.lock().unwrap().clone();
    Json(Value::Array(
        list.iter()
            .filter(|x| seen.contains(&x.clip))
            .map(|x| {
                let mut v = serde_json::to_value(x).unwrap_or_default();
                v["targetName"] = json!(names.get(&x.target).cloned().unwrap_or_default());
                // Its own: asked by this account, or sent by itself for its clip.
                v["mine"] = json!(x.by == me.id || (x.by.is_empty() && mine_clips.contains(&x.clip)));
                v
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct SendBody {
    clips: Vec<String>,
    target: String,
}

async fn send(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<SendBody>) -> ApiResult<Json<Value>> {
    require(me.can("clips.view") && me.can("extract.send"))?;
    let groups = crate::groups::of(&s, &me.id);
    let target = s.sends.target(&b.target).ok_or_else(|| not_found("place"))?;
    if !target.usable_by(&me, &groups) {
        return Err((StatusCode::FORBIDDEN, "you do not have the rights for this".into()));
    }
    let mut out = Vec::new();
    for id in b.clips.iter().take(500) {
        let clip = s.clips.get(id).filter(|c| c.visible(&me, &groups)).ok_or_else(|| not_found("clip"))?;
        out.push(s.sends.queue(&clip, &target, &me.id));
    }
    tracing::info!("{} sends {} clip(s) to {}", me.username, out.len(), target.name);
    Ok(Json(json!(out)))
}

/// Who may act on a send: who asked for it, the maker of its clip (sent by itself), an admin.
fn may_act(s: &Shared, me: &User, x: &Send) -> bool {
    me.is_admin() || x.by == me.id || (x.by.is_empty() && s.clips.owned_ids(me).contains(&x.clip))
}

async fn retry(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let x = s.sends.list.lock().unwrap().iter().find(|x| x.id == id).cloned().ok_or_else(|| not_found("send"))?;
    if !may_act(&s, &me, &x) {
        return Err((StatusCode::FORBIDDEN, "you do not have the rights for this".into()));
    }
    let clip = s.clips.get(&x.clip).ok_or_else(|| not_found("clip"))?;
    let target = s.sends.target(&x.target).ok_or_else(|| not_found("place"))?;
    Ok(Json(json!(s.sends.queue(&clip, &target, &x.by))))
}

/// Cancels a send waiting or going; a send done or failed: its record taken away.
async fn dismiss(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    let x = s.sends.list.lock().unwrap().iter().find(|x| x.id == id).cloned().ok_or_else(|| not_found("send"))?;
    if !may_act(&s, &me, &x) {
        return Err((StatusCode::FORBIDDEN, "you do not have the rights for this".into()));
    }
    if let Some(tx) = s.sends.stops.lock().unwrap().get(&id) {
        let _ = tx.send(true);
    }
    s.sends.list.lock().unwrap().retain(|x| x.id != id);
    s.sends.save_list();
    s.sends.changed(true);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders() {
        let v = expand("{source}/{date}/../{kind}", &[("{source}", "Plateau 1 / Régie"), ("{date}", "2026-11-25"), ("{kind}", "Vidéo")]);
        assert_eq!(v, vec!["Plateau 1 - Régie", "2026-11-25", "Vidéo"]);
        assert_eq!(expand("", &[]), Vec::<String>::new());
        assert_eq!(safe(" ..a:b "), "a-b");
    }

    #[test]
    fn addresses() {
        let t = Target { kind: "smb".into(), host: "nas".into(), share: "Rushs".into(), path: "/Strata/".into(), ..Default::default() };
        assert_eq!(t.address(), "smb://nas/Rushs/Strata");
        let f = Target { kind: "ftp".into(), host: "ftp.example".into(), tls: "implicit".into(), path: "depot".into(), ..Default::default() };
        assert_eq!(f.port(), 990);
        let tr = Transfer { target: &f, temp: PathBuf::new(), stop: watch::channel(false).1 };
        assert_eq!(tr.url("depot/Plateau 1", "a b.mp4"), "ftps://ftp.example:990/depot/Plateau%201/a%20b.mp4");
        let s = Target { kind: "sftp".into(), host: "web".into(), path: "videos".into(), ..Default::default() };
        let tr = Transfer { target: &s, temp: PathBuf::new(), stop: watch::channel(false).1 };
        assert_eq!(tr.url("videos", "x.mp4"), "sftp://web:22/~/videos/x.mp4");
    }

    #[test]
    fn checks() {
        let mut t = Target { name: "NAS".into(), kind: "smb".into(), host: "nas".into(), share: "".into(), ..Default::default() };
        assert!(t.validate().is_err(), "a share");
        t.share = "Rushs".into();
        assert!(t.validate().is_ok());
        t.folder = "{source}/../..".into();
        assert!(t.validate().is_err());
        let d = Target { name: "Dossier".into(), kind: "dir".into(), path: "mnt/nas".into(), ..Default::default() };
        assert!(d.validate().is_err(), "a full path");
        // The saved password: for the same server only.
        let old = Target { password: "secret".into(), user: "u".into(), host: "nas".into(), ..t.clone() };
        let mut same = Target { password: MASKED.into(), ..old.clone() };
        assert!(keep_password(&mut same, Some(&old)).is_ok() && same.password == "secret");
        let mut other = Target { password: MASKED.into(), host: "evil".into(), ..old.clone() };
        assert!(keep_password(&mut other, Some(&old)).is_err());
    }

    #[test]
    fn names_decoded() {
        assert_eq!(decode("Plateau%201%2F%C3%A9"), "Plateau 1/é");
        assert_eq!(decode("50%"), "50%");
    }

    #[test]
    fn meters() {
        let said = b"  % Total    % Received % Xferd  Average Speed   Time    Time     Time  Current\r 45  100M    0     0   45  45.0M      0  38.2M  0:00:02  0:00:01  0:00:01 38.2M";
        let (done, speed) = meter(said, 100_000_000).unwrap();
        assert_eq!(done, 45_000_000);
        assert_eq!(speed, (38.2 * 1024.0 * 1024.0) as u64);
    }
}
