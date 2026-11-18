//! Clips cut from the loops, at the frame: a queue of FFmpeg jobs, then files
//! to download.
use axum::{
    extract::{Path as UrlPath, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{io::AsyncBufReadExt, sync::Notify};
use tower::ServiceExt;

use crate::{
    api::{bad, internal, new_id, not_found, require, ApiResult},
    auth::CurrentUser,
    config::ServerConfig,
    recorder::{now_ms, Manager, Pin},
    state::{Audience, Events, Shared},
};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: String,
    pub name: String,
    pub recorder: String,
    pub recorder_name: String,
    /// Server time of the first frame, ms.
    pub start: i64,
    pub frames: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub format: String,
    pub size: u64,
    pub created_at: i64,
    pub created_by: String,
    #[serde(skip_serializing, default)]
    pub file: PathBuf,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Error,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub name: String,
    pub recorder: String,
    pub state: JobState,
    pub progress: f64,
    pub message: String,
    pub clip: Option<String>,
    #[serde(skip)]
    req: ClipRequest,
    #[serde(skip)]
    by: String,
    #[serde(skip)]
    ended: Option<Instant>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClipRequest {
    pub recorder: String,
    pub run: String,
    /// Source time of the first frame, seconds.
    pub t: f64,
    pub frames: u32,
    #[serde(default)]
    pub name: String,
}

pub struct Clips {
    data_dir: PathBuf,
    dir: Mutex<PathBuf>,
    events: Events,
    recorders: Arc<Manager>,
    clips: Mutex<Vec<Clip>>,
    jobs: Mutex<Vec<Job>>,
    wake: Notify,
    /// The running job's FFmpeg, to cancel it.
    running: Mutex<Option<(String, u32)>>,
}

impl Clips {
    pub fn start(data_dir: &Path, cfg: &ServerConfig, events: Events, recorders: Arc<Manager>) -> Arc<Self> {
        let clips: Vec<Clip> = crate::paths::read_json::<Vec<Clip>>(&data_dir.join("clips.json")).unwrap_or_default().into_iter().filter(|c| c.file.exists()).collect();
        let c = Arc::new(Clips {
            data_dir: data_dir.to_path_buf(),
            dir: Mutex::new(cfg.clips_dir(data_dir)),
            events,
            recorders,
            clips: Mutex::new(clips),
            jobs: Mutex::new(Vec::new()),
            wake: Notify::new(),
            running: Mutex::new(None),
        });
        tokio::spawn(worker(c.clone()));
        c
    }

    pub fn set_config(&self, cfg: &ServerConfig) {
        *self.dir.lock().unwrap() = cfg.clips_dir(&self.data_dir);
    }

    pub fn dir(&self) -> PathBuf {
        self.dir.lock().unwrap().clone()
    }

    pub fn jobs_json(&self) -> Value {
        serde_json::to_value(&*self.jobs.lock().unwrap()).unwrap_or_default()
    }

    fn jobs_changed(&self) {
        self.events.send(Audience::All, json!({"type": "jobs", "jobs": self.jobs_json()}));
    }

    fn clips_changed(&self) {
        self.events.send(Audience::All, json!({"type": "changed", "what": "clips"}));
    }

    fn save(&self) {
        let list = self.clips.lock().unwrap().clone();
        let full: Vec<Value> = list.iter().map(|c| {
            let mut v = serde_json::to_value(c).unwrap_or_default();
            v["file"] = json!(c.file);
            v
        }).collect();
        if let Err(e) = crate::paths::write_json(&self.data_dir.join("clips.json"), &full, false) {
            tracing::error!("clips.json: {e}");
        }
    }

    fn thumb_path(&self, id: &str) -> PathBuf {
        self.data_dir.join("clip-thumbs").join(format!("{id}.jpg"))
    }

    /// Queued jobs of a deleted recorder are dropped.
    pub fn recorder_deleted(&self, id: &str) {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.retain(|j| !(j.recorder == id && j.state == JobState::Queued));
        drop(jobs);
        if self.running.lock().unwrap().as_ref().is_some_and(|(r, _)| r == id) {
            self.cancel_running();
        }
        self.jobs_changed();
    }

    fn cancel_running(&self) {
        if let Some((_, pid)) = self.running.lock().unwrap().as_ref() {
            unsafe {
                libc::kill(*pid as i32, libc::SIGKILL);
            }
        }
    }

    /// Checks a request and queues it.
    fn queue(&self, req: ClipRequest, by: &str) -> Result<Job, String> {
        let rec = self.recorders.get(&req.recorder).ok_or("no such recorder")?;
        if req.frames == 0 {
            return Err("the clip has no frame".into());
        }
        let name = req.name.trim().to_string();
        {
            let runs = rec.runs.read().unwrap();
            let run = runs.iter().find(|r| r.meta.id == req.run).ok_or("this part of the loop was removed")?;
            let fps = run.meta.media.as_ref().map(|m| m.fps()).unwrap_or(25.0);
            if req.frames as f64 / fps > 4.0 * 3600.0 {
                return Err("a clip lasts 4 hours at most".into());
            }
            let half = 0.5 / fps;
            if run.covering(req.t - half, req.t + req.frames as f64 / fps - half).is_none() {
                return Err(if run.first_wall().is_some() && run.master.front().is_some_and(|s| s.t0 > req.t) { "this part of the loop was removed".into() } else { "the clip crosses a gap in the recording".into() });
            }
        }
        let id = new_id();
        let name = if name.is_empty() { format!("{} {}", rec.config().name, id) } else { name };
        self.recorders.pin(Pin { id: id.clone(), recorder: req.recorder.clone(), run: req.run.clone(), from: req.t - 1.0 });
        let job = Job { id, name, recorder: req.recorder.clone(), state: JobState::Queued, progress: 0.0, message: String::new(), clip: None, req, by: by.to_string(), ended: None };
        self.jobs.lock().unwrap().push(job.clone());
        self.jobs_changed();
        self.wake.notify_one();
        Ok(job)
    }

    fn set_job(&self, id: &str, f: impl FnOnce(&mut Job)) {
        if let Some(j) = self.jobs.lock().unwrap().iter_mut().find(|j| j.id == id) {
            f(j);
        }
        self.jobs_changed();
    }

    /// Cuts one clip; returns the clip or an error message.
    async fn cut(&self, job: &Job) -> Result<Clip, String> {
        let rec = self.recorders.get(&job.req.recorder).ok_or("the recorder was deleted")?;
        let (segs, media, wall) = {
            let runs = rec.runs.read().unwrap();
            let run = runs.iter().find(|r| r.meta.id == job.req.run).ok_or("this part of the loop was removed")?;
            let media = run.meta.media.clone().unwrap_or_default();
            let fps = if media.fps_den == 0 { 25.0 } else { media.fps() };
            let half = 0.5 / fps;
            let segs = run.covering(job.req.t - half, job.req.t + job.req.frames as f64 / fps - half).ok_or("this part of the loop was removed")?;
            let paths: Vec<PathBuf> = segs.iter().map(|s| run.master_path(s.n)).collect();
            (paths, media, run.wall_of(job.req.t).unwrap_or(0.0))
        };
        let fps = if media.fps_den == 0 { 25.0 } else { media.fps() };
        let (fps_num, fps_den) = if media.fps_den == 0 { (25, 1) } else { (media.fps_num, media.fps_den) };
        let dir = self.dir();
        tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("{}: {e}", dir.display()))?;
        let part = dir.join(format!("{}.part.mp4", job.id));
        let out = dir.join(format!("{}.mp4", job.id));
        let start = job.req.t - 0.5 / fps;
        let dur = job.req.frames as f64 / fps;
        let input = format!("concat:{}", segs.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("|"));
        let mut args: Vec<String> = vec!["-hide_banner".into(), "-nostdin".into(), "-v".into(), "error".into(), "-progress".into(), "pipe:1".into(), "-copyts".into(), "-i".into(), input];
        args.extend(["-map", "0:v:0", "-map", "0:a?"].map(String::from));
        args.extend(["-vf".into(), format!("trim=start={start:.6},setpts=PTS-STARTPTS")]);
        args.extend(["-af".into(), format!("atrim=start={start:.6}:end={:.6},asetpts=PTS-STARTPTS", start + dur)]);
        args.extend(["-frames:v".into(), job.req.frames.to_string()]);
        args.extend(["-c:v", "libx264", "-preset", "fast", "-crf", "16", "-profile:v", "high", "-pix_fmt", "yuv420p"].map(String::from));
        if media.interlaced {
            args.extend(["-flags", "+ildct+ilme"].map(String::from));
        }
        args.extend(["-c:a", "aac", "-b:a", "256k", "-movflags", "+faststart", "-y"].map(String::from));
        args.push(part.to_string_lossy().into_owned());
        let mut child = tokio::process::Command::new(&self.recorders.ffmpeg).args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn().map_err(|e| format!("cannot start ffmpeg: {e}"))?;
        *self.running.lock().unwrap() = child.id().map(|p| (job.req.recorder.clone(), p));
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let stderr = child.stderr.take().unwrap();
        let err_task = tokio::spawn(async move {
            let mut s = String::new();
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                s = l;
            }
            s
        });
        let mut last = Instant::now();
        while let Ok(Some(l)) = lines.next_line().await {
            if let Some(f) = l.strip_prefix("frame=").and_then(|f| f.trim().parse::<f64>().ok()) {
                if last.elapsed() > Duration::from_millis(400) {
                    last = Instant::now();
                    let p = (f / job.req.frames as f64).min(0.99);
                    self.set_job(&job.id, |j| j.progress = p);
                }
            }
        }
        let status = child.wait().await;
        *self.running.lock().unwrap() = None;
        let err = err_task.await.unwrap_or_default();
        match status {
            Ok(s) if s.success() => {}
            Ok(_) | Err(_) => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(if err.is_empty() { "cancelled".into() } else { err });
            }
        }
        tokio::fs::rename(&part, &out).await.map_err(|e| e.to_string())?;
        let size = tokio::fs::metadata(&out).await.map(|m| m.len()).unwrap_or(0);
        // A still from the middle for the list.
        let thumb = self.thumb_path(&job.id);
        if let Some(d) = thumb.parent() {
            let _ = tokio::fs::create_dir_all(d).await;
        }
        let _ = tokio::process::Command::new(&self.recorders.ffmpeg)
            .args(["-v", "error", "-ss", &format!("{:.3}", if dur > 1.0 { dur / 2.0 } else { 0.0 }), "-i"])
            .arg(&out)
            .args(["-frames:v", "1", "-vf", "scale=-2:144", "-q:v", "5", "-y"])
            .arg(&thumb)
            .stdin(Stdio::null())
            .status()
            .await;
        Ok(Clip {
            id: job.id.clone(),
            name: job.name.clone(),
            recorder: job.req.recorder.clone(),
            recorder_name: rec.config().name,
            start: wall.round() as i64,
            frames: job.req.frames,
            fps_num,
            fps_den,
            format: "H.264 · AAC".into(),
            size,
            created_at: now_ms(),
            created_by: job.by.clone(),
            file: out,
        })
    }
}

async fn worker(c: Arc<Clips>) {
    loop {
        // Finished jobs leave the list after a minute.
        let expired = {
            let mut jobs = c.jobs.lock().unwrap();
            let before = jobs.len();
            jobs.retain(|j| j.ended.is_none_or(|e| e.elapsed() < Duration::from_secs(60)));
            before != jobs.len()
        };
        if expired {
            c.jobs_changed();
        }
        let next = c.jobs.lock().unwrap().iter().find(|j| j.state == JobState::Queued).cloned();
        let Some(job) = next else {
            tokio::select! { _ = c.wake.notified() => {}, _ = tokio::time::sleep(Duration::from_secs(10)) => {} }
            continue;
        };
        c.set_job(&job.id, |j| j.state = JobState::Running);
        let result = c.cut(&job).await;
        c.recorders.unpin(&job.id);
        match result {
            Ok(clip) => {
                tracing::info!("clip {} cut ({} frames)", clip.name, clip.frames);
                let id = clip.id.clone();
                c.clips.lock().unwrap().push(clip);
                c.save();
                c.set_job(&job.id, |j| {
                    j.state = JobState::Done;
                    j.progress = 1.0;
                    j.clip = Some(id);
                    j.ended = Some(Instant::now());
                });
                c.clips_changed();
            }
            Err(e) => {
                tracing::warn!("clip {}: {e}", job.name);
                c.set_job(&job.id, |j| {
                    j.state = JobState::Error;
                    j.message = e;
                    j.ended = Some(Instant::now());
                });
            }
        }
    }
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/clips", get(list).post(create))
        .route("/api/clips/{id}", delete(remove))
        .route("/api/clips/{id}/download", get(download))
        .route("/api/clips/{id}/thumb.jpg", get(thumb))
        .route("/api/jobs/{id}", delete(cancel))
}

async fn list(State(s): State<Shared>, CurrentUser(_): CurrentUser) -> Json<Value> {
    let mut list = s.clips.clips.lock().unwrap().clone();
    list.sort_by_key(|c| std::cmp::Reverse(c.created_at));
    Json(serde_json::to_value(list).unwrap_or_default())
}

async fn create(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(req): Json<ClipRequest>) -> ApiResult<(StatusCode, Json<Value>)> {
    require(me.can_extract())?;
    let job = s.clips.queue(req, me.name()).map_err(bad)?;
    Ok((StatusCode::ACCEPTED, Json(serde_json::to_value(job).unwrap_or_default())))
}

async fn remove(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.can_extract())?;
    let clip = {
        let mut list = s.clips.clips.lock().unwrap();
        let i = list.iter().position(|c| c.id == id).ok_or_else(|| not_found("clip"))?;
        list.remove(i)
    };
    s.clips.save();
    let _ = tokio::fs::remove_file(&clip.file).await;
    let _ = tokio::fs::remove_file(s.clips.thumb_path(&id)).await;
    s.clips.clips_changed();
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.can_extract())?;
    let state = s.clips.jobs.lock().unwrap().iter().find(|j| j.id == id).map(|j| j.state).ok_or_else(|| not_found("job"))?;
    match state {
        JobState::Running => s.clips.cancel_running(),
        _ => {
            s.clips.jobs.lock().unwrap().retain(|j| j.id != id);
            s.clips.recorders.unpin(&id);
            s.clips.jobs_changed();
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// "Plateau 1 / 14:32" → "Plateau 1 - 14-32.mp4" (no path characters).
fn file_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '-' } else { c }).collect();
    format!("{}.mp4", clean.trim())
}

async fn download(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath(id): UrlPath<String>, req: Request) -> ApiResult<Response> {
    let clip = s.clips.clips.lock().unwrap().iter().find(|c| c.id == id).cloned().ok_or_else(|| not_found("clip"))?;
    // `?inline=1`: played in the page (the clips' player), not saved.
    let inline = req.uri().query().is_some_and(|q| q.split('&').any(|p| p == "inline=1"));
    let mut res = tower_http::services::ServeFile::new(&clip.file).oneshot(req).await.map_err(internal)?.into_response();
    if inline {
        return Ok(res);
    }
    let name = file_name(&clip.name);
    let ascii: String = name.chars().map(|c| if c.is_ascii() && c != '"' { c } else { '_' }).collect();
    let encoded: String = name.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")) {
        res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

async fn thumb(State(s): State<Shared>, CurrentUser(_): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Response> {
    let data = tokio::fs::read(s.clips.thumb_path(&id)).await.map_err(|_| not_found("image"))?;
    let mut r = Response::new(axum::body::Body::from(data));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    Ok(r)
}

#[cfg(test)]
mod tests {
    #[test]
    fn file_names() {
        assert_eq!(super::file_name("Plateau 1 / 14:32"), "Plateau 1 - 14-32.mp4");
    }
}
