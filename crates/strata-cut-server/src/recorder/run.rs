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
/// Sound kept on its timestamps: a hole of more than 10 ms is filled with silence, an
/// overlap cut (by default FFmpeg joins the samples and the sound drifts ahead after a loss).
pub const AUDIO_SYNC: &str = "aresample=async=1:min_hard_comp=0.01";

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
        // Stopped: it records only during its programmed slots, from a minute before
        // (a recorder already recording just goes on: its slots change nothing).
        if !cfg.enabled {
            let now = now_ms();
            let tz = m.tz();
            if !m.wants(&rec.id, now) {
                let next = m.schedules.next(&rec.id, &tz, now);
                rec.set_state(if next.is_some() { State::Waiting } else { State::Off }, "");
                rec.refresh_status();
                let wait = next.map(|(_, a, _)| (a - super::PREROLL_MS - now).clamp(0, 30_000)).unwrap_or(30_000);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(wait as u64)) => {}
                    r = cfg_rx.changed() => { if r.is_ok() { rec.no_audio.store(false, Ordering::Relaxed); } }
                    _ = stop_rx.changed() => {}
                    _ = m.schedules.changed.notified() => {}
                }
                continue;
            }
            if matches!(rec.status.lock().unwrap().state, State::Off | State::Waiting) {
                let slot = m.schedules.active(&rec.id, &tz, now).or_else(|| m.schedules.active(&rec.id, &tz, now + super::PREROLL_MS));
                if let Some((slot, a, b)) = slot {
                    let hm = |ms: i64| jiff::Timestamp::from_millisecond(ms).map(|t| t.to_zoned(tz.clone()).strftime("%H:%M").to_string()).unwrap_or_default();
                    tracing::info!("{}: programmed slot \"{}\" {}–{}: recording starts", cfg.name, slot.name, hm(a), hm(b));
                }
            }
        }
        if matches!(rec.status.lock().unwrap().state, State::Off | State::Waiting) {
            rec.set_state(State::Starting, "");
        }
        match record_once(&m, &rec, &cfg, &mut cfg_rx, &mut stop_rx).await {
            Outcome::Restart => {
                rec.no_audio.store(false, Ordering::Relaxed);
                failures = 0;
            }
            Outcome::Stopped => break,
            Outcome::Exited { had_data, message } => {
                let bot = message == super::web::BOT || message == super::web::BOT_COOKIES;
                if had_data {
                    failures = 0;
                    rec.set_state(State::Nosignal, "");
                } else {
                    failures += 1;
                    // A setting that cannot work is an error; anything else means the source is not there.
                    let broken = ["Address already in use", "Cannot assign requested address", "Permission denied", "Protocol not found", "Invalid argument", "cannot start", "Error opening output", "No space left", "not installed", "javascript runtime"].iter().any(|k| message.contains(k))
                        // A card that cannot be captured (missing module, card not found, settings refused).
                        || (cfg.protocol == Protocol::Decklink && message.starts_with("decklink: "));
                    if broken {
                        // Two recorders on the same UDP address and port: only one can receive it.
                        let message = if matches!(cfg.protocol, Protocol::Udp | Protocol::Rtp) && message.contains("Address already in use") { "udp: this address and port are already received by another recorder".to_string() } else { message };
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
                            Protocol::Web if message.starts_with("web: ") => message.clone(),
                            _ => String::new(),
                        };
                        // Why, in the journal: at the first failure, then every tenth.
                        if !reason.is_empty() && (failures == 1 || failures.is_multiple_of(10)) && cfg.protocol != Protocol::Web && !(cfg.protocol == Protocol::Srt && cfg.mode == Mode::Caller) {
                            tracing::warn!("{}: {reason}", cfg.name);
                        }
                        rec.set_state(State::Nosignal, reason);
                    }
                }
                rec.refresh_status();
                // A web page is asked less often (the platforms limit the requests): 15 s, 30 s, then every
                // minute; every 10 minutes when YouTube checks for bots (asking more often makes it worse).
                let wait = if cfg.protocol == Protocol::Web && bot {
                    Duration::from_secs(600)
                } else if cfg.protocol == Protocol::Web && !had_data {
                    Duration::from_secs(15 * (1u64 << failures.clamp(1, 3).saturating_sub(1)).min(4))
                } else {
                    Duration::from_secs((1 + failures.min(5) * 2) as u64)
                };
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
pub(super) fn best_line(lines: &[String]) -> String {
    let generic = |raw: &str| {
        let l = clean_message(raw);
        // The input's description and the files opened are not errors.
        raw.starts_with(' ') || l.contains("for writing") || l.starts_with("Input #") || l.starts_with("Output #") || l.starts_with("Stream mapping") ||
        l.is_empty() || l.starts_with("Error opening input") || l.starts_with("Error opening output") || l.starts_with("Exiting") || l.starts_with("Conversion failed") ||
        // A warning about the system's network buffers, not the reason it stopped.
        l.contains("attempted to set receive buffer")
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
            // The receiving thread's buffer (in 188-byte packets): ~47 MB, about 37 s at 10 Mb/s of
            // FFmpeg held up before a packet is lost; the kernel's buffer behind it (16 MB).
            let mut extra = "fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout=5000000".to_string();
            if !c.iface.is_empty() {
                extra = format!("localaddr={}&{extra}", c.iface);
            }
            join(&c.url, &extra)
        }
        Protocol::Rtp => {
            let mut extra = "timeout=5000000&buffer_size=16777216".to_string();
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
        // Read from the page before each recording (super::web).
        Protocol::Web => c.url.clone(),
        // GStreamer's named pipe, made for each recording (record_once).
        Protocol::Decklink => String::new(),
    };
    a.extend(["-i".into(), url]);
    a
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// The receiver's FFmpeg (without the program): the stream as received in segments, the
/// guide on its output, and a copy of the stream on the loopback for the preview.
/// Copying only, it keeps up with the source even when the server is short of CPU: the
/// preview's encoding, the heavy part, runs in another process and can never hold it up.
/// `describe`: FFmpeg also describes the input (programmes, streams: how the guide is found);
/// `epg`: the guide's stream, written raw on FFmpeg's output.
pub fn receiver_args(input: Vec<String>, run_dir: &Path, describe: bool, epg: Option<&str>, preview_port: u16) -> Vec<String> {
    let d = |p: &str| run_dir.join(p).to_string_lossy().into_owned();
    let level = if describe { "info" } else { "warning" };
    // Damaged packets are kept (no +discardcorrupt): a frame with a few lost packets decodes
    // with a small concealed patch, where a frame thrown away left every frame after it
    // without its reference (mosaic up to the next key frame) and the clips a frame short.
    let mut a: Vec<String> = ["-hide_banner", "-nostdin", "-nostats", "-y", "-loglevel", level, "-copyts", "-thread_queue_size", "4096"].map(String::from).to_vec();
    a.extend(input);
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    // 1. The stream as received, in segments.
    a.extend(s(&["-map", "0:v:0", "-map", "0:a?", "-c", "copy", "-muxdelay", "0", "-muxpreload", "0", "-f", "segment", "-segment_time", &SEGMENT.to_string(), "-segment_format", "mpegts", "-reset_timestamps", "0", "-segment_list_type", "csv", "-segment_list_flags", "+live", "-segment_list"]));
    a.push(d("master/list.csv"));
    a.push(d("master/s%08d.ts"));
    // 2. The same picture and first sound, same timestamps, for the preview (UDP never waits for it).
    a.extend(s(&["-map", "0:v:0", "-map", "0:a:0?", "-c", "copy", "-muxdelay", "0", "-muxpreload", "0", "-f", "mpegts"]));
    a.push(format!("udp://127.0.0.1:{preview_port}?pkt_size=1316"));
    // 3. The programme guide (DVB EIT sections), decoded by the recorder.
    if let Some(map) = epg {
        a.extend(s(&["-map", map, "-c", "copy", "-f", "data", "pipe:1"]));
    }
    a
}

/// The preview's FFmpeg: from the receiver's copy, the browser proxy (video and audio apart,
/// the same timestamps) and the card's still.
/// The preview making nothing this long (ms) while the recording goes on: it is stuck.
const PREVIEW_STUCK_MS: i64 = 60_000;

pub fn preview_args(preview_port: u16, run_dir: &Path, thumb: &Path, audio: bool) -> Vec<String> {
    let d = |p: &str| run_dir.join(p).to_string_lossy().into_owned();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut a = s(&["-hide_banner", "-nostdin", "-nostats", "-y", "-loglevel", "warning", "-copyts", "-thread_queue_size", "4096", "-i"]);
    // No timeout: it waits for the receiver, and is stopped with it.
    a.push(format!("udp://127.0.0.1:{preview_port}?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216"));
    // 1. Video: same frames, same timestamps, light.
    let keyframes = "expr:if(isnan(prev_forced_t),1,gte(t,prev_forced_t+1))";
    a.extend(s(&["-map", "0:v:0", "-vf", "yadif=deint=interlaced,scale=-2:'min(540,ih)',format=yuv420p", "-c:v", "libx264", "-preset", "veryfast", "-profile:v", "main", "-bf", "0", "-force_key_frames", keyframes, "-b:v", "1200k", "-maxrate", "1600k", "-bufsize", "2400k", "-an", "-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", &PROXY_SEGMENT.to_string(), "-hls_list_size", "10", "-hls_flags", "+independent_segments", "-hls_fmp4_init_filename", "init.mp4", "-hls_segment_filename"]));
    a.push(d("video/p%08d.m4s"));
    a.push(d("video/index.m3u8"));
    // 2. Audio (first track, stereo).
    if audio {
        // Lost or damaged audio packets leave a hole in the times: filled with silence (and
        // overlaps cut), else the sound after it is joined early and drifts ahead of the picture.
        a.extend(s(&["-map", "0:a:0", "-vn", "-af", AUDIO_SYNC, "-c:a", "aac", "-b:a", "128k", "-ac", "2", "-ar", "48000", "-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", &PROXY_SEGMENT.to_string(), "-hls_list_size", "10", "-hls_fmp4_init_filename", "init.mp4", "-hls_segment_filename"]));
        a.push(d("audio/p%08d.m4s"));
        a.push(d("audio/index.m3u8"));
    }
    // 3. A still every 2 s for the recorder's card.
    a.extend(s(&["-map", "0:v:0", "-vf", "fps=1/2,yadif=deint=interlaced,scale=480:-2", "-q:v", "5", "-update", "1", "-atomic_writing", "1"]));
    a.push(thumb.to_string_lossy().into_owned());
    a
}

/// A free UDP port on the loopback, for the receiver's copy to the preview.
fn free_port() -> Option<u16> {
    std::net::UdpSocket::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|a| a.port())
}

async fn record_once(m: &Arc<Manager>, rec: &Arc<Rec>, cfg: &RecorderConfig, cfg_rx: &mut watch::Receiver<RecorderConfig>, stop_rx: &mut watch::Receiver<bool>) -> Outcome {
    // A web page: is it live, and where is its stream? (No run is opened while it is not.)
    let source = if cfg.protocol == Protocol::Web {
        match super::web::resolve(m.data_dir(), cfg).await {
            Ok(s) => Some(s),
            Err(reason) => {
                // In the journal (Système › Santé): why the page gives no live.
                tracing::warn!("{}: {reason}", cfg.name);
                return Outcome::Exited { had_data: false, message: reason };
            }
        }
    } else {
        None
    };
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
    // A DeckLink card: its frames (Blackmagic's SDK) go to FFmpeg, which encodes them into a
    // named pipe the receiver reads.
    let mut capture: Option<CardCapture> = None;
    let clog: Arc<Mutex<VecDeque<String>>> = Default::default();
    let input = match &source {
        Some(s) => super::web::input_args(s),
        None if cfg.protocol == Protocol::Decklink => {
            let opened = {
                let c = cfg.clone();
                tokio::task::spawn_blocking(move || super::decklink::open_recording(&c)).await.unwrap_or_else(|e| Err(e.to_string()))
            };
            let r = match opened {
                Ok(r) => r,
                Err(e) => {
                    finish_run(rec, &run_id);
                    return Outcome::Exited { had_data: false, message: format!("decklink: {e}") };
                }
            };
            let pipes = ["ts", "uyvy", "pcm"].map(|ext| super::decklink::fifo(m.data_dir(), &rec.id, ext));
            let [Ok(ts), Ok(video), Ok(sound)] = pipes else {
                finish_run(rec, &run_id);
                let e = pipes.into_iter().find_map(|p| p.err()).map(|e| e.to_string()).unwrap_or_default();
                return Outcome::Exited { had_data: false, message: format!("decklink: {e}") };
            };
            rec.no_signal.store(!r.signal, Ordering::Relaxed);
            if r.signal {
                tracing::info!("{}: capturing {} from the card", cfg.name, r.mode.format());
            } else {
                tracing::warn!("{}: no signal on the card's input (waiting for it in {})", cfg.name, r.mode.format());
            }
            let args = super::decklink::encoder_args(cfg, &r.mode, &video, &sound, &ts);
            tracing::debug!("{}: encoder: ffmpeg {}", cfg.name, args.join(" "));
            let spawned = Command::new(&m.ffmpeg).args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true).spawn();
            match spawned {
                Ok(mut encoder) => {
                    if let Some(err) = encoder.stderr.take() {
                        let (clog, name) = (clog.clone(), cfg.name.clone());
                        tokio::spawn(async move {
                            let mut lines = tokio::io::BufReader::new(err).lines();
                            while let Ok(Some(l)) = lines.next_line().await {
                                tracing::debug!("{name}: encoder: {l}");
                                let mut q = clog.lock().unwrap();
                                q.push_back(l);
                                if q.len() > 30 {
                                    q.pop_front();
                                }
                            }
                        });
                    }
                    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let (tx, done) = tokio::sync::oneshot::channel();
                    let (stop2, rec2, name, channels) = (stop.clone(), rec.clone(), cfg.name.clone(), cfg.audio_channels);
                    std::thread::spawn(move || {
                        let end = super::decklink::pump(r, video, sound, channels, stop2, |on| {
                            rec2.no_signal.store(!on, Ordering::Relaxed);
                            if on {
                                tracing::info!("{name}: the card's input has a signal again");
                            } else {
                                tracing::warn!("{name}: no signal on the card's input");
                            }
                        });
                        let _ = tx.send(end);
                    });
                    capture = Some(CardCapture { encoder, stop, done });
                }
                Err(e) => {
                    finish_run(rec, &run_id);
                    return Outcome::Exited { had_data: false, message: format!("decklink: cannot start the encoder (FFmpeg): {e}") };
                }
            }
            vec!["-f".into(), "mpegts".into(), "-i".into(), ts.to_string_lossy().into_owned()]
        }
        None => input_args(cfg),
    };
    // MPEG-TS sources may carry a programme guide: FFmpeg describes the input to find it.
    let ts = matches!(cfg.protocol, Protocol::Udp | Protocol::Rtp | Protocol::Srt);
    let found = rec.epg_found.lock().unwrap().clone();
    let epg_map = found.stream.clone().filter(|_| ts && !rec.epg_failed.load(Ordering::Relaxed));
    let Some(port) = free_port() else {
        finish_run(rec, &run_id);
        return Outcome::Exited { had_data: false, message: "no free local port for the preview".into() };
    };
    let args = receiver_args(input, &run_dir, ts, epg_map.as_deref(), port);
    let preview_args = preview_args(port, &run_dir, &base.join("thumb.jpg"), audio);
    tracing::debug!("{}: ffmpeg {}", cfg.name, args.join(" "));
    tracing::debug!("{}: preview: ffmpeg {}", cfg.name, preview_args.join(" "));
    // The preview first: it listens before the receiver sends. A little lower priority: when the
    // server is short of CPU, the preview falls behind, never the recording.
    let mut pcmd = Command::new(&m.ffmpeg);
    pcmd.args(&preview_args).current_dir(&run_dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
    super::lower_priority(&mut pcmd, 5);
    let mut preview = match pcmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            finish_run(rec, &run_id);
            return Outcome::Exited { had_data: false, message: format!("cannot start {}: {e}", m.ffmpeg) };
        }
    };
    let plog: Arc<Mutex<VecDeque<String>>> = Default::default();
    if let Some(err) = preview.stderr.take() {
        let (plog, rec2, name) = (plog.clone(), rec.clone(), cfg.name.clone());
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                // Its own input overflowing: the server is too busy for the preview (the recording is
                // whole). Its decoder's complaints are not counted: the receiver already saw the
                // damaged packets, and a preview that skips makes its own.
                if l.contains("Circular buffer overrun") {
                    rec2.preview_dropped();
                }
                tracing::debug!("{name}: preview: {l}");
                let mut q = plog.lock().unwrap();
                q.push_back(l);
                if q.len() > 30 {
                    q.pop_front();
                }
            }
        });
    }
    // Picture and sound apart on the page: yt-dlp joins them and feeds FFmpeg.
    let mut feeder = None;
    let mut stdin = Stdio::null();
    if let Some(super::web::Source::Piped) = source {
        match super::web::pipe_command(m.data_dir(), cfg).spawn() {
            Ok(mut f) => {
                match f.stdout.take().map(TryInto::<Stdio>::try_into) {
                    Some(Ok(out)) => stdin = out,
                    _ => {
                        finish_run(rec, &run_id);
                        return Outcome::Exited { had_data: false, message: "web: cannot read yt-dlp's output".into() };
                    }
                }
                if let Some(err) = f.stderr.take() {
                    let name = cfg.name.clone();
                    tokio::spawn(async move {
                        let mut lines = tokio::io::BufReader::new(err).lines();
                        while let Ok(Some(l)) = lines.next_line().await {
                            // Its errors only; the end of a pipe closed by FFmpeg is not one.
                            if l.contains("ERROR") && !l.contains("ffmpeg exited with code") {
                                tracing::warn!("{name}: yt-dlp: {}", l.trim());
                            } else if !l.trim().is_empty() {
                                tracing::debug!("{name}: yt-dlp: {}", l.trim());
                            }
                        }
                    });
                }
                feeder = Some(f);
            }
            Err(e) => {
                finish_run(rec, &run_id);
                return Outcome::Exited { had_data: false, message: format!("web: cannot start yt-dlp: {e}") };
            }
        }
    }
    let stdout = if epg_map.is_some() { Stdio::piped() } else { Stdio::null() };
    let child = Command::new(&m.ffmpeg).args(&args).current_dir(&run_dir).stdin(stdin).stdout(stdout).stderr(Stdio::piped()).kill_on_drop(true).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            stop_child(&mut preview).await;
            finish_run(rec, &run_id);
            return Outcome::Exited { had_data: false, message: format!("cannot start {}: {e}", m.ffmpeg) };
        }
    };
    let log: Arc<Mutex<VecDeque<String>>> = Default::default();
    // The guide found in the input while FFmpeg does not write it yet: start again with it.
    let epg_new = Arc::new(tokio::sync::Notify::new());
    if let Some(err) = child.stderr.take() {
        let log = log.clone();
        let name = cfg.name.clone();
        let (rec2, epg_new2, mapped, data_dir2) = (rec.clone(), epg_new.clone(), epg_map.is_some(), m.data_dir().to_path_buf());
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(err).lines();
            let mut info = super::epg::InfoReader::default();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Some(kind) = glitch_of(&l) {
                    rec2.glitch(kind);
                }
                if ts && info.line(&l) && !mapped && !rec2.epg_failed.load(Ordering::Relaxed) {
                    tracing::info!("{name}: the source carries a programme guide");
                    *rec2.epg_found.lock().unwrap() = info.found.clone();
                    rec2.epg_from_disk.store(false, Ordering::Relaxed);
                    // Remembered: the next start (another run, a restart) maps it at once.
                    let path = super::epg::found_path(&data_dir2, &rec2.id);
                    if let Some(d) = path.parent() {
                        let _ = std::fs::create_dir_all(d);
                    }
                    let _ = crate::paths::write_json(&path, &info.found, false);
                    epg_new2.notify_one();
                }
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
    if let Some(out) = child.stdout.take() {
        tokio::spawn(read_guide(m.data_dir().to_path_buf(), rec.clone(), found.service, out));
    }

    let mut scan = Scanner { dup_end: *rec.last_end.lock().unwrap(), ..Default::default() };
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut lag_logged = 0i64;
    // The preview's pictures: how far they went, and since when they have not gone further; when
    // the recording last went further (by the clock of the server: the source's may go back).
    let mut video_seen: (Option<u32>, i64) = (None, now_ms());
    let mut audio_seen: (Option<u32>, i64) = (None, now_ms());
    let mut master_seen: (Option<f64>, i64) = (None, now_ms());
    let outcome = loop {
        tokio::select! {
            status = child.wait() => {
                tokio::time::sleep(Duration::from_millis(200)).await; // the last lines of the lists
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                let lines: Vec<String> = log.lock().unwrap().iter().cloned().collect();
                let message = best_line(&lines);
                match status {
                    Ok(s) if s.success() => tracing::info!("{}: the source stopped", cfg.name),
                    Ok(s) => tracing::warn!("{}: ffmpeg ended ({s}): {message}", cfg.name),
                    Err(e) => tracing::warn!("{}: ffmpeg: {e}", cfg.name),
                }
                if !scan.had_data {
                    // FFmpeg's last words, for the log (the reason is often a line above the last).
                    let tail: Vec<&str> = lines.iter().rev().filter(|l| !l.starts_with(' ') && !l.contains("for writing")).take(6).map(|l| l.as_str()).collect();
                    tracing::debug!("{}: ffmpeg said: {}", cfg.name, tail.into_iter().rev().collect::<Vec<_>>().join(" | "));
                    // The guide written along may be what FFmpeg refuses: record without it from now on.
                    if epg_map.is_some() {
                        // Remembered from an earlier start, the source may have changed: found again.
                        // Found this time and refused all the same: recorded without it.
                        let remembered = super::epg::found_path(m.data_dir(), &rec.id);
                        if rec.epg_from_disk.swap(false, Ordering::Relaxed) {
                            let _ = std::fs::remove_file(&remembered);
                            tracing::info!("{}: the programme guide is looked for again", cfg.name);
                        } else {
                            rec.epg_failed.store(true, Ordering::Relaxed);
                            let _ = std::fs::remove_file(&remembered);
                            tracing::warn!("{}: recording without the programme guide (FFmpeg did not start with it)", cfg.name);
                        }
                        rec.epg_found.lock().unwrap().stream = None;
                        break Outcome::Restart;
                    }
                }
                break Outcome::Exited { had_data: scan.had_data, message: clean_message(&message) };
            }
            end = async { match capture.as_mut() { Some(c) => c.ended().await, None => std::future::pending().await } } => {
                // The capture ended: the signal changed format, the card is gone, or the encoder stopped.
                stop_child(&mut child).await;
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                match end {
                    super::decklink::End::FormatChanged(f) => {
                        tracing::info!("{}: the card's input is now {f}: the capture starts again in it", cfg.name);
                        break Outcome::Restart;
                    }
                    super::decklink::End::Failed(why) => {
                        let detail = clog.lock().unwrap().iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
                        tracing::warn!("{}: the capture stopped: {why}{}", cfg.name, if detail.is_empty() { String::new() } else { format!(" ({detail})") });
                        break Outcome::Exited { had_data: scan.had_data, message: format!("decklink: {why}") };
                    }
                    super::decklink::End::Stopped => break Outcome::Exited { had_data: scan.had_data, message: "decklink: the capture stopped".into() },
                }
            }
            status = preview.wait() => {
                // The preview ended on its own: it cannot go on without the receiver, nor this run without it.
                let lines: Vec<String> = plog.lock().unwrap().iter().cloned().collect();
                stop_child(&mut child).await;
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                if audio && lines.iter().any(|l| l.contains("matches no streams") && l.contains("0:a:0")) {
                    rec.no_audio.store(true, Ordering::Relaxed);
                    tracing::info!("{}: no audio in the source", cfg.name);
                    break Outcome::Exited { had_data: scan.had_data, message: String::new() };
                }
                let message = best_line(&lines);
                match status {
                    Ok(s) => tracing::warn!("{}: the preview stopped ({s}): {message}", cfg.name),
                    Err(e) => tracing::warn!("{}: the preview: {e}", cfg.name),
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
            _ = epg_new.notified() => {
                stop_child(&mut child).await;
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                break Outcome::Restart;
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
                let lag = scan.preview_lag();
                rec.preview_lag.store(lag as u32, Ordering::Relaxed);
                // In the journal (not on the card): once every 10 minutes at most.
                if lag >= 20.0 && now_ms() - lag_logged > 600_000 {
                    lag_logged = now_ms();
                    tracing::warn!("{}: the server is too busy, the preview is {lag:.0} s behind the recording (the recording is whole)", cfg.name);
                }
                // The preview stuck: alive, but making nothing while the recording goes on (seen on
                // web lives: a stream it can no longer follow). Without it no live, no search, until
                // the recorder is restarted by hand: a new recording starts by itself.
                if scan.last_t1 != master_seen.0 {
                    master_seen = (scan.last_t1, now_ms());
                }
                // Its pictures and its sound, each (the sound alone may stop: no live either).
                if scan.last_video != video_seen.0 {
                    video_seen = (scan.last_video, now_ms());
                }
                if scan.last_audio != audio_seen.0 {
                    audio_seen = (scan.last_audio, now_ms());
                }
                let stuck = |seen: &(Option<u32>, i64)| seen.0.is_some() && now_ms() - seen.1 > PREVIEW_STUCK_MS;
                let sound = !rec.no_audio.load(Ordering::Relaxed);
                if (stuck(&video_seen) || (sound && stuck(&audio_seen))) && now_ms() - master_seen.1 < 15_000 {
                    let (what, since) = if stuck(&video_seen) { ("pictures", video_seen.1) } else { ("sound", audio_seen.1) };
                    let lines: Vec<String> = plog.lock().unwrap().iter().cloned().collect();
                    // Its last error, if it said one (not its mere warnings).
                    let why = best_line(&lines);
                    let lower = why.to_ascii_lowercase();
                    let why = if ["error", "invalid", "non monoton", "corrupt", "fail", "discontinu"].iter().any(|k| lower.contains(k)) { why } else { String::new() };
                    tracing::warn!("{}: the preview has made no {what} for {} s while the recording goes on{}: a new recording starts", cfg.name, (now_ms() - since) / 1000, if why.is_empty() { String::new() } else { format!(" ({why})") });
                    stop_child(&mut child).await;
                    tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                    break Outcome::Restart;
                }
                if let Some((from, to)) = scan.jumped {
                    tracing::warn!("{}: the source's clock jumped ({from:.1} s → {to:.1} s): a new recording starts", cfg.name);
                    stop_child(&mut child).await;
                    tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                    break Outcome::Restart;
                }
                // Programmed: the slot is over (or was removed).
                if !cfg.enabled && !m.wants(&rec.id, now_ms()) {
                    tracing::info!("{}: end of the programmed slot", cfg.name);
                    stop_child(&mut child).await;
                    tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                    break Outcome::Restart;
                }
            }
        }
    };
    // The preview ends with the receiver: what it still holds is written, then the last fragments read.
    // Once the receiver has stopped, the preview waits for a stream that no longer comes and
    // does not end on its own: 1 s, then it is killed (each restart waited 5 s per recorder).
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop_within(&mut preview, Duration::from_secs(1)).await;
    tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
    rec.preview_lag.store(0, Ordering::Relaxed);
    if let Some(end) = scan.last_t1 {
        *rec.last_end.lock().unwrap() = Some(end);
    }
    if let Some(c) = capture {
        c.finish().await;
        rec.no_signal.store(false, Ordering::Relaxed);
    }
    if let Some(mut f) = feeder {
        let _ = f.start_kill();
        let _ = f.wait().await;
    }
    finish_run(rec, &run_id);
    outcome
}

/// A reception problem in FFmpeg's words: packets lost before FFmpeg (the network or a
/// receiver too slow), or a damaged stream (what the picture shows as blocks, freezes).
fn glitch_of(line: &str) -> Option<super::Glitch> {
    let l = line.to_ascii_lowercase();
    const DAMAGED: [&str; 11] = [
        "packet corrupt", "pes packet size mismatch", "error while decoding", "decode_slice_header error", "concealing",
        "invalid nal unit", "missing picture", "non-existing pps", "continuity check failed", "corrupt decoded frame", "no frame!",
    ];
    if l.contains("circular buffer overrun") || l.contains("packet loss") || l.contains("missed") && l.contains("packets") {
        Some(super::Glitch::Lost)
    } else if DAMAGED.iter().any(|k| l.contains(k)) {
        Some(super::Glitch::Damaged)
    } else {
        None
    }
}

/// Decodes the guide FFmpeg writes on its output, keeps it, saves it now and then.
async fn read_guide(data_dir: PathBuf, rec: Arc<Rec>, service: Option<u16>, mut out: tokio::process::ChildStdout) {
    use tokio::io::AsyncReadExt;
    let path = super::epg::path(&data_dir, &rec.id);
    let mut sections = super::epg::Sections::default();
    let mut decoder = super::epg::Decoder { service };
    let mut buf = vec![0u8; 16 * 1024];
    let mut saved = std::time::Instant::now();
    loop {
        let n = match out.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let events: Vec<_> = sections.push(&buf[..n]).iter().flat_map(|s| decoder.section(s)).collect();
        if events.is_empty() {
            continue;
        }
        let hours = rec.config().loop_hours;
        let mut g = rec.epg.lock().unwrap();
        g.add(events);
        g.trim(now_ms() as f64, hours);
        if g.changed && saved.elapsed() > Duration::from_secs(10) {
            g.changed = false;
            saved = std::time::Instant::now();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = crate::paths::write_json(&path, &g.events, false);
        }
    }
    let mut g = rec.epg.lock().unwrap();
    if g.changed {
        g.changed = false;
        let _ = crate::paths::write_json(&path, &g.events, false);
    }
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
    stop_within(child, Duration::from_secs(5)).await
}

/// A DeckLink card's capture: the thread moving the card's frames into the encoder's pipes, and
/// the encoder (FFmpeg) writing the MPEG-TS the receiver reads.
struct CardCapture {
    encoder: tokio::process::Child,
    stop: Arc<std::sync::atomic::AtomicBool>,
    done: tokio::sync::oneshot::Receiver<super::decklink::End>,
}

impl CardCapture {
    /// How it ended (the capture thread, or the encoder on its own).
    async fn ended(&mut self) -> super::decklink::End {
        tokio::select! {
            end = &mut self.done => end.unwrap_or_else(|_| super::decklink::End::Failed("the capture stopped".into())),
            status = self.encoder.wait() => super::decklink::End::Failed(format!("the encoder stopped ({})", status.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string()))),
        }
    }

    /// Stops the capture (the card is released), then the encoder (it ends with its input).
    async fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = tokio::time::timeout(Duration::from_secs(2), &mut self.done).await;
        stop_within(&mut self.encoder, Duration::from_secs(2)).await;
    }
}

/// Asks a child to end (SIGINT: FFmpeg closes its files), killed after `grace`.
async fn stop_within(child: &mut tokio::process::Child, grace: Duration) {
    if let Some(pid) = child.id() {
        let t = std::time::Instant::now();
        unsafe {
            libc::kill(pid as i32, libc::SIGINT);
        }
        if tokio::time::timeout(grace, child.wait()).await.is_ok() {
            tracing::debug!("pid {pid} stopped in {} ms", t.elapsed().as_millis());
            return;
        }
        tracing::debug!("pid {pid} did not stop in {} ms: killed", grace.as_millis());
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
    /// The end of the last segment read (source time, s).
    last_t1: Option<f64>,
    /// The source's clock jumped (a new encoder, a splice): (from, to).
    jumped: Option<(f64, f64)>,
    /// The start of the first segment, and the end of the preview's last video fragment (source time, s).
    first_t0: Option<f64>,
    video_t1: Option<f64>,
    /// Where the previous recording of this source ended (source time): a live web page read
    /// again after a cut gives its last segments once more; those are already recorded.
    dup_end: Option<f64>,
}

impl Scanner {
    /// Already recorded: on the previous recording's timeline, before its end.
    fn duplicate(&self, t0: f64, t1: f64) -> bool {
        self.dup_end.is_some_and(|e| t0 >= e - 300.0 && t1 <= e + 0.1)
    }
}

impl Scanner {
    /// How far the preview is behind the recording (s): the server is short of CPU when it grows.
    fn preview_lag(&self) -> f64 {
        match (self.last_t1, self.video_t1.or(self.first_t0)) {
            (Some(rec), Some(prev)) => lag_between(rec, prev),
            _ => 0.0,
        }
    }

    fn scan(&mut self, m: &Manager, rec: &Rec, run_id: &str, last: bool) {
        let Some(dir) = rec.runs.read().unwrap().iter().find(|r| r.meta.id == run_id).map(|r| r.dir.clone()) else { return };
        let now = now_ms() as f64;
        let new_master = self.read_csv(&m.ffmpeg, &dir, now);
        let new_segments = !new_master.is_empty();
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
                    if self.duplicate(t0, t0 + dur) {
                        continue;
                    }
                    run.append_proxy(video, PSeg { n, t0, t1: t0 + dur, bytes });
                    if video {
                        self.video_t1 = Some(self.video_t1.map_or(t0 + dur, |v| v.max(t0 + dur)));
                    }
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
        // Is the stream still coming? The segment being written grows, or a new one is listed.
        // (FFmpeg writes the segments by 1 MB: at a low bitrate, one only shows when it closes.)
        let size = newest_size(&dir.join(super::index::MASTER));
        if size != self.growing_size || new_segments {
            self.growing_size = size;
            self.still_ticks = 0;
            if size > 0 || new_segments {
                self.had_data = true;
                // A card without signal goes on giving (black) frames: recorded, but said.
                rec.set_state(if rec.no_signal.load(Ordering::Relaxed) { State::Nosignal } else { State::Recording }, "");
            }
        } else {
            self.still_ticks += 1;
            // 12 s: more than a segment (6 s, up to the next key frame).
            if self.still_ticks == 24 && self.had_data {
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
            if self.last_t1.is_none() && self.duplicate(t0, t1) {
                continue;
            }
            // The source's clock jumped back or far ahead: the times no longer follow the
            // server's; the recording starts again (a new run keeps the index right).
            if let Some(prev) = self.last_t1 {
                if t0 < prev - 1.0 || t0 > prev + 30.0 {
                    self.jumped = Some((prev, t0));
                    break;
                }
            }
            self.last_t1 = Some(t1);
            self.first_t0.get_or_insert(t0);
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

/// How far the preview is behind the recording (s), from their times. The recording keeps a
/// source's times whole; the preview reads them through MPEG-TS, whose clock starts again from 0
/// every 26 h 30 (33 bits at 90 kHz): a source with large times (an RTMP encoder running for days)
/// gives the two a whole number of turns apart, which is no lag.
fn lag_between(recorded: f64, previewed: f64) -> f64 {
    let wrap = super::PTS_WRAP;
    let d = (recorded - previewed).rem_euclid(wrap);
    // Just past a turn: the preview a little ahead, not a whole turn behind.
    if d > wrap / 2.0 { 0.0 } else { d }
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
        RecorderConfig { id: "x".into(), name: "x".into(), protocol, url: "udp://239.1.1.1:5000".into(), mode, port: 9000, path: "live/a".into(), latency: 120, passphrase: String::new(), iface: "10.0.0.2".into(), cookies: String::new(), loop_hours: 24, bitrate: 8.0, enabled: true, device: 0, connection: "auto".into(), video_mode: "auto".into(), audio_channels: 2, audio_input: "auto".into() }
    }

    #[test]
    fn audio_kept_on_its_times() {
        let a = preview_args(5000, Path::new("/r"), Path::new("/t.jpg"), true);
        let i = a.iter().position(|x| x == "0:a:0").unwrap();
        assert_eq!(a[i + 2..i + 4], ["-af".to_string(), AUDIO_SYNC.to_string()]);
        assert!(a.contains(&"udp://127.0.0.1:5000?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216".to_string()));
        // The receiver only copies (no encoding), and keeps the damaged frames: thrown away, they
        // make the mosaic last to the next key frame.
        let r = receiver_args(vec!["-i".into(), "x".into()], Path::new("/r"), false, None, 5000);
        assert!(!r.iter().any(|x| x.contains("discardcorrupt") || x.contains("libx264") || x == "-vf" || x == "-af"));
        assert!(r.contains(&"udp://127.0.0.1:5000?pkt_size=1316".to_string()));
    }

    #[test]
    fn lag_past_the_clock_turns() {
        let wrap = crate::recorder::PTS_WRAP;
        assert!((lag_between(100.0, 97.5) - 2.5).abs() < 1e-9);
        // An RTMP source's large times: ten turns apart, 3 s behind.
        assert!((lag_between(10.0 * wrap + 500.0, 497.0) - 3.0).abs() < 1e-6);
        // The preview a hair ahead: no lag.
        assert_eq!(lag_between(500.0, 500.5), 0.0);
    }

    #[test]
    fn glitches() {
        assert_eq!(glitch_of("[udp @ 0x5] Circular buffer overrun. Surge of data? To avoid, increase fifo_size URL option."), Some(super::super::Glitch::Lost));
        assert_eq!(glitch_of("[h264 @ 0x7] concealing 845 DC, 845 AC, 845 MV errors in P frame"), Some(super::super::Glitch::Damaged));
        assert_eq!(glitch_of("[mpegts @ 0x9] Packet corrupt (stream = 0, dts = 1234)."), Some(super::super::Glitch::Damaged));
        assert_eq!(glitch_of("[segment @ 0x1] Opening 'x.ts' for writing"), None);
    }

    #[test]
    fn inputs() {
        assert_eq!(input_args(&cfg(Protocol::Udp, Mode::Listener)).last().unwrap(), "udp://239.1.1.1:5000?localaddr=10.0.0.2&fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout=5000000");
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
