//! Health: on-disk journal (30 days), crash reports (90 days: panics, and stops the server did not
//! make itself), the Health page's summary and the support export (spec § 5.11).
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    api::{blocking, internal, require, ApiResult},
    auth::CurrentUser,
    logs::Entry,
    state::Shared,
};

const JOURNAL_DAYS: u64 = 30;
const CRASH_DAYS: u64 = 90;
const VERSION: &str = env!("CARGO_PKG_VERSION");

struct Health {
    dir: PathBuf,
    started_at: i64,
    /// One writer at a time.
    write: Mutex<()>,
}

static HEALTH: OnceLock<Health> = OnceLock::new();
/// Set by `stopped()`: the heartbeat no longer writes the marker.
static STOPPED: AtomicBool = AtomicBool::new(false);

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn ms_of(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn today() -> String {
    jiff::Zoned::now().date().to_string()
}

/// One line of the journal.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub at: i64,
    pub level: String,
    #[serde(default)]
    pub target: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fields: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Crash {
    pub id: String,
    pub at: i64,
    /// "panic" | "stop"
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub version: String,
}

fn append(h: &Health, e: &JournalEntry) {
    let _g = h.write.lock().unwrap_or_else(|p| p.into_inner());
    let path = h.dir.join(format!("journal-{}.jsonl", today()));
    if let (Ok(mut f), Ok(line)) = (std::fs::OpenOptions::new().create(true).append(true).open(path), serde_json::to_string(e)) {
        let _ = writeln!(f, "{line}");
    }
}

/// Called by the log layer for each warning and error (never logs itself).
pub fn record_log(e: &Entry) {
    if let Some(h) = HEALTH.get() {
        append(h, &JournalEntry { at: e.at, level: e.level.to_string(), target: e.target.clone(), message: e.message.clone(), fields: e.fields.clone() });
    }
}

fn write_crash(h: &Health, c: &Crash) {
    let _ = std::fs::create_dir_all(h.dir.join("crashes"));
    let _ = crate::paths::write_json(&h.dir.join("crashes").join(format!("{}.json", c.id)), c, false);
    append(h, &JournalEntry { at: c.at, level: "CRASH".into(), target: c.kind.clone(), message: c.message.clone(), fields: String::new() });
}

fn prune(dir: &Path, days: u64) {
    let limit = SystemTime::now() - Duration::from_secs(days * 86400);
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.metadata().and_then(|m| m.modified()).is_ok_and(|m| m < limit) && e.path().is_file() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Running {
    pid: u32,
    version: String,
    started_at: i64,
}

/// Starts the on-disk journal and crash reports.
pub fn start(data_dir: &Path) {
    let dir = data_dir.join("health");
    let _ = std::fs::create_dir_all(dir.join("crashes"));
    prune(&dir, JOURNAL_DAYS);
    prune(&dir.join("crashes"), CRASH_DAYS);
    let h = HEALTH.get_or_init(|| Health { dir: dir.clone(), started_at: now_ms(), write: Mutex::new(()) });
    // A marker left by the previous run: it stopped without saying so.
    let marker = dir.join("running.json");
    if let Some(prev) = crate::paths::read_json::<Running>(&marker) {
        let at = std::fs::metadata(&marker).and_then(|m| m.modified()).map(ms_of).unwrap_or_else(|_| now_ms());
        write_crash(h, &Crash {
            id: format!("{at}-stop"),
            at,
            kind: "stop".into(),
            message: "the server stopped unexpectedly".into(),
            detail: format!("process {} (version {}) started at {} did not stop by itself: killed, out of memory, power cut or crash", prev.pid, prev.version, jiff::Timestamp::from_millisecond(prev.started_at).map(|t| t.to_string()).unwrap_or_default()),
            version: prev.version,
        });
    }
    let running = Running { pid: std::process::id(), version: VERSION.into(), started_at: h.started_at };
    let _ = crate::paths::write_json(&marker, &running, false);
    append(h, &JournalEntry { at: now_ms(), level: "INFO".into(), target: "start".into(), message: format!("{} {VERSION} started", crate::config::BRAND), fields: String::new() });
    // Heartbeat: the marker's date tells when an unexpected stop happened.
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        let Some(h) = HEALTH.get() else { return };
        let _g = h.write.lock().unwrap_or_else(|p| p.into_inner());
        if STOPPED.load(Ordering::SeqCst) {
            return;
        }
        let _ = crate::paths::write_json(&marker, &running, false);
    });
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(h) = HEALTH.get() {
            let message = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into());
            let at = now_ms();
            let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
            let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
            write_crash(h, &Crash {
                id: format!("{at}-panic"),
                at,
                kind: "panic".into(),
                message: format!("panic: {message}"),
                detail: format!("thread '{thread}' at {location}\n{}", std::backtrace::Backtrace::force_capture()),
                version: VERSION.into(),
            });
        }
        previous(info);
    }));
}

/// Marks a clean stop (not counted as a crash).
pub fn stopped() {
    if let Some(h) = HEALTH.get() {
        {
            let _g = h.write.lock().unwrap_or_else(|p| p.into_inner());
            STOPPED.store(true, Ordering::SeqCst);
            let _ = std::fs::remove_file(h.dir.join("running.json"));
        }
        append(h, &JournalEntry { at: now_ms(), level: "INFO".into(), target: "stop".into(), message: format!("{} stopped", crate::config::BRAND), fields: String::new() });
    }
}

fn journal(dir: &Path) -> Vec<JournalEntry> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("journal-") && n.ends_with(".jsonl"))).collect())
        .unwrap_or_default();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        if let Ok(file) = std::fs::File::open(&f) {
            out.extend(std::io::BufReader::new(file).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str::<JournalEntry>(&l).ok()));
        }
    }
    out
}

fn crashes(dir: &Path) -> Vec<Crash> {
    let mut out: Vec<Crash> = std::fs::read_dir(dir.join("crashes"))
        .map(|rd| rd.flatten().filter_map(|e| crate::paths::read_json::<Crash>(&e.path())).collect())
        .unwrap_or_default();
    out.sort_by_key(|c| c.at);
    out
}

#[derive(Serialize, Deserialize, Default)]
struct Seen {
    at: i64,
}

/// "strata_cut::recorder::ffmpeg" → "recorder"; other crates: their name.
fn category(target: &str) -> String {
    let mut parts = target.split("::");
    let first = parts.next().unwrap_or("");
    if first == "strata_cut" { parts.next().unwrap_or("server").to_string() } else { first.to_string() }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    kind: &'static str,
    level: String,
    cat: String,
    title: String,
    detail: String,
    first: i64,
    last: i64,
    count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    crash: Option<String>,
}

/// The same message grouped with its count; crashes one by one; newest first.
pub fn incidents(journal: &[JournalEntry], crashes: &[Crash]) -> Vec<Incident> {
    let mut map: HashMap<(String, String, String), Incident> = HashMap::new();
    for e in journal.iter().filter(|e| e.level == "ERROR" || e.level == "WARN") {
        let key = (e.level.clone(), e.target.clone(), e.message.clone());
        let i = map.entry(key).or_insert_with(|| Incident {
            kind: "log",
            level: e.level.clone(),
            cat: category(&e.target),
            title: e.message.lines().next().unwrap_or("").chars().take(300).collect(),
            detail: String::new(),
            first: e.at,
            last: e.at,
            count: 0,
            crash: None,
        });
        i.count += 1;
        i.first = i.first.min(e.at);
        if e.at >= i.last {
            i.last = e.at;
            i.detail = [e.message.as_str(), e.fields.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("\n");
        }
    }
    let mut out: Vec<Incident> = map.into_values().collect();
    out.extend(crashes.iter().map(|c| Incident { kind: "crash", level: "CRASH".into(), cat: c.kind.clone(), title: c.message.clone(), detail: c.detail.clone(), first: c.at, last: c.at, count: 1, crash: Some(c.id.clone()) }));
    out.sort_by_key(|i| std::cmp::Reverse(i.last));
    out.truncate(500);
    out
}

/// Total and free bytes of the disk holding `path`.
pub fn disk(path: &Path) -> (u64, u64) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return (0, 0) };
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: c is a valid C string, st a valid out pointer.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return (0, 0);
    }
    let frsize = st.f_frsize as u64;
    (st.f_blocks as u64 * frsize, st.f_bavail as u64 * frsize)
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/system/health", get(summary))
        .route("/api/system/health/journal", get(get_journal))
        .route("/api/system/health/seen", post(seen))
        .route("/api/system/health/clear", post(clear))
        .route("/api/system/health/export.zip", get(export))
}

fn health_dir(s: &Shared) -> PathBuf {
    s.data_dir.join("health")
}

async fn summary(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let dir = health_dir(&s);
    let data = s.data_dir.clone();
    let started_at = HEALTH.get().map(|h| h.started_at).unwrap_or_else(|| now_ms() - s.started_at.elapsed().as_millis() as i64);
    let uptime = s.started_at.elapsed().as_secs();
    let v = blocking(move || {
        let j = journal(&dir);
        let c = crashes(&dir);
        let seen = crate::paths::read_json::<Seen>(&dir.join("seen.json")).unwrap_or_default().at;
        let hour_ago = now_ms() - 3_600_000;
        let last_crash = c.last().map(|c| c.at);
        let (total, free) = disk(&data);
        json!({
            "version": VERSION,
            "startedAt": started_at,
            "uptimeSecs": uptime,
            "crashes": c.len(),
            "lastCrash": last_crash,
            "unseenCrash": last_crash.is_some_and(|at| at > seen),
            "errors": j.iter().filter(|e| e.level == "ERROR").count(),
            "warnings": j.iter().filter(|e| e.level == "WARN").count(),
            "lastHourErrors": j.iter().filter(|e| e.level == "ERROR" && e.at >= hour_ago).count(),
            "disk": {"total": total, "free": free},
            "incidents": incidents(&j, &c),
        })
    })
    .await?;
    Ok(Json(v))
}

async fn get_journal(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Vec<JournalEntry>>> {
    require(me.is_admin())?;
    let dir = health_dir(&s);
    let mut j = blocking(move || journal(&dir)).await?;
    let skip = j.len().saturating_sub(5000);
    j.drain(..skip);
    Ok(Json(j))
}

async fn seen(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    let path = health_dir(&s).join("seen.json");
    blocking(move || crate::paths::write_json(&path, &Seen { at: now_ms() }, false)).await?.map_err(internal)?;
    s.changed("health");
    Ok(StatusCode::NO_CONTENT)
}

/// Empties the journal (incidents), the crash reports and the log in memory.
async fn clear(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    let dir = health_dir(&s);
    blocking(move || {
        let _g = HEALTH.get().map(|h| h.write.lock().unwrap_or_else(|p| p.into_inner()));
        for sub in [dir.clone(), dir.join("crashes")] {
            for e in std::fs::read_dir(&sub).into_iter().flatten().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let doomed = (name.starts_with("journal-") && name.ends_with(".jsonl")) || (sub.ends_with("crashes") && name.ends_with(".json"));
                if doomed {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        crate::paths::write_json(&dir.join("seen.json"), &Seen { at: now_ms() }, false)
    })
    .await?
    .map_err(internal)?;
    crate::logs::clear();
    tracing::info!("health journal emptied by {}", me.username);
    s.changed("health");
    Ok(StatusCode::NO_CONTENT)
}

/// Everything for the support: journal, crash reports, log, system, masked settings, update history.
async fn export(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<impl IntoResponse> {
    require(me.is_admin())?;
    let data = s.data_dir.clone();
    let system = crate::system::info(&s, true);
    let log_file = crate::logs::file(&s);
    let bytes = blocking(move || -> Result<Vec<u8>, String> {
        use zip::write::SimpleFileOptions;
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut add = |name: &str, data: &[u8]| -> Result<(), String> {
            z.start_file(name, opt).map_err(|e| e.to_string())?;
            z.write_all(data).map_err(|e| e.to_string())
        };
        add("system.json", &serde_json::to_vec_pretty(&system).unwrap_or_default())?;
        add("stratacut.log", crate::logs::text().as_bytes())?;
        let dir = data.join("health");
        for sub in [dir.clone(), dir.join("crashes")] {
            if let Ok(rd) = std::fs::read_dir(&sub) {
                for e in rd.flatten().filter(|e| e.path().is_file()) {
                    let rel = e.path().strip_prefix(&data).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    if let Ok(d) = std::fs::read(e.path()) {
                        add(&rel, &d)?;
                    }
                }
            }
        }
        if let Ok(d) = std::fs::read(data.join("update").join("history.json")) {
            add("update/history.json", &d)?;
        }
        if let Some(f) = log_file {
            if let Ok(d) = std::fs::read(&f) {
                let tail = &d[d.len().saturating_sub(32 << 20)..];
                add(&format!("logs/{}.log", crate::BIN), tail)?;
            }
        }
        Ok(z.finish().map_err(|e| e.to_string())?.into_inner())
    })
    .await?
    .map_err(internal)?;
    let name = format!("attachment; filename=\"stratacut-health-{}.zip\"", today());
    Ok(([(header::CONTENT_TYPE, "application/zip".to_string()), (header::CONTENT_DISPOSITION, name)], bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn je(at: i64, level: &str, msg: &str) -> JournalEntry {
        JournalEntry { at, level: level.into(), target: "strata_cut::recorder".into(), message: msg.into(), fields: String::new() }
    }

    #[test]
    fn grouping() {
        let j = vec![je(1, "WARN", "no signal"), je(5, "WARN", "no signal"), je(3, "ERROR", "ffmpeg exited"), je(4, "INFO", "started")];
        let c = vec![Crash { id: "9-panic".into(), at: 9, kind: "panic".into(), message: "panic: boom".into(), detail: String::new(), version: "0.1.0".into() }];
        let i = incidents(&j, &c);
        assert_eq!(i.len(), 3);
        assert_eq!(i[0].kind, "crash");
        assert_eq!(i[1].title, "no signal");
        assert_eq!((i[1].count, i[1].first, i[1].last), (2, 1, 5));
        assert_eq!(i[1].cat, "recorder");
        assert_eq!(category("hyper::proto"), "hyper");
    }

    #[test]
    fn disk_space() {
        let (total, free) = disk(Path::new("/"));
        assert!(total > 0 && free <= total);
    }
}
