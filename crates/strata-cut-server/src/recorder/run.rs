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
/// Length of the proxy segments, seconds: short, so that the live plays a second or two
/// behind the source (a fragment is read once it is whole).
const PROXY_SEGMENT: u32 = 1;
/// Sound kept on its timestamps: a hole of more than 100 ms is filled with silence, an
/// overlap cut (by default FFmpeg joins the samples and the sound drifts ahead after a loss).
/// Not less: the times of the sound jump by a frame or so (some tens of ms) where a stream is
/// made of fragments (a YouTube live, HLS), and each jump "put right" is a click: measured on
/// jumps of 11 ms a second, 26 clicks in 20 s at 10 ms, none at 50 or 100 ms.
pub const AUDIO_SYNC: &str = "aresample=async=1:min_hard_comp=0.1";

enum Outcome {
    /// The settings changed: start again at once.
    Restart,
    /// The recorder was deleted or the server stops.
    Stopped,
    /// FFmpeg ended by itself.
    Exited { had_data: bool, message: String },
    /// The source has no sound, or fewer tracks than expected: started again at once with what
    /// it has (not a failure).
    NoAudio,
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
                let next = if cfg.slots_off { None } else { m.schedules.next(&rec.id, &tz, now) };
                rec.set_state(if next.is_some() { State::Waiting } else { State::Off }, "");
                rec.refresh_status();
                let wait = next.map(|(_, a, _)| (a - super::PREROLL_MS - now).clamp(0, 30_000)).unwrap_or(30_000);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(wait as u64)) => {}
                    r = cfg_rx.changed() => { if r.is_ok() { rec.no_audio.store(false, Ordering::Relaxed); } }
                    _ = stop_rx.changed() => {}
                    _ = m.schedules.changed.notified() => {}
                    _ = rec.wake.notified() => {}
                }
                continue;
            }
            if matches!(rec.state(), State::Off | State::Waiting) {
                let slot = m.schedules.active(&rec.id, &tz, now).or_else(|| m.schedules.active(&rec.id, &tz, now + super::PREROLL_MS));
                if let Some((slot, a, b)) = slot {
                    let hm = |ms: i64| jiff::Timestamp::from_millisecond(ms).map(|t| t.to_zoned(tz.clone()).strftime("%H:%M").to_string()).unwrap_or_default();
                    tracing::info!("{}: programmed slot \"{}\" {}–{}: recording starts", cfg.name, slot.name, hm(a), hm(b));
                }
            }
        }
        if matches!(rec.state(), State::Off | State::Waiting) {
            rec.set_state(State::Starting, "");
        }
        match record_once(&m, &rec, &cfg, &mut cfg_rx, &mut stop_rx).await {
            Outcome::Restart => {
                rec.no_audio.store(false, Ordering::Relaxed);
                failures = 0;
            }
            Outcome::Stopped => break,
            Outcome::NoAudio => failures = 0,
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
                    // NDI: a source not there yet is looked for again; the library missing is an error.
                    let broken = broken || message.contains("ndi library") || message.contains("cannot run the ndi");
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
                            Protocol::Hls => hls_reason(&message),
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

/// Two settings that record the same way (only the name, the loop length or when it records
/// differ: a recording under way goes on, and stops at the end of its slot if it must).
fn same_source(a: &RecorderConfig, b: &RecorderConfig) -> bool {
    let strip = |c: &RecorderConfig| RecorderConfig { name: String::new(), loop_hours: 0, bitrate: 0.0, enabled: false, slots_off: false, ..c.clone() };
    strip(a) == strip(b)
}

/// The most telling line of FFmpeg's last words (not the generic "Error opening input files").
pub(crate) fn best_line(lines: &[String]) -> String {
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

/// What FFmpeg's RTP reading takes, after the address: the kernel's buffer (FFmpeg has no
/// buffer of its own for RTP: 32 MB, about 12 s at 10 Mb/s of FFmpeg held up), and no report
/// sent back to the group (`ttl=0`: the server only listens).
const RTP_OPTIONS: &str = "timeout=5000000&buffer_size=33554432&ttl=0";

/// The FFmpeg input for a recorder.
pub fn input_args(c: &RecorderConfig) -> Vec<String> {
    input_args_as(c, false)
}

/// `rtp`: a source given as UDP sends RTP (see `udp_in::sniff`): read as RTP, FFmpeg puts the
/// datagrams back in order (within 100 ms). (It does not drop one sent twice, read as RTP or as
/// UDP: measured, 18 damaged packets for 18 doubles; the witness counts them.)
pub fn input_args_as(c: &RecorderConfig, rtp: bool) -> Vec<String> {
    let join = |url: &str, extra: &str| if url.contains('?') { format!("{url}&{extra}") } else { format!("{url}?{extra}") };
    let mut a: Vec<String> = Vec::new();
    let url = match c.protocol {
        Protocol::Udp if rtp => {
            // The address alone: the options of a UDP reading mean nothing to the RTP one.
            let address = c.url.split('?').next().unwrap_or(&c.url).replacen("udp://", "rtp://", 1);
            let card = super::udp_in::Target::parse(&c.url, &c.iface).map(|t| t.iface).filter(|i| !i.is_unspecified());
            match card {
                Some(i) => format!("{address}?localaddr={i}&{RTP_OPTIONS}"),
                None => format!("{address}?{RTP_OPTIONS}"),
            }
        }
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
            let mut extra = RTP_OPTIONS.to_string();
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
        // The playlist, read again by FFmpeg as it grows; a server silent for 20 s ends the reading.
        Protocol::Hls => {
            a.extend(["-rw_timeout".into(), "20000000".into()]);
            c.url.clone()
        }
        // GStreamer's named pipe, made for each recording (record_once).
        Protocol::Decklink | Protocol::Ndi => String::new(),
        // Its browser's pictures, encoded here, on a local port (record_once).
        Protocol::Page => {
            a.extend(["-f".into(), "mpegts".into()]);
            format!("udp://127.0.0.1:{}?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout=10000000", crate::pages::port(&c.id).unwrap_or(0))
        }
        // Made by its own FFmpeg, read on the standard input (record_once).
        Protocol::Pattern => {
            a.extend(["-f".into(), "mpegts".into()]);
            "pipe:0".into()
        }
    };
    a.extend(["-i".into(), url]);
    a
}

/// Why an HLS stream cannot be read, from FFmpeg's last words: what its server answered.
pub(super) fn hls_reason(message: &str) -> String {
    if let Some(i) = message.find("Server returned ") {
        let answer = &message[i + "Server returned ".len()..];
        return if answer.starts_with("404") {
            "hls: no stream at this address (404)"
        } else if answer.starts_with("401") || answer.starts_with("403") {
            "hls: the server refuses to give the stream (access denied)"
        } else {
            "hls: the server answers with an error"
        }
        .into();
    }
    if ["Connection refused", "Connection timed out", "Name or service not known", "Failed to resolve", "No route to host"].iter().any(|k| message.contains(k)) {
        return "hls: no answer from the server (address or network)".into();
    }
    if message.contains("Invalid data found") {
        return "hls: this address is not an hls playlist (.m3u8)".into();
    }
    String::new()
}

use crate::util::urlencode;

/// What FFmpeg says of a source, without its secrets (it is logged, shown, exported for support):
/// its address as the host alone (a key or a token in its path or query), its passphrase as given
/// or as written in the address, any `passphrase=` value.
pub(crate) fn redact(text: &str, cfg: &RecorderConfig) -> String {
    let mut t = text.to_string();
    let url = cfg.url.trim();
    if url.contains("://") && t.contains(url) {
        t = t.replace(url, &super::only_host(url));
    }
    for secret in [cfg.passphrase.clone(), urlencode(&cfg.passphrase)] {
        if secret.len() >= 3 && secret != super::MASKED {
            t = t.replace(&secret, "••••");
        }
    }
    let mut out = String::with_capacity(t.len());
    let mut rest = t.as_str();
    while let Some(i) = rest.find("passphrase=") {
        let (head, tail) = rest.split_at(i + "passphrase=".len());
        out.push_str(head);
        let end = tail.find(|c: char| c == '&' || c.is_whitespace()).unwrap_or(tail.len());
        if end > 0 {
            out.push_str("••••");
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// The receiver's FFmpeg (without the program): the stream as received in segments, the
/// guide on its output, and a copy of the stream on the loopback for the preview.
/// Copying only, it keeps up with the source even when the server is short of CPU: the
/// preview's encoding, the heavy part, runs in another process and can never hold it up.
/// `describe`: FFmpeg also describes the input (programmes, streams: how the guide is found);
/// `epg`: the guide's stream, written raw on FFmpeg's output.
/// `program`: the only program read (HLS: the variant of the quality chosen, see `super::hls`).
/// `empty`: the input's streams left out (sending nothing, FFmpeg cannot copy them).
pub fn receiver_args(input: Vec<String>, run_dir: &Path, describe: bool, epg: Option<&str>, preview_port: u16, program: Option<usize>, empty: &[u32]) -> Vec<String> {
    let d = |p: &str| run_dir.join(p).to_string_lossy().into_owned();
    let level = if describe { "info" } else { "warning" };
    // Damaged packets are kept (no +discardcorrupt): a frame with a few lost packets decodes
    // with a small concealed patch, where a frame thrown away left every frame after it
    // without its reference (mosaic up to the next key frame) and the clips a frame short.
    let mut a: Vec<String> = ["-hide_banner", "-nostdin", "-nostats", "-y", "-loglevel", level, "-copyts", "-thread_queue_size", "4096"].map(String::from).to_vec();
    a.extend(input);
    let s = crate::util::strs;
    let from = program.map_or("0:".to_string(), |p| format!("0:p:{p}:"));
    let (video, sounds) = (format!("{from}v:0"), format!("{from}a?"));
    // Its subtitles too (DVB subtitles, teletext), as they come.
    let subs = format!("{from}s?");
    let left_out: Vec<String> = empty.iter().flat_map(|n| ["-map".to_string(), format!("-0:{n}")]).collect();
    // 1. The stream as received, in segments.
    a.extend(s(&["-map", &video, "-map", &sounds, "-map", &subs]));
    a.extend(left_out.clone());
    // A sound that stops (an encoder that no longer sends it) does not hold the picture back:
    // FFmpeg waits for it 1 s at most (10 s by default, the source said lost meanwhile).
    a.extend(s(&["-c", "copy", "-max_interleave_delta", "1000000", "-muxdelay", "0", "-muxpreload", "0", "-f", "segment", "-segment_time", &SEGMENT.to_string(), "-segment_format", "mpegts", "-reset_timestamps", "0", "-segment_list_type", "csv", "-segment_list_flags", "+live", "-segment_list"]));
    a.push(d("master/list.csv"));
    a.push(d("master/s%08d.ts"));
    // 2. The same picture, its sounds and subtitles, same timestamps, for the preview and the
    // streams (UDP never waits for them).
    a.extend(s(&["-map", &video, "-map", &sounds, "-map", &subs]));
    a.extend(left_out);
    a.extend(s(&["-c", "copy", "-max_interleave_delta", "1000000", "-muxdelay", "0", "-muxpreload", "0", "-f", "mpegts"]));
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

/// The preview's picture: light, and watched (a black picture, a frozen one: their start and end
/// in its log).
const PREVIEW_VIDEO: &str = "yadif=deint=interlaced,scale=-2:'min(540,ih)',format=yuv420p,blackdetect=d=1:pic_th=0.98,metadata=mode=print:key=lavfi.black_start,metadata=mode=print:key=lavfi.black_end,freezedetect=n=-60dB:d=2";
/// Its sound: in step (see `AUDIO_SYNC`), and watched (silence).
const PREVIEW_AUDIO: &str = "aresample=async=1:min_hard_comp=0.1,silencedetect=n=-60dB:d=2";

pub fn preview_args(preview_port: u16, run_dir: &Path, thumb: &Path, audio: bool, extra_audio: u32) -> Vec<String> {
    let d = |p: &str| run_dir.join(p).to_string_lossy().into_owned();
    let s = crate::util::strs;
    // Its info lines too, tagged: what the picture and the sound show (black, frozen, silent).
    let mut a = s(&["-hide_banner", "-nostdin", "-nostats", "-y", "-loglevel", "level+info", "-copyts", "-thread_queue_size", "4096", "-i"]);
    // No timeout: it waits for the receiver, and is stopped with it.
    a.push(format!("udp://127.0.0.1:{preview_port}?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216"));
    // 1. Video: same frames, same timestamps, light; no look-ahead (the live a second or two
    // behind the source).
    let keyframes = "expr:if(isnan(prev_forced_t),1,gte(t,prev_forced_t+1))";
    a.extend(s(&["-map", "0:v:0", "-vf", PREVIEW_VIDEO, "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency", "-profile:v", "main", "-bf", "0", "-force_key_frames", keyframes, "-b:v", "1200k", "-maxrate", "1600k", "-bufsize", "2400k", "-an", "-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", &PROXY_SEGMENT.to_string(), "-hls_list_size", "30", "-hls_flags", "+independent_segments", "-hls_fmp4_init_filename", "init.mp4", "-hls_segment_filename"]));
    a.push(d("video/p%08d.m4s"));
    a.push(d("video/index.m3u8"));
    // 2. Audio (first track, stereo).
    if audio {
        // A sound track for the browser: stereo AAC in fragments of the proxy's length.
        // Lost or damaged audio packets leave a hole in the times: filled with silence (and
        // overlaps cut), else the sound after it is joined early and drifts ahead of the picture.
        let mut track = |k: u32, filter: &str, dir: &str| {
            a.extend(s(&["-map", &format!("0:a:{k}"), "-vn", "-af", filter, "-c:a", "aac", "-b:a", "128k", "-ac", "2", "-ar", "48000", "-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", &PROXY_SEGMENT.to_string(), "-hls_list_size", "30", "-hls_fmp4_init_filename", "init.mp4", "-hls_segment_filename"]));
            a.push(d(&format!("{dir}/p%08d.m4s")));
            a.push(d(&format!("{dir}/index.m3u8")));
        };
        // The first one is watched (silence) and gives the waveforms.
        track(0, PREVIEW_AUDIO, "audio");
        // The other tracks (another language, the audio description), for the player's choice;
        // their fragments are numbered as the first track's.
        for k in 1..=extra_audio.min(super::PREVIEW_EXTRA) {
            track(k, AUDIO_SYNC, &format!("audio{k}"));
        }
    }
    // 3. A still every 2 s for the recorder's card.
    a.extend(s(&["-map", "0:v:0", "-vf", "fps=1/2,yadif=deint=interlaced,scale=480:-2", "-q:v", "5", "-update", "1", "-atomic_writing", "1"]));
    a.push(thumb.to_string_lossy().into_owned());
    a
}

/// Copies the stream as received (MPEG-TS on the loopback, from the receiver) to the preview,
/// and to the streams that send this source somewhere (`Rec::taps`): a stream starts and stops
/// without touching the recording. Stops when dropped.
struct Relay(Arc<std::sync::atomic::AtomicBool>);

impl Drop for Relay {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl Relay {
    fn start(sock: std::net::UdpSocket, preview: u16, rec: Arc<Rec>) -> Relay {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = stop.clone();
        set_receive_buffer(&sock, 8 << 20);
        let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
        let local = std::net::Ipv4Addr::LOCALHOST;
        let key = format!("rec:{}", rec.id);
        let spawned = std::thread::Builder::new().name("relay".into()).spawn(move || {
            crate::procs::tag_thread(key, "Receiving");
            let mut buf = vec![0u8; 65536];
            let mut taps: Vec<u16> = Vec::new();
            let mut looked = 0i64;
            // Its audio tracks, read as they pass: does the source still send sound?
            let mut seen = crate::ts::Seen::default();
            while !done.load(Ordering::Relaxed) {
                let Ok(n) = sock.recv(&mut buf) else { continue };
                let _ = sock.send_to(&buf[..n], (local, preview));
                let now = super::now_ms();
                for p in buf[..n].chunks(188) {
                    seen.packet(p, now);
                }
                // The taps, looked at again every 50 ms (by time: a source of little bitrate makes
                // few packets).
                if now - looked >= 50 {
                    looked = now;
                    taps = rec.taps.lock().unwrap().iter().map(|t| t.1).collect();
                    rec.last_packet.store(now, Ordering::Relaxed);
                    rec.last_sound.store(seen.newest(), Ordering::Relaxed);
                }
                for &port in &taps {
                    let _ = sock.send_to(&buf[..n], (local, port));
                }
            }
        });
        if let Err(e) = spawned {
            tracing::error!("cannot start the relay of a recorder: {e}");
        }
        Relay(stop)
    }
}

/// A larger buffer for a socket receiving a stream (the system may give less).
fn set_receive_buffer(sock: &std::net::UdpSocket, bytes: i32) {
    use std::os::fd::AsRawFd;
    // SAFETY: setsockopt on an open socket, with a pointer to an int and its size.
    unsafe {
        libc::setsockopt(sock.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVBUF, (&bytes as *const i32).cast(), std::mem::size_of::<i32>() as libc::socklen_t);
    }
}

/// A free UDP port on the loopback, for the receiver's copy to the preview.
fn free_port() -> Option<u16> {
    crate::util::free_port(false)
}

async fn record_once(m: &Arc<Manager>, rec: &Arc<Rec>, cfg: &RecorderConfig, cfg_rx: &mut watch::Receiver<RecorderConfig>, stop_rx: &mut watch::Receiver<bool>) -> Outcome {
    // A web page: is it live, and where is its stream? (No run is opened while it is not.)
    let source = if cfg.protocol == Protocol::Web {
        // Reading the page takes up to a minute and a half: a stop or new settings do not wait for it.
        let resolved = tokio::select! {
            r = super::web::resolve(m.data_dir(), cfg) => r,
            _ = stop_rx.changed() => return Outcome::Stopped,
            r = cfg_rx.changed() => return if r.is_ok() { Outcome::Restart } else { Outcome::Stopped },
        };
        match resolved {
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
    // A web page: its browser started (or kept), before its stream is read.
    if cfg.protocol == Protocol::Page && crate::pages::ensure(cfg).is_none() {
        finish_run(rec, &run_id);
        return Outcome::Exited { had_data: false, message: "page: cannot open its browser".into() };
    }
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
            if let Ok(c) = &spawned {
                crate::procs::tag(c.id(), format!("rec:{}", rec.id), "Encoding");
            }
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
                        crate::procs::tag_thread(format!("rec:{}", rec2.id), "Capture");
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
        // An NDI source: its pictures and sound (the NDI library) go to FFmpeg, which encodes
        // them into a named pipe the receiver reads, as for a card.
        None if cfg.protocol == Protocol::Ndi => {
            let opened = {
                let c = cfg.clone();
                tokio::task::spawn_blocking(move || super::ndi::open(&c)).await.unwrap_or_else(|e| Err(e.to_string()))
            };
            let r = match opened {
                Ok(r) => r,
                Err(e) => {
                    finish_run(rec, &run_id);
                    return Outcome::Exited { had_data: false, message: e };
                }
            };
            let pipes = ["ts", "raw", "f32"].map(|ext| super::decklink::fifo(m.data_dir(), &rec.id, ext));
            let [Ok(ts), Ok(video), Ok(sound)] = pipes else {
                finish_run(rec, &run_id);
                let e = pipes.into_iter().find_map(|p| p.err()).map(|e| e.to_string()).unwrap_or_default();
                return Outcome::Exited { had_data: false, message: format!("ndi: {e}") };
            };
            tracing::info!("{}: receiving {} from the NDI source{}", cfg.name, r.format.name(), if r.format.silent { " (no sound)" } else { "" });
            let args = super::ndi::encoder_args(cfg, &r.format, &video, &sound, &ts);
            tracing::debug!("{}: encoder: ffmpeg {}", cfg.name, args.join(" "));
            let spawned = Command::new(&m.ffmpeg).args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true).spawn();
            if let Ok(c) = &spawned {
                crate::procs::tag(c.id(), format!("rec:{}", rec.id), "Encoding");
            }
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
                    let (stop2, key) = (stop.clone(), format!("rec:{}", rec.id));
                    std::thread::spawn(move || {
                        crate::procs::tag_thread(key, "Capture");
                        let _ = tx.send(super::ndi::pump(r, video, sound, stop2));
                    });
                    capture = Some(CardCapture { encoder, stop, done });
                }
                Err(e) => {
                    finish_run(rec, &run_id);
                    return Outcome::Exited { had_data: false, message: format!("ndi: cannot start the encoder (FFmpeg): {e}") };
                }
            }
            vec!["-f".into(), "mpegts".into(), "-i".into(), ts.to_string_lossy().into_owned()]
        }
        None => {
            // A source given as UDP may send RTP (most IPTV head-ends do): its first datagram says.
            let rtp = cfg.protocol == Protocol::Udp && {
                let target = super::udp_in::Target::parse(&cfg.url, &cfg.iface);
                let kind = match target {
                    Some(t) => tokio::task::spawn_blocking(move || super::udp_in::sniff(&t, Duration::from_millis(1500))).await.ok().flatten(),
                    None => None,
                };
                kind == Some(super::udp_in::Kind::Rtp)
            };
            if rtp && !rec.rtp.swap(true, Ordering::Relaxed) {
                tracing::info!("{}: this UDP source sends RTP: read as RTP (datagrams that come out of order are put back in order)", cfg.name);
            } else if !rtp {
                rec.rtp.store(cfg.protocol == Protocol::Rtp, Ordering::Relaxed);
            }
            input_args_as(cfg, rtp)
        }
    };
    // MPEG-TS sources may carry a programme guide: FFmpeg describes the input to find it.
    let ts = matches!(cfg.protocol, Protocol::Udp | Protocol::Rtp | Protocol::Srt);
    let found = rec.epg_found.lock().unwrap().clone();
    let epg_map = found.stream.clone().filter(|_| ts && !rec.epg_failed.load(Ordering::Relaxed));
    // The receiver sends its copy of the stream to the relay, which gives it to the preview and
    // to the streams.
    let (Some(port), Ok(relay_sock)) = (free_port(), std::net::UdpSocket::bind("127.0.0.1:0")) else {
        finish_run(rec, &run_id);
        return Outcome::Exited { had_data: false, message: "no free local port for the preview".into() };
    };
    let relay_port = relay_sock.local_addr().map(|a| a.port()).unwrap_or(0);
    // HLS: the variant of the quality chosen, when the playlist offers several.
    let program = if cfg.protocol == Protocol::Hls {
        // Up to 15 s to read the playlist: a stop or new settings do not wait for it.
        tokio::select! {
            c = super::hls::choose(&cfg.url, &cfg.variant) => c.1,
            _ = stop_rx.changed() => {
                finish_run(rec, &run_id);
                return Outcome::Stopped;
            }
            r = cfg_rx.changed() => {
                finish_run(rec, &run_id);
                return if r.is_ok() { Outcome::Restart } else { Outcome::Stopped };
            }
        }
    } else {
        None
    };
    let empty = std::mem::take(&mut *rec.empty_streams.lock().unwrap());
    if !empty.is_empty() {
        tracing::warn!("{}: streams {empty:?} of the source send nothing: left out", cfg.name);
    }
    let args = receiver_args(input, &run_dir, ts, epg_map.as_deref(), relay_port, program, &empty);
    let extra_audio = if audio { rec.audio_extra.load(Ordering::Relaxed) } else { 0 };
    for k in 1..=extra_audio {
        let _ = std::fs::create_dir_all(run_dir.join(format!("audio{k}")));
    }
    let preview_args = preview_args(port, &run_dir, &base.join("thumb.jpg"), audio, extra_audio);
    tracing::debug!("{}: ffmpeg {}", cfg.name, args.join(" "));
    tracing::debug!("{}: preview: ffmpeg {}", cfg.name, preview_args.join(" "));
    // The preview first: it listens before the receiver sends. A little lower priority: when the
    // server is short of CPU, the preview falls behind, never the recording.
    let mut pcmd = Command::new(&m.ffmpeg);
    pcmd.args(&preview_args).current_dir(&run_dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
    super::lower_priority(&mut pcmd, 5);
    let mut preview = match pcmd.spawn() {
        Ok(c) => {
            crate::procs::tag(c.id(), format!("rec:{}", rec.id), "Preview");
            c
        }
        Err(e) => {
            finish_run(rec, &run_id);
            return Outcome::Exited { had_data: false, message: format!("cannot start {}: {e}", m.ffmpeg) };
        }
    };
    let _relay = Relay::start(relay_sock, port, rec.clone());
    let plog: Arc<Mutex<VecDeque<String>>> = Default::default();
    if let Some(err) = preview.stderr.take() {
        let (plog, rec2, name) = (plog.clone(), rec.clone(), cfg.name.clone());
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                // What the picture and the sound show; its other info lines are not kept.
                if rec2.content.lock().unwrap().read(&l, super::now_ms()) {
                    continue;
                }
                if l.contains("[info]") || l.contains("[verbose]") {
                    continue;
                }
                let l = l.replacen("[warning] ", "", 1).replacen("[error] ", "", 1).replacen("[fatal] ", "", 1);
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
    // A test pattern: its FFmpeg feeds the receiver.
    if cfg.protocol == Protocol::Pattern {
        match super::pattern::command(&m.ffmpeg, m.data_dir(), cfg).spawn() {
            Ok(mut f) => {
                crate::procs::tag(f.id(), format!("rec:{}", rec.id), "Test pattern");
                match f.stdout.take().map(TryInto::<Stdio>::try_into) {
                    Some(Ok(out)) => stdin = out,
                    _ => {
                        finish_run(rec, &run_id);
                        return Outcome::Exited { had_data: false, message: "pattern: cannot read its output".into() };
                    }
                }
                if let Some(err) = f.stderr.take() {
                    let name = cfg.name.clone();
                    tokio::spawn(async move {
                        let mut lines = tokio::io::BufReader::new(err).lines();
                        while let Ok(Some(l)) = lines.next_line().await {
                            // The receiver stopping closes the pattern's output: its "Broken pipe"
                            // lines are the end of a recording, not a fault.
                            if pattern_closed(&l) {
                                tracing::debug!("{name}: pattern: {}", l.trim());
                            } else {
                                tracing::warn!("{name}: pattern: {}", l.trim());
                            }
                        }
                    });
                }
                feeder = Some(f);
            }
            Err(e) => {
                finish_run(rec, &run_id);
                return Outcome::Exited { had_data: false, message: format!("pattern: cannot start {}: {e}", m.ffmpeg) };
            }
        }
    }
    let stdout = if epg_map.is_some() { Stdio::piped() } else { Stdio::null() };
    let child = Command::new(&m.ffmpeg).args(&args).current_dir(&run_dir).stdin(stdin).stdout(stdout).stderr(Stdio::piped()).kill_on_drop(true).spawn();
    if let Ok(c) = &child {
        crate::procs::tag(c.id(), format!("rec:{}", rec.id), "Recording");
    }
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            stop_child(&mut preview).await;
            finish_run(rec, &run_id);
            return Outcome::Exited { had_data: false, message: format!("cannot start {}: {e}", m.ffmpeg) };
        }
    };
    // A multicast source: the server watches the same group beside FFmpeg (the stream as it
    // arrives: missing packets, sent twice, marked in error), until the run ends.
    let _witness = if matches!(cfg.protocol, Protocol::Udp | Protocol::Rtp) {
        super::udp_in::Target::parse(&cfg.url.replacen("rtp://", "udp://", 1), &cfg.iface).filter(|t| t.multicast()).and_then(|t| super::udp_in::watch(&t, rec.net.clone()).map_err(|e| tracing::debug!("{}: no witness: {e}", cfg.name)).ok())
    } else {
        None
    };
    let log: Arc<Mutex<VecDeque<String>>> = Default::default();
    // The guide found in the input while FFmpeg does not write it yet: start again with it.
    let epg_new = Arc::new(tokio::sync::Notify::new());
    if let Some(err) = child.stderr.take() {
        let log = log.clone();
        let name = cfg.name.clone();
        let (rec2, epg_new2, mapped, data_dir2) = (rec.clone(), epg_new.clone(), epg_map.is_some(), m.data_dir().to_path_buf());
        let said_of = cfg.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(err).lines();
            let mut info = super::epg::InfoReader::default();
            // Joining a stream in its course, FFmpeg complains of the pictures begun before it
            // (references missing up to the first key frame): nothing was damaged.
            let joined = std::time::Instant::now();
            // Which of the input's streams are pictures ("Stream #0:1[0x101]: Video: …").
            let mut pictures: Vec<u32> = Vec::new();
            while let Ok(Some(l)) = lines.next_line().await {
                let l = redact(&l, &said_of);
                if let Some(n) = empty_stream(&l) {
                    let mut e = rec2.empty_streams.lock().unwrap();
                    if !e.contains(&n) {
                        e.push(n);
                    }
                }
                if let Some((n, video)) = stream_kind(&l) {
                    if video && !pictures.contains(&n) {
                        pictures.push(n);
                    }
                }
                if let Some(kind) = glitch_of(&l).filter(|_| joined.elapsed() > Duration::from_secs(5)) {
                    rec2.glitch(kind);
                    // Of picture or of sound ("Packet corrupt (stream = 1, dts = …)").
                    if let Some(n) = corrupt_stream(&l) {
                        rec2.damage[if pictures.is_empty() && n == 0 || pictures.contains(&n) { 0 } else { 1 }].fetch_add(1, Ordering::Relaxed);
                    }
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

    // Only a live web page gives its last segments again when it is read anew (its DVR window);
    // another source restarting its clock from 0 (an encoder reconnecting, SDI, NDI, the test
    // pattern) is new picture, never a repeat: nothing of it is passed over.
    let dup_end = if cfg.protocol == Protocol::Web { *rec.last_end.lock().unwrap() } else { None };
    let mut scan = Scanner { dup_end, ..Default::default() };
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut lag_logged = 0i64;
    // The preview's pictures: how far they went, and since when they have not gone further; when
    // the recording last went further (by the clock of the server: the source's may go back).
    let mut video_seen: (Option<u32>, i64) = (None, now_ms());
    let mut audio_seen: (Option<u32>, i64) = (None, now_ms());
    let mut master_seen: (Option<f64>, i64) = (None, now_ms());
    let mut waves_at = now_ms();
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
                let what = if cfg.protocol == Protocol::Ndi { "ndi" } else { "decklink" };
                match end {
                    super::decklink::End::FormatChanged(f) => {
                        tracing::info!("{}: the source is now {f}: the capture starts again in it", cfg.name);
                        break Outcome::Restart;
                    }
                    super::decklink::End::Failed(why) => {
                        let detail = clog.lock().unwrap().iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
                        tracing::warn!("{}: the capture stopped: {why}{}", cfg.name, if detail.is_empty() { String::new() } else { format!(" ({detail})") });
                        break Outcome::Exited { had_data: scan.had_data, message: format!("{what}: {why}") };
                    }
                    super::decklink::End::Stopped => break Outcome::Exited { had_data: scan.had_data, message: format!("{what}: the capture stopped") },
                }
            }
            status = preview.wait() => {
                // The preview ended on its own: it cannot go on without the receiver, nor this run without it.
                let lines: Vec<String> = plog.lock().unwrap().iter().cloned().collect();
                stop_child(&mut child).await;
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                // Fewer sound tracks than its last recording had (another programme, another
                // source): the preview makes only those it has, at once.
                let missing = lines.iter().filter(|l| l.contains("matches no streams")).filter_map(|l| l.split("0:a:").nth(1)?.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().ok()).filter(|k| *k >= 1).min();
                if let Some(k) = missing {
                    rec.audio_extra.store(k - 1, Ordering::Relaxed);
                    tracing::info!("{}: {} sound track(s) in the source now", cfg.name, k);
                    break Outcome::NoAudio;
                }
                if audio && lines.iter().any(|l| l.contains("matches no streams") && l.contains("0:a:0")) {
                    rec.no_audio.store(true, Ordering::Relaxed);
                    tracing::info!("{}: no audio in the source", cfg.name);
                    break Outcome::NoAudio;
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
            _ = rec.restart_run.notified() => {
                stop_child(&mut child).await;
                tokio::task::block_in_place(|| scan.scan(m, rec, &run_id, true));
                break Outcome::Restart;
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
                // Measured by batches: a few fragments, or the oldest waiting for 4 s.
                if scan.waves.len() >= WAVE_BATCH || (!scan.waves.is_empty() && now_ms() - waves_at > WAVE_BATCH as i64 * 1000) {
                    measure(m, rec, &run_id, std::mem::take(&mut scan.waves));
                    waves_at = now_ms();
                }
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
                // The sound: only when the source still sends it (one that stopped sending sound
                // is not the preview's fault, and a new recording would cut the lives).
                let sound = !rec.no_audio.load(Ordering::Relaxed) && now_ms() - rec.last_sound.load(Ordering::Relaxed) < 10_000;
                // Not while lives send the source: a new recording would cut them (a person stops
                // or starts them again, never the server).
                let lives = !rec.taps.lock().unwrap().is_empty();
                if !lives && (stuck(&video_seen) || (sound && stuck(&audio_seen))) && now_ms() - master_seen.1 < 15_000 {
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
                // (Its settings as they are now: the recording mode may have changed meanwhile.)
                if !cfg_rx.borrow().enabled && !m.wants(&rec.id, now_ms()) {
                    if cfg.protocol == super::Protocol::Pattern {
                        tracing::info!("{}: no longer shown nor sent: stopped", cfg.name);
                    } else {
                        tracing::info!("{}: end of the programmed slot (or of its last stream)", cfg.name);
                    }
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
    // The last sound levels (those waiting for their batch, and the last fragments).
    measure(m, rec, &run_id, std::mem::take(&mut scan.waves));
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

/// "  Stream #0:1[0x101](fra): Audio: mp2 …" → (1, false): a stream of the input, and whether it is pictures.
fn stream_kind(line: &str) -> Option<(u32, bool)> {
    let rest = line.trim_start().strip_prefix("Stream #0:")?;
    let n: u32 = rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    let video = rest.contains(": Video:");
    (video || rest.contains(": Audio:")).then_some((n, video))
}

/// "[mpegts @ 0x9] Could not find codec parameters for stream 2 (Audio: mp3 …): unspecified frame
/// size" → 2: an audio track that sent nothing while FFmpeg looked (it cannot be copied).
fn empty_stream(line: &str) -> Option<u32> {
    let rest = line.split_once("Could not find codec parameters for stream ")?.1;
    let (n, kind) = rest.split_once(' ')?;
    kind.starts_with("(Audio").then(|| n.parse().ok()).flatten()
}

/// "[mpegts @ 0x9] Packet corrupt (stream = 1, dts = 1234)." → 1.
fn corrupt_stream(line: &str) -> Option<u32> {
    line.split_once("Packet corrupt (stream = ")?.1.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// A reception problem in FFmpeg's words: packets lost before FFmpeg (the network or a
/// receiver too slow), or a damaged stream (what the picture shows as blocks, freezes).
/// One line a damaged packet: what FFmpeg says again of the same packet (its size, then once
/// for each thing written) is not counted.
fn glitch_of(line: &str) -> Option<super::Glitch> {
    let l = line.to_ascii_lowercase();
    if l.contains("corrupt input packet") || l.contains("pes packet size mismatch") {
        return None;
    }
    const DAMAGED: [&str; 10] = [
        "packet corrupt", "error while decoding", "decode_slice_header error", "concealing",
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
/// The sound levels of new audio fragments, measured together: one FFmpeg for a batch of
/// fragments that follow each other (one per fragment, every second, loaded the server).
fn measure(m: &Arc<Manager>, rec: &Arc<Rec>, run_id: &str, todo: Vec<Wave>) {
    if todo.is_empty() {
        return;
    }
    let (ffmpeg, rec, run_id) = (m.ffmpeg.clone(), rec.clone(), run_id.to_string());
    tokio::task::spawn_blocking(move || {
        // Batches of fragments in a row (a gap in the numbers starts another).
        let mut batches: Vec<Vec<Wave>> = Vec::new();
        for w in todo {
            match batches.last_mut() {
                Some(b) if b.last().is_some_and(|l| l.n + 1 == w.n && l.init == w.init) => b.push(w),
                _ => batches.push(vec![w]),
            }
        }
        for batch in batches {
            let segs: Vec<(&Path, f64)> = batch.iter().map(|w| (w.seg.as_path(), w.dur)).collect();
            let Some(levels) = super::index::wave::compute_many(&ffmpeg, &batch[0].init, &segs) else { continue };
            for (w, levels) in batch.iter().zip(levels) {
                if levels.is_empty() {
                    continue;
                }
                let _ = std::fs::write(&w.out, &levels);
                let mut runs = rec.runs.write().unwrap();
                if let Some(r) = runs.iter_mut().find(|r| r.meta.id == run_id) {
                    if r.audio.segs.iter().any(|s| s.n == w.n) {
                        r.audio.waves.insert(w.n, Arc::new(levels));
                    }
                }
            }
        }
    });
}

/// An audio fragment to measure: its number, its track's init, itself, its duration (s), its level file.
struct Wave {
    n: u32,
    init: PathBuf,
    seg: PathBuf,
    dur: f64,
    out: PathBuf,
}
/// The fragments measured together: about every 4 s.
const WAVE_BATCH: usize = 4;

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
pub(crate) async fn stop_within(child: &mut tokio::process::Child, grace: Duration) {
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
    // What the preview saw ends with it.
    {
        let mut c = rec.content.lock().unwrap();
        (c.black, c.frozen, c.silent) = (None, None, None);
    }
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

/// A line of the test pattern's FFmpeg written as its output is closed (the receiver stopped).
fn pattern_closed(line: &str) -> bool {
    ["Broken pipe", "Error muxing a packet", "Conversion failed", "Error writing trailer", "Error closing file", "Terminating thread with return code"].iter().any(|k| line.contains(k))
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
    /// New audio fragments to measure.
    waves: Vec<Wave>,
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
        // The preview's fragments are read once the recording has its first segment: their times
        // are put on its turn of 26 h 30 (see `same_turn`).
        let ready = self.first_t0.is_some();
        let new_video = if ready { self.read_proxy(&dir, true) } else { Vec::new() };
        let new_audio = if !ready || rec.no_audio.load(Ordering::Relaxed) { Vec::new() } else { self.read_proxy(&dir, false) };
        let mut media = None;
        if !self.probed {
            if let Some(first) = new_master.first() {
                self.probed = true;
                media = probe(&m.ffmpeg, &dir.join(super::index::MASTER).join(format!("s{:08}.ts", first.n)));
                // More sound tracks than the preview makes: the run starts again with them all.
                let want = media.as_ref().map(|mi| mi.audio_tracks.saturating_sub(1).min(super::PREVIEW_EXTRA)).unwrap_or(0);
                if want > rec.audio_extra.load(Ordering::Relaxed) && !rec.no_audio.load(Ordering::Relaxed) {
                    rec.audio_extra.store(want, Ordering::Relaxed);
                    rec.restart_run.notify_one();
                }
            }
        }
        {
            let mut runs = rec.runs.write().unwrap();
            let Some(run) = runs.iter_mut().find(|r| r.meta.id == run_id) else { return };
            for s in new_master {
                run.append_master(s);
            }
            for (video, segs) in [(true, new_video), (false, new_audio)] {
                // Once the recording has a segment: the preview reads the source through MPEG-TS,
                // its times may be whole turns of 26 h 30 below the recording's (an RTMP encoder
                // up for days); they are put on its turn.
                let near = run.master.front().map(|s| s.t0).or(self.first_t0);
                if let (None, Some(near)) = (run.track(video).offset, near) {
                    if let Ok(init) = std::fs::read(run.track_dir(video).join("init.mp4")) {
                        if let Some((offset, ts)) = mp4::track_offset(&init) {
                            let t = run.track_mut(video);
                            t.offset = Some(super::same_turn(offset, near));
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
                        self.waves.push(Wave { n, init: run.track_dir(false).join("init.mp4"), seg: run.proxy_path(false, n), dur, out: run.wave_path(n) });
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
                // The file's clock starts again from 0 every 26 h 30 (MPEG-TS), the list's goes on:
                // the start read in the file, on the list's turn of the clock.
                t0 = start_time(ffmpeg, &file).map(|t| super::same_turn(t, t1)).unwrap_or(t1 - SEGMENT as f64);
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

/// A sound track's kind from its disposition (the DVB audio type FFmpeg reads).
fn audio_kind(d: &serde_json::Value) -> &'static str {
    let on = |k: &str| d[k].as_i64() == Some(1);
    if on("visual_impaired") {
        "ad"
    } else if on("hearing_impaired") {
        "hi"
    } else if on("clean_effects") {
        "intl"
    } else {
        ""
    }
}

/// Codec, size, frame rate, scan and audio tracks of a segment.
pub fn probe(ffmpeg: &str, file: &Path) -> Option<MediaInfo> {
    let out = std::process::Command::new(ffprobe_of(ffmpeg)).args(["-v", "error", "-show_entries", "stream=codec_type,codec_name,width,height,r_frame_rate,avg_frame_rate,field_order:stream_tags=language:stream_disposition=visual_impaired,hearing_impaired,clean_effects", "-of", "json"]).arg(file).output().ok()?;
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
        audio_langs: streams.iter().filter(|s| s["codec_type"] == "audio").map(|s| s["tags"]["language"].as_str().unwrap_or("").to_string()).collect(),
        audio_kinds: streams.iter().filter(|s| s["codec_type"] == "audio").map(|s| audio_kind(&s["disposition"]).to_string()).collect(),
        subtitles: streams.iter().filter(|s| s["codec_type"] == "subtitle").map(|s| s["codec_name"].as_str().unwrap_or("").to_string()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(protocol: Protocol, mode: Mode) -> RecorderConfig {
        RecorderConfig { id: "x".into(), name: "x".into(), protocol, url: "udp://239.1.1.1:5000".into(), mode, port: 9000, path: "live/a".into(), latency: 120, passphrase: String::new(), iface: "10.0.0.2".into(), cookies: String::new(), loop_hours: 24, bitrate: 8.0, cbr: false, enabled: true, device: 0, connection: "auto".into(), video_mode: "auto".into(), audio_channels: 2, audio_input: "auto".into(), variant: String::new(), host: String::new(), slots_off: false, pattern: "smpte".into(), rec_by: String::new() }
    }

    #[test]
    fn pattern_end_is_not_a_fault() {
        assert!(pattern_closed("[out#0/mpegts @ 0x55] Error writing trailer: Broken pipe"));
        assert!(pattern_closed("[out#0/mpegts @ 0x55] Error muxing a packet"));
        assert!(!pattern_closed("[lavfi @ 0x55] Unable to find a suitable output format"));
    }

    #[test]
    fn duplicates_only_before_the_previous_end() {
        let s = Scanner { dup_end: Some(25.5), ..Default::default() };
        assert!(s.duplicate(18.0, 24.0));
        assert!(!s.duplicate(24.0, 30.0));
        // Without a previous end (every source but a web page): nothing is passed over.
        assert!(!Scanner::default().duplicate(0.0, 6.0));
    }

    #[test]
    fn audio_kept_on_its_times() {
        let a = preview_args(5000, Path::new("/r"), Path::new("/t.jpg"), true, 0);
        let i = a.iter().position(|x| x == "0:a:0").unwrap();
        assert_eq!(a[i + 2], "-af");
        assert!(a[i + 3].starts_with(AUDIO_SYNC));
        assert!(a.contains(&"udp://127.0.0.1:5000?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216".to_string()));
        // The receiver only copies (no encoding), and keeps the damaged frames: thrown away, they
        // make the mosaic last to the next key frame.
        let r = receiver_args(vec!["-i".into(), "x".into()], Path::new("/r"), false, None, 5000, None, &[]);
        assert!(!r.iter().any(|x| x.contains("discardcorrupt") || x.contains("libx264") || x == "-vf" || x == "-af"));
        assert!(r.contains(&"udp://127.0.0.1:5000?pkt_size=1316".to_string()));
        // A track that sends nothing: left out of both outputs.
        let r = receiver_args(vec!["-i".into(), "x".into()], Path::new("/r"), false, None, 5000, None, &[2]);
        assert_eq!(r.iter().filter(|x| *x == "-0:2").count(), 2);
        assert_eq!(empty_stream("[mpegts @ 0x5] Could not find codec parameters for stream 2 (Audio: mp3 (mp3float), 0 channels, fltp): unspecified frame size"), Some(2));
        assert_eq!(empty_stream("[mpegts @ 0x5] Could not find codec parameters for stream 3 (Subtitle: dvb_teletext)"), None);
    }

    #[test]
    fn a_file_start_on_the_right_turn() {
        let wrap = crate::recorder::PTS_WRAP;
        // Ten turns: the file says 500 s, the list ends the segment 6 s later on the tenth turn.
        assert!((crate::recorder::same_turn(500.0, 10.0 * wrap + 506.0) - (10.0 * wrap + 500.0)).abs() < 1e-6);
        assert_eq!(crate::recorder::same_turn(500.0, 506.0), 500.0);
        // Just after a turn: the file says near the end of the turn, the list a little past it.
        assert!((crate::recorder::same_turn(wrap - 2.0, wrap + 4.0) - (wrap - 2.0)).abs() < 1e-6);
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
        // Said again of the same packet: not counted twice.
        assert_eq!(glitch_of("[in#0/mpegts @ 0x5] corrupt input packet in stream 1"), None);
        assert_eq!(glitch_of("[mpegts @ 0x9] PES packet size mismatch"), None);
        assert_eq!(glitch_of("[rtp @ 0x9] RTP: missed 3 packets"), Some(super::super::Glitch::Lost));
        assert_eq!(corrupt_stream("[mpegts @ 0x9] Packet corrupt (stream = 1, dts = 1234)."), Some(1));
        assert_eq!(stream_kind("  Stream #0:0[0x100]: Video: h264 (High), yuv420p"), Some((0, true)));
        assert_eq!(stream_kind("  Stream #0:1[0x101](fra): Audio: mp2, 48000 Hz"), Some((1, false)));
        assert_eq!(stream_kind("  Stream #0:2[0x12]: Data: epg"), None);
    }

    #[test]
    fn inputs() {
        assert_eq!(input_args(&cfg(Protocol::Udp, Mode::Listener)).last().unwrap(), "udp://239.1.1.1:5000?localaddr=10.0.0.2&fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout=5000000");
        // The same address sending RTP: read as RTP, on the same card.
        assert_eq!(input_args_as(&cfg(Protocol::Udp, Mode::Listener), true).last().unwrap(), "rtp://239.1.1.1:5000?localaddr=10.0.0.2&timeout=5000000&buffer_size=33554432&ttl=0");
        let r = input_args(&cfg(Protocol::Rtmp, Mode::Listener));
        assert_eq!(r, vec!["-listen", "1", "-i", "rtmp://0.0.0.0:9000/live/a"]);
        let mut c = cfg(Protocol::Srt, Mode::Listener);
        c.passphrase = "secret pass&1".into();
        assert_eq!(input_args(&c).last().unwrap(), "srt://0.0.0.0:9000?mode=listener&latency=120000&passphrase=secret%20pass%261");
        let mut c = cfg(Protocol::Hls, Mode::Caller);
        c.url = "https://cdn.example/live/index.m3u8".into();
        assert_eq!(input_args(&c), vec!["-rw_timeout", "20000000", "-i", "https://cdn.example/live/index.m3u8"]);
        assert!(c.validate().is_ok());
        c.url = "cdn.example/live/index.m3u8".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn hls_reasons() {
        assert_eq!(hls_reason("[https @ 0x1] HTTP error 404 Not Found\nError opening input files: Server returned 404 Not Found"), "hls: no stream at this address (404)");
        assert_eq!(hls_reason("Error opening input files: Invalid data found when processing input"), "hls: this address is not an hls playlist (.m3u8)");
        assert_eq!(hls_reason("something else"), "");
    }

    #[test]
    fn udp_reasons() {
        let lines: Vec<String> = ["[udp @ 0x1] setsockopt(IP_ADD_MEMBERSHIP): No such device", "Error opening input file udp://x.", "Error opening input files: No such device"].map(String::from).to_vec();
        assert!(best_line(&lines).contains("IP_ADD_MEMBERSHIP"));
        assert_eq!(udp_reason(&best_line(&lines)), "udp: cannot join the multicast on this network card");
        assert_eq!(udp_reason("Error opening input files: Input/output error"), "udp: nothing received (address, network card, multicast)");
    }

    #[test]
    fn secrets_not_said() {
        let srt = RecorderConfig { url: "srt://encoder.example:9000?mode=caller".into(), passphrase: "très secret&1".into(), ..cfg(Protocol::Srt, Mode::Caller) };
        let said = "[srt @ 0x1] Connection to srt://encoder.example:9000?mode=caller&latency=200000&passphrase=tr%C3%A8s%20secret%261 failed: Input/output error";
        let r = redact(said, &srt);
        assert!(!r.contains("secret") && !r.contains("%C3"), "{r}");
        assert!(r.contains("srt://encoder.example:9000") && r.contains("failed: Input/output error"), "{r}");
        let rtmp = RecorderConfig { url: "rtmp://live.example/app/KEY123456".into(), ..cfg(Protocol::Srt, Mode::Caller) };
        assert_eq!(redact("rtmp://live.example/app/KEY123456: I/O error", &rtmp), "rtmp://live.example: I/O error");
        assert_eq!(redact("x passphrase=abc&y=1", &cfg(Protocol::Udp, Mode::Caller)), "x passphrase=••••&y=1");
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
