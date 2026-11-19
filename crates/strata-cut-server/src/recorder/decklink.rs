//! Blackmagic DeckLink capture cards (SDI, HDMI…), through GStreamer's DeckLink module.
//!
//! A card gives raw pictures and sound: GStreamer captures and encodes them (H.264 at the
//! recorder's bitrate, AAC by stereo pair) into an MPEG-TS written in a named pipe, which the
//! recorder's receiver reads as any other stream. Everything after (segments, preview, clips)
//! is the same as for a network source.
//!
//! Needs Blackmagic's Desktop Video driver (the card's driver and its library) and GStreamer
//! with its "bad" (DeckLink), "ugly" (x264) and libav (AAC) modules.

use super::RecorderConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const GST_LAUNCH: &str = "gst-launch-1.0";

/// The card's inputs, as GStreamer names them.
pub const CONNECTIONS: [&str; 7] = ["auto", "sdi", "hdmi", "optical-sdi", "component", "composite", "svideo"];
/// The video formats offered ("auto": detected by the card, when it can).
pub const MODES: [&str; 22] = [
    "auto", "1080i50", "1080i5994", "1080i60", "1080p25", "1080p2997", "1080p30", "1080p50", "1080p5994", "1080p60", "1080p24", "1080p2398", "720p50", "720p5994", "720p60", "pal", "ntsc",
    "2160p25", "2160p2997", "2160p50", "2160p5994", "2160p60",
];
/// Where the sound comes from.
pub const AUDIO_INPUTS: [&str; 4] = ["auto", "embedded", "aes", "analog"];
/// Channels captured (recorded as stereo pairs: 1, 4 or 8 tracks).
pub const AUDIO_CHANNELS: [u32; 3] = [2, 8, 16];
/// The recording's bitrate (Mb/s): by default, and the range allowed.
pub const DEFAULT_BITRATE: f64 = 25.0;
pub const BITRATES: std::ops::RangeInclusive<f64> = 5.0..=200.0;

pub fn validate(c: &RecorderConfig) -> Result<(), String> {
    if c.device > 63 {
        return Err("the card number is 1 to 64".into());
    }
    if !CONNECTIONS.contains(&c.connection.as_str()) {
        return Err("unknown video input".into());
    }
    if !MODES.contains(&c.video_mode.as_str()) {
        return Err("unknown video format".into());
    }
    if !AUDIO_INPUTS.contains(&c.audio_input.as_str()) {
        return Err("unknown audio input".into());
    }
    if !AUDIO_CHANNELS.contains(&c.audio_channels) {
        return Err("the card captures 2, 8 or 16 audio channels".into());
    }
    if !BITRATES.contains(&c.bitrate) {
        return Err("the recording bitrate is 5 to 200 mb/s".into());
    }
    Ok(())
}

/// A row of the mix matrix that keeps channel `keep` of `n`.
fn row(n: u32, keep: u32) -> String {
    let v: Vec<&str> = (0..n).map(|k| if k == keep { "(float)1" } else { "(float)0" }).collect();
    format!("<{}>", v.join(","))
}

/// GStreamer's command line (without the program): the card captured, encoded and written as
/// MPEG-TS in `out` (the named pipe the receiver reads).
pub fn pipeline(c: &RecorderConfig, out: &Path) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let kbps = (c.bitrate * 1000.0).round() as u32;
    // `-e`: stopped with SIGINT, it ends the stream properly. No `-q`: its messages (signal
    // lost / recovered, errors) are read on its output.
    let mut a = s(&["-e"]);
    // Picture: the card's frames, never dropped by GStreamer (a queue of 3 s absorbs the
    // encoder's peaks), 4:2:0 for H.264; interlacing is kept (x264 reads it from the caps).
    a.extend(s(&["decklinkvideosrc", &format!("device-number={}", c.device), &format!("connection={}", c.connection), &format!("mode={}", c.video_mode), "drop-no-signal-frames=false", "!"]));
    a.extend(s(&["queue", "max-size-buffers=0", "max-size-bytes=0", "max-size-time=3000000000", "!", "videoconvert", "!", "video/x-raw,format=I420", "!"]));
    a.extend(s(&["x264enc", &format!("bitrate={kbps}"), &format!("vbv-buf-capacity={}", 1000), "speed-preset=superfast", "key-int-max=25", "!", "h264parse", "!", "queue", "!", "mux."]));
    // Sound: the channels captured, each stereo pair a track of its own.
    let n = c.audio_channels;
    a.extend(s(&["decklinkaudiosrc", &format!("device-number={}", c.device), &format!("connection={}", c.audio_input), &format!("channels={n}"), "!"]));
    a.extend(s(&["queue", "max-size-buffers=0", "max-size-bytes=0", "max-size-time=3000000000", "!", "tee", "name=sound"]));
    for pair in 0..n / 2 {
        let matrix = format!("mix-matrix=\"<{},{}>\"", row(n, 2 * pair), row(n, 2 * pair + 1));
        a.extend(s(&["sound.", "!", "queue", "!", "audioconvert", &matrix, "!", "audio/x-raw,channels=2,channel-mask=(bitmask)0x3", "!"]));
        a.extend(s(&["avenc_aac", "bitrate=192000", "!", "aacparse", "!", "queue", "!", "mux."]));
    }
    a.extend(s(&["mpegtsmux", "name=mux", "alignment=7", "!", "filesink", &format!("location={}", out.display()), "sync=false"]));
    a
}

/// The named pipe between GStreamer and the receiver (made anew).
pub fn fifo(data_dir: &Path, id: &str) -> std::io::Result<PathBuf> {
    let dir = data_dir.join("capture");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.ts"));
    let _ = std::fs::remove_file(&path);
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: a valid C string; mkfifo only creates the file.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(path)
}

/// What a line of GStreamer says of the input signal: lost (false) or back (true).
pub fn signal(line: &str) -> Option<bool> {
    if line.contains("Signal lost") {
        Some(false)
    } else if line.contains("Signal recovered") {
        Some(true)
    } else {
        None
    }
}

/// Why GStreamer could not capture, in the interface's words (empty: not known).
pub fn reason(lines: &[String]) -> String {
    let all = lines.join("\n");
    let missing = |element: &str| all.contains(&format!("no element \"{element}\""));
    if missing("decklinkvideosrc") || missing("decklinkaudiosrc") {
        "decklink: GStreamer's DeckLink module is missing (gstreamer1.0-plugins-bad)".into()
    } else if missing("x264enc") {
        "decklink: the H.264 encoder is missing (gstreamer1.0-plugins-ugly)".into()
    } else if missing("avenc_aac") {
        "decklink: the AAC encoder is missing (gstreamer1.0-libav)".into()
    } else if !driver_installed() {
        "decklink: Blackmagic's Desktop Video driver is not installed".into()
    } else if all.contains("has no input") || all.contains("Failed to acquire input") || all.contains("Failed to get device") || all.contains("Failed to set pipeline to PAUSED") {
        "decklink: card not found or already in use (driver, card number, another program)".into()
    } else if all.contains("Failed to enable") || all.contains("not supported") || all.contains("Failed to set") {
        "decklink: the card refuses these settings (input, format or audio channels)".into()
    } else if all.contains("not-negotiated") || all.contains("negotiation") {
        "decklink: the format detected cannot be recorded (choose the format)".into()
    } else {
        lines.iter().rev().find(|l| l.starts_with("ERROR")).map(|l| format!("decklink: {}", l.split(": ").last().unwrap_or(l).trim())).unwrap_or_default()
    }
}

/// Blackmagic's Desktop Video: the library GStreamer opens to reach the cards.
pub fn driver_installed() -> bool {
    ["/usr/lib/libDeckLinkAPI.so", "/usr/lib64/libDeckLinkAPI.so", "/usr/lib/x86_64-linux-gnu/libDeckLinkAPI.so"].iter().any(|p| Path::new(p).exists())
}

/// What the server can do with DeckLink cards: GStreamer and its module, the driver, the cards.
pub async fn status() -> Value {
    let gst = tokio::process::Command::new("gst-inspect-1.0").arg("decklinkvideosrc").output();
    let module = matches!(tokio::time::timeout(Duration::from_secs(10), gst).await, Ok(Ok(o)) if o.status.success());
    // Blackmagic's Desktop Video: its library, and the cards' device files.
    let driver = driver_installed();
    let nodes = std::fs::read_dir("/dev/blackmagic").map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("io")).count()).unwrap_or(0);
    let devices = if module && driver { list().await } else { Vec::new() };
    json!({ "gstreamer": module, "driver": driver, "nodes": nodes, "devices": devices })
}

/// The cards GStreamer sees (its device monitor), with their number.
async fn list() -> Vec<Value> {
    let run = tokio::process::Command::new("gst-device-monitor-1.0").args(["-f", "Video/Source"]).kill_on_drop(true).output();
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(8), run).await else { return Vec::new() };
    parse_monitor(&String::from_utf8_lossy(&out.stdout))
}

/// Reads gst-device-monitor's report: each "Device found" block, its name and its launch line.
fn parse_monitor(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for block in text.split("Device found:").skip(1) {
        let Some(launch) = block.lines().find(|l| l.contains("decklinkvideosrc")) else { continue };
        let name = block.lines().find_map(|l| l.trim().strip_prefix("name").map(|r| r.trim_start_matches([' ', ':']).trim().to_string())).unwrap_or_default();
        // The number is left out of the launch line when it is the default, 0.
        let number = launch.split_whitespace().find_map(|w| w.strip_prefix("device-number=")?.parse::<u32>().ok()).unwrap_or(0);
        out.push(json!({ "number": number, "name": name }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::{Mode, Protocol};

    fn cfg() -> RecorderConfig {
        RecorderConfig {
            id: "x".into(),
            name: "x".into(),
            protocol: Protocol::Decklink,
            url: String::new(),
            mode: Mode::Listener,
            port: 0,
            path: String::new(),
            latency: 120,
            passphrase: String::new(),
            iface: String::new(),
            cookies: String::new(),
            loop_hours: 24,
            bitrate: 25.0,
            enabled: true,
            device: 1,
            connection: "sdi".into(),
            video_mode: "1080i50".into(),
            audio_channels: 8,
            audio_input: "embedded".into(),
        }
    }

    #[test]
    fn pipelines() {
        let a = pipeline(&cfg(), Path::new("/d/capture/x.ts"));
        let line = a.join(" ");
        assert!(line.starts_with("-e decklinkvideosrc device-number=1 connection=sdi mode=1080i50 drop-no-signal-frames=false !"));
        assert!(line.contains("x264enc bitrate=25000 "));
        assert!(line.contains("decklinkaudiosrc device-number=1 connection=embedded channels=8 !"));
        // 8 channels: 4 stereo tracks, the second one made of channels 3 and 4.
        assert_eq!(a.iter().filter(|x| *x == "avenc_aac").count(), 4);
        assert!(a.contains(&"mix-matrix=\"<<(float)0,(float)0,(float)1,(float)0,(float)0,(float)0,(float)0,(float)0>,<(float)0,(float)0,(float)0,(float)1,(float)0,(float)0,(float)0,(float)0>>\"".to_string()));
        assert!(line.ends_with("filesink location=/d/capture/x.ts sync=false"));
        assert!(validate(&cfg()).is_ok());
        assert!(validate(&RecorderConfig { audio_channels: 6, ..cfg() }).is_err());
        assert!(validate(&RecorderConfig { video_mode: "1080i49".into(), ..cfg() }).is_err());
    }

    #[test]
    fn messages() {
        assert_eq!(signal("WARNING: from element /GstPipeline:pipeline0/GstDecklinkVideoSrc:decklinkvideosrc0: Signal lost"), Some(false));
        assert_eq!(signal("INFO: … Signal recovered"), Some(true));
        let l = |s: &str| vec![s.to_string()];
        assert!(reason(&l("WARNING: erroneous pipeline: no element \"decklinkvideosrc\"")).contains("plugins-bad"));
        let card = reason(&l("ERROR: from element /GstPipeline:pipeline0/GstDecklinkVideoSrc:decklinkvideosrc0: Failed to acquire input"));
        assert!(if driver_installed() { card.contains("not found") } else { card.contains("Desktop Video") }, "{card}");
        let m = "Device found:\n\n\tname  : DeckLink Duo (2)\n\tclass : Video/Source\n\tgst-launch-1.0 decklinkvideosrc device-number=1 persistent-id=123 ! ...\n\nDevice found:\n\n\tname  : DeckLink Duo (1)\n\tclass : Video/Source\n\tgst-launch-1.0 decklinkvideosrc persistent-id=122 ! ...\n";
        let d = parse_monitor(m);
        assert_eq!(d, vec![json!({"number": 1, "name": "DeckLink Duo (2)"}), json!({"number": 0, "name": "DeckLink Duo (1)"})]);
    }
}
