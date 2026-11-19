//! A source tried before its recorder is made (the recorder's window): FFmpeg reads it a few
//! seconds, as the recorder would, for a still and what the stream carries (picture, sound
//! tracks, programme guide, services). A listening source (SRT, RTMP) cannot be tried: it comes
//! to the server once its recorder listens.

use super::{web, Manager, Protocol, RecorderConfig};
use base64::Engine;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Two tries at a time at most (each is an FFmpeg reading a stream).
static TRIES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// What FFmpeg says of the stream: its picture, its sound tracks, a guide, its services' names.
#[derive(Debug, Default, PartialEq)]
pub struct Described {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub interlaced: bool,
    pub audio: u32,
    pub epg: bool,
    pub services: Vec<String>,
}

/// Reads FFmpeg's description of its input (`-loglevel info`).
pub fn describe(text: &str) -> Described {
    let mut d = Described::default();
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("service_name").and_then(|r| r.trim_start().strip_prefix(':')) {
            let name = rest.trim().to_string();
            if !name.is_empty() && !d.services.contains(&name) {
                d.services.push(name);
            }
        }
        if !l.starts_with("Stream #0:") {
            continue;
        }
        if let Some(v) = l.split(": Video: ").nth(1) {
            if !d.codec.is_empty() {
                continue;
            }
            d.codec = v.split([' ', ',']).next().unwrap_or("").to_string();
            d.interlaced = v.contains("top first") || v.contains("bottom first") || v.contains("top coded first") || v.contains("bottom coded first");
            for part in v.split(", ") {
                let first = part.split_whitespace().next().unwrap_or("");
                if let Some((w, h)) = first.split_once('x') {
                    if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                        if d.width == 0 {
                            (d.width, d.height) = (w, h);
                        }
                    }
                }
                if let Some(f) = part.strip_suffix(" fps").and_then(|f| f.trim().parse::<f64>().ok()) {
                    d.fps = f;
                }
            }
        } else if l.contains(": Audio: ") {
            d.audio += 1;
        } else if l.contains(": Data: epg") {
            d.epg = true;
        }
    }
    d
}

/// Tries a source: `Ok` with what it carries (and a still when it has a picture), `Err` with why
/// not ("listener" for a listening source).
pub async fn probe(m: &Manager, mut c: RecorderConfig) -> Result<Value, String> {
    // Saved secrets (masked in the window) are those of the recorder being edited.
    if let Some(r) = (!c.id.is_empty()).then(|| m.get(&c.id)).flatten() {
        let saved = r.config();
        if c.passphrase == super::MASKED {
            c.passphrase = saved.passphrase;
        }
        if c.cookies == super::MASKED {
            c.cookies = saved.cookies;
        }
    } else {
        if c.passphrase == super::MASKED {
            c.passphrase.clear();
        }
        if c.cookies == super::MASKED {
            c.cookies.clear();
        }
    }
    if c.protocol == Protocol::Decklink {
        return Err("a card's input has its own preview".into());
    }
    if c.listens() {
        return Err("listener".into());
    }
    // Only what makes the source matters here.
    let named = RecorderConfig { name: "x".into(), loop_hours: 1, bitrate: c.bitrate.clamp(0.1, 2000.0), ..c.clone() };
    named.validate()?;
    let _one = TRIES.acquire().await.map_err(|e| e.to_string())?;
    // The cookies are written for yt-dlp under a name of their own, removed after.
    let id = format!("probe-{}", uuid::Uuid::new_v4().simple());
    let c = RecorderConfig { id: id.clone(), ..c };
    let out = run(m, &c).await;
    let _ = std::fs::remove_file(m.data_dir().join("cookies").join(format!("{id}.txt")));
    out
}

async fn run(m: &Manager, c: &RecorderConfig) -> Result<Value, String> {
    let mut feeder = None;
    let input = if c.protocol == Protocol::Web {
        let source = tokio::time::timeout(Duration::from_secs(40), web::resolve(m.data_dir(), c)).await.map_err(|_| "web: the page does not answer".to_string())??;
        if matches!(source, web::Source::Piped) {
            feeder = Some(web::pipe_command(m.data_dir(), c).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().map_err(|e| format!("web: cannot start yt-dlp: {e}"))?);
        }
        web::input_args(&source)
    } else {
        super::run::input_args(c)
    };
    let mut args: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "info", "-analyzeduration", "3000000", "-probesize", "8000000"].map(String::from).to_vec();
    args.extend(input);
    // One picture: deinterlaced, 640 wide.
    args.extend(["-map", "0:v:0?", "-frames:v", "1", "-vf", "yadif=deint=interlaced,scale=640:-2", "-q:v", "4", "-f", "image2", "-c:v", "mjpeg", "pipe:1"].map(String::from));
    let mut cmd = Command::new(&m.ffmpeg);
    cmd.args(&args).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut pipe = None;
    match feeder.as_mut().and_then(|f| f.stdout.take()) {
        Some(o) => {
            cmd.stdin(Stdio::piped());
            pipe = Some(o);
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot start FFmpeg: {e}"))?;
    if let (Some(mut from), Some(mut to)) = (pipe, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut from, &mut to).await;
        });
    }
    let mut stdout = child.stdout.take().ok_or("no output")?;
    let mut stderr = child.stderr.take().ok_or("no output")?;
    let (mut jpeg, mut text) = (Vec::new(), Vec::new());
    let read = async {
        let _ = tokio::join!(stdout.read_to_end(&mut jpeg), stderr.read_to_end(&mut text));
        child.wait().await
    };
    let timed_out = tokio::time::timeout(Duration::from_secs(15), read).await.is_err();
    drop(feeder);
    let text = String::from_utf8_lossy(&text).into_owned();
    let d = describe(&text);
    if d.codec.is_empty() && d.audio == 0 {
        let why = super::run::best_line(&text.lines().map(str::to_string).collect::<Vec<_>>());
        // Nothing came (FFmpeg only says its input could not be read): said plainly.
        let silent = ["Input/output error", "timed out", "Error opening input", "Connection timed out", "End of file"].iter().any(|k| why.contains(k));
        let why = if timed_out || silent { String::new() } else { why };
        return Err(if why.is_empty() { "nothing received from this source".into() } else { why });
    }
    let still = (jpeg.len() > 100).then(|| format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&jpeg)));
    Ok(json!({
        "still": still,
        "video": (!d.codec.is_empty()).then(|| json!({ "codec": d.codec, "width": d.width, "height": d.height, "fps": d.fps, "interlaced": d.interlaced })),
        "audio": d.audio,
        "epg": d.epg,
        "services": d.services,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn described() {
        let text = "Input #0, mpegts, from 'udp://239.1.1.1:5000':\n  Duration: N/A, start: 1.4, bitrate: N/A\n  Program 1 \n    Metadata:\n      service_name    : France 2\n      service_provider: FTV\n  Stream #0:0[0x100]: Video: h264 (High) ([27][0][0][0] / 0x001B), yuv420p(tv, bt709, top first), 1920x1080 [SAR 1:1 DAR 16:9], 25 fps, 50 tbr, 90k tbn\n  Stream #0:1[0x101](fra): Audio: mp2 ([3][0][0][0] / 0x0003), 48000 Hz, stereo, fltp, 192 kb/s\n  Stream #0:2[0x102](qad): Audio: aac_latm, 48000 Hz, stereo, fltp\n  Stream #0:3[0x12]: Data: epg\n";
        let d = describe(text);
        assert_eq!(d, Described { codec: "h264".into(), width: 1920, height: 1080, fps: 25.0, interlaced: true, audio: 2, epg: true, services: vec!["France 2".into()] });
        let p = describe("  Stream #0:0: Video: hevc (Main), yuv420p(tv), 1280x720, 50 fps, 50 tbr\n");
        assert_eq!((p.codec.as_str(), p.width, p.fps, p.interlaced, p.audio), ("hevc", 1280, 50.0, false, 0));
    }
}
