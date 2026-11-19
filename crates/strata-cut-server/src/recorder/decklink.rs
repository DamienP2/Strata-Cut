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
/// Every video format the cards know, as GStreamer names them ("auto": detected by the card,
/// when it can): SD, HD 720 and 1080, 2K, UHD and 4K DCI, 8K, at every frame rate.
pub const MODES: [&str; 68] = [
    "auto", "ntsc", "ntsc2398", "pal", "ntsc-p", "pal-p", "1080p2398", "1080p24", "1080p25", "1080p2997", "1080p30", "1080i50", "1080i5994",
    "1080i60", "1080p50", "1080p5994", "1080p60", "720p50", "720p5994", "720p60", "1556p2398", "1556p24", "1556p25", "2kdcip2398", "2kdcip24",
    "2kdcip25", "2kdcip2997", "2kdcip30", "2kdcip50", "2kdcip5994", "2kdcip60", "2160p2398", "2160p24", "2160p25", "2160p2997", "2160p30", "2160p50",
    "2160p5994", "2160p60", "ntsc-widescreen", "ntsc2398-widescreen", "pal-widescreen", "ntsc-p-widescreen", "pal-p-widescreen", "4kdcip2398",
    "4kdcip24", "4kdcip25", "4kdcip2997", "4kdcip30", "4kdcip50", "4kdcip5994", "4kdcip60", "8kp2398", "8kp24", "8kp25", "8kp2997", "8kp30", "8kp50",
    "8kp5994", "8kp60", "8kdcip2398", "8kdcip24", "8kdcip25", "8kdcip2997", "8kdcip30", "8kdcip50", "8kdcip5994", "8kdcip60",
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
    let known = ["/usr/lib/libDeckLinkAPI.so", "/usr/lib64/libDeckLinkAPI.so", "/usr/local/lib/libDeckLinkAPI.so"].iter().any(|p| Path::new(p).exists());
    // Wherever the package put it (/usr/lib/<arch>/…), or the cards' device files the driver makes.
    let in_arch = std::fs::read_dir("/usr/lib").into_iter().flatten().flatten().any(|e| e.path().join("libDeckLinkAPI.so").exists());
    known || in_arch || Path::new("/dev/blackmagic").is_dir()
}

/// What the server can do with DeckLink cards: GStreamer and its module, the driver, the cards.
pub async fn status(data_dir: &Path) -> Value {
    let gst = tokio::process::Command::new("gst-inspect-1.0").arg("decklinkvideosrc").output();
    let module = matches!(tokio::time::timeout(Duration::from_secs(10), gst).await, Ok(Ok(o)) if o.status.success());
    // Blackmagic's Desktop Video: its library, and the cards' device files.
    let driver = driver_installed();
    let nodes = std::fs::read_dir("/dev/blackmagic").map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("io")).count()).unwrap_or(0);
    let devices = if module && driver { list(data_dir).await } else { Vec::new() };
    json!({ "gstreamer": module, "driver": driver, "nodes": nodes, "devices": devices })
}

/// One input of a card, as the driver shows it (a GStreamer device).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    /// GStreamer's device number (the recorder's `device`).
    pub number: u32,
    /// "DeckLink Duo (2)".
    pub name: String,
    /// The card's serial number, when the driver gives it (the inputs of a card share it).
    #[serde(default)]
    pub serial: String,
    #[serde(default)]
    pub persistent_id: Option<i64>,
}

/// The cards adopted: every input seen once is kept (`hardware-cards.json`), shown even when the
/// driver does not list it for a moment, until it is forgotten (Système › Matériel).
fn adopted_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hardware-cards.json")
}

fn adopted(data_dir: &Path) -> Vec<Input> {
    crate::paths::read_json(&adopted_path(data_dir)).unwrap_or_default()
}

/// The inputs the last search found (`None`: not searched yet since the server started).
static FOUND: tokio::sync::Mutex<Option<Vec<Input>>> = tokio::sync::Mutex::const_new(None);

/// Adds what a search found to the adopted inputs: a known input (same name, same serial) takes
/// its new number; an absent one whose number another input now has is dropped.
fn adopt(known: &[Input], found: &[Input]) -> Vec<Input> {
    let same = |a: &Input, b: &Input| a.name == b.name && a.serial == b.serial;
    let mut all: Vec<Input> = known.iter().filter(|a| found.iter().any(|f| same(a, f)) || !found.iter().any(|f| f.number == a.number)).cloned().collect();
    for f in found {
        match all.iter_mut().find(|a| same(a, f)) {
            Some(a) => *a = f.clone(),
            None => all.push(f.clone()),
        }
    }
    all.sort_by_key(|i| i.number);
    all
}

/// The adopted inputs and the numbers of those the driver lists. The cards are searched once
/// after the server starts, then only when asked (`search`): not all the time.
async fn inputs(data_dir: &Path, search: bool) -> (Vec<Input>, Vec<u32>) {
    let mut found = FOUND.lock().await;
    if search || found.is_none() {
        let gst = tokio::process::Command::new("gst-inspect-1.0").arg("decklinkvideosrc").stdin(std::process::Stdio::null()).output();
        let module = matches!(tokio::time::timeout(Duration::from_secs(10), gst).await, Ok(Ok(o)) if o.status.success());
        let list = if module && driver_installed() { list_inputs().await } else { Vec::new() };
        let known = adopted(data_dir);
        let all = adopt(&known, &list);
        if all != known {
            if let Err(e) = crate::paths::write_json(&adopted_path(data_dir), &all, false) {
                tracing::warn!("decklink: cannot keep the cards adopted: {e}");
            }
        }
        tracing::info!("decklink: {} input(s) found, {} adopted", list.len(), all.len());
        *found = Some(list);
    }
    let here = found.as_ref().map(|l| l.iter().map(|i| i.number).collect()).unwrap_or_default();
    (adopted(data_dir), here)
}

/// The search made once after the server starts.
pub async fn search_at_start(data_dir: PathBuf) {
    let _ = inputs(&data_dir, false).await;
}

/// Forgets a card that is not there any more (its inputs leave the adopted ones).
pub async fn forget(data_dir: &Path, key: &str) -> Result<(), String> {
    let (all, here) = inputs(data_dir, false).await;
    let keys = card_keys(&all);
    let gone: Vec<u32> = all.iter().filter(|i| keys[&i.number] == key).map(|i| i.number).collect();
    if gone.is_empty() {
        return Err("unknown card".into());
    }
    if gone.iter().any(|n| here.contains(n)) {
        return Err("this card is there: it cannot be forgotten".into());
    }
    let left: Vec<Input> = all.into_iter().filter(|i| !gone.contains(&i.number)).collect();
    crate::paths::write_json(&adopted_path(data_dir), &left, false).map_err(|e| e.to_string())?;
    *SNAPSHOT.lock().unwrap() = None;
    Ok(())
}

/// The cards GStreamer sees (its device monitor).
async fn list_inputs() -> Vec<Input> {
    // The video sources, listed once: no `-f` (it means "follow": it would never end).
    let run = tokio::process::Command::new("gst-device-monitor-1.0").arg("Video/Source").stdin(std::process::Stdio::null()).kill_on_drop(true).output();
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(8), run).await else { return Vec::new() };
    parse_monitor(&String::from_utf8_lossy(&out.stdout))
}

/// The cards' inputs for the recorder's form (the adopted ones): each with its card (the name
/// given to it, else its model), its rank on that card, and whether the driver lists it now.
async fn list(data_dir: &Path) -> Vec<Value> {
    // Every adopted input: one the driver did not list this time is still offered.
    let (inputs, here) = inputs(data_dir, false).await;
    let names = names(data_dir);
    let keys = card_keys(&inputs);
    let mut rank: std::collections::HashMap<String, u32> = Default::default();
    let mut sorted = inputs.clone();
    sorted.sort_by(connector_order);
    sorted
        .iter()
        .map(|i| {
            let key = keys[&i.number].clone();
            let k = rank.entry(key.clone()).or_insert(0);
            *k += 1;
            json!({ "number": i.number, "name": i.name, "card": names.get(&key).cloned().unwrap_or_else(|| model_of(&i.name)), "input": *k, "present": here.contains(&i.number) })
        })
        .collect()
}

/// The input's number on its card, from the driver's name: "DeckLink Quad 2 (5)" → 5. It is the
/// connector's (SDI 5); the driver's own numbering goes in another order (1, 5, 2, 6… on a Quad 2).
fn sub_index(name: &str) -> Option<u32> {
    let n = name.trim();
    let i = n.rfind(" (")?;
    n.ends_with(')').then_some(())?;
    n[i + 2..n.len() - 1].parse().ok()
}

/// A card, for grouping its inputs and naming it: the driver gives each input of a card a number
/// of its own (…cb00 to …cb07 on a Quad 2); what they share, else the model.
fn card_key(i: &Input) -> String {
    let model = model_of(&i.name);
    if i.serial.chars().count() > 1 {
        let s: String = i.serial.chars().take(i.serial.chars().count() - 1).collect();
        format!("{model} {s}")
    } else {
        model
    }
}

/// Each input's card: by `card_key`, except that inputs of a model each alone on its "card"
/// (numbers that share nothing) are one card of that model.
fn card_keys(inputs: &[Input]) -> std::collections::HashMap<u32, String> {
    let mut keys: std::collections::HashMap<u32, String> = inputs.iter().map(|i| (i.number, card_key(i))).collect();
    let mut by_model: std::collections::HashMap<String, Vec<&Input>> = Default::default();
    for i in inputs {
        by_model.entry(model_of(&i.name)).or_default().push(i);
    }
    for (model, list) in by_model {
        let alone = list.iter().all(|i| list.iter().filter(|o| keys[&o.number] == keys[&i.number]).count() == 1);
        if list.len() > 1 && alone {
            for i in list {
                keys.insert(i.number, model.clone());
            }
        }
    }
    keys
}

/// The inputs in the order of the card's connectors (the number in their name), else the driver's.
fn connector_order(a: &Input, b: &Input) -> std::cmp::Ordering {
    (sub_index(&a.name).unwrap_or(u32::MAX), a.number).cmp(&(sub_index(&b.name).unwrap_or(u32::MAX), b.number))
}

/// The names given to the cards (Système › Matériel), by card.
fn names_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hardware-names.json")
}

pub fn names(data_dir: &Path) -> std::collections::HashMap<String, String> {
    crate::paths::read_json(&names_path(data_dir)).unwrap_or_default()
}

/// Names a card (an empty name gives it back its model's).
pub async fn set_name(data_dir: &Path, key: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    if key.is_empty() || key.len() > 200 {
        return Err("unknown card".into());
    }
    if name.chars().count() > 60 {
        return Err("the name has 60 characters at most".into());
    }
    let mut all = names(data_dir);
    if name.is_empty() {
        all.remove(key);
    } else {
        all.insert(key.to_string(), name.to_string());
    }
    crate::paths::write_json(&names_path(data_dir), &all, false).map_err(|e| e.to_string())
}

/// Reads gst-device-monitor's report: each "Device found" block, its name, its properties
/// ("key = value") and its launch line.
fn parse_monitor(text: &str) -> Vec<Input> {
    let mut out = Vec::new();
    for block in text.split("Device found:").skip(1) {
        let Some(launch) = block.lines().find(|l| l.contains("decklinkvideosrc")) else { continue };
        let name = block.lines().find_map(|l| l.trim().strip_prefix("name").map(|r| r.trim_start_matches([' ', ':']).trim().to_string())).unwrap_or_default();
        let prop = |key: &str| {
            block.lines().find_map(|l| {
                let (k, v) = l.trim().split_once(" = ")?;
                (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
            })
        };
        // The number is left out of the launch line when it is the default, 0.
        let number = launch
            .split_whitespace()
            .find_map(|w| w.strip_prefix("device-number=")?.parse::<u32>().ok())
            .or_else(|| prop("device-number").and_then(|v| v.parse().ok()))
            .unwrap_or(0);
        let persistent_id = launch.split_whitespace().find_map(|w| w.strip_prefix("persistent-id=")?.parse::<i64>().ok()).or_else(|| prop("persistent-id").and_then(|v| v.parse().ok()));
        let serial = prop("hw-serial-number").filter(|v| v != "NULL" && v != "(null)").unwrap_or_default();
        out.push(Input { number, name, serial, persistent_id });
    }
    out
}

/// "DeckLink Duo (2)" → "DeckLink Duo": the card's model, its inputs numbered by the driver.
fn model_of(name: &str) -> String {
    let n = name.trim();
    match n.rfind(" (") {
        Some(i) if n.ends_with(')') && n[i + 2..n.len() - 1].chars().all(|c| c.is_ascii_digit()) => n[..i].to_string(),
        _ => n.to_string(),
    }
}

/// The connector of a model's inputs, for the picture: HDMI on the Intensity cards and the
/// HDMI models, SDI (BNC) on the others.
fn connector_of(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    if m.contains("hdmi") || m.contains("intensity") {
        "hdmi"
    } else {
        "sdi"
    }
}

/// A video format as the cards name it ("1080i50", "720p5994", "2160p25", "4kdcip24",
/// "pal", "ntsc2398"…), from its size, scan and frame rate (frames per second).
pub fn mode_of(width: u32, height: u32, interlaced: bool, num: u32, den: u32) -> String {
    if den == 0 || num == 0 {
        return String::new();
    }
    let fps = num as f64 / den as f64;
    // The cards count fields for interlaced formats (1080i50: 25 frames, 50 fields).
    let rate = if interlaced { fps * 2.0 } else { fps };
    let r = if (rate - rate.round()).abs() < 0.01 { format!("{}", rate.round() as u32) } else { format!("{}", (rate * 100.0).round() as u32) };
    let name = match (width, height) {
        // SD: PAL (576 lines), NTSC (486 or 480).
        (_, 576) => match (interlaced, r.as_str()) {
            (true, _) => "pal".to_string(),
            (false, _) => "pal-p".to_string(),
        },
        (_, 486 | 480) => match (interlaced, r.as_str()) {
            (true, "2398") => "ntsc2398".to_string(),
            (true, _) => "ntsc".to_string(),
            (false, _) => "ntsc-p".to_string(),
        },
        (2048, 1556) => format!("1556p{r}"),
        (2048, 1080) => format!("2kdcip{r}"),
        (4096, 2160) => format!("4kdcip{r}"),
        (8192, 4320) => format!("8kdcip{r}"),
        (_, 4320) => format!("8kp{r}"),
        _ => format!("{height}{}{r}", if interlaced { 'i' } else { 'p' }),
    };
    name
}

/// One capture at a time on an input (the probes of Système › Matériel and the previews).
fn input_lock(number: u32) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::Mutex<Vec<std::sync::Arc<tokio::sync::Mutex<()>>>> = std::sync::Mutex::new(Vec::new());
    let mut l = LOCKS.lock().unwrap();
    while l.len() <= number as usize {
        l.push(Default::default());
    }
    l[number as usize].clone()
}

/// A still of what an input receives (JPEG, 640 wide) and its format, for the recorder's form.
/// `None`: no picture within a few seconds (no signal, or the input is busy).
pub async fn preview(number: u32, connection: &str, mode: &str) -> Option<(Vec<u8>, String)> {
    let lock = input_lock(number);
    let _one = lock.lock().await;
    let file = std::env::temp_dir().join(format!("strata-cut-preview-{}-{number}.jpg", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let args: Vec<String> = [
        // Without signal the card still gives (black) frames, in a default format: not a picture.
        "-v", "decklinkvideosrc", &format!("device-number={number}"), &format!("connection={connection}"), &format!("mode={mode}"), "drop-no-signal-frames=true", "num-buffers=4", "!",
        "deinterlace", "!", "videoconvert", "!", "videoscale", "!", "video/x-raw,width=640,pixel-aspect-ratio=1/1", "!",
        "jpegenc", "quality=80", "snapshot=true", "!", "filesink", &format!("location={}", file.display()),
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    let run = tokio::process::Command::new(GST_LAUNCH).args(&args).stdin(std::process::Stdio::null()).kill_on_drop(true).output();
    let out = tokio::time::timeout(Duration::from_secs(5), run).await.ok()?.ok()?;
    let jpeg = std::fs::read(&file).ok().filter(|b| b.len() > 100);
    let _ = std::fs::remove_file(&file);
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Some((jpeg?, caps_mode(&text).unwrap_or_default()))
}

/// What a free input receives now: (signal, format). A short capture: the card tells whether it
/// has a signal (its `signal` property) and the format it detects; a frame that comes through
/// (frames without signal are dropped) is a signal too. `None`: busy (another program).
async fn probe(number: u32) -> Option<(bool, String)> {
    use tokio::io::AsyncBufReadExt;
    let lock = input_lock(number);
    let _one = lock.lock().await;
    let args = ["-v", "decklinkvideosrc", &format!("device-number={number}"), "mode=auto", "drop-no-signal-frames=true", "num-buffers=1", "!", "fakesink", "sync=false"];
    let spawn = tokio::process::Command::new(GST_LAUNCH)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = spawn else { return Some((false, String::new())) };
    let mut out = tokio::io::BufReader::new(child.stdout.take()?).lines();
    let mut err = tokio::io::BufReader::new(child.stderr.take()?).lines();
    let (mut out_done, mut err_done) = (false, false);
    let (mut signal, mut format) = (None::<bool>, String::new());
    // The card takes a few seconds to start and detect the format.
    let deadline = tokio::time::sleep(Duration::from_secs(8));
    tokio::pin!(deadline);
    let mut ended = false;
    while !(out_done && err_done) {
        let line = tokio::select! {
            l = out.next_line(), if !out_done => match l { Ok(Some(l)) => l, _ => { out_done = true; continue } },
            l = err.next_line(), if !err_done => match l { Ok(Some(l)) => l, _ => { err_done = true; continue } },
            _ = &mut deadline => break,
        };
        if line.contains("Failed to acquire input") || line.contains("has no input") {
            return None;
        }
        if let Some(on) = signal_property(&line).or_else(|| self::signal(&line)) {
            signal = Some(on);
        }
        if let Some(f) = caps_mode(&line) {
            format = f;
        }
        if line.contains("Got EOS") {
            ended = true;
        }
    }
    if out_done && err_done {
        // The frame asked for came through: a real picture.
        ended |= matches!(tokio::time::timeout(Duration::from_secs(1), child.wait()).await, Ok(Ok(s)) if s.success());
    }
    let on = signal.unwrap_or(false) || (ended && signal != Some(false));
    Some((on, if on { format } else { String::new() }))
}

/// The card's `signal` property, as `gst-launch -v` prints its changes:
/// "/GstPipeline:pipeline0/GstDecklinkVideoSrc:decklinkvideosrc0: signal = true".
fn signal_property(line: &str) -> Option<bool> {
    let (who, what) = line.trim().rsplit_once(": ")?;
    if !who.contains("ecklinkVideoSrc") && !who.contains("decklinkvideosrc") {
        return None;
    }
    match what.trim() {
        "signal = true" => Some(true),
        "signal = false" => Some(false),
        _ => None,
    }
}

/// The format in the caps GStreamer prints (`-v`): "video/x-raw, …, width=(int)1920, height=(int)1080, interlace-mode=(string)interleaved, framerate=(fraction)25/1".
fn caps_mode(text: &str) -> Option<String> {
    // The card's own pad first (the caps after it are scaled for the preview).
    let caps = |l: &&str| l.contains("caps = video/x-raw") && l.contains("height=(int)");
    let line = text.lines().filter(caps).find(|l| l.contains("decklinkvideosrc")).or_else(|| text.lines().find(caps))?;
    let field = |k: &str| line.split(&format!("{k}=")).nth(1).map(|r| r.split([',', ';']).next().unwrap_or("").trim().to_string());
    let num = |v: Option<String>| v.and_then(|v| v.rsplit(')').next().map(str::to_string));
    let height: u32 = num(field("height"))?.parse().ok()?;
    let width: u32 = num(field("width")).and_then(|w| w.parse().ok()).unwrap_or(0);
    let interlaced = field("interlace-mode").is_some_and(|v| v.contains("interleaved") || v.contains("mixed") || v.contains("fields"));
    let rate = num(field("framerate"))?;
    let (n, d) = rate.split_once('/')?;
    let m = mode_of(width, height, interlaced, n.parse().ok()?, d.parse().ok()?);
    (!m.is_empty()).then_some(m)
}

/// Blackmagic's devices on the PCI bus (vendor 0xbdbd), seen with or without the driver.
async fn pci_devices() -> Vec<String> {
    let run = tokio::process::Command::new("lspci").args(["-mm", "-d", "bdbd:"]).output();
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(5), run).await else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            // 01:00.0 "Multimedia video controller" "Blackmagic Design" "DeckLink Duo 2" …
            let quoted: Vec<&str> = l.split('"').skip(1).step_by(2).collect();
            quoted.get(2).map(|d| d.to_string()).unwrap_or_else(|| l.to_string())
        })
        .collect()
}

/// Blackmagic's driver package version (Desktop Video), when installed from its .deb.
async fn driver_version() -> String {
    let run = tokio::process::Command::new("dpkg-query").args(["-W", "-f=${Version}", "desktopvideo"]).output();
    match tokio::time::timeout(Duration::from_secs(5), run).await {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// The last report on the cards and their inputs, and when it was made: Système › Matériel shows
/// it at once; a newer one is made in the background (probing the free inputs takes seconds).
static SNAPSHOT: std::sync::Mutex<Option<(std::time::Instant, Value)>> = std::sync::Mutex::new(None);
static REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Système › Matériel: the cards adopted, each with its inputs: signal, format, the recorder
/// using it. `search`: look for new cards (else, only once after the server starts).
pub async fn hardware(m: &std::sync::Arc<super::Manager>, search: bool) -> Value {
    let last = SNAPSHOT.lock().unwrap().clone();
    let v = match last {
        // The first time: the cards at once, their free inputs being checked.
        None if !search => {
            let v = report(m, false, false).await;
            if !REFRESHING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let m = m.clone();
                tokio::spawn(async move {
                    refresh(&m, false).await;
                    REFRESHING.store(false, std::sync::atomic::Ordering::SeqCst);
                });
            }
            v
        }
        Some((at, v)) if !search => {
            if at.elapsed() > Duration::from_secs(10) && !REFRESHING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let m = m.clone();
                tokio::spawn(async move {
                    refresh(&m, false).await;
                    REFRESHING.store(false, std::sync::atomic::Ordering::SeqCst);
                });
            }
            v
        }
        _ => refresh(m, search).await,
    };
    live(with_names(v, m.data_dir()), m)
}

/// The signal of an input a recorder uses: the recorder knows it at once (the card tells it when
/// the signal goes and comes back). `None`: no recorder running on it.
fn running_state(m: &super::Manager, number: u32) -> Option<(&'static str, String)> {
    let r = m.recs().into_iter().find(|r| r.config().protocol == super::Protocol::Decklink && r.config().device == number)?;
    let s = r.status.lock().unwrap().clone();
    match s.state {
        super::State::Recording => Some(("signal", s.media.as_ref().map(|md| mode_of(md.width, md.height, md.interlaced, md.fps_num, md.fps_den)).unwrap_or_default())),
        super::State::Nosignal => Some(("nosignal", String::new())),
        super::State::Starting => Some(("unknown", String::new())),
        _ => None,
    }
}

/// The inputs recorders use, as they are now (the report may be a few seconds old).
fn live(mut v: Value, m: &super::Manager) -> Value {
    if let Some(cards) = v.get_mut("cards").and_then(|c| c.as_array_mut()) {
        for port in cards.iter_mut().filter_map(|c| c.get_mut("inputs").and_then(|i| i.as_array_mut())).flatten() {
            let Some(n) = port["number"].as_u64() else { continue };
            if port["state"] == "absent" {
                continue;
            }
            if let Some((state, format)) = running_state(m, n as u32) {
                port["state"] = json!(state);
                port["format"] = json!(format);
                if let Some(r) = port.get_mut("recorder").filter(|r| r.is_object()) {
                    r["running"] = json!(true);
                }
            }
        }
    }
    v
}

/// The names given to the cards, at the time of asking (a new name shows at once).
fn with_names(mut v: Value, data_dir: &Path) -> Value {
    let names = names(data_dir);
    if let Some(cards) = v.get_mut("cards").and_then(|c| c.as_array_mut()) {
        for c in cards {
            let name = c["key"].as_str().and_then(|k| names.get(k)).cloned().unwrap_or_default();
            c["name"] = json!(name);
        }
    }
    v
}

async fn refresh(m: &super::Manager, search: bool) -> Value {
    let _one = REFRESH.lock().await;
    // Made while this one waited.
    if !search {
        if let Some((at, v)) = SNAPSHOT.lock().unwrap().as_ref() {
            if at.elapsed() < Duration::from_secs(3) {
                return v.clone();
            }
        }
    }
    let v = report(m, search, true).await;
    *SNAPSHOT.lock().unwrap() = Some((std::time::Instant::now(), v.clone()));
    v
}

/// `probe`: probe the free inputs (else they are "checking").
async fn report(m: &super::Manager, search: bool, probe_them: bool) -> Value {
    let gst = tokio::process::Command::new("gst-inspect-1.0").arg("decklinkvideosrc").stdin(std::process::Stdio::null()).output();
    let module = matches!(tokio::time::timeout(Duration::from_secs(10), gst).await, Ok(Ok(o)) if o.status.success());
    let driver = driver_installed();
    let (inputs, here) = inputs(m.data_dir(), search).await;
    // The recorders on each input: a running one tells its signal and format itself.
    let recs: Vec<_> = m.recs().into_iter().filter(|r| r.config().protocol == super::Protocol::Decklink).collect();
    let running_of = |n: u32| {
        recs.iter().find(|r| r.config().device == n).map(|r| r.status.lock().unwrap().clone()).filter(|s| matches!(s.state, super::State::Recording | super::State::Nosignal | super::State::Starting))
    };
    // The free inputs that are there are probed all at once (a few seconds without signal).
    let mut probes = tokio::task::JoinSet::new();
    if module && driver && probe_them {
        for input in inputs.iter().filter(|i| here.contains(&i.number) && running_of(i.number).is_none()) {
            let n = input.number;
            probes.spawn(async move { (n, probe(n).await) });
        }
    }
    let mut probed = std::collections::HashMap::new();
    while let Some(Ok((n, p))) = probes.join_next().await {
        probed.insert(n, p);
    }
    let keys = card_keys(&inputs);
    let mut ordered: Vec<&Input> = inputs.iter().collect();
    ordered.sort_by(|a, b| connector_order(a, b));
    let mut cards: Vec<(String, String, Vec<Value>)> = Vec::new();
    for input in ordered {
        let model = model_of(&input.name);
        let key = keys[&input.number].clone();
        let user = recs.iter().find(|r| r.config().device == input.number);
        let running = running_of(input.number);
        let (state, format) = match &running {
            Some(s) if s.state == super::State::Recording => ("signal", s.media.as_ref().map(|md| mode_of(md.width, md.height, md.interlaced, md.fps_num, md.fps_den)).unwrap_or_default()),
            Some(s) if s.state == super::State::Nosignal => ("nosignal", String::new()),
            Some(_) => ("unknown", String::new()),
            None if !here.contains(&input.number) => ("absent", String::new()),
            None if !probe_them => ("checking", String::new()),
            None => match probed.get(&input.number).cloned().flatten() {
                Some((true, f)) => ("signal", f),
                Some((false, _)) => ("nosignal", String::new()),
                None => ("busy", String::new()),
            },
        };
        let port = json!({
            "number": input.number,
            "name": input.name,
            "persistentId": input.persistent_id,
            "state": state,
            "format": format,
            "recorder": user.map(|r| json!({ "id": r.id, "name": r.config().name, "running": running.is_some(), "connection": r.config().connection })),
        });
        match cards.iter_mut().find(|c| c.0 == key) {
            Some(c) => c.2.push(port),
            None => cards.push((key, model, vec![port])),
        }
    }
    let cards: Vec<Value> = cards
        .into_iter()
        .map(|(key, model, ports)| {
            // Already in the order of the connectors.
            let serial = key.strip_prefix(&model).map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| format!("{s}x")).unwrap_or_default();
            let present = ports.iter().any(|p| p["state"] != "absent");
            json!({ "key": key, "name": "", "model": model, "serial": serial, "connector": connector_of(&model), "present": present, "inputs": ports })
        })
        .collect();
    json!({ "gstreamer": module, "driver": driver, "driverVersion": if driver { driver_version().await } else { String::new() }, "cards": cards, "pci": pci_devices().await, "at": super::now_ms() })
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
        assert_eq!(d.iter().map(|i| (i.number, i.name.as_str(), i.persistent_id)).collect::<Vec<_>>(), vec![(1, "DeckLink Duo (2)", Some(123)), (0, "DeckLink Duo (1)", Some(122))]);
        assert_eq!(model_of("DeckLink Duo (2)"), "DeckLink Duo");
        // A Quad 2: its inputs numbered …cb00 to …cb07 by the driver, in the order 1, 5, 2, 6…
        let quad: Vec<Input> = [1, 5, 2, 6, 3, 7, 4, 8]
            .iter()
            .enumerate()
            .map(|(k, n)| Input { number: k as u32, name: format!("DeckLink Quad 2 ({n})"), serial: format!("5848cb0{}", n - 1), persistent_id: None })
            .collect();
        let keys = card_keys(&quad);
        assert!(keys.values().all(|k| k == "DeckLink Quad 2 5848cb0"), "{keys:?}");
        let mut order: Vec<&Input> = quad.iter().collect();
        order.sort_by(|a, b| connector_order(a, b));
        assert_eq!(order.iter().map(|i| i.number).collect::<Vec<_>>(), vec![0, 2, 4, 6, 1, 3, 5, 7]);
        // Numbers that share nothing: still one card of that model.
        let odd: Vec<Input> = (0..4).map(|k| Input { number: k, name: format!("DeckLink Duo ({})", k + 1), serial: format!("{}ab", k * 7), persistent_id: None }).collect();
        assert!(card_keys(&odd).values().all(|k| k == "DeckLink Duo"));
        // Adopted: an input not listed this time stays; one that moved takes its new number.
        let adopted = adopt(&quad, &quad[..3]);
        assert_eq!(adopted.len(), 8);
        let moved = Input { number: 9, ..quad[7].clone() };
        let again = adopt(&adopted, &[moved.clone()]);
        assert_eq!(again.len(), 8);
        assert!(again.contains(&moved) && !again.iter().any(|i| i.number == 7));
        // Another card on the number of an absent one: the absent one goes.
        let other = Input { number: 0, name: "DeckLink Mini Recorder".into(), serial: String::new(), persistent_id: None };
        let mixed = adopt(&again, &[other.clone()]);
        assert!(mixed.contains(&other) && mixed.iter().filter(|i| i.number == 0).count() == 1);
        assert_eq!(signal_property("/GstPipeline:pipeline0/GstDecklinkVideoSrc:decklinkvideosrc0: signal = true"), Some(true));
        assert_eq!(signal_property("/GstPipeline:pipeline0/GstDecklinkVideoSrc:decklinkvideosrc0: signal = false"), Some(false));
        assert_eq!(signal_property("/GstPipeline:pipeline0/GstFakeSink:fakesink0: signal = true"), None);
        assert_eq!(model_of("Intensity Pro 4K"), "Intensity Pro 4K");
        assert_eq!(connector_of("Intensity Pro 4K"), "hdmi");
        assert_eq!(connector_of("DeckLink Quad 2"), "sdi");
        let caps = "/GstPipeline:pipeline0/GstFakeSink:fakesink0.GstPad:sink: caps = video/x-raw, format=(string)UYVY, width=(int)1920, height=(int)1080, interlace-mode=(string)interleaved, pixel-aspect-ratio=(fraction)1/1, framerate=(fraction)25/1";
        assert_eq!(caps_mode(caps).as_deref(), Some("1080i50"));
        assert_eq!(caps_mode(&caps.replace("interleaved", "progressive").replace("25/1", "30000/1001")).as_deref(), Some("1080p2997"));
        assert_eq!(mode_of(720, 576, true, 25, 1), "pal");
        // Every kind of signal, not only 1080i50.
        for (w, h, i, n, d, want) in [
            (1280, 720, false, 60000, 1001, "720p5994"),
            (1920, 1080, true, 30000, 1001, "1080i5994"),
            (1920, 1080, false, 24000, 1001, "1080p2398"),
            (1920, 1080, false, 50, 1, "1080p50"),
            (3840, 2160, false, 25, 1, "2160p25"),
            (4096, 2160, false, 24, 1, "4kdcip24"),
            (2048, 1080, false, 25, 1, "2kdcip25"),
            (7680, 4320, false, 50, 1, "8kp50"),
            (720, 486, true, 30000, 1001, "ntsc"),
        ] {
            let m = mode_of(w, h, i, n, d);
            assert_eq!(m, want);
            assert!(MODES.contains(&m.as_str()), "{m}");
        }
    }
}
