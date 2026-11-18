//! A recorder's FFmpeg process, and what it writes turned into the index.
use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};
use tokio::{io::AsyncBufReadExt, process::Command, sync::watch};

use super::{
    index::{mp4, MediaInfo, PSeg, Run, RunMeta, Seg},
    now_ms, Manager, Mode, Protocol, Rec, RecorderConfig, State,
};

/// Length of the original segments, seconds (cut at the source's key frames).
const SEGMENT: u32 = 6;
/// Length of the proxy segments, seconds.
const PROXY_SEGMENT: u32 = 4;

enum Outcome {
    /// The settings changed: start again at once.
    Restart,
    /// The recorder was deleted or the server stops.
    Stopped,
    /// FFmpeg ended by itself.
    Exited { had_data: bool, message: String },
}

pub async fn supervise(m: Arc<Manager>, rec: Arc<Rec>) {
    let mut cfg_rx = rec.cfg.subscribe();
    let mut stop_rx = rec.stop.subscribe();
    let mut failures = 0u32;
    let mut diagnosis = String::new();
    loop {
        if *stop_rx.borrow() {
            break;
        }
        let cfg = cfg_rx.borrow_and_update().clone();
        if !cfg.enabled {
            rec.set_state(State::Off, "");
            rec.refresh_status();
            tokio::select! { _ = cfg_rx.changed() => {}, _ = stop_rx.changed() => {} }
            rec.no_audio.store(false, Ordering::Relaxed);
            continue;
        }
        if rec.status.lock().unwrap().state == State::Off {
            rec.set_state(State::Starting, "");
        }
        match record_once(&m, &rec, &cfg, &mut cfg_rx, &mut stop_rx).await {
            Outcome::Restart => {
                rec.no_audio.store(false, Ordering::Relaxed);
                failures = 0;
            }
            Outcome::Stopped => break,
            Outcome::Exited { had_data, message } => {
                if had_data {
                    failures = 0;
                    rec.set_state(State::Nosignal, "");
                } else {
                    failures += 1;
                    // A setting that cannot work is an error; anything else means the source is not there.
                    let broken = ["Address already in use", "Cannot assign requested address", "Permission denied", "Protocol not found", "Invalid argument", "cannot start", "Error opening output", "No space left"].iter().any(|k| message.contains(k));
                    if broken {
                        rec.set_state(State::Error, message);
                    } else {
                        // SRT as caller: FFmpeg only says "Input/output error"; ask once in a while why.
                        if cfg.protocol == Protocol::Srt && cfg.mode == Mode::Caller && (failures == 1 || failures.is_multiple_of(10)) {
                            if let Some(url) = input_args(&cfg).last() {
                                diagnosis = srt_diagnose(&m.ffmpeg, url).await;
                                if !diagnosis.is_empty() {
                                    tracing::warn!("{}: {diagnosis}", cfg.name);
                                }
                            }
                        }
                        let reason = match cfg.protocol {
                            Protocol::Srt if cfg.mode == Mode::Caller => diagnosis.clone(),
                            Protocol::Udp | Protocol::Rtp => udp_reason(&message),
                            _ => String::new(),
                        };
                        rec.set_state(State::Nosignal, reason);
                    }
                }
                rec.refresh_status();
                let wait = Duration::from_secs((1 + failures.min(5) * 2) as u64);
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    r = cfg_rx.changed() => { if r.is_ok() { rec.no_audio.store(false, Ordering::Relaxed); failures = 0; } }
                    _ = stop_rx.changed() => {}
                }
            }
        }
    }
    rec.set_state(State::Off, "");
    rec.refresh_status();
}

/// Two settings that record the same way (only the name or the loop length differ).
fn same_source(a: &RecorderConfig, b: &RecorderConfig) -> bool {
    let strip = |c: &RecorderConfig| RecorderConfig { name: String::new(), loop_hours: 0, bitrate: 0.0, ..c.clone() };
    strip(a) == strip(b)
}

/// The most telling line of FFmpeg's last words (not the generic "Error opening input files").
fn best_line(lines: &[String]) -> String {
    let generic = |l: &str| {
        let l = clean_message(l);
        l.is_empty() || l.starts_with("Error opening input") || l.starts_with("Exiting") || l.starts_with("Conversion failed")
    };
    lines.iter().rev().find(|l| !generic(l)).or_else(|| lines.iter().rev().find(|l| !l.trim().is_empty())).cloned().unwrap_or_default()
}

/// What a failing UDP / RTP input means.
pub fn udp_reason(message: &str) -> String {
    if message.contains("IP_ADD_MEMBERSHIP") || message.contains("No such device") {
        "udp: cannot join the multicast on this network card".into()
    } else if message.contains("Input/output error") || message.contains("timed out") || message.is_empty() {
        "udp: nothing received (address, network card, multicast)".into()
    } else {
        String::new()
    }
}

/// Why an SRT call fails: FFmpeg hides it, SRT tells it in debug logs.
async fn srt_diagnose(ffmpeg: &str, url: &str) -> String {
    let run = tokio::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "debug", "-i", url, "-t", "0.04", "-f", "null", "-"])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(15), run).await else { return String::new() };
    // The SRT library writes its own log lines on stdout, FFmpeg on stderr.
    let mut log = String::from_utf8_lossy(&out.stdout).into_owned();
    log.push_str(&String::from_utf8_lossy(&out.stderr));
    srt_reason(&log)
}

/// "…rejection HS response: ERROR:BADSECRET" → "srt: wrong passphrase".
pub fn srt_reason(log: &str) -> String {
    if let Some(code) = log.lines().find_map(|l| l.split("rejection HS response: ERROR:").nth(1)) {
        let code = code.split_whitespace().next().unwrap_or("");
        return match code {
            "BADSECRET" => "srt: wrong passphrase",
            "UNSECURE" => "srt: passphrase missing on one side",
            "BACKLOG" | "RESOURCE" => "srt: the source accepts no more connections",
            "VERSION" => "srt: srt versions do not match",
            "CONGESTION" | "FILTER" | "MESSAGEAPI" | "GROUP" => "srt: settings refused by the source",
            "TIMEOUT" => "srt: no answer from the source (address, port or firewall)",
            _ => "srt: connection refused by the source (stream id?)",
        }
        .into();
    }
    if log.contains("Input #0") {
        return String::new(); // it connects now
    }
    if log.contains("Connection to srt://") && log.contains("failed") {
        return "srt: no answer from the source (address, port or firewall)".into();
    }
    String::new()
}

/// The FFmpeg input for a recorder.
pub fn input_args(c: &RecorderConfig) -> Vec<String> {
    let join = |url: &str, extra: &str| if url.contains('?') { format!("{url}&{extra}") } else { format!("{url}?{extra}") };
    let mut a: Vec<String> = Vec::new();
    let url = match c.protocol {
        Protocol::Udp => {
            let mut extra = "fifo_size=1000000&overrun_nonfatal=1&buffer_size=8388608&timeout=5000000".to_string();
            if !c.iface.is_empty() {
                extra = format!("localaddr={}&{extra}", c.iface);
            }
            join(&c.url, &extra)
        }
        Protocol::Rtp => {
            let mut extra = "timeout=5000000&buffer_size=8388608".to_string();
            if !c.iface.is_empty() {
                extra = format!("localaddr={}&{extra}", c.iface);
            }
            join(&c.url, &extra)
        }
        Protocol::Rtmp => match c.mode {
            Mode::Listener => {
                a.extend(["-listen".into(), "1".into()]);
                format!("rtmp://0.0.0.0:{}/{}", c.port, c.path)
            }
            Mode::Caller => c.url.clone(),
        },
        Protocol::Srt => {
            let mut extra = format!("latency={}", c.latency as u64 * 1000);
            if !c.passphrase.is_empty() {
                extra.push_str(&format!("&passphrase={}", urlencode(&c.passphrase)));
            }
            match c.mode {
                Mode::Listener => join(&format!("srt://0.0.0.0:{}", c.port), &format!("mode=listener&{extra}")),
                Mode::Caller => join(&c.url, &format!("mode=caller&{extra}")),
            }
        }
    };
    a.extend(["-i".into(), url]);
    a
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// The whole FFmpeg command line (without the program).
pub fn ffmpeg_args(c: &RecorderConfig, run_dir: &Path, thumb: &Path, audio: bool) -> Vec<String> {
    let d = |p: &str| run_dir.join(p).to_string_lossy().into_owned();
    let mut a: Vec<String> = ["-hide_banner", "-nostdin", "-y", "-loglevel", "warning", "-copyts", "-fflags", "+discardcorrupt", "-thread_queue_size", "4096"].map(String::from).to_vec();
    a.extend(input_args(c));
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    // 1. The stream as received, in segments.
    a.extend(s(&["-map", "0:v:0", "-map", "0:a?", "-c", "copy", "-muxdelay", "0", "-muxpreload", "0", "-f", "segment", "-segment_time", &SEGMENT.to_string(), "-segment_format", "mpegts", "-reset_timestamps", "0", "-segment_list_type", "csv", "-segment_list_flags", "+live", "-segment_list"]));
    a.push(d("master/list.csv"));
    a.push(d("master/s%08d.ts"));
    // 2. The browser proxy, video: same frames, same timestamps, light.
    let keyframes = "expr:if(isnan(prev_forced_t),1,gte(t,prev_forced_t+1))";
    a.extend(s(&["-map", "0:v:0", "-vf", "yadif=deint=interlaced,scale=-2:'min(540,ih)',format=yuv420p", "-c:v", "libx264", "-preset", "veryfast", "-profile:v", "main", "-bf", "0", "-force_key_frames", keyframes, "-b:v", "1200k", "-maxrate", "1600k", "-bufsize", "2400k", "-an", "-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", &PROXY_SEGMENT.to_string(), "-hls_list_size", "10", "-hls_flags", "+independent_segments", "-hls_fmp4_init_filename", "init.mp4", "-hls_segment_filename"]));
    a.push(d("video/p%08d.m4s"));
    a.push(d("video/index.m3u8"));
    // 3. The browser proxy, audio (first track, stereo).
    if audio {
        a.extend(s(&["-map", "0:a:0", "-vn", "-c:a", "aac", "-b:a", "128k", "-ac", "2", "-ar", "48000", "-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", &PROXY_SEGMENT.to_string(), "-hls_list_size", "10", "-hls_fmp4_init_filename", "init.mp4", "-hls_segment_filename"]));
        a.push(d("audio/p%08d.m4s"));
        a.push(d("audio/index.m3u8"));
    }
    // 4. A still every 2 s for the recorder's card.
    a.extend(s(&["-map", "0:v:0", "-vf", "fps=1/2,yadif=deint=interlaced,scale=480:-2", "-q:v", "5", "-update", "1", "-atomic_writing", "1"]));
    a.push(thumb.to_string_lossy().into_owned());
    a
}

async fn record_once(m: &Arc<Manager>, rec: &Arc<Rec>, cfg: &RecorderConfig, cfg_rx: &mut watch::Receiver<RecorderConfig>, stop_rx: &mut watch::Receiver<bool>) -> Outcome {
    let base = m.recordings_dir().join(&rec.id);
    *rec.dir.lock().unwrap() = base.clone();
    let started = now_ms();
    let run_id = started.to_string();
    let run_dir = base.join(&run_id);
    for sub in [super::index::MASTER, super::index::VIDEO, super::index::AUDIO] {
        if let Err(e) = std::fs::create_dir_all(run_dir.join(sub)) {
            return Outcome::Exited { had_data: false, message: format!("{}: {e}", run_dir.display()) };
        }
    }
    let run = Run::new(run_dir.clone(), RunMeta { id: run_id.clone(), started, media: None });
    run.save_meta();
    rec.runs.write().unwrap().push(run);

    let audio = !rec.no_audio.load(Ordering::Relaxed);
    let args = ffmpeg_args(cfg, &run_dir, &base.join("thumb.jpg"), audio);
    tracing::debug!("{}: ffmpeg {}", cfg.name, args.join(" "));
    let child = Command::new(&m.ffmpeg).args(&args).current_dir(&run_dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            finish_run(rec, &run_id);
            return Outcome::Exited { had_data: false, message: format!("cannot start {}: {e}", m.ffmpeg) };
        }
    };
    let log: Arc<Mutex<VecDeque<String>>> = Default::default();
    if let Some(err) = child.stderr.take() {
        let log = log.clone();
        let name = cfg.name.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                tracing::debug!("{name}: {l}");
                let mut q = log.lock().unwrap();
                q.push_back(l);
                if q.len() > 30 {
                    q.pop_front();
                }
            }
        });
    }
    tracing::info!("{}: recording ({})", cfg.name, run_dir.display());

    let mut scan = Scanner::default();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let outcome = loop {
        tokio::select! {
            status = child.wait() => {
                tokio::time::sleep(Duration::from_millis(200)).await; // the last lines of the lists
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                let lines: Vec<String> = log.lock().unwrap().iter().cloned().collect();
                if lines.iter().any(|l| l.contains("matches no streams") && l.contains("0:a:0")) && audio {
                    rec.no_audio.store(true, Ordering::Relaxed);
                    tracing::info!("{}: no audio in the source", cfg.name);
                    break Outcome::Exited { had_data: scan.had_data, message: String::new() };
                }
                let message = best_line(&lines);
                match status {
                    Ok(s) if s.success() => tracing::info!("{}: the source stopped", cfg.name),
                    Ok(s) => tracing::warn!("{}: ffmpeg ended ({s}): {message}", cfg.name),
                    Err(e) => tracing::warn!("{}: ffmpeg: {e}", cfg.name),
                }
                break Outcome::Exited { had_data: scan.had_data, message: clean_message(&message) };
            }
            r = cfg_rx.changed() => {
                // The name or the loop length alone: no need to stop recording.
                if r.is_ok() && same_source(&cfg_rx.borrow(), cfg) {
                    continue;
                }
                stop_child(&mut child).await;
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                break if r.is_ok() { Outcome::Restart } else { Outcome::Stopped };
            }
            _ = stop_rx.changed() => {
                stop_child(&mut child).await;
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                break Outcome::Stopped;
            }
            _ = rec.new_run.notified() => {
                // Only when this run is not already in the loops' current folder.
                if run_dir.starts_with(m.recordings_dir()) {
                    continue;
                }
                stop_child(&mut child).await;
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                break Outcome::Restart;
            }
            _ = tick.tick() => {
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, false));
                measure(m, rec, &run_id, std::mem::take(&mut scan.waves));
            }
        }
    };
    finish_run(rec, &run_id);
    outcome
}

/// Measures new audio fragments for the timeline's waveform, off the async threads.
fn measure(m: &Arc<Manager>, rec: &Arc<Rec>, run_id: &str, todo: Vec<(u32, PathBuf, PathBuf, PathBuf)>) {
    if todo.is_empty() {
        return;
    }
    let (ffmpeg, rec, run_id) = (m.ffmpeg.clone(), rec.clone(), run_id.to_string());
    tokio::task::spawn_blocking(move || {
        for (n, init, seg, out) in todo {
            let Some(w) = super::index::wave::compute(&ffmpeg, &init, &seg) else { continue };
            let _ = std::fs::write(&out, &w);
            let mut runs = rec.runs.write().unwrap();
            if let Some(r) = runs.iter_mut().find(|r| r.meta.id == run_id) {
                if r.audio.segs.iter().any(|s| s.n == n) {
                    r.audio.waves.insert(n, Arc::new(w));
                }
            }
        }
    });
}

/// "[in#0 @ 0x55…] Error opening input: Connection refused" → "Error opening input: Connection refused".
fn clean_message(l: &str) -> String {
    let l = l.trim();
    let l = if l.starts_with('[') { l.split_once("] ").map(|(_, r)| r).unwrap_or(l) } else { l };
    l.chars().take(200).collect()
}

/// SIGINT first, so FFmpeg closes its segments and lists; kill after 5 s.
async fn stop_child(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as i32, libc::SIGINT);
        }
        if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_ok() {
            return;
        }
    }
    let _ = child.kill().await;
}

/// The run is no longer written; an empty one is removed.
fn finish_run(rec: &Rec, run_id: &str) {
    let mut runs = rec.runs.write().unwrap();
    if let Some(i) = runs.iter().position(|r| r.meta.id == run_id) {
        runs[i].live = false;
        if runs[i].master.is_empty() {
            let dir = runs[i].dir.clone();
            runs.remove(i);
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Follows what FFmpeg writes.
#[derive(Default)]
struct Scanner {
    csv_offset: u64,
    /// Server time minus source time, ms (the smallest delay seen, drifting slowly).
    anchor: Option<f64>,
    last_video: Option<u32>,
    last_audio: Option<u32>,
    probed: bool,
    had_data: bool,
    growing_size: u64,
    still_ticks: u32,
    /// New audio fragments to measure: (number, init, fragment, level file).
    waves: Vec<(u32, PathBuf, PathBuf, PathBuf)>,
}

impl Scanner {
    fn scan(&mut self, m: &Manager, rec: &Rec, run_id: &str, last: bool) {
        let Some(dir) = rec.runs.read().unwrap().iter().find(|r| r.meta.id == run_id).map(|r| r.dir.clone()) else { return };
        let now = now_ms() as f64;
        let new_master = self.read_csv(&m.ffmpeg, &dir, now);
        let new_video = self.read_proxy(&dir, true);
        let new_audio = if rec.no_audio.load(Ordering::Relaxed) { Vec::new() } else { self.read_proxy(&dir, false) };
        let mut media = None;
        if !self.probed {
            if let Some(first) = new_master.first() {
                self.probed = true;
                media = probe(&m.ffmpeg, &dir.join(super::index::MASTER).join(format!("s{:08}.ts", first.n)));
            }
        }
        {
            let mut runs = rec.runs.write().unwrap();
            let Some(run) = runs.iter_mut().find(|r| r.meta.id == run_id) else { return };
            for s in new_master {
                run.append_master(s);
            }
            for (video, segs) in [(true, new_video), (false, new_audio)] {
                if run.track(video).offset.is_none() {
                    if let Ok(init) = std::fs::read(run.track_dir(video).join("init.mp4")) {
                        if let Some((offset, ts)) = mp4::track_offset(&init) {
                            let t = run.track_mut(video);
                            t.offset = Some(offset);
                            t.timescale = ts;
                        }
                    }
                }
                let Some(offset) = run.track(video).offset else { continue };
                let ts = run.track(video).timescale.max(1) as f64;
                for (n, dur, tfdt, bytes) in segs {
                    let t0 = offset + tfdt as f64 / ts;
                    run.append_proxy(video, PSeg { n, t0, t1: t0 + dur, bytes });
                    if !video {
                        self.waves.push((n, run.track_dir(false).join("init.mp4"), run.proxy_path(false, n), run.wave_path(n)));
                    }
                }
            }
            if media.is_some() {
                run.meta.media = media;
                run.save_meta();
            }
        }
        if last {
            return;
        }
        // Is the stream still coming? The segment being written grows.
        let size = newest_size(&dir.join(super::index::MASTER));
        if size != self.growing_size {
            self.growing_size = size;
            self.still_ticks = 0;
            if size > 0 {
                self.had_data = true;
                rec.set_state(State::Recording, "");
            }
        } else {
            self.still_ticks += 1;
            if self.still_ticks == 8 && self.had_data {
                rec.set_state(State::Nosignal, "");
            }
        }
    }

    fn read_csv(&mut self, ffmpeg: &str, dir: &Path, now: f64) -> Vec<Seg> {
        let path = dir.join(super::index::MASTER).join("list.csv");
        let Ok(mut f) = std::fs::File::open(&path) else { return Vec::new() };
        if f.seek(SeekFrom::Start(self.csv_offset)).is_err() {
            return Vec::new();
        }
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_err() {
            return Vec::new();
        }
        let Some(end) = buf.rfind('\n') else { return Vec::new() };
        self.csv_offset += end as u64 + 1;
        let mut out = Vec::new();
        for line in buf[..end].lines() {
            let mut it = line.split(',');
            let (Some(name), Some(t0), Some(t1)) = (it.next(), it.next(), it.next()) else { continue };
            let (Some(n), Ok(mut t0), Ok(t1)) = (seq_of(name), t0.parse::<f64>(), t1.parse::<f64>()) else { continue };
            let file = dir.join(super::index::MASTER).join(name);
            // The first segment says it starts at 0: read its real start.
            if self.anchor.is_none() || t1 - t0 > (SEGMENT * 4) as f64 {
                t0 = start_time(ffmpeg, &file).unwrap_or(t1 - SEGMENT as f64);
            }
            if t1 <= t0 {
                continue;
            }
            // The segment closed when its last packet arrived: now.
            let sample = now - t1 * 1000.0;
            let anchor = match self.anchor {
                None => sample,
                Some(a) if sample < a => sample,
                Some(a) => a + (sample - a) * 0.02,
            };
            self.anchor = Some(anchor);
            let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            out.push(Seg { n, t0, t1, wall0: anchor + t0 * 1000.0, bytes });
        }
        out
    }

    /// New proxy fragments: (number, duration, first decode time).
    fn read_proxy(&mut self, dir: &Path, video: bool) -> Vec<(u32, f64, u64, u64)> {
        let tdir = dir.join(if video { super::index::VIDEO } else { super::index::AUDIO });
        let Ok(list) = std::fs::read_to_string(tdir.join("index.m3u8")) else { return Vec::new() };
        let last = if video { &mut self.last_video } else { &mut self.last_audio };
        let mut out = Vec::new();
        let mut dur = None;
        for line in list.lines() {
            if let Some(d) = line.strip_prefix("#EXTINF:") {
                dur = d.trim_end_matches(',').split(',').next().and_then(|d| d.parse::<f64>().ok());
                continue;
            }
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let (Some(n), Some(d)) = (seq_of(line), dur.take()) else { continue };
            if last.is_some_and(|l| n <= l) {
                continue;
            }
            let mut head = vec![0u8; 64 * 1024];
            let Ok(mut f) = std::fs::File::open(tdir.join(line)) else { continue };
            let len = f.read(&mut head).unwrap_or(0);
            head.truncate(len);
            let Some(tfdt) = mp4::first_tfdt(&head) else { continue };
            *last = Some(n);
            let bytes = f.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((n, d, tfdt, bytes));
        }
        out
    }
}

/// "s00000012.ts" → 12.
fn seq_of(name: &str) -> Option<u32> {
    let stem = Path::new(name).file_stem()?.to_str()?;
    stem.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().ok()
}

fn newest_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".ts")).max_by_key(|e| e.file_name()).and_then(|e| e.metadata().ok()).map(|m| m.len()).unwrap_or(0))
        .unwrap_or(0)
}

fn ffprobe_of(ffmpeg: &str) -> PathBuf {
    let p = PathBuf::from(ffmpeg);
    match p.file_name().and_then(|n| n.to_str()) {
        Some(n) if n.contains("ffmpeg") => p.with_file_name(n.replace("ffmpeg", "ffprobe")),
        _ => PathBuf::from("ffprobe"),
    }
}

fn start_time(ffmpeg: &str, file: &Path) -> Option<f64> {
    let out = std::process::Command::new(ffprobe_of(ffmpeg)).args(["-v", "error", "-show_entries", "format=start_time", "-of", "csv=p=0"]).arg(file).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Codec, size, frame rate, scan and audio tracks of a segment.
pub fn probe(ffmpeg: &str, file: &Path) -> Option<MediaInfo> {
    let out = std::process::Command::new(ffprobe_of(ffmpeg)).args(["-v", "error", "-show_entries", "stream=codec_type,codec_name,width,height,r_frame_rate,avg_frame_rate,field_order", "-of", "json"]).arg(file).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let streams = v["streams"].as_array()?;
    let video = streams.iter().find(|s| s["codec_type"] == "video")?;
    // avg_frame_rate counts frames (an interlaced stream may give its field rate as r_frame_rate).
    let rate = video["avg_frame_rate"].as_str().filter(|r| *r != "0/0").or(video["r_frame_rate"].as_str().filter(|r| *r != "0/0")).unwrap_or("25/1");
    let (num, den) = rate.split_once('/').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).unwrap_or((25, 1));
    let field = video["field_order"].as_str().unwrap_or("progressive");
    let interlaced = matches!(field, "tt" | "bb" | "tb" | "bt");
    Some(MediaInfo {
        codec: video["codec_name"].as_str().unwrap_or("").to_string(),
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        fps_num: num,
        fps_den: den,
        interlaced,
        audio_tracks: streams.iter().filter(|s| s["codec_type"] == "audio").count() as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(protocol: Protocol, mode: Mode) -> RecorderConfig {
        RecorderConfig { id: "x".into(), name: "x".into(), protocol, url: "udp://239.1.1.1:5000".into(), mode, port: 9000, path: "live/a".into(), latency: 120, passphrase: String::new(), iface: "10.0.0.2".into(), loop_hours: 24, bitrate: 8.0, enabled: true }
    }

    #[test]
    fn inputs() {
        assert_eq!(input_args(&cfg(Protocol::Udp, Mode::Listener)).last().unwrap(), "udp://239.1.1.1:5000?localaddr=10.0.0.2&fifo_size=1000000&overrun_nonfatal=1&buffer_size=8388608&timeout=5000000");
        let r = input_args(&cfg(Protocol::Rtmp, Mode::Listener));
        assert_eq!(r, vec!["-listen", "1", "-i", "rtmp://0.0.0.0:9000/live/a"]);
        let mut c = cfg(Protocol::Srt, Mode::Listener);
        c.passphrase = "secret pass&1".into();
        assert_eq!(input_args(&c).last().unwrap(), "srt://0.0.0.0:9000?mode=listener&latency=120000&passphrase=secret%20pass%261");
    }

    #[test]
    fn udp_reasons() {
        let lines: Vec<String> = ["[udp @ 0x1] setsockopt(IP_ADD_MEMBERSHIP): No such device", "Error opening input file udp://x.", "Error opening input files: No such device"].map(String::from).to_vec();
        assert!(best_line(&lines).contains("IP_ADD_MEMBERSHIP"));
        assert_eq!(udp_reason(&best_line(&lines)), "udp: cannot join the multicast on this network card");
        assert_eq!(udp_reason("Error opening input files: Input/output error"), "udp: nothing received (address, network card, multicast)");
    }

    #[test]
    fn srt_reasons() {
        assert_eq!(srt_reason("x processConnectResponse: rejecting per reception of a rejection HS response: ERROR:BADSECRET\n"), "srt: wrong passphrase");
        assert_eq!(srt_reason("…rejection HS response: ERROR:UNSECURE"), "srt: passphrase missing on one side");
        assert_eq!(srt_reason("…rejection HS response: ERROR:1403"), "srt: connection refused by the source (stream id?)");
        assert_eq!(srt_reason("[srt @ 0x1] Connection to srt://1.2.3.4:9000 failed: Input/output error"), "srt: no answer from the source (address, port or firewall)");
        assert_eq!(srt_reason("Input #0, mpegts"), "");
    }

    #[test]
    fn sequence_numbers() {
        assert_eq!(seq_of("s00000012.ts"), Some(12));
        assert_eq!(seq_of("p00000003.m4s"), Some(3));
        assert_eq!(seq_of("init.mp4"), None);
    }
}
