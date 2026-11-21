//! The recorders' routes: settings, live stills, the loop for the player.
use axum::{
    body::Body,
    extract::{Path as UrlPath, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
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
        .route("/api/decklink", get(decklink))
        .route("/api/ndi", get(ndi))
        .route("/api/system/hardware", get(hardware))
        .route("/api/system/hardware/name", axum::routing::put(hardware_name))
        .route("/api/system/hardware/search", axum::routing::post(hardware_search))
        .route("/api/system/hardware/forget", axum::routing::post(hardware_forget))
        .route("/api/decklink/{n}/preview.jpg", get(decklink_preview))
        .route("/api/recorders/{id}", axum::routing::put(update).delete(delete))
        .route("/api/recorders/probe", post(probe))
        .route("/api/recorders/{id}/enabled", post(set_enabled))
        .route("/api/recorders/{id}/recording", post(set_recording))
        .route("/api/recorders/{id}/clear", post(clear))
        .route("/api/recorders/{id}/logo", get(logo_get).put(logo_put).delete(logo_delete).layer(axum::extract::DefaultBodyLimit::max(super::logo::MAX + 4096)))
        .route("/api/recorders/{id}/logo/url", post(logo_url))
        .route("/api/schedules", get(schedules).post(put_slot))
        .route("/api/schedules/occurrences", get(occurrences))
        .route("/api/schedules/{id}", axum::routing::put(put_slot_id).delete(delete_slot))
        .route("/api/recorders/{id}/thumb.jpg", get(thumb))
        .route("/api/recorders/{id}/media", get(media))
        .route("/api/recorders/{id}/coverage", get(coverage))
        .route("/api/recorders/{id}/frame.jpg", get(frame))
        .route("/api/recorders/{id}/wave", get(wave))
        .route("/api/recorders/{id}/epg", get(epg))
        .route("/api/recorders/{id}/runs/{run}/{track}/{file}", get(fragment))
        .route("/api/storage", get(storage))
}

/// DeckLink cards: what the server has (GStreamer's module, Blackmagic's driver) and the cards seen.
async fn decklink(State(s): State<Shared>, CurrentUser(_): CurrentUser) -> Json<Value> {
    Json(super::decklink::status(&s.recorders).await)
}

#[derive(Deserialize)]
struct NdiQuery {
    /// Addresses to ask too (comma separated).
    #[serde(default)]
    host: String,
}

/// NDI: whether its library is installed, and the sources found on the network now.
async fn ndi(CurrentUser(me): CurrentUser, Query(q): Query<NdiQuery>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let host = q.host.split(',').map(str::trim).filter(|h| h.parse::<std::net::Ipv4Addr>().is_ok()).collect::<Vec<_>>().join(",");
    let found = tokio::task::spawn_blocking(move || {
        let installed = super::ndi::installed();
        let sources = if installed { super::ndi::sources(&host, std::time::Duration::from_millis(1800)).unwrap_or_default() } else { Vec::new() };
        json!({ "installed": installed, "version": super::ndi::version(), "sources": sources })
    })
    .await
    .map_err(internal)?;
    Ok(Json(found))
}

#[derive(Deserialize)]
struct CardName {
    key: String,
    name: String,
}

/// Système › Matériel: a name given to a card, to tell it from the others.
async fn hardware_name(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<CardName>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    super::decklink::set_name(s.recorders.data_dir(), &b.key, &b.name).await.map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

/// A recorder's logo (an SVG is shown as an image only: no script of its own runs).
async fn logo_get(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Response> {
    let (path, mime) = super::logo::find(s.recorders.data_dir(), &id).ok_or_else(|| not_found("logo"))?;
    let bytes = tokio::fs::read(&path).await.map_err(|_| not_found("logo"))?;
    Ok((
        [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "private, max-age=86400"), (header::X_CONTENT_TYPE_OPTIONS, "nosniff"), (header::CONTENT_SECURITY_POLICY, "default-src 'none'; style-src 'unsafe-inline'; sandbox")],
        bytes,
    )
        .into_response())
}

/// A logo sent by the browser (the image itself as the body).
async fn logo_put(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, body: axum::body::Bytes) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    super::logo::save(s.recorders.data_dir(), &id, &body).map_err(bad)?;
    s.recorders.list_changed();
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct LogoUrl {
    url: String,
}

/// A logo fetched from an address.
async fn logo_url(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<LogoUrl>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let bytes = tokio::task::spawn_blocking(move || super::logo::fetch(&b.url)).await.map_err(internal)?.map_err(bad)?;
    super::logo::save(s.recorders.data_dir(), &id, &bytes).map_err(bad)?;
    s.recorders.list_changed();
    Ok(StatusCode::NO_CONTENT)
}

async fn logo_delete(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    super::logo::remove(s.recorders.data_dir(), &id);
    s.recorders.list_changed();
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PreviewQuery {
    #[serde(default)]
    connection: Option<String>,
    #[serde(default)]
    mode: Option<String>,
}

/// A still of what a card's input receives, for the recorder's form (its format in `X-Format`).
/// An input a recorder captures gives that recorder's still; no picture: 204.
async fn decklink_preview(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath(n): UrlPath<u32>, Query(q): Query<PreviewQuery>) -> ApiResult<Response> {
    use super::decklink::{CONNECTIONS, MODES};
    let connection = q.connection.filter(|c| CONNECTIONS.contains(&c.as_str())).unwrap_or_else(|| "auto".into());
    let mode = q.mode.filter(|m| MODES.contains(&m.as_str())).unwrap_or_else(|| "auto".into());
    let no_store = [(header::CACHE_CONTROL, "no-store")];
    // Captured by a recorder: the card cannot be opened twice, its recorder's still says it.
    if let Some(r) = s.recorders.recs().into_iter().find(|r| {
        let c = r.config();
        c.protocol == super::Protocol::Decklink && c.device == n && matches!(r.status.lock().unwrap().state, super::State::Recording | super::State::Nosignal | super::State::Starting)
    }) {
        let format = r.status.lock().unwrap().media.as_ref().map(|m| super::decklink::mode_of(m.width, m.height, m.interlaced, m.fps_num, m.fps_den)).unwrap_or_default();
        let thumb = r.dir.lock().unwrap().join("thumb.jpg");
        return Ok(match tokio::fs::read(&thumb).await {
            Ok(b) => (no_store, [(header::CONTENT_TYPE, "image/jpeg")], [("x-format", format)], b).into_response(),
            Err(_) => (StatusCode::NO_CONTENT, no_store).into_response(),
        });
    }
    Ok(match super::decklink::preview(&s.recorders.ffmpeg, n, &connection, &mode).await {
        Some((jpeg, format)) => (no_store, [(header::CONTENT_TYPE, "image/jpeg")], [("x-format", format)], jpeg).into_response(),
        None => (StatusCode::NO_CONTENT, no_store).into_response(),
    })
}

/// Système › Matériel: the capture cards and their inputs.
async fn hardware(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    Ok(Json(super::decklink::hardware(&s.recorders, false).await))
}

/// Système › Matériel: looks for new cards (the search is made only when asked).
async fn hardware_search(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    Ok(Json(super::decklink::hardware(&s.recorders, true).await))
}

#[derive(Deserialize)]
struct CardKey {
    key: String,
}

/// Système › Matériel: forgets a card adopted that is not there any more.
async fn hardware_forget(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<CardKey>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    super::decklink::forget(s.recorders.data_dir(), &b.key).await.map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list(State(s): State<Shared>, CurrentUser(_): CurrentUser) -> Json<Value> {
    Json(s.recorders.list_json())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopyOf {
    copy_of: Option<String>,
}

/// `?copyOf=<id>`: a duplicate, which keeps the original's passphrase (never sent to clients).
async fn create(State(s): State<Shared>, CurrentUser(me): CurrentUser, Query(q): Query<CopyOf>, Json(mut c): Json<RecorderConfig>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    if c.passphrase == super::MASKED {
        c.passphrase = q.copy_of.as_ref().and_then(|id| s.recorders.get(id)).map(|r| r.config().passphrase).unwrap_or_default();
    }
    if c.cookies == super::MASKED {
        c.cookies = q.copy_of.as_ref().and_then(|id| s.recorders.get(id)).map(|r| r.config().cookies).unwrap_or_default();
    }
    // Recording at once: is there room for it?
    if c.enabled {
        crate::limits::check(&s, &crate::limits::recording(&c))?;
    }
    s.recorders.create(c).map(Json).map_err(bad)
}

/// The recorder's window: a source tried before its recorder is made (a still, what it carries).
async fn probe(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(c): Json<RecorderConfig>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    super::probe::probe(&s.recorders, c).await.map(Json).map_err(bad)
}

async fn update(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(c): Json<RecorderConfig>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    guard_start(&s, &rec, &c)?;
    s.recorders.update(&id, c).map(Json).map_err(bad)
}

/// Would the source record now with these settings: all the time, or one of its programmed
/// recordings is on now (unless it is never recorded)?
fn records_now(s: &Shared, id: &str, c: &RecorderConfig) -> bool {
    let m = &s.recorders;
    c.enabled || (!c.slots_off && m.schedules.active(id, &m.tz(), super::now_ms()).is_some())
}

/// A change made by hand that starts a recording now: is there room for it? (A programmed
/// recording that starts at its time is never refused.)
fn guard_start(s: &Shared, rec: &super::Rec, new: &RecorderConfig) -> ApiResult<()> {
    if !rec.running() && records_now(s, &rec.id, new) && !records_now(s, &rec.id, &rec.config()) {
        crate::limits::check(s, &crate::limits::recording(new))?;
    }
    Ok(())
}

async fn delete(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    // Its lives first: they would go on sending nothing until their input timed out.
    s.streams.end_source(&id);
    s.recorders.delete(&id).await.map_err(internal)?;
    s.clips.recorder_deleted(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Enabled {
    enabled: bool,
}

async fn set_enabled(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<Enabled>) -> ApiResult<Json<Value>> {
    require(me.can("schedule.edit"))?;
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let mut c = rec.config();
    c.enabled = b.enabled;
    guard_start(&s, &rec, &c)?;
    s.recorders.update(&id, c).map(Json).map_err(bad)
}

#[derive(Deserialize)]
struct Recording {
    /// "continuous", "slots" or "off".
    mode: String,
}

/// Programmation: how a source is recorded (all the time, during its slots, not at all).
async fn set_recording(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<Recording>) -> ApiResult<Json<Value>> {
    require(me.can("schedule.edit"))?;
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let mut c = rec.config();
    (c.enabled, c.slots_off) = match b.mode.as_str() {
        "continuous" => (true, false),
        "slots" => (false, false),
        "off" => (false, true),
        _ => return Err(bad("the recording is continuous, by slots or off")),
    };
    // Recording from now on (all the time, or a programmed recording on now): is there room for it?
    guard_start(&s, &rec, &c)?;
    s.recorders.update(&id, c).map(Json).map_err(bad)
}

/// The programmed slots, each with its next occurrence.
async fn schedules(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.can("schedule.view"))?;
    let m = &s.recorders;
    Ok(Json(json!({ "slots": m.schedules.json(&m.tz(), super::now_ms()), "timezone": m.tz().iana_name().unwrap_or("UTC") })))
}

/// Every occurrence of the slots between two server times (the calendar).
async fn occurrences(State(s): State<Shared>, CurrentUser(me): CurrentUser, Query(q): Query<Window>) -> ApiResult<Json<Value>> {
    require(me.can("schedule.view"))?;
    let m = &s.recorders;
    let tz = m.tz();
    let (from, to) = (q.from as i64, q.to as i64);
    if to <= from || to - from > 62 * 86_400_000 {
        return Err(bad("the window is 1 ms to 62 days"));
    }
    let list: Vec<Value> = m
        .schedules
        .all()
        .iter()
        .flat_map(|sl| sl.times(&tz, from, to).into_iter().map(move |(a, b)| json!({"slot": sl.id, "recorder": sl.recorder, "name": sl.name, "start": a, "end": b, "enabled": sl.enabled})))
        .collect();
    Ok(Json(json!({ "occurrences": list })))
}

/// Tells every client the programme changed.
fn schedules_changed(s: &Shared) {
    let m = &s.recorders;
    m.events.send(crate::state::Audience::All, json!({"type": "schedules", "slots": m.schedules.json(&m.tz(), super::now_ms())}));
}

async fn put_slot(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(slot): Json<super::schedule::Slot>) -> ApiResult<Json<Value>> {
    require(me.can("schedule.edit"))?;
    save_slot(&s, slot)
}

async fn put_slot_id(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(mut slot): Json<super::schedule::Slot>) -> ApiResult<Json<Value>> {
    require(me.can("schedule.edit"))?;
    slot.id = id;
    save_slot(&s, slot)
}

fn save_slot(s: &Shared, slot: super::schedule::Slot) -> ApiResult<Json<Value>> {
    let m = s.recorders.clone();
    let rec = m.get(&slot.recorder).ok_or_else(|| not_found("recorder"))?;
    // A programmed recording saved while its time is on starts now: by hand, so the guard asks.
    let c = rec.config();
    let now = super::now_ms();
    let mut probe = slot.clone();
    probe.normalize();
    if probe.enabled && !c.slots_off && !rec.running() && !records_now(s, &rec.id, &c) && probe.occurrences(&m.tz(), now, now + 1).iter().any(|&(a, b)| a <= now && now < b) {
        crate::limits::check(s, &crate::limits::recording(&c))?;
    }
    let before = m.reserved();
    let saved = m
        .schedules
        .put(slot, |_| {
            // The recordings kept must fit, as a loop's.
            let after = m.reserved();
            match m.loops_capacity() {
                Some(cap) if after > cap && after > before => Err("not enough disk space for these recordings".into()),
                _ => Ok(()),
            }
        })
        .map_err(bad)?;
    schedules_changed(s);
    Ok(Json(serde_json::to_value(saved).unwrap_or_default()))
}

async fn delete_slot(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    require(me.can("schedule.edit"))?;
    s.recorders.schedules.delete(&id).map_err(bad)?;
    schedules_changed(&s);
    Ok(Json(json!({ "ok": true })))
}

/// Empties a recorder's loop (it goes on recording).
async fn clear(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let m = s.recorders.clone();
    tokio::task::spawn_blocking(move || m.clear(&rec)).await.map_err(internal)?;
    Ok(Json(json!({ "ok": true })))
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
async fn media(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Query(w): Query<Window>) -> ApiResult<Json<Value>> {
    require(me.can("extract.view") || me.can("stream.view"))?;
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
            // Up to the live: the newest preview fragments, ahead of the recording's last segment
            // (6 s, listed once whole).
            let upto = if run.live && w.to >= last { f64::INFINITY } else { t_to + 4.0 };
            let track = |video: bool| {
                let t = run.track(video);
                t.offset.map(|offset| json!({"offset": offset, "segs": t.segs.iter().filter(|p| p.t1 >= t_from - 4.0 && p.t0 <= upto).map(|p| json!([p.n, p.t0, p.t1])).collect::<Vec<_>>()}))
            };
            inits.push((out.len(), run.track_dir(true).join("init.mp4"), run.track_dir(false).join("init.mp4")));
            out.push(json!({
                "id": run.meta.id, "live": run.live, "media": run.meta.media,
                "from": first, "to": last,
                "map": map, "video": track(true), "audio": track(false),
                // The other sound tracks the preview made (fragments numbered as the first's).
                "extraAudio": (1..=3).take_while(|k| run.dir.join(format!("audio{k}")).join("init.mp4").exists()).count(),
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

/// A file of the browser's proxy: `init.mp4` or a fragment `p<n>.m4s`, of the video, the first
/// sound track (`audio`) or another one (`audio1` to `audio3`, numbered as the first's).
async fn fragment(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath((id, run, track, file)): UrlPath<(String, String, String, String)>) -> ApiResult<Response> {
    require(me.can("extract.view") || me.can("stream.view"))?;
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    // The fragment's number (none: the init segment); nothing else names a file.
    let n: Option<u32> = if file == "init.mp4" {
        None
    } else {
        Some(file.strip_prefix('p').and_then(|f| f.strip_suffix(".m4s")).and_then(|n| n.parse().ok()).ok_or_else(|| not_found("fragment"))?)
    };
    let path = {
        let runs = rec.runs.read().unwrap();
        let r = runs.iter().find(|r| r.meta.id == run).ok_or_else(|| not_found("run"))?;
        let dir = match track.as_str() {
            "video" | "audio" => {
                let video = track == "video";
                // An indexed track: only a fragment of its list.
                if n.is_some_and(|n| !r.track(video).segs.iter().any(|p| p.n == n)) {
                    return Err(not_found("fragment"));
                }
                r.track_dir(video)
            }
            other => match other.strip_prefix("audio").and_then(|k| k.parse::<u32>().ok()).filter(|k| (1..=3).contains(k)) {
                Some(k) => r.dir.join(format!("audio{k}")),
                None => return Err(not_found("track")),
            },
        };
        match n {
            None => dir.join("init.mp4"),
            Some(n) => dir.join(format!("p{n:08}.m4s")),
        }
    };
    let mut data = tokio::fs::read(&path).await.map_err(|_| not_found("fragment"))?;
    // The init segment without its edit lists (Safari applies them, see `mp4::without_edits`).
    if n.is_none() {
        data = super::index::mp4::without_edits(&data);
    }
    Ok(file_response(data, "video/mp4", "private, max-age=86400"))
}

/// The programmes of the source's guide between two server times.
async fn epg(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath(id): UrlPath<String>, Query(q): Query<Window>) -> ApiResult<Json<Value>> {
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let events = rec.epg.lock().unwrap().between(q.from, q.to);
    Ok(Json(json!({ "events": events })))
}

#[derive(Deserialize)]
struct WaveQuery {
    from: f64,
    to: f64,
    n: usize,
}

/// The sound level between two server times, in `n` columns (peak of each;
/// -1 where nothing was recorded).
async fn wave(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath(id): UrlPath<String>, Query(q): Query<WaveQuery>) -> ApiResult<Json<Value>> {
    let rec = s.recorders.get(&id).ok_or_else(|| not_found("recorder"))?;
    let n = q.n.clamp(1, 4000);
    if q.to <= q.from || q.to.is_nan() || q.from.is_nan() {
        return Err(bad("empty window"));
    }
    // A whole loop holds millions of levels: off the async threads, a column at a time.
    let out = tokio::task::spawn_blocking(move || wave_columns(&rec, q.from, q.to, n)).await.map_err(internal)?;
    Ok(Json(json!({"from": q.from, "to": q.to, "levels": out})))
}

/// The peak of each of `n` columns between `from` and `to` (-1 where nothing was recorded).
fn wave_columns(rec: &super::Rec, from: f64, to: f64, n: usize) -> Vec<i16> {
    let per = (to - from) / n as f64;
    let mut out = vec![-1i16; n];
    let step = 1000.0 / super::index::wave::PER_SECOND;
    let runs = rec.runs.read().unwrap();
    for run in runs.iter() {
        let (Some(first), Some(last)) = (run.first_wall(), run.last_wall()) else { continue };
        if last < from || first > to {
            continue;
        }
        for seg in &run.audio.segs {
            let Some(w) = run.audio.waves.get(&seg.n) else { continue };
            let Some(w0) = run.wall_of(seg.t0) else { continue };
            let w1 = w0 + w.len() as f64 * step;
            if w1 < from || w0 > to {
                continue;
            }
            // The levels inside the window, then one slice per column (its peak at once).
            let mut j = if w0 < from { ((from - w0) / step).ceil() as usize } else { 0 };
            let j_end = (((to - w0) / step).ceil().max(0.0) as usize).min(w.len());
            while j < j_end {
                let i = (((w0 + j as f64 * step) - from) / per) as usize;
                if i >= n {
                    break;
                }
                let col_end = from + (i + 1) as f64 * per;
                let next = ((((col_end - w0) / step).ceil().max(0.0)) as usize).clamp(j + 1, j_end);
                if let Some(peak) = w[j..next].iter().max() {
                    out[i] = out[i].max(*peak as i16);
                }
                j = next;
            }
        }
    }
    out
}

#[derive(Deserialize)]
struct At {
    at: f64,
    #[serde(default = "default_h")]
    h: u32,
    /// A still for a wide view: the nearest key frame is enough (much faster to decode).
    #[serde(default)]
    fast: u8,
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
        // The fragment holding t, else the nearest (a small hole between two fragments).
        let p = run
            .video
            .segs
            .iter()
            .find(|p| p.t0 <= t && t < p.t1)
            .or_else(|| run.video.segs.iter().min_by(|a, b| (a.t0 - t).abs().min((a.t1 - t).abs()).total_cmp(&(b.t0 - t).abs().min((b.t1 - t).abs()))).filter(|p| (p.t0 - t).abs().min((p.t1 - t).abs()) < 2.0))
            .ok_or_else(|| not_found("image"))?;
        let t = t.clamp(p.t0, p.t1);
        // One still per 1/10 s is enough for a strip (per second for a wide view).
        let key = if q.fast > 0 { format!("{id}/{}/{}/k{}/{h}", run.meta.id, p.n, t.round() as i64) } else { format!("{id}/{}/{}/{}/{h}", run.meta.id, p.n, (t * 10.0).round() as i64) };
        (run.track_dir(true).join("init.mp4"), run.proxy_path(true, p.n), t, key)
    };
    if let Some(img) = frame_cache().lock().unwrap().get(&key).cloned() {
        return Ok(file_response(img.to_vec(), "image/jpeg", "private, max-age=86400"));
    }
    let _permit = s.recorders.grabs.acquire().await.map_err(internal)?;
    // Key frame only, else the frame itself, else the fragment's first frame (t past its last one).
    let ff = &s.recorders.ffmpeg;
    let mut img = grab(ff, &init, &seg, t, h, q.fast > 0).await;
    if img.is_none() && q.fast > 0 {
        img = grab(ff, &init, &seg, t, h, false).await;
    }
    if img.is_none() {
        img = grab(ff, &init, &seg, 0.0, h, false).await;
    }
    let img = img.ok_or_else(|| not_found("image"))?;
    let mut cache = frame_cache().lock().unwrap();
    if cache.len() > 4000 {
        cache.clear();
    }
    cache.insert(key, Arc::new(img.clone()));
    Ok(file_response(img, "image/jpeg", "private, max-age=86400"))
}

async fn grab(ffmpeg: &str, init: &Path, seg: &Path, t: f64, h: u32, fast: bool) -> Option<Vec<u8>> {
    let input = format!("concat:{}|{}", init.display(), seg.display());
    let mut cmd = tokio::process::Command::new(ffmpeg);
    super::background(&mut cmd);
    cmd.args(["-v", "error", "-threads", "1"]);
    // Key frames only (one per second in the proxy): no decoding of the frames between.
    if fast {
        cmd.args(["-skip_frame", "nokey"]);
    }
    let from = if fast { t - 0.6 } else { t - 0.02 };
    let out = cmd
        .args(["-copyts", "-i", &input, "-vf"])
        .arg(format!("select='gte(t,{from:.4})',scale=-2:{h}"))
        .args(["-frames:v", "1", "-q:v", "6", "-f", "image2pipe", "-c:v", "mjpeg", "pipe:1"])
        .stdin(std::process::Stdio::null())
        // A still nobody waits for any more (the view moved) is not finished.
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    (!out.stdout.is_empty()).then_some(out.stdout)
}

fn disk(path: &Path) -> Value {
    super::disk_space(path).map(|(total, free)| json!({"total": total, "free": free})).unwrap_or(Value::Null)
}

/// The disks of the loops and the clips, what each loop reserves and what fits.
async fn storage(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let m = s.recorders.clone();
    let clips_dir = s.clips.dir();
    let clips_of = s.clips.by_recorder();
    let v = tokio::task::spawn_blocking(move || {
        let rec_dir = m.recordings_dir();
        let recorders: Vec<Value> = m
            .recs()
            .iter()
            .map(|r| {
                let st = r.status.lock().unwrap().clone();
                let c = r.config();
                json!({
                    "id": r.id, "name": c.name, "enabled": c.enabled, "loopHours": c.loop_hours,
                    "expected": c.bitrate, "bitrate": st.bitrate, "bytes": st.bytes,
                    "need": m.need_of(&c, st.measured), "oldest": st.oldest, "newest": st.newest,
                    "spaceLimited": st.space_limited,
                    // Its clips: how many, and their bytes.
                    "clips": clips_of.get(&r.id).map_or(0, |c| c.0), "clipBytes": clips_of.get(&r.id).map_or(0, |c| c.1),
                })
            })
            .collect();
        let clips_bytes = super::dir_size(&clips_dir);
        // The clips of recorders deleted since.
        let known: std::collections::HashSet<String> = m.recs().iter().map(|r| r.id.clone()).collect();
        let orphans: (u64, u64) = clips_of.iter().filter(|(id, _)| !known.contains(*id)).fold((0, 0), |a, (_, c)| (a.0 + c.0, a.1 + c.1));
        json!({
            "recordings": {"dir": rec_dir, "disk": disk(&rec_dir), "bytes": m.loops_bytes()},
            "clips": {"dir": clips_dir, "disk": disk(&clips_dir), "bytes": clips_bytes, "sameDisk": super::same_disk(&rec_dir, &clips_dir), "count": clips_of.values().map(|c| c.0).sum::<u64>(), "orphans": orphans.0, "orphanBytes": orphans.1},
            "capacity": m.loops_capacity(),
            "reserved": m.reserved(),
            "disks": crate::storage::disks(),
            "loopsDisk": crate::storage::usage(&rec_dir, m.loops_bytes_under(&rec_dir), if super::same_disk(&rec_dir, &clips_dir) { clips_bytes } else { 0 }),
            "clipsDisk": crate::storage::usage(&clips_dir, if super::same_disk(&rec_dir, &clips_dir) { m.loops_bytes_under(&rec_dir) } else { 0 }, clips_bytes),
            "prepares": crate::storage::managed(),
            "recorders": recorders,
        })
    })
    .await
    .map_err(internal)?;
    Ok(Json(v))
}

