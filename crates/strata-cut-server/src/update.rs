//! Updates from GitHub releases (spec § 5.6). Never installs on its own: checking only informs.
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::{
    cmp::Ordering,
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime},
};

use crate::{
    api::{bad, blocking, require, ApiResult},
    auth::CurrentUser,
    state::Shared,
};

pub const DEFAULT_REPO: &str = "DamienP2/Strata-Cut";
pub const CURRENT: &str = crate::VERSION;
const MAX_PACKAGE: u64 = 512 << 20;
const BUSY: &str = "an update is in progress";

pub fn platform() -> &'static str {
    if cfg!(target_arch = "aarch64") { "linux-arm64" } else { "linux-x86_64" }
}

/// `Strata-Cut-<version>-<platform>.tar.gz`
pub fn package_name(version: &str) -> String {
    format!("Strata-Cut-{version}-{}.tar.gz", platform())
}

/// An HTTP client with the system's certificates (corporate proxies), no automatic redirect
/// unless asked, HTTP errors returned as answers.
pub fn http_agent(redirects: u32, timeout: Duration) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(timeout))
        .max_redirects(redirects)
        .http_status_as_error(false)
        .user_agent(format!("stratacut/{CURRENT}"))
        .build()
        .into()
}

use crate::util::now_ms;

/// Numeric comparison: "1.2.10" > "1.2.9"; a leading `v` is ignored.
pub fn cmp_versions(a: &str, b: &str) -> Ordering {
    let parse = |s: &str| -> Vec<u64> { s.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']).map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parse(a), parse(b));
    for i in 0..a.len().max(b.len()) {
        match a.get(i).unwrap_or(&0).cmp(b.get(i).unwrap_or(&0)) {
            Ordering::Equal => continue,
            o => return o,
        }
    }
    Ordering::Equal
}

/// `owner/name` or a github.com URL → `owner/name`.
pub fn parse_repo(s: &str) -> Option<String> {
    let mut r = s.trim();
    for p in ["https://", "http://"] {
        r = r.strip_prefix(p).unwrap_or(r);
    }
    r = r.strip_prefix("www.").unwrap_or(r);
    r = r.strip_prefix("github.com/").unwrap_or(r);
    let r = r.trim_end_matches('/');
    let r = r.strip_suffix(".git").unwrap_or(r);
    let parts: Vec<&str> = r.split('/').collect();
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    (parts.len() == 2 && parts.iter().all(|p| ok(p))).then(|| r.to_string())
}

// ---------------------------------------------------------------- settings and files

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Settings {
    #[serde(default = "default_repo")]
    repo: String,
    /// Read-only token for a private repository; never sent back.
    #[serde(default)]
    token: String,
    #[serde(default = "yes")]
    auto_check: bool,
}

fn default_repo() -> String {
    DEFAULT_REPO.into()
}
fn yes() -> bool {
    true
}

fn dir(data: &Path) -> PathBuf {
    data.join("update")
}

fn settings(data: &Path) -> Settings {
    crate::paths::read_json(&dir(data).join("settings.json")).unwrap_or_else(|| serde_json::from_str("{}").unwrap())
}

#[derive(Serialize, Deserialize)]
struct Pending {
    from: String,
    to: String,
    starts: u32,
}

#[derive(Serialize, Deserialize, Clone)]
struct HistoryEntry {
    from: String,
    to: String,
    at: i64,
    outcome: String,
}

fn history(data: &Path) -> Vec<HistoryEntry> {
    crate::paths::read_json(&dir(data).join("history.json")).unwrap_or_default()
}

fn add_history(data: &Path, from: &str, to: &str, outcome: &str) {
    let mut h = history(data);
    h.push(HistoryEntry { from: from.into(), to: to.into(), at: now_ms(), outcome: outcome.into() });
    let skip = h.len().saturating_sub(50);
    let _ = crate::paths::write_json(&dir(data).join("history.json"), &h[skip..], false);
}

/// The running program, as it was at start (an update renames it).
fn exe() -> &'static PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| std::env::current_exe().and_then(|p| p.canonicalize()).unwrap_or_else(|_| PathBuf::from(crate::BIN)))
}

fn sibling(suffix: &str) -> PathBuf {
    let e = exe();
    let name = e.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| crate::BIN.into());
    e.with_file_name(format!("{name}{suffix}"))
}

/// `<bin> --version` → "x.y.z".
fn program_version(path: &Path) -> Option<String> {
    let out = std::process::Command::new(path).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v = text.trim().strip_prefix(crate::BIN)?.trim();
    (out.status.success() && !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))).then(|| v.to_string())
}

/// The version kept as `<bin>.previous` (cached while the file does not change).
fn previous_version() -> Option<String> {
    type Stamp = (SystemTime, u64);
    static CACHE: Mutex<Option<(Stamp, Option<String>)>> = Mutex::new(None);
    let path = sibling(".previous");
    let m = std::fs::metadata(&path).ok()?;
    let key = (m.modified().ok()?, m.len());
    let mut c = CACHE.lock().unwrap();
    if let Some((k, v)) = c.as_ref() {
        if *k == key {
            return v.clone();
        }
    }
    let v = program_version(&path);
    *c = Some((key, v.clone()));
    v
}

// ---------------------------------------------------------------- state

#[derive(Serialize, Clone, Copy, PartialEq, Default, Debug)]
#[serde(rename_all = "lowercase")]
enum Phase {
    #[default]
    Idle,
    Checking,
    Downloading,
    Installing,
    Restarting,
    Error,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: String,
    pub tag: String,
    pub name: String,
    pub date: String,
    pub notes: String,
    pub size: u64,
    pub asset_id: u64,
    pub asset_name: String,
    #[serde(skip)]
    pub digest: Option<String>,
    /// Downloaded from github.com directly (no API: its hourly limit was reached).
    #[serde(skip)]
    pub direct: Option<String>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Status {
    phase: Phase,
    progress: f64,
    message: String,
    latest: Option<Release>,
    releases: Vec<Release>,
    last_check: Option<i64>,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

fn status() -> Status {
    STATUS.lock().unwrap().clone().unwrap_or_default()
}

fn set_status(f: impl FnOnce(&mut Status)) {
    let mut s = STATUS.lock().unwrap();
    f(s.get_or_insert_with(Status::default));
}

fn busy() -> bool {
    matches!(status().phase, Phase::Checking | Phase::Downloading | Phase::Installing | Phase::Restarting)
}

/// Takes the work if nothing else is running.
fn begin(phase: Phase, message: &str) -> bool {
    let mut s = STATUS.lock().unwrap();
    let s = s.get_or_insert_with(Status::default);
    if matches!(s.phase, Phase::Checking | Phase::Downloading | Phase::Installing | Phase::Restarting) {
        return false;
    }
    s.phase = phase;
    s.progress = 0.0;
    s.message = message.into();
    true
}

fn failed(e: &str) {
    tracing::error!("update: {e}");
    set_status(|s| {
        s.phase = Phase::Error;
        s.message = e.into();
    });
}

// ---------------------------------------------------------------- check

/// Releases that are not drafts nor pre-releases, whose package for this platform is fully uploaded; newest first.
pub fn parse_releases(list: &Value, platform: &str) -> Vec<Release> {
    let suffix = format!("-{platform}.tar.gz");
    let mut out: Vec<Release> = list
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false) && !r["prerelease"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            let asset = r["assets"].as_array()?.iter().find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(&suffix)) && a["state"].as_str() == Some("uploaded"))?;
            Some(Release {
                version: tag.trim_start_matches(['v', 'V']).to_string(),
                tag: tag.to_string(),
                name: r["name"].as_str().unwrap_or(tag).to_string(),
                date: r["published_at"].as_str().unwrap_or("").to_string(),
                notes: r["body"].as_str().unwrap_or("").to_string(),
                size: asset["size"].as_u64().unwrap_or(0),
                asset_id: asset["id"].as_u64()?,
                asset_name: asset["name"].as_str().unwrap_or("").to_string(),
                digest: asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(|d| d.to_lowercase()),
                direct: None,
            })
        })
        .collect();
    out.sort_by(|a, b| cmp_versions(&b.version, &a.version));
    out
}

fn github_error(status: u16, repo: &str, has_token: bool) -> String {
    match status {
        401 => "GitHub refused the token (expired or revoked?)".into(),
        403 => format!("GitHub refused the request (rate limit, or the token cannot read {repo})"),
        404 if !has_token => format!("{repo} not found: the repository is private, add a read-only token"),
        404 => format!("{repo} not found"),
        n => format!("GitHub answered HTTP {n}"),
    }
}

fn fetch_releases(data: &Path) -> Result<Vec<Release>, String> {
    let st = settings(data);
    let url = format!("https://api.github.com/repos/{}/releases?per_page=50", st.repo);
    let mut req = http_agent(0, Duration::from_secs(600)).get(&url).header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
    if !st.token.is_empty() {
        req = req.header("Authorization", &format!("Bearer {}", st.token));
    }
    let mut res = req.call().map_err(|e| format!("cannot reach GitHub: {e}"))?;
    if !res.status().is_success() {
        let code = res.status().as_u16();
        // Without a token the API allows 60 requests an hour per public address (often
        // shared by a whole office): the release pages of github.com tell the newest one too.
        if st.token.is_empty() && (code == 403 || code == 429) {
            if let Ok(r) = latest_from_web(&st.repo) {
                tracing::info!("update: GitHub API limit reached, the newest release read from github.com");
                return Ok(vec![r]);
            }
        }
        return Err(github_error(code, &st.repo, !st.token.is_empty()));
    }
    let text = res.body_mut().with_config().limit(32 << 20).read_to_string().map_err(|e| format!("GitHub: {e}"))?;
    let list: Value = serde_json::from_str(&text).map_err(|e| format!("GitHub: {e}"))?;
    Ok(parse_releases(&list, platform()))
}

/// The newest release from github.com's page (its redirect names the tag); the package's
/// address follows the release workflow's naming.
fn latest_from_web(repo: &str) -> Result<Release, String> {
    let url = format!("https://github.com/{repo}/releases/latest");
    let res = http_agent(0, Duration::from_secs(30)).get(&url).call().map_err(|e| format!("cannot reach GitHub: {e}"))?;
    let to = res.headers().get("location").and_then(|v| v.to_str().ok()).ok_or("no release")?;
    let tag = to.rsplit("/tag/").next().filter(|t| !t.is_empty() && *t != to).ok_or("no release")?.to_string();
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    let asset_name = format!("Strata-Cut-{version}-{}.tar.gz", platform());
    Ok(Release {
        name: tag.clone(),
        direct: Some(format!("https://github.com/{repo}/releases/download/{tag}/{asset_name}")),
        version,
        tag,
        date: String::new(),
        notes: String::new(),
        size: 0,
        asset_id: 0,
        asset_name,
        digest: None,
    })
}

/// Checks now: the newest release, or None.
fn check(data: &Path) -> Result<Option<Release>, String> {
    if !begin(Phase::Checking, "") {
        return Err(BUSY.into());
    }
    match fetch_releases(data) {
        Ok(rel) => {
            let latest = rel.first().cloned();
            if let Some(l) = &latest {
                if cmp_versions(&l.version, CURRENT) == Ordering::Greater {
                    tracing::info!("update: version {} is available", l.version);
                }
            }
            set_status(|s| {
                s.phase = Phase::Idle;
                s.message.clear();
                s.latest = latest.clone();
                s.releases = rel;
                s.last_check = Some(now_ms());
            });
            Ok(latest)
        }
        Err(e) => {
            failed(&e);
            Err(e)
        }
    }
}

// ---------------------------------------------------------------- install

fn download(data: &Path, r: &Release) -> Result<Vec<u8>, String> {
    let st = settings(data);
    let url = format!("https://api.github.com/repos/{}/releases/assets/{}", st.repo, r.asset_id);
    let mut req = match &r.direct {
        Some(u) => http_agent(0, Duration::from_secs(600)).get(u).header("Accept", "application/octet-stream"),
        None => http_agent(0, Duration::from_secs(600)).get(&url).header("Accept", "application/octet-stream"),
    };
    if !st.token.is_empty() && r.direct.is_none() {
        req = req.header("Authorization", &format!("Bearer {}", st.token));
    }
    let mut res = req.call().map_err(|e| format!("cannot reach GitHub: {e}"))?;
    if res.status().is_redirection() {
        // the storage: followed without the token
        let to = res.headers().get("location").and_then(|v| v.to_str().ok()).ok_or("GitHub sent a redirection without a location")?.to_string();
        res = http_agent(5, Duration::from_secs(600)).get(&to).header("Accept", "application/octet-stream").call().map_err(|e| format!("download: {e}"))?;
    }
    if !res.status().is_success() {
        return Err(github_error(res.status().as_u16(), &st.repo, !st.token.is_empty()));
    }
    let total = res.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(r.size).max(1);
    if total > MAX_PACKAGE {
        return Err("the package is too large".into());
    }
    let mut reader = res.into_body().into_with_config().limit(MAX_PACKAGE).reader();
    let mut out = Vec::with_capacity(total as usize);
    let mut buf = vec![0u8; 256 << 10];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download: {e}"))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        let p = (out.len() as f64 / total as f64).min(1.0);
        set_status(|s| s.progress = p);
    }
    if let Some(d) = &r.digest {
        let got = hex(&sha2::Sha256::digest(&out));
        if &got != d {
            return Err("the downloaded package is damaged (SHA-256 differs)".into());
        }
    }
    Ok(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The server program in a package: `.tar.gz` holding `…/strata-cut`, or a bare ELF program.
pub fn extract_program(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.starts_with(b"\x7fELF") {
        return Ok(bytes.to_vec());
    }
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in ar.entries().map_err(|e| format!("bad package: {e}"))? {
            let entry = entry.map_err(|e| format!("bad package: {e}"))?;
            let is_bin = entry.header().entry_type().is_file() && entry.path().ok().is_some_and(|p| p.file_name().is_some_and(|n| n == crate::BIN));
            if is_bin {
                let mut out = Vec::new();
                entry.take(MAX_PACKAGE).read_to_end(&mut out).map_err(|e| format!("bad package: {e}"))?;
                return Ok(out);
            }
        }
        return Err(format!("the package has no {} program", crate::BIN));
    }
    Err("this file is not a Strata LiveCut package".into())
}

fn io_error(e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        "no permission to replace the server program: run the installer again".into()
    } else {
        format!("cannot replace the server program: {e}")
    }
}

/// Puts a package in place of the running program: `<bin>.new`, version check, `<bin>.previous`, rename.
fn install_bytes(data: &Path, bytes: &[u8], allow_older: bool) -> Result<String, String> {
    let program = extract_program(bytes)?;
    let new = sibling(".new");
    let _ = std::fs::remove_file(&new);
    std::fs::write(&new, &program).map_err(io_error)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).map_err(io_error)?;
    }
    let Some(version) = program_version(&new) else {
        let _ = std::fs::remove_file(&new);
        return Err("this program is not a Strata LiveCut server for this machine".into());
    };
    if !allow_older && cmp_versions(&version, CURRENT) != Ordering::Greater {
        let _ = std::fs::remove_file(&new);
        return Err(format!("version {version} is not newer than {CURRENT}"));
    }
    let prev = sibling(".previous");
    std::fs::rename(exe(), &prev).map_err(io_error)?;
    if let Err(e) = std::fs::rename(&new, exe()) {
        let _ = std::fs::rename(&prev, exe());
        return Err(io_error(e));
    }
    let _ = crate::paths::write_json(&dir(data).join("pending.json"), &Pending { from: CURRENT.into(), to: version.clone(), starts: 0 }, false);
    add_history(data, CURRENT, &version, "installed");
    tracing::info!("update: version {version} installed, restarting");
    Ok(version)
}

/// The process starts again once it has stopped cleanly (see `restart_soon`).
pub static RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Kills what this process started and still runs (the recorders' FFmpeg). Left behind, they
/// stay in the service's group: one of them (the preview, waiting for a stream that no longer
/// comes) does not end on the service manager's signal, which then waits 90 s before it starts
/// the server again: a hole of a minute and a half in every recording, at each update.
pub fn kill_children() {
    let me = std::process::id();
    let Ok(dir) = std::fs::read_dir("/proc") else { return };
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
        let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
        // "pid (name) state ppid …": the name may hold spaces and parentheses.
        let ppid = stat.rsplit_once(") ").and_then(|(_, rest)| rest.split_whitespace().nth(1)).and_then(|p| p.parse::<u32>().ok());
        if ppid == Some(me) {
            // SAFETY: a signal sent to a process this one started.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

/// Restarts the process: supervised → exit(75) (the service manager starts it again);
/// in the foreground → starts the same program with the same arguments, then exit(0).
pub fn restart_process() -> ! {
    crate::health::stopped();
    relaunch()
}

/// The same, once the server has stopped by itself (its stop already written).
pub fn relaunch() -> ! {
    kill_children();
    if crate::supervised() {
        std::process::exit(75);
    }
    match std::process::Command::new(exe()).args(std::env::args_os().skip(1)).spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => {
            tracing::error!("cannot start {}: {e}", exe().display());
            std::process::exit(75)
        }
    }
}

/// Ends now (what it started killed first): started again if an update asked for it.
pub fn relaunch_or_exit() -> ! {
    if RESTART.load(std::sync::atomic::Ordering::SeqCst) {
        relaunch();
    }
    kill_children();
    std::process::exit(0)
}

fn restart_soon() {
    set_status(|s| {
        s.phase = Phase::Restarting;
        s.progress = 1.0;
        s.message.clear();
    });
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(800));
        // Stopped as the system would: the recorders close their files and their FFmpeg end,
        // then the process starts again (`main`). Should that hang: by force after 20 s.
        RESTART.store(true, std::sync::atomic::Ordering::SeqCst);
        // SAFETY: a signal sent to this very process.
        unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
        std::thread::sleep(Duration::from_secs(20));
        tracing::warn!("update: the server did not stop by itself in 20 s: restarted by force");
        restart_process();
    });
}

// ---------------------------------------------------------------- start-up

/// If the new version keeps failing to start (more than 3 starts without 60 s of running),
/// puts `<bin>.previous` back and restarts.
pub fn startup_guard(data_dir: &Path) {
    let _ = exe();
    let path = dir(data_dir).join("pending.json");
    let Some(mut p) = crate::paths::read_json::<Pending>(&path) else { return };
    if p.to != CURRENT {
        let _ = std::fs::remove_file(&path);
        return;
    }
    p.starts += 1;
    let _ = crate::paths::write_json(&path, &p, false);
    if p.starts <= 3 {
        return;
    }
    let prev = sibling(".previous");
    if !prev.exists() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    tracing::error!("update: version {} failed to start {} times, putting {} back", p.to, p.starts - 1, p.from);
    match std::fs::rename(&prev, exe()) {
        Ok(()) => {
            add_history(data_dir, &p.to, &p.from, "restored automatically");
            let _ = std::fs::remove_file(&path);
            restart_process();
        }
        Err(e) => tracing::error!("update: cannot restore the previous version: {e}"),
    }
}

/// After running this long, the new version is confirmed (`pending.json` removed).
pub fn confirm_after(data_dir: PathBuf, after: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        let path = dir(&data_dir).join("pending.json");
        if crate::paths::read_json::<Pending>(&path).is_some_and(|p| p.to == CURRENT) {
            let _ = std::fs::remove_file(&path);
            tracing::info!("update: version {CURRENT} confirmed");
        }
    });
}

/// 30 s after start, then every 6 h, when `autoCheck` is on and nothing is running.
pub fn spawn_auto_check(data_dir: PathBuf) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            if settings(&data_dir).auto_check && !busy() {
                let d = data_dir.clone();
                let _ = tokio::task::spawn_blocking(move || check(&d)).await;
            }
            tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
        }
    });
}

// ---------------------------------------------------------------- routes

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/system/update", get(info))
        .route("/api/system/update/settings", put(put_settings))
        .route("/api/system/update/check", post(check_now))
        .route("/api/system/update/install", post(install))
        .route("/api/system/update/upload", post(upload).layer(DefaultBodyLimit::max(MAX_PACKAGE as usize)))
        .route("/api/system/update/rollback", post(rollback))
}

async fn info(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let data = s.data_dir.clone();
    let (st, hist, previous) = blocking(move || (settings(&data), history(&data), previous_version())).await?;
    let status = status();
    let pkg_version = status.latest.as_ref().map(|l| l.version.clone()).unwrap_or_else(|| CURRENT.into());
    Ok(Json(json!({
        "current": CURRENT,
        "platform": platform(),
        "package": package_name(&pkg_version),
        "repo": st.repo,
        "hasToken": !st.token.is_empty(),
        "autoCheck": st.auto_check,
        "previous": previous,
        "history": hist,
        "status": status,
        "supervised": crate::supervised(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsBody {
    repo: Option<String>,
    /// null = keep, "" = remove.
    #[serde(default)]
    token: Option<String>,
    auto_check: Option<bool>,
}

async fn put_settings(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<SettingsBody>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    let data = s.data_dir.clone();
    blocking(move || {
        let mut st = settings(&data);
        if let Some(r) = b.repo {
            st.repo = parse_repo(&r).ok_or_else(|| bad("the repository is written owner/name"))?;
        }
        if let Some(t) = b.token {
            st.token = t.trim().to_string();
        }
        if let Some(a) = b.auto_check {
            st.auto_check = a;
        }
        crate::paths::write_json(&dir(&data).join("settings.json"), &st, true).map_err(crate::api::internal)
    })
    .await??;
    Ok(StatusCode::NO_CONTENT)
}

async fn check_now(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Option<Release>>> {
    require(me.is_admin())?;
    if busy() {
        return Err((StatusCode::CONFLICT, BUSY.into()));
    }
    let data = s.data_dir.clone();
    blocking(move || check(&data)).await?.map(Json).map_err(|e| (StatusCode::BAD_GATEWAY, e))
}

#[derive(Deserialize)]
struct InstallQuery {
    version: Option<String>,
}

async fn install(State(s): State<Shared>, CurrentUser(me): CurrentUser, Query(q): Query<InstallQuery>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    if busy() {
        return Err((StatusCode::CONFLICT, BUSY.into()));
    }
    let data = s.data_dir.clone();
    let d = data.clone();
    let mut releases = status().releases;
    if releases.is_empty() {
        blocking(move || check(&d)).await?.map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        releases = status().releases;
    }
    let (release, chosen) = match q.version.as_deref().map(|v| v.trim().trim_start_matches('v')) {
        Some(v) => (releases.iter().find(|r| r.version == v).cloned().ok_or_else(|| crate::api::not_found("version"))?, true),
        None => (releases.first().cloned().ok_or_else(|| crate::api::not_found("version"))?, false),
    };
    if !chosen && cmp_versions(&release.version, CURRENT) != Ordering::Greater {
        return Err(bad("this version is already installed"));
    }
    if !begin(Phase::Downloading, &format!("downloading {}", release.version)) {
        return Err((StatusCode::CONFLICT, BUSY.into()));
    }
    std::thread::spawn(move || {
        let r = download(&data, &release).and_then(|bytes| {
            set_status(|s| {
                s.phase = Phase::Installing;
                s.message = format!("installing {}", release.version);
            });
            install_bytes(&data, &bytes, true)
        });
        match r {
            Ok(_) => restart_soon(),
            Err(e) => failed(&e),
        }
    });
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadQuery {
    #[serde(default)]
    allow_older: bool,
}

async fn upload(State(s): State<Shared>, CurrentUser(me): CurrentUser, Query(q): Query<UploadQuery>, body: Bytes) -> ApiResult<impl IntoResponse> {
    require(me.is_admin())?;
    if !begin(Phase::Installing, "installing the package") {
        return Err((StatusCode::CONFLICT, BUSY.into()));
    }
    let data = s.data_dir.clone();
    match blocking(move || install_bytes(&data, &body, q.allow_older)).await? {
        Ok(version) => {
            restart_soon();
            Ok(Json(json!({ "version": version })))
        }
        Err(e) => {
            set_status(|s| {
                s.phase = Phase::Idle;
                s.message.clear();
            });
            Err(bad(e))
        }
    }
}

async fn rollback(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    if !begin(Phase::Installing, "going back") {
        return Err((StatusCode::CONFLICT, BUSY.into()));
    }
    let data = s.data_dir.clone();
    let r = blocking(move || -> Result<String, String> {
        let prev = sibling(".previous");
        let version = previous_version().ok_or("there is no previous version")?;
        let tmp = sibling(".rollback");
        std::fs::rename(exe(), &tmp).map_err(io_error)?;
        if let Err(e) = std::fs::rename(&prev, exe()) {
            let _ = std::fs::rename(&tmp, exe());
            return Err(io_error(e));
        }
        let _ = std::fs::rename(&tmp, &prev);
        let _ = std::fs::remove_file(dir(&data).join("pending.json"));
        add_history(&data, CURRENT, &version, "rolled back");
        tracing::info!("update: back to version {version}, restarting");
        Ok(version)
    })
    .await?;
    match r {
        Ok(version) => {
            restart_soon();
            Ok(Json(json!({ "version": version })))
        }
        Err(e) => {
            set_status(|s| {
                s.phase = Phase::Idle;
                s.message.clear();
            });
            Err(bad(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(cmp_versions("1.2.10", "1.2.9"), Ordering::Greater);
        assert_eq!(cmp_versions("1.2.9", "1.2.10"), Ordering::Less);
        assert_eq!(cmp_versions("v1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(cmp_versions("2.0.0", "1.99.99"), Ordering::Greater);
        assert_eq!(cmp_versions("0.1.0", "0.1.0"), Ordering::Equal);
        assert_eq!(cmp_versions("1.10.0", "1.9.0"), Ordering::Greater);
    }

    #[test]
    fn repos() {
        assert_eq!(parse_repo("DamienP2/Strata-Cut").as_deref(), Some("DamienP2/Strata-Cut"));
        assert_eq!(parse_repo(" https://github.com/DamienP2/Strata-Cut.git/ ").as_deref(), Some("DamienP2/Strata-Cut"));
        assert_eq!(parse_repo("github.com/a/b").as_deref(), Some("a/b"));
        assert_eq!(parse_repo("a"), None);
        assert_eq!(parse_repo("a/b/c"), None);
        assert_eq!(parse_repo("a b/c"), None);
    }

    #[test]
    fn releases() {
        let p = platform();
        let list = json!([
            {"tag_name": "v1.2.9", "draft": false, "prerelease": false, "assets": [{"id": 1, "name": format!("Strata-Cut-1.2.9-{p}.tar.gz"), "state": "uploaded", "size": 10, "digest": "sha256:ABCD"}]},
            {"tag_name": "v1.2.10", "draft": false, "prerelease": false, "assets": [{"id": 2, "name": format!("Strata-Cut-1.2.10-{p}.tar.gz"), "state": "uploaded", "size": 10}]},
            {"tag_name": "v1.3.0", "draft": false, "prerelease": true, "assets": [{"id": 3, "name": format!("Strata-Cut-1.3.0-{p}.tar.gz"), "state": "uploaded"}]},
            {"tag_name": "v1.4.0", "draft": true, "prerelease": false, "assets": [{"id": 4, "name": format!("Strata-Cut-1.4.0-{p}.tar.gz"), "state": "uploaded"}]},
            {"tag_name": "v1.5.0", "draft": false, "prerelease": false, "assets": [{"id": 5, "name": format!("Strata-Cut-1.5.0-{p}.tar.gz"), "state": "new"}]},
            {"tag_name": "v1.6.0", "draft": false, "prerelease": false, "assets": [{"id": 6, "name": "Strata-Cut-1.6.0-other.tar.gz", "state": "uploaded"}]}
        ]);
        let r = parse_releases(&list, p);
        assert_eq!(r.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), vec!["1.2.10", "1.2.9"]);
        assert_eq!(r[1].digest.as_deref(), Some("abcd"));
    }

    #[test]
    fn packages() {
        let mut tgz = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut tgz, flate2::Compression::fast());
            let mut t = tar::Builder::new(gz);
            let data = b"\x7fELFprogram";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            t.append_data(&mut h, format!("Strata-Cut-1.0.0-{}/strata-cut", platform()), &data[..]).unwrap();
            t.into_inner().unwrap().finish().unwrap();
        }
        assert_eq!(extract_program(&tgz).unwrap(), b"\x7fELFprogram");
        assert_eq!(extract_program(b"\x7fELFbare").unwrap(), b"\x7fELFbare");
        assert!(extract_program(b"PK\x03\x04").is_err());
        assert_eq!(package_name("1.2.0"), format!("Strata-Cut-1.2.0-{}.tar.gz", platform()));
    }
}
