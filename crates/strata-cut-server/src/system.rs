//! The System page: server facts, settings, hot restart, machine metrics (spec § 2.3, 5.10).
use axum::{extract::State, http::StatusCode, routing::{get, post, put}, Json, Router};
use serde_json::{json, Value};

use crate::{
    api::{bad, blocking, internal, require, ApiResult},
    auth::CurrentUser,
    config::ServerConfig,
    sso::MASK,
    state::Shared,
};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/system", get(get_system))
        .route("/api/system/config", put(put_config))
        .route("/api/system/restart", post(restart))
        .route("/api/system/metrics", get(metrics))
        .route("/api/player/trace", post(player_trace))
}

/// The settings as a client sees them: the SSO secret masked.
pub fn masked(cfg: &ServerConfig) -> ServerConfig {
    let mut c = cfg.clone();
    if !c.security.sso.client_secret.is_empty() {
        c.security.sso.client_secret = MASK.into();
    }
    // The mail server's password: never sent back.
    if !c.alerts.smtp_password.is_empty() {
        c.alerts.smtp_password = MASK.into();
    }
    // A YouTube account's cookies and its token: never sent back.
    if !c.youtube.cookies.is_empty() {
        c.youtube.cookies = MASK.into();
    }
    if !c.youtube.po_token.is_empty() {
        c.youtube.po_token = MASK.into();
    }
    c
}

/// Every network interface but the loopback: `[{name, ip}]`.
pub fn interfaces() -> Vec<Value> {
    let mut list: Vec<(String, String)> = if_addrs::get_if_addrs()
        .map(|l| l.into_iter().filter(|i| !i.is_loopback()).map(|i| (i.name.clone(), i.ip().to_string())).collect())
        .unwrap_or_default();
    list.sort();
    list.into_iter().map(|(name, ip)| json!({"name": name, "ip": ip})).collect()
}

/// `{version, os, arch, hostname, cpus, dataDir, bind, uptimeSecs, timezone, config?, interfaces}`.
pub fn info(s: &Shared, with_config: bool) -> Value {
    let cfg = s.config.load();
    let mut v = json!({
        "version": crate::VERSION,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "hostname": crate::perf::hostname(),
        "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        "dataDir": s.data_dir.display().to_string(),
        "bind": s.bind.to_string(),
        "uptimeSecs": s.started_at.elapsed().as_secs(),
        "timezone": cfg.timezone,
        "interfaces": interfaces(),
    });
    if with_config {
        v["config"] = serde_json::to_value(masked(&cfg)).unwrap_or(Value::Null);
    }
    v
}

/// Public, for supervision and the Updates page waiting for the restart: `{name, version, bind, needsSetup}`.
async fn health(State(s): State<Shared>) -> Json<Value> {
    Json(json!({"name": s.config.load().name, "version": crate::VERSION, "bind": s.bind.to_string(), "needsSetup": s.auth.needs_setup()}))
}

async fn get_system(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    let s2 = s.clone();
    Ok(Json(blocking(move || info(&s2, me.is_admin())).await?))
}

async fn put_config(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(mut cfg): Json<ServerConfig>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    cfg.normalize();
    cfg.validate().map_err(bad)?;
    let old = s.config.load_full();
    if cfg.security.sso.client_secret == MASK {
        cfg.security.sso.client_secret = old.security.sso.client_secret.clone();
    }
    if cfg.alerts.smtp_password == MASK {
        cfg.alerts.smtp_password = old.alerts.smtp_password.clone();
    }
    if cfg.youtube.cookies == MASK {
        cfg.youtube.cookies = old.youtube.cookies.clone();
    }
    if cfg.youtube.po_token == MASK {
        cfg.youtube.po_token = old.youtube.po_token.clone();
    }
    if cfg.bind != old.bind {
        if let Some(addr) = cfg.bind.filter(|a| *a != s.bind) {
            blocking(move || std::net::TcpListener::bind(addr).map(drop)).await?.map_err(|e| bad(format!("cannot listen on {addr}: {e}")))?;
        }
    }
    // The space given to each use must really be there.
    if cfg.loops_max_gb != old.loops_max_gb || cfg.clips_max_gb != old.clips_max_gb || cfg.recordings_dir != old.recordings_dir || cfg.clips_dir != old.clips_dir {
        let (s2, c2) = (s.clone(), cfg.clone());
        blocking(move || crate::storage::check_allocation(&s2, &c2)).await?.map_err(bad)?;
    }
    // A new folder for the loops or the clips: the service must be able to write there.
    if cfg.recordings_dir != old.recordings_dir {
        crate::storage::ensure_writable(&s.data_dir, &cfg.recordings_dir(&s.data_dir)).await.map_err(bad)?;
    }
    if cfg.clips_dir != old.clips_dir {
        crate::storage::ensure_writable(&s.data_dir, &cfg.clips_dir(&s.data_dir)).await.map_err(bad)?;
    }
    let data = s.data_dir.clone();
    let c = cfg.clone();
    blocking(move || c.save(&data)).await?.map_err(internal)?;
    tracing::info!("settings changed by {}", me.username);
    s.config.store(std::sync::Arc::new(cfg));
    s.changed("config");
    s.restart.notify_one();
    Ok(StatusCode::ACCEPTED)
}

async fn restart(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    tracing::info!("restart asked by {}", me.username);
    s.restart.notify_one();
    Ok(StatusCode::ACCEPTED)
}

#[derive(serde::Deserialize)]
struct PlayerTrace {
    #[serde(default)]
    page: String,
    #[serde(default)]
    ua: String,
    #[serde(default)]
    back: f64,
    #[serde(default)]
    events: Vec<String>,
}

/// The browser's player went back during the first minute of a live: what led to it, in the
/// journal (Système › Santé) for the support. A few per user and per 10 minutes.
async fn player_trace(CurrentUser(me): CurrentUser, Json(t): Json<PlayerTrace>) -> ApiResult<StatusCode> {
    static SEEN: std::sync::Mutex<Vec<(String, i64)>> = std::sync::Mutex::new(Vec::new());
    let now = crate::recorder::now_ms();
    {
        let mut seen = SEEN.lock().unwrap();
        seen.retain(|(_, at)| now - at < 600_000);
        if seen.iter().filter(|(u, _)| *u == me.username).count() >= 5 {
            return Ok(StatusCode::NO_CONTENT);
        }
        seen.push((me.username.clone(), now));
    }
    let clip = |x: &str, n: usize| -> String { x.chars().filter(|c| !c.is_control()).take(n).collect() };
    let browser = if t.ua.contains("Firefox/") {
        "Firefox"
    } else if t.ua.contains("Edg/") {
        "Edge"
    } else if t.ua.contains("Chrome/") {
        "Chrome"
    } else if t.ua.contains("Safari/") {
        "Safari"
    } else {
        "other"
    };
    let events: Vec<String> = t.events.iter().take(40).map(|e| clip(e, 200)).collect();
    tracing::warn!("player: the live went back {:.1} s ({}, {browser}, {}): {}", t.back, clip(&t.page, 80), me.username, events.join(" | "));
    tracing::info!("player: browser {}", clip(&t.ua, 200));
    Ok(StatusCode::NO_CONTENT)
}

async fn metrics(State(s): State<Shared>, CurrentUser(_me): CurrentUser) -> ApiResult<Json<Value>> {
    let m = blocking(crate::perf::machine).await?;
    // The load of each resource, in % (the top bar's gauge shows the highest): the processor,
    // the memory, the fullest disk of the loops and the clips, the bandwidth going out against
    // its limit (none without a limit).
    let dirs = [s.recorders.recordings_dir(), s.clips.dir()];
    let disk = blocking(move || {
        dirs.iter()
            .map(|d| crate::health::disk(d))
            .filter(|&(total, _)| total > 0)
            .map(|(total, free)| (total - free.min(total)) as f64 / total as f64 * 100.0)
            .fold(0.0_f64, f64::max)
    })
    .await?;
    let memory = if m.mem_total > 0 { m.mem_used as f64 / m.mem_total as f64 * 100.0 } else { 0.0 };
    let out_mbps = s.streams.out_kbps() / 1000.0;
    let limit = s.config.load().limits.upload_mbps;
    let upload = (limit > 0.0).then(|| out_mbps / limit * 100.0);
    let r = |v: f64| (v * 10.0).round() / 10.0;
    Ok(Json(json!({
        "machine": m,
        "load": { "cpu": r(m.cpu as f64), "memory": r(memory), "disk": r(disk), "upload": upload.map(r), "outMbps": r(out_mbps), "uploadLimit": limit },
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_masked() {
        let mut c = ServerConfig::default();
        assert_eq!(masked(&c).security.sso.client_secret, "");
        c.security.sso.client_secret = "s3cret".into();
        assert_eq!(masked(&c).security.sso.client_secret, MASK);
    }
}
