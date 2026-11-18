//! The recorders' routes: settings, live stills, the loop for the player.
use axum::{
    body::Body,
    extract::{Path as UrlPath, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use super::{index::mp4, RecorderConfig};
use crate::{
    api::{bad, internal, not_found, require, ApiResult},
    auth::CurrentUser,
    state::Shared,
};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/recorders", get(list).post(create))
        .route("/api/recorders/{id}", axum::routing::put(update).delete(delete))
        .route("/api/recorders/{id}/enabled", post(set_enabled))
        .route("/api/recorders/{id}/thumb.jpg", get(thumb))
        .route("/api/recorders/{id}/media", get(media))
        .route("/api/recorders/{id}/coverage", get(coverage))
        .route("/api/recorders/{id}/frame.jpg", get(frame))
        .route("/api/recorders/{id}/runs/{run}/{track}/{file}", get(fragment))
        .route("/api/storage", get(storage))
}

async fn list(State(s): State<Shared>, CurrentUser(_): CurrentUser) -> Json<Value> {
    Json(s.recorders.list_json())
}

async fn create(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(c): Json<RecorderConfig>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    s.recorders.create(c).map(Json).map_err(bad)
}

async fn update(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(c): Json<RecorderConfig>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    s.recorders.update(&id, c).map(Json).map_err(bad)
}

async fn delete(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    s.recorders.delete(&id).await.map_err(internal)?;
    s.clips.recorder_deleted(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Enabled {
    enabled: bool,
}

async fn set_enabled(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<Enabled>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let mut c = rec.config();
    c.enabled = b.enabled;
    s.recorders.update(&id, c).map(Json).map_err(bad)
}

fn file_response(data: Vec<u8>, mime: &'static str, cache: &'static str) -> Response {
    let mut r = Response::new(Body::from(data));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    r
}

async fn thumb(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Response> {
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let path = rec.dir.lock().unwrap().join("thumb.jpg");
    let data = tokio::fs::read(path).await.map_err(|_| not_found("image"))?;
    Ok(file_response(data, "image/jpeg", "no-cache"))
}

#[derive(Deserialize)]
struct Window {
    from: f64,
    to: f64,
}

/// What the player needs between two server times: per run, the proxy
/// fragments and the original segments' clock (`map`: [t0, t1, wall0]).
async fn media(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath(id): UrlPath<String>, Query(w): Query<Window>) -> ApiResult<Json<Value>> {
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let mut out = Vec::new();
    let mut inits: Vec<(usize, PathBuf, PathBuf)> = Vec::new();
    {
        let runs = rec.runs.read().unwrap();
        for run in runs.iter() {
            let (Some(first), Some(last)) = (run.first_wall(), run.last_wall()) else { continue };
            if last < w.from || first > w.to {
                continue;
            }
            let (Some(t_from), Some(t_to)) = (run.t_of(w.from.max(first)), run.t_of(w.to.min(last))) else { continue };
            let map: Vec<Value> = run.master.iter().filter(|m| m.t1 >= t_from && m.t0 <= t_to).map(|m| json!([m.t0, m.t1, m.wall0])).collect();
            let track = |video: bool| {
                let t = run.track(video);
                t.offset.map(|offset| json!({"offset": offset, "segs": t.segs.iter().filter(|p| p.t1 >= t_from - 4.0 && p.t0 <= t_to + 4.0).map(|p| json!([p.n, p.t0, p.t1])).collect::<Vec<_>>()}))
            };
            inits.push((out.len(), run.track_dir(true).join("init.mp4"), run.track_dir(false).join("init.mp4")));
            out.push(json!({
                "id": run.meta.id, "live": run.live, "media": run.meta.media,
                "from": first, "to": last,
                "map": map, "video": track(true), "audio": track(false),
            }));
        }
    }
    // MediaSource needs the codecs.
    for (i, v, a) in inits {
        for (key, path) in [("video", v), ("audio", a)] {
            if out[i][key].is_object() {
                let codec = tokio::fs::read(&path).await.ok().and_then(|b| mp4::codec(&b));
                out[i][key]["codec"] = json!(codec);
            }
        }
    }
    Ok(Json(json!({"runs": out, "now": super::now_ms()})))
}

/// The recorded spans of the loop, for the overview bar.
async fn coverage(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let runs = rec.runs.read().unwrap();
    let spans: Vec<Value> = runs
        .iter()
        .filter_map(|r| {
            // Holes inside a run (a stalled source) split it.
            let mut spans = Vec::new();
            let mut cur: Option<(f64, f64)> = None;
            for m in &r.master {
                cur = match cur {
                    Some((a, b)) if m.wall0 - b < 2000.0 => Some((a, m.wall1())),
                    Some(span) => {
                        spans.push(span);
                        Some((m.wall0, m.wall1()))
                    }
                    None => Some((m.wall0, m.wall1())),
                };
            }
            spans.extend(cur);
            (!spans.is_empty()).then(|| spans.into_iter().map(|(a, b)| json!({"run": r.meta.id, "from": a, "to": b})).collect::<Vec<_>>())
        })
        .flatten()
        .collect();
    Ok(Json(json!({"spans": spans, "loopHours": rec.config().loop_hours, "now": super::now_ms()})))
}

async fn fragment(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath((id, run, track, file)): UrlPath<(String, String, String, String)>) -> ApiResult<Response> {
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let video = match track.as_str() {
        "video" => true,
        "audio" => false,
        _ => return Err(not_found("track")),
    };
    let path = {
        let runs = rec.runs.read().unwrap();
        let r = runs.iter().find(|r| r.meta.id == run).ok_or_else(|| not_found("run"))?;
        if file == "init.mp4" {
            r.track_dir(video).join("init.mp4")
        } else {
            let n: u32 = file.strip_prefix('p').and_then(|f| f.strip_suffix(".m4s")).and_then(|n| n.parse().ok()).ok_or_else(|| not_found("fragment"))?;
            if !r.track(video).segs.iter().any(|p| p.n == n) {
                return Err(not_found("fragment"));
            }
            r.proxy_path(video, n)
        }
    };
    let data = tokio::fs::read(&path).await.map_err(|_| not_found("fragment"))?;
    Ok(file_response(data, "video/mp4", "private, max-age=86400"))
}

#[derive(Deserialize)]
struct At {
    at: f64,
    #[serde(default = "default_h")]
    h: u32,
}
fn default_h() -> u32 {
    72
}

type FrameCache = Mutex<HashMap<String, Arc<Vec<u8>>>>;
fn frame_cache() -> &'static FrameCache {
    static C: OnceLock<FrameCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// A still of the loop at a server time, from the proxy (the timeline's strip).
async fn frame(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath(id): UrlPath<String>, Query(q): Query<At>) -> ApiResult<Response> {
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let h = q.h.clamp(36, 540) & !1;
    let (init, seg, t, key) = {
        let runs = rec.runs.read().unwrap();
        let run = runs
            .iter()
            .find(|r| matches!((r.first_wall(), r.last_wall()), (Some(a), Some(b)) if a <= q.at && q.at < b))
            .ok_or_else(|| not_found("image"))?;
        let t = run.t_of(q.at).ok_or_else(|| not_found("image"))?;
        let p = run.video.segs.iter().find(|p| p.t0 <= t && t < p.t1).ok_or_else(|| not_found("image"))?;
        // One still per 1/10 s is enough for a strip.
        let key = format!("{id}/{}/{}/{}/{h}", run.meta.id, p.n, (t * 10.0).round() as i64);
        (run.track_dir(true).join("init.mp4"), run.proxy_path(true, p.n), t, key)
    };
    if let Some(img) = frame_cache().lock().unwrap().get(&key).cloned() {
        return Ok(file_response(img.to_vec(), "image/jpeg", "private, max-age=86400"));
    }
    let _permit = s.recorders.grabs.acquire().await.map_err(internal)?;
    let img = grab(&s.recorders.ffmpeg, &init, &seg, t, h).await.ok_or_else(|| not_found("image"))?;
    let mut cache = frame_cache().lock().unwrap();
    if cache.len() > 4000 {
        cache.clear();
    }
    cache.insert(key, Arc::new(img.clone()));
    Ok(file_response(img, "image/jpeg", "private, max-age=86400"))
}

async fn grab(ffmpeg: &str, init: &Path, seg: &Path, t: f64, h: u32) -> Option<Vec<u8>> {
    let input = format!("concat:{}|{}", init.display(), seg.display());
    let out = tokio::process::Command::new(ffmpeg)
        .args(["-v", "error", "-copyts", "-i", &input, "-vf"])
        .arg(format!("select='gte(t,{:.4})',scale=-2:{h}", t - 0.02))
        .args(["-frames:v", "1", "-q:v", "6", "-f", "image2pipe", "-c:v", "mjpeg", "pipe:1"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    (!out.stdout.is_empty()).then_some(out.stdout)
}

fn disk(path: &Path) -> Value {
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            break;
        }
    }
    let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).unwrap_or_default();
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Value::Null;
    }
    json!({"total": st.f_blocks as u64 * st.f_frsize as u64, "free": st.f_bavail as u64 * st.f_frsize as u64})
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten().map(|e| match e.file_type() {
        Ok(t) if t.is_dir() => dir_size(&e.path()),
        Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
        Err(_) => 0,
    }).sum()
}

/// The disks of the loops and the clips.
async fn storage(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let rec_dir = s.recorders.recordings_dir();
    let clips_dir = s.clips.dir();
    let recorders: Vec<Value> = s.recorders.recs().iter().map(|r| {
        let st = r.status.lock().unwrap();
        let c = r.config();
        // Bytes per hour at the current rate: what the full loop will take.
        let per_hour = st.bitrate as f64 / 8.0 * 3600.0;
        json!({"id": r.id, "name": c.name, "bytes": st.bytes, "loopHours": c.loop_hours, "full": (per_hour * c.loop_hours as f64) as u64})
    }).collect();
    let (rd, cd) = (rec_dir.clone(), clips_dir.clone());
    let clips_bytes = tokio::task::spawn_blocking(move || dir_size(&cd)).await.unwrap_or(0);
    Ok(Json(json!({
        "recordings": {"dir": rec_dir, "disk": disk(&rd)},
        "clips": {"dir": clips_dir, "disk": disk(&clips_dir), "bytes": clips_bytes},
        "recorders": recorders,
    })))
}
