//! Système › Ressources: how loaded the server is against the guard's limit, what each source,
//! Streamer, clip… costs, what still fits; and a warning in the journal when the processor stays
//! over its limit (the UDP sources then lose packets: their picture breaks up).
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{
    api::{bad, require, ApiResult},
    auth::CurrentUser,
    procs::{self, Use},
    recorder::{Protocol, State as RecState},
    state::{AppState, Shared},
};

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/system/resources", get(resources)).route("/api/system/resources/free", post(free))
}

/// Samples every 2 seconds; says in the journal when the processor stays over its limit
/// (6 seconds, at most every 5 minutes), with what costs the most.
pub fn start(s: Shared) {
    static SAID: AtomicI64 = AtomicI64::new(0);
    let weak = Arc::downgrade(&s);
    drop(s);
    tokio::spawn(async move {
        let mut over = 0u32;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let Some(s) = weak.upgrade() else { return };
            let Ok(uses) = tokio::task::spawn_blocking(procs::sample).await else { continue };
            let limit = s.config.load().limits.max_cpu as f32;
            let (cpu, _) = procs::cpu();
            over = if cpu.total > limit { over + 1 } else { 0 };
            let now = crate::util::now_ms();
            if over >= 3 && now - SAID.load(Ordering::Relaxed) > 300_000 {
                SAID.store(now, Ordering::Relaxed);
                let top = uses.iter().find(|u| u.key != procs::SERVER).map(|u| format!(", the most: {} ({:.1} cores)", name_of(&s, &u.key), u.cores)).unwrap_or_default();
                tracing::warn!("processor over its limit: {:.0} % (limit {:.0} %){top}", cpu.total, limit);
            }
        }
    });
}

/// What a key is, said plainly: a source's name, "Streamer · <source>", a clip's name.
fn name_of(s: &AppState, key: &str) -> String {
    match key.split_once(':') {
        Some(("rec", id)) => s.recorders.get(id).map(|r| r.config().name.clone()).unwrap_or_else(|| id.into()),
        Some(("streamer", id)) => format!("Streamer · {}", s.recorders.get(id).map(|r| r.config().name.clone()).unwrap_or_else(|| id.into())),
        Some(("clip", id)) => s.clips.job_name(id).map(|n| format!("clip “{n}”")).unwrap_or_else(|| "a clip".into()),
        _ if key == procs::SERVER => "LiveCut".into(),
        _ => key.into(),
    }
}

/// The kind of source, as Système › Sources groups them.
fn kind_of(p: Protocol) -> &'static str {
    match p {
        Protocol::Decklink => "sdi",
        Protocol::Ndi => "ndi",
        Protocol::Page => "page",
        Protocol::Pattern => "pattern",
        Protocol::Web | Protocol::Hls => "online",
        _ => "network",
    }
}

/// Cores a new one of each kind would take, measured on those running (else an estimate).
fn room(flows: &[Value], margin: f64) -> Vec<Value> {
    let avg = |kinds: &[&str], guess: f64| {
        let v: Vec<f64> = flows.iter().filter(|f| f["kind"] == "source" && kinds.contains(&f["source"].as_str().unwrap_or("")) && f["running"] == true).filter_map(|f| f["cores"].as_f64()).filter(|c| *c > 0.05).collect();
        if v.is_empty() { guess } else { v.iter().sum::<f64>() / v.len() as f64 }
    };
    [("network", avg(&["network", "online"], 0.8)), ("sdi", avg(&["sdi", "ndi"], 1.5)), ("page", avg(&["page"], 2.0)), ("copy", 0.05)]
        .into_iter()
        .map(|(what, cores)| json!({"what": what, "cores": (cores * 10.0).round() / 10.0, "count": if margin > 0.0 { (margin / cores).floor() as i64 } else { 0 }}))
        .collect()
}

async fn resources(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let m = crate::perf::machine();
    let limits = s.config.load().limits.clone();
    let (cpu, history) = procs::cpu();
    let cores = m.cores.max(1) as f64;
    let uses = procs::last();
    let flows: Vec<Value> = uses.iter().map(|u| flow(&s, u)).collect();
    // Above the limit in the last 10 minutes: how many times it went over.
    let lim = limits.max_cpu as f32;
    let overs = history.windows(2).filter(|w| w[0] <= lim && w[1] > lim).count() + usize::from(history.first().is_some_and(|v| *v > lim));
    let used = cpu.total as f64 / 100.0 * cores;
    let margin = limits.max_cpu as f64 / 100.0 * cores - used;
    let nics: Vec<Value> = s.recorders.nics_now().into_iter().map(|(name, mbps, speed)| json!({"name": name, "mbps": (mbps * 10.0).round() / 10.0, "speed": speed})).collect();
    let writes: f64 = uses.iter().map(|u| u.writes).sum();
    let lost: u32 = s.recorders.recs().iter().map(|r| r.status.lock().unwrap().lost).sum();
    Ok(Json(json!({
        "cores": m.cores, "cpuName": m.cpu_name,
        "cpu": {"total": cpu.total, "busiestCore": cpu.busiest_core, "softirq": cpu.softirq, "steal": cpu.steal, "limit": limits.max_cpu, "guard": limits.enabled, "overs": overs, "history": history, "usedCores": (used * 10.0).round() / 10.0, "margin": (margin * 10.0).round() / 10.0},
        "memory": {"used": m.mem_used, "total": m.mem_total, "limit": limits.max_memory},
        "nics": nics,
        "writes": writes,
        "lost": lost,
        "room": room(&flows, margin),
        "flows": flows,
    })))
}

/// One owner, said for the page: what it is, what runs for it, what it costs, how it goes.
fn flow(s: &AppState, u: &Use) -> Value {
    let r1 = |v: f64| (v * 100.0).round() / 100.0;
    let history: Vec<f32> = u.history.iter().map(|v| (v * 100.0).round() / 100.0).collect();
    let mut v = json!({"key": u.key, "name": name_of(s, &u.key), "roles": u.roles, "cores": r1(u.cores), "memory": u.memory, "writes": u.writes, "history": history, "running": true});
    match u.key.split_once(':') {
        Some(("rec", id)) => {
            v["kind"] = json!("source");
            v["id"] = json!(id);
            if let Some(r) = s.recorders.get(id) {
                let c = r.config();
                let st = r.status.lock().unwrap().clone();
                let running = matches!(st.state, RecState::Recording | RecState::Starting | RecState::Nosignal);
                v["source"] = json!(kind_of(c.protocol));
                v["protocol"] = json!(c.protocol);
                v["running"] = json!(running);
                v["inMbps"] = json!(r1(st.bitrate as f64 / 1e6));
                v["lost"] = json!(st.lost);
                v["damaged"] = json!(st.damaged);
                v["previewLag"] = json!(st.preview_lag);
                v["previewDropped"] = json!(st.preview_dropped);
                v["state"] = json!(st.state);
                // Stopped, but still working: something was not let go.
                v["leftover"] = json!(!running && c.protocol != Protocol::Pattern && u.cores > 0.05);
            } else {
                v["leftover"] = json!(u.cores > 0.05);
            }
        }
        Some(("streamer", id)) => {
            v["kind"] = json!("streamer");
            v["id"] = json!(id);
            let lives: Vec<String> = s.streams.of_source(id).into_iter().map(|(d, _)| s.streams.get(&d).map(|x| x.name).unwrap_or(d)).collect();
            v["leftover"] = json!(lives.is_empty() && u.cores > 0.05);
            v["lives"] = json!(lives);
        }
        Some(("clip", id)) => {
            v["kind"] = json!("clip");
            v["id"] = json!(id);
        }
        _ if u.key == procs::SERVER => v["kind"] = json!("server"),
        _ => v["kind"] = json!("other"),
    }
    v
}

#[derive(Deserialize)]
struct Free {
    key: String,
}

/// Stops what still works for a stopped source, or a Streamer no live reads (only those).
async fn free(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<Free>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let u = procs::last().into_iter().find(|u| u.key == b.key).ok_or_else(|| bad("nothing runs for it now"))?;
    if flow(&s, &u)["leftover"] != true {
        return Err(bad("it is in use: stop it from its page"));
    }
    let n = procs::stop(&b.key);
    tracing::warn!("resources: {} let go by {} ({n} processes stopped)", name_of(&s, &b.key), me.name());
    Ok(Json(json!({"stopped": n})))
}
