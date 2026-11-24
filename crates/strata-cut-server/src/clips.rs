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

/// Clip jobs waiting (queued or being cut), in all and for one account.
const MAX_WAITING: usize = 100;
const MAX_WAITING_EACH: usize = 20;

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
    /// The account that made it (its id; empty for the clips made before accounts were kept).
    #[serde(default)]
    pub owner: String,
    /// Who else sees it.
    #[serde(default)]
    pub share: Share,
    /// Picture and sound, the sound alone, or one picture.
    #[serde(default)]
    pub kind: Kind,
    #[serde(skip_serializing, default)]
    pub file: PathBuf,
}

/// What a clip holds: the picture with its sound (MP4), the sound alone (M4A, every track), or
/// the one picture under the playhead (JPEG, full size).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Video,
    Audio,
    Image,
}

impl Kind {
    fn ext(self) -> &'static str {
        match self {
            Kind::Video => "mp4",
            Kind::Audio => "m4a",
            Kind::Image => "jpg",
        }
    }
}

/// The formats a clip is made in: its kind, its name, its file's extension (the first of a kind:
/// what is made when none is said).
pub const FORMATS: [(Kind, &str, &str); 10] = [
    (Kind::Video, "master", "mp4"),
    (Kind::Video, "web", "mp4"),
    (Kind::Video, "light", "mp4"),
    (Kind::Video, "prores", "mov"),
    (Kind::Video, "copy", "ts"),
    (Kind::Audio, "m4a", "m4a"),
    (Kind::Audio, "mp3", "mp3"),
    (Kind::Audio, "wav", "wav"),
    (Kind::Image, "jpg", "jpg"),
    (Kind::Image, "png", "png"),
];

/// A request's format (its name, its extension): the one asked for, if the kind has it.
fn format_of(kind: Kind, asked: &str) -> Option<(&'static str, &'static str)> {
    let mut of_kind = FORMATS.iter().filter(|f| f.0 == kind);
    let first = of_kind.clone().next()?;
    let f = if asked.is_empty() { first } else { of_kind.find(|f| f.1 == asked)? };
    Some((f.1, f.2))
}

/// A clip shared: with everyone, or with groups of users.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Share {
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub groups: Vec<String>,
}

impl Clip {
    /// Made by this account (an older clip: by its name).
    fn mine(&self, me: &crate::auth::User) -> bool {
        if self.owner.is_empty() {
            self.created_by == me.name() || self.created_by == me.username
        } else {
            self.owner == me.id
        }
    }

    /// Shared with this account (with everyone, or with a group it is in).
    fn shared_with(&self, groups: &[String]) -> bool {
        self.share.all || self.share.groups.iter().any(|g| groups.contains(g))
    }

    /// An account sees its clips, those shared with it; an admin sees them all.
    fn visible(&self, me: &crate::auth::User, groups: &[String]) -> bool {
        me.is_admin() || self.mine(me) || self.shared_with(groups)
    }
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
    by_id: String,
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
    #[serde(default)]
    pub kind: Kind,
    /// Its format (`FORMATS`; none: the kind's first).
    #[serde(default)]
    pub format: String,
    /// Video: the source's subtitles (DVB, teletext) burnt into the picture (an MP4 cannot hold
    /// them as they are).
    #[serde(default)]
    pub subtitles: bool,
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
    /// The job being cut: (its id, its recorder, FFmpeg's pid).
    running: Mutex<Option<(String, String, u32)>>,
    /// Clips older than this many days are deleted (0 = kept).
    keep_days: std::sync::atomic::AtomicU32,
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
            keep_days: cfg.clips_keep_days.into(),
        });
        // A clip being cut when the server stopped (a crash, a restart) left its part file: its job
        // is gone with the server, the file too.
        if let Ok(list) = std::fs::read_dir(c.dir()) {
            for e in list.flatten() {
                if e.file_name().to_string_lossy().contains(".part.") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        tokio::spawn(worker(c.clone()));
        tokio::spawn(purge(c.clone()));
        c
    }

    pub fn set_config(&self, cfg: &ServerConfig) {
        *self.dir.lock().unwrap() = cfg.clips_dir(&self.data_dir);
        self.keep_days.store(cfg.clips_keep_days, std::sync::atomic::Ordering::Relaxed);
    }

    /// Deletes a clip: its entry, its file and its still.
    pub async fn delete(&self, id: &str) -> Option<Clip> {
        let clip = {
            let mut list = self.clips.lock().unwrap();
            let i = list.iter().position(|c| c.id == id)?;
            list.remove(i)
        };
        self.save();
        let _ = tokio::fs::remove_file(&clip.file).await;
        let _ = tokio::fs::remove_file(self.thumb_path(id)).await;
        self.clips_changed();
        Some(clip)
    }

    pub fn dir(&self) -> PathBuf {
        self.dir.lock().unwrap().clone()
    }

    /// The clips of each recorder (by its id): how many, and their bytes.
    pub fn by_recorder(&self) -> std::collections::HashMap<String, (u64, u64)> {
        let mut out: std::collections::HashMap<String, (u64, u64)> = Default::default();
        for c in self.clips.lock().unwrap().iter() {
            let e = out.entry(c.recorder.clone()).or_default();
            e.0 += 1;
            e.1 += c.size;
        }
        out
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
        let dropped: Vec<String> = jobs.iter().filter(|j| j.recorder == id && j.state == JobState::Queued).map(|j| j.id.clone()).collect();
        jobs.retain(|j| !(j.recorder == id && j.state == JobState::Queued));
        drop(jobs);
        // Their part of the loop no longer needs keeping.
        for job in &dropped {
            self.recorders.unpin(job);
        }
        let running = self.running.lock().unwrap().as_ref().filter(|(_, r, _)| r == id).map(|(j, _, _)| j.clone());
        if let Some(job) = running {
            self.cancel_running(&job);
        }
        self.jobs_changed();
    }

    /// Stops the cutting of this job (only this one: the next may have started meanwhile).
    fn cancel_running(&self, job: &str) {
        if let Some((_, _, pid)) = self.running.lock().unwrap().as_ref().filter(|(j, _, _)| j == job) {
            // SAFETY: a signal to the FFmpeg this job started (still running, as it is still listed).
            unsafe {
                libc::kill(*pid as i32, libc::SIGKILL);
            }
        }
    }

    /// Checks a request and queues it (at most `MAX_WAITING` jobs waiting, `MAX_WAITING_EACH` an account).
    fn queue(&self, mut req: ClipRequest, by: &str, by_id: &str) -> Result<Job, String> {
        let rec = self.recorders.get(&req.recorder).ok_or("no such recorder")?;
        if req.kind == Kind::Image {
            req.frames = 1;
        }
        if req.frames == 0 {
            return Err("the clip has no frame".into());
        }
        if format_of(req.kind, &req.format).is_none() {
            return Err("this format is not one of this kind of clip".into());
        }
        let name = req.name.trim().to_string();
        // A queue that stays short: each waiting job holds its recording back from the loop's trim.
        {
            let jobs = self.jobs.lock().unwrap();
            let waiting = |j: &&Job| matches!(j.state, JobState::Queued | JobState::Running);
            if jobs.iter().filter(waiting).count() >= MAX_WAITING {
                return Err("too many clips are waiting: wait for some to be ready".into());
            }
            if jobs.iter().filter(waiting).filter(|j| j.by_id == by_id).count() >= MAX_WAITING_EACH {
                return Err("you have too many clips waiting: wait for some to be ready".into());
            }
        }
        // The clips' space: their limit, and the free margin of their disk.
        {
            let lim = self.recorders.limits.lock().unwrap().clone();
            let dir = self.dir();
            if lim.clips_max > 0 && crate::recorder::dir_size(&dir) >= lim.clips_max {
                return Err("the clips storage is full".into());
            }
            if let Some((total, free)) = crate::recorder::disk_space(&dir) {
                if free < lim.min_free(&dir, total) / 2 {
                    return Err("the disk of the clips is full".into());
                }
            }
        }
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
        let job = Job { id, name, recorder: req.recorder.clone(), state: JobState::Queued, progress: 0.0, message: String::new(), clip: None, req, by: by.to_string(), by_id: by_id.to_string(), ended: None };
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
        let (segs, media, wall, seg_t0) = {
            let runs = rec.runs.read().unwrap();
            let run = runs.iter().find(|r| r.meta.id == job.req.run).ok_or("this part of the loop was removed")?;
            let media = run.meta.media.clone().unwrap_or_default();
            let fps = if media.fps_den == 0 { 25.0 } else { media.fps() };
            let half = 0.5 / fps;
            let segs = run.covering(job.req.t - half, job.req.t + job.req.frames as f64 / fps - half).ok_or("this part of the loop was removed")?;
            let paths: Vec<PathBuf> = segs.iter().map(|s| run.master_path(s.n)).collect();
            (paths, media, run.wall_of(job.req.t).unwrap_or(0.0), segs.first().map(|s| s.t0).unwrap_or(0.0))
        };
        let fps = if media.fps_den == 0 { 25.0 } else { media.fps() };
        let (fps_num, fps_den) = if media.fps_den == 0 { (25, 1) } else { (media.fps_num, media.fps_den) };
        let dir = self.dir();
        tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("{}: {e}", dir.display()))?;
        let kind = job.req.kind;
        if kind == Kind::Audio && media.audio_tracks == 0 {
            return Err("this recording has no sound".into());
        }
        let (fmt, ext) = format_of(kind, &job.req.format).ok_or("this format is not one of this kind of clip")?;
        let part = dir.join(format!("{}.part.{ext}", job.id));
        let out = dir.join(format!("{}.{ext}", job.id));
        // The recorder's times go on past the 33-bit wrap; the files' start again from 0 at it.
        let base = (seg_t0 / crate::recorder::PTS_WRAP).floor() * crate::recorder::PTS_WRAP;
        let start = job.req.t - base - 0.5 / fps;
        let dur = job.req.frames as f64 / fps;
        let input = format!("concat:{}", segs.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("|"));
        let burn = kind == Kind::Video && job.req.subtitles && !media.subtitles.is_empty();
        let mut args: Vec<String> = vec!["-hide_banner".into(), "-nostdin".into(), "-v".into(), "error".into(), "-progress".into(), "pipe:1".into(), "-copyts".into()];
        // Teletext: its subtitle pages only, drawn as pictures.
        if burn && media.subtitles[0] == "dvb_teletext" {
            args.extend(["-txt_format", "bitmap", "-txt_page", "subtitle"].map(String::from));
        }
        args.extend(["-i".into(), input]);
        let sound = format!("{},atrim=start={start:.6}:end={:.6},asetpts=PTS-STARTPTS", crate::recorder::AUDIO_SYNC, start + dur);
        // The picture made smaller, deinterlaced (web, light), or kept as it is.
        let fit = |h: u32| format!("{}scale=-2:'min({h},ih)',format=yuv420p", if media.interlaced { "yadif," } else { "" });
        match (kind, fmt) {
            // The recording as it is, from the key picture before IN: no encoding (its own times).
            (Kind::Video, "copy") => {
                let from = (job.req.t - seg_t0 - 0.5 / fps).max(0.0);
                args = ["-hide_banner", "-nostdin", "-v", "error", "-progress", "pipe:1"].map(String::from).to_vec();
                args.extend(["-ss".into(), format!("{from:.6}"), "-i".into(), format!("concat:{}", segs.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("|"))]);
                args.extend(["-t".into(), format!("{dur:.6}")]);
                args.extend(["-map", "0:v:0", "-map", "0:a?", "-map", "0:s?", "-c", "copy", "-muxdelay", "0", "-f", "mpegts", "-y"].map(String::from));
            }
            (Kind::Video, _) => {
                let scale = match fmt {
                    "web" => format!(",{}", fit(1080)),
                    "light" => format!(",{}", fit(720)),
                    _ => String::new(),
                };
                if burn {
                    // The subtitles over the picture, on the source's timeline, then cut.
                    let (w, h) = (media.width.max(2), media.height.max(2));
                    args.extend(["-filter_complex".into(), format!("[0:s:0]scale={w}:{h}[s];[0:v:0][s]overlay=eof_action=pass,trim=start={start:.6},setpts=PTS-STARTPTS{scale}[v]"), "-map".into(), "[v]".into(), "-map".into(), "0:a?".into()]);
                } else {
                    args.extend(["-map", "0:v:0", "-map", "0:a?"].map(String::from));
                    args.extend(["-vf".into(), format!("trim=start={start:.6},setpts=PTS-STARTPTS{scale}")]);
                }
                args.extend(["-af".into(), sound]);
                args.extend(["-frames:v".into(), job.req.frames.to_string()]);
                match fmt {
                    "web" => args.extend(["-c:v", "libx264", "-preset", "fast", "-b:v", "6000k", "-maxrate", "6000k", "-bufsize", "12000k", "-profile:v", "high", "-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart"].map(String::from)),
                    "light" => args.extend(["-c:v", "libx264", "-preset", "fast", "-b:v", "2500k", "-maxrate", "2500k", "-bufsize", "5000k", "-profile:v", "high", "-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart"].map(String::from)),
                    // For editing: ProRes 422, the sound as it is (PCM).
                    "prores" => {
                        args.extend(["-c:v", "prores_ks", "-profile:v", "2", "-pix_fmt", "yuv422p10le"].map(String::from));
                        if media.interlaced {
                            args.extend(["-flags", "+ildct+ilme"].map(String::from));
                        }
                        args.extend(["-c:a", "pcm_s16le"].map(String::from));
                    }
                    _ => {
                        args.extend(["-c:v", "libx264", "-preset", "fast", "-crf", "16", "-profile:v", "high", "-pix_fmt", "yuv420p"].map(String::from));
                        if media.interlaced {
                            args.extend(["-flags", "+ildct+ilme"].map(String::from));
                        }
                        args.extend(["-c:a", "aac", "-b:a", "256k", "-movflags", "+faststart"].map(String::from));
                    }
                }
                args.push("-y".into());
            }
            // The first track (the one heard), in MP3.
            (Kind::Audio, "mp3") => {
                args.extend(["-map", "0:a:0", "-vn", "-af"].map(String::from));
                args.push(sound);
                args.extend(["-c:a", "libmp3lame", "-b:a", "192k", "-y"].map(String::from));
            }
            // Every track, side by side in one WAV (a WAV holds one stream).
            (Kind::Audio, "wav") => {
                let n = media.audio_tracks.max(1) as usize;
                if n > 1 {
                    let mut g: Vec<String> = (0..n).map(|k| format!("[0:a:{k}]{sound}[a{k}]")).collect();
                    g.push(format!("{}amerge=inputs={n}[a]", (0..n).map(|k| format!("[a{k}]")).collect::<String>()));
                    args.extend(["-filter_complex".into(), g.join(";"), "-map".into(), "[a]".into()]);
                } else {
                    args.extend(["-map", "0:a:0", "-af"].map(String::from));
                    args.push(sound);
                }
                args.extend(["-vn", "-c:a", "pcm_s16le", "-y"].map(String::from));
            }
            // Every sound track, as they are recorded.
            (Kind::Audio, _) => {
                args.extend(["-map", "0:a", "-vn", "-af"].map(String::from));
                args.push(sound);
                args.extend(["-c:a", "aac", "-b:a", "256k", "-movflags", "+faststart", "-y"].map(String::from));
            }
            // The picture under the playhead, whole (an interlaced one deinterlaced).
            (Kind::Image, _) => {
                args.extend(["-map", "0:v:0", "-an", "-vf"].map(String::from));
                args.push(format!("trim=start={start:.6},setpts=PTS-STARTPTS{}", if media.interlaced { ",yadif" } else { "" }));
                args.extend(["-frames:v", "1"].map(String::from));
                if fmt == "jpg" {
                    args.extend(["-q:v", "2"].map(String::from));
                }
                args.extend(["-update", "1", "-y"].map(String::from));
            }
        }
        args.push(part.to_string_lossy().into_owned());
        let mut cmd = tokio::process::Command::new(&self.recorders.ffmpeg);
        cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        crate::recorder::background(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("cannot start ffmpeg: {e}"))?;
        *self.running.lock().unwrap() = child.id().map(|p| (job.id.clone(), job.req.recorder.clone(), p));
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
            // How far it is: the time written (a sound alone has no frames).
            if let Some(us) = l.strip_prefix("out_time_us=").and_then(|f| f.trim().parse::<f64>().ok()) {
                if last.elapsed() > Duration::from_millis(400) && dur > 0.0 {
                    last = Instant::now();
                    let p = (us / 1e6 / dur).clamp(0.0, 0.99);
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
        let mut thumb_cmd = tokio::process::Command::new(&self.recorders.ffmpeg);
        crate::recorder::background(&mut thumb_cmd);
        thumb_cmd.args(["-v", "error"]);
        match kind {
            Kind::Video => {
                thumb_cmd.args(["-ss", &format!("{:.3}", if dur > 1.0 { dur / 2.0 } else { 0.0 }), "-i"]).arg(&out);
            }
            Kind::Image => {
                thumb_cmd.arg("-i").arg(&out);
            }
            // The sound alone: no picture (the list shows its icon).
            Kind::Audio => {}
        }
        if kind != Kind::Audio {
            let _ = thumb_cmd.args(["-vf", "scale=-2:144", "-frames:v", "1", "-q:v", "5", "-update", "1", "-y"]).arg(&thumb).stdin(Stdio::null()).status().await;
        }
        Ok(Clip {
            id: job.id.clone(),
            name: job.name.clone(),
            recorder: job.req.recorder.clone(),
            recorder_name: rec.config().name,
            start: wall.round() as i64,
            frames: job.req.frames,
            fps_num,
            fps_den,
            format: match fmt {
                "web" => "H.264 1080p · 6 Mb/s · AAC".into(),
                "light" => "H.264 720p · 2,5 Mb/s · AAC".into(),
                "prores" => "ProRes 422 · PCM".into(),
                "copy" => "MPEG-TS · copie".into(),
                "m4a" => format!("AAC · {} × 2.0", media.audio_tracks.max(1)),
                "mp3" => "MP3 · 192 kb/s".into(),
                "wav" => format!("WAV · {} canaux", media.audio_tracks.max(1) * 2),
                "jpg" => format!("JPEG · {}×{}", media.width, media.height),
                "png" => format!("PNG · {}×{}", media.width, media.height),
                _ => "H.264 · AAC".into(),
            },
            size,
            created_at: now_ms(),
            created_by: job.by.clone(),
            owner: job.by_id.clone(),
            share: Share::default(),
            kind,
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
        .route("/api/clips/{id}/share", axum::routing::put(share))
        .route("/api/clips/{id}/download", get(download))
        .route("/api/clips/{id}/thumb.jpg", get(thumb))
        .route("/api/jobs/{id}", delete(cancel))
        .route("/api/clips/space", get(space))
}

/// The free space where clips are made (the Extraction says when one would not fit).
async fn space(State(s): State<Shared>, CurrentUser(_me): CurrentUser) -> Json<Value> {
    let dir = s.clips.dir();
    let free = crate::api::blocking(move || crate::recorder::disk_space(&dir).map(|(_, f)| f)).await.ok().flatten();
    Json(json!({ "free": free }))
}

/// The clips this account sees: its own, those shared with it (an admin: all), each with who
/// made it, whether it is this account's, whether it is shared with it, and until when it is kept.
async fn list(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> Json<Value> {
    // Not allowed to see the clips: none.
    if !me.can("clips.view") {
        return Json(Value::Array(Vec::new()));
    }
    let groups = crate::groups::of(&s, &me.id);
    let users = s.auth.users();
    let keep = s.clips.keep_days.load(std::sync::atomic::Ordering::Relaxed) as i64;
    let mut list: Vec<Clip> = s.clips.clips.lock().unwrap().iter().filter(|c| c.visible(&me, &groups)).cloned().collect();
    list.sort_by_key(|c| std::cmp::Reverse(c.created_at));
    let out: Vec<Value> = list
        .iter()
        .map(|c| {
            let mut v = serde_json::to_value(c).unwrap_or_default();
            // Its maker's name now (renamed since), else as it was written.
            if let Some(u) = users.iter().find(|u| u.id == c.owner) {
                v["createdBy"] = json!(u.name());
            }
            v["mine"] = json!(c.mine(&me));
            v["sharedWithMe"] = json!(!c.mine(&me) && c.shared_with(&groups));
            v["expiresAt"] = if keep > 0 { json!(c.created_at + keep * 86_400_000) } else { Value::Null };
            v
        })
        .collect();
    Json(Value::Array(out))
}

/// A clip this account may act on: its own (an admin: any).
fn owned(s: &Shared, me: &crate::auth::User, id: &str) -> ApiResult<Clip> {
    let clip = s.clips.clips.lock().unwrap().iter().find(|c| c.id == id).cloned().ok_or_else(|| not_found("clip"))?;
    if me.is_admin() || clip.mine(me) {
        Ok(clip)
    } else if clip.visible(me, &crate::groups::of(s, &me.id)) {
        Err((StatusCode::FORBIDDEN, "only the one who made this clip can change it".into()))
    } else {
        Err(not_found("clip"))
    }
}

/// A clip this account sees.
fn seen(s: &Shared, me: &crate::auth::User, id: &str) -> ApiResult<Clip> {
    let clip = s.clips.clips.lock().unwrap().iter().find(|c| c.id == id).cloned().ok_or_else(|| not_found("clip"))?;
    if clip.visible(me, &crate::groups::of(s, &me.id)) {
        Ok(clip)
    } else {
        Err(not_found("clip"))
    }
}

/// Shares a clip with everyone, or with groups (empty: with nobody).
async fn share(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<Share>) -> ApiResult<StatusCode> {
    require(me.can("clips.share"))?;
    owned(&s, &me, &id)?;
    let known: Vec<String> = crate::groups::list(&s).into_iter().map(|g| g.id).collect();
    let mut groups: Vec<String> = b.groups.into_iter().filter(|g| known.contains(g)).collect();
    groups.sort();
    groups.dedup();
    {
        let mut list = s.clips.clips.lock().unwrap();
        let c = list.iter_mut().find(|c| c.id == id).ok_or_else(|| not_found("clip"))?;
        c.share = Share { all: b.all, groups };
    }
    s.clips.save();
    s.clips.clips_changed();
    Ok(StatusCode::NO_CONTENT)
}

async fn create(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(req): Json<ClipRequest>) -> ApiResult<(StatusCode, Json<Value>)> {
    require(me.can_extract())?;
    let job = s.clips.queue(req, me.name(), &me.id).map_err(bad)?;
    Ok((StatusCode::ACCEPTED, Json(serde_json::to_value(job).unwrap_or_default())))
}

async fn remove(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.can("clips.delete"))?;
    owned(&s, &me, &id)?;
    s.clips.delete(&id).await.ok_or_else(|| not_found("clip"))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.can_extract())?;
    let (state, by) = s.clips.jobs.lock().unwrap().iter().find(|j| j.id == id).map(|j| (j.state, j.by_id.clone())).ok_or_else(|| not_found("job"))?;
    if by != me.id && !me.is_admin() {
        return Err((StatusCode::FORBIDDEN, "only the one who asked for this clip can cancel it".into()));
    }
    match state {
        JobState::Running => s.clips.cancel_running(&id),
        _ => {
            s.clips.jobs.lock().unwrap().retain(|j| j.id != id);
            s.clips.recorders.unpin(&id);
            s.clips.jobs_changed();
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// "Plateau 1 / 14:32" → "Plateau 1 - 14-32.mp4" (no path characters), with the file's extension.
fn file_name(name: &str, ext: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '-' } else { c }).collect();
    format!("{}.{ext}", clean.trim())
}

async fn download(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, req: Request) -> ApiResult<Response> {
    let clip = seen(&s, &me, &id)?;
    // `?inline=1`: played in the page (the clips' player), not saved.
    let inline = req.uri().query().is_some_and(|q| q.split('&').any(|p| p == "inline=1"));
    let mut res = tower_http::services::ServeFile::new(&clip.file).oneshot(req).await.map_err(internal)?.into_response();
    if inline {
        return Ok(res);
    }
    let name = file_name(&clip.name, clip.file.extension().and_then(|e| e.to_str()).unwrap_or_else(|| clip.kind.ext()));
    let ascii: String = name.chars().map(|c| if c.is_ascii() && c != '"' { c } else { '_' }).collect();
    let encoded: String = name.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")) {
        res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

async fn thumb(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Response> {
    seen(&s, &me, &id)?;
    let data = tokio::fs::read(s.clips.thumb_path(&id)).await.map_err(|_| not_found("image"))?;
    let mut r = Response::new(axum::body::Body::from(data));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    Ok(r)
}

/// Système › Stockage: the clips older than the days kept are deleted, every half hour.
async fn purge(c: Arc<Clips>) {
    tokio::time::sleep(Duration::from_secs(60)).await;
    loop {
        let days = c.keep_days.load(std::sync::atomic::Ordering::Relaxed);
        if days > 0 {
            let cutoff = now_ms() - days as i64 * 86_400_000;
            let old: Vec<(String, String)> = c.clips.lock().unwrap().iter().filter(|x| x.created_at < cutoff).map(|x| (x.id.clone(), x.name.clone())).collect();
            for (id, name) in &old {
                c.delete(id).await;
                tracing::info!("clip \"{name}\" deleted: older than {days} days");
            }
        }
        tokio::time::sleep(Duration::from_secs(1800)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_sees_a_clip() {
        use crate::auth::{Role, User};
        let ana = User::without_password("ana".into(), "Ana".into(), Role::Operator, String::new());
        let bob = User::without_password("bob".into(), "Bob".into(), Role::Viewer, String::new());
        let boss = User::without_password("boss".into(), String::new(), Role::Admin, String::new());
        let mut c = Clip { id: "c".into(), name: "x".into(), recorder: "r".into(), recorder_name: "R".into(), start: 0, frames: 1, fps_num: 25, fps_den: 1, format: String::new(), size: 0, created_at: 0, created_by: "Ana".into(), owner: ana.id.clone(), share: Share::default(), kind: Kind::Video, file: PathBuf::new() };
        assert!(c.visible(&ana, &[]) && !c.visible(&bob, &[]) && c.visible(&boss, &[]));
        c.share.groups = vec!["g".into()];
        assert!(c.visible(&bob, &["g".into()]) && !c.visible(&bob, &["h".into()]));
        c.share = Share { all: true, groups: vec![] };
        assert!(c.visible(&bob, &[]) && !c.mine(&bob));
        // A clip made before the accounts were kept: by its maker's name.
        let old = Clip { owner: String::new(), share: Share::default(), ..c };
        assert!(old.mine(&ana) && !old.mine(&bob));
    }

    #[test]
    fn file_names() {
        assert_eq!(super::file_name("Plateau 1 / 14:32", "mp4"), "Plateau 1 - 14-32.mp4");
        assert_eq!(super::file_name("Son", "m4a"), "Son.m4a");
        assert_eq!(super::format_of(super::Kind::Video, ""), Some(("master", "mp4")));
        assert_eq!(super::format_of(super::Kind::Video, "prores"), Some(("prores", "mov")));
        assert_eq!(super::format_of(super::Kind::Audio, "prores"), None);
    }
}
