//! Blackmagic DeckLink capture cards (SDI, HDMI…), through Blackmagic's SDK (see `sdk.rs`), as
//! the CG server does: the devices Desktop Video lists, grouped into cards; the signal and format
//! of each input; its capture.
//!
//! A card gives raw pictures (UYVY) and sound (32-bit, 48 kHz): the server writes them into two
//! named pipes that FFmpeg encodes (H.264 at the recorder's bitrate, AAC by stereo pair) into an
//! MPEG-TS, written in a third named pipe the recorder's receiver reads as any other stream.
//! Everything after (segments, preview, clips) is the same as for a network source.
//!
//! Needs Blackmagic's Desktop Video (the card's driver and its library), loaded at run time.

use super::sdk::{self, Capture, DeviceInfo, Packet};
use super::RecorderConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The card's inputs.
pub const CONNECTIONS: [&str; 7] = ["auto", "sdi", "hdmi", "optical-sdi", "component", "composite", "svideo"];
/// Every video format the cards know ("auto": detected by the card, when it can): SD, HD 720
/// and 1080, 2K, UHD and 4K DCI, 8K, at every frame rate.
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

/// The SDK's BMDVideoConnection of a video input (0: as the card is set).
fn video_connection(name: &str) -> i64 {
    match name {
        "sdi" => 1,
        "hdmi" => 2,
        "optical-sdi" => 4,
        "component" => 8,
        "composite" => 16,
        "svideo" => 32,
        _ => 0,
    }
}

/// The SDK's BMDAudioConnection of a sound input (0: as the card is set).
fn audio_connection(name: &str) -> i64 {
    match name {
        "embedded" => 1,
        "aes" => 2,
        "analog" => 4,
        _ => 0,
    }
}

/// A named pipe of a recorder's capture (`<data>/capture/<id>.<ext>`, made anew).
pub fn fifo(data_dir: &Path, id: &str, ext: &str) -> std::io::Result<PathBuf> {
    let dir = data_dir.join("capture");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.{ext}"));
    let _ = std::fs::remove_file(&path);
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: a valid C string; mkfifo only creates the file.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(path)
}

/// Removes a recorder's capture pipes.
pub fn remove_fifos(data_dir: &Path, id: &str) {
    for ext in ["ts", "uyvy", "pcm"] {
        let _ = std::fs::remove_file(data_dir.join("capture").join(format!("{id}.{ext}")));
    }
}

/// Blackmagic's Desktop Video: its library loads (the SDK's entry point).
pub fn driver_installed() -> bool {
    sdk::runtime()
}

/// What the server can do with DeckLink cards: the driver, the inputs adopted, each with what it
/// receives now (as Système › Matériel last saw it) and the recorder using it.
pub async fn status(m: &Arc<super::Manager>) -> Value {
    let data_dir = m.data_dir();
    let driver = tokio::task::spawn_blocking(driver_installed).await.unwrap_or(false);
    let nodes = std::fs::read_dir("/dev/blackmagic").map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("io")).count()).unwrap_or(0);
    let mut devices = list(data_dir).await;
    let hw = hardware(m, false).await;
    let ports: Vec<&Value> = hw["cards"].as_array().into_iter().flatten().flat_map(|c| c["inputs"].as_array().into_iter().flatten()).collect();
    for d in &mut devices {
        if let Some(p) = ports.iter().find(|p| p["number"] == d["number"]) {
            d["state"] = p["state"].clone();
            d["format"] = p["format"].clone();
            d["recorder"] = p["recorder"].get("id").cloned().unwrap_or(Value::Null);
        }
    }
    json!({ "sdk": cfg!(decklink_shim) || std::env::var_os("STRATA_FAKE_DECKLINK").is_some(), "driver": driver, "nodes": nodes, "devices": devices })
}

/// One input of a card, as Desktop Video lists it (a device of the SDK).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    /// The device's number in Desktop Video's order (the recorder's `device`).
    pub number: u32,
    /// "DeckLink Quad 2 (5)".
    pub name: String,
    /// What the inputs of a card share (its device group, in hexadecimal).
    #[serde(default)]
    pub serial: String,
    #[serde(default)]
    pub persistent_id: Option<i64>,
    /// "DeckLink Quad 2".
    #[serde(default)]
    pub model: String,
    /// Its rank on the card (from 0).
    #[serde(default)]
    pub sub: Option<i64>,
    /// The card detects the format of its input.
    #[serde(default)]
    pub detection: bool,
}

impl Input {
    fn of(d: &DeviceInfo) -> Input {
        Input {
            number: d.index,
            name: d.display_name.clone(),
            serial: d.group_id.or(d.persistent_id).map(|g| format!("{g:x}")).unwrap_or_default(),
            persistent_id: d.persistent_id,
            model: if d.model_name.is_empty() { model_of(&d.display_name) } else { d.model_name.clone() },
            sub: d.sub_device_index,
            detection: d.detection,
        }
    }

    fn model(&self) -> String {
        if self.model.is_empty() {
            model_of(&self.name)
        } else {
            self.model.clone()
        }
    }
}

/// The cards adopted: every input seen once is kept (`hardware-cards.json`), shown even when the
/// driver does not list it for a moment, until it is forgotten (Système › Matériel).
fn adopted_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hardware-cards.json")
}

fn adopted(data_dir: &Path) -> Vec<Input> {
    crate::paths::read_json(&adopted_path(data_dir)).unwrap_or_default()
}

/// Searched once after the server starts: the inputs found then.
static FOUND: tokio::sync::Mutex<Option<Vec<Input>>> = tokio::sync::Mutex::const_new(None);

/// Adds what a search found to the adopted inputs: a known input (same name, same card) takes
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

/// The adopted inputs and the numbers of those the driver lists. New cards are searched once
/// after the server starts, then only when asked (`search`): not all the time.
async fn inputs(data_dir: &Path, search: bool) -> (Vec<Input>, Vec<u32>) {
    let mut found = FOUND.lock().await;
    if search || found.is_none() {
        let list = tokio::task::spawn_blocking(search_devices).await.unwrap_or_default();
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

const FULL_DUPLEX: [&[u8; 4]; 2] = [b"1dfd", b"2dfd"];
const HALF_DUPLEX: [&[u8; 4]; 3] = [b"4dhd", b"2dhd", b"1dhd"];

/// The profile that makes each connector an input of its own (half duplex), for a device in a
/// full-duplex profile (a connector of the pair is then an output): this server only records.
fn half_duplex(d: &DeviceInfo) -> Option<u32> {
    let full = d.profile.is_some_and(|p| FULL_DUPLEX.iter().any(|f| u32::from_be_bytes(**f) == p));
    if !full {
        return None;
    }
    HALF_DUPLEX.iter().map(|h| u32::from_be_bytes(**h)).find(|h| d.profiles.contains(h))
}

/// The search: the devices with an input Desktop Video lists. The cards in full duplex are first
/// switched to half duplex (every connector an input), as the CG server does for its inputs.
fn search_devices() -> Vec<Input> {
    let Ok(mut devices) = sdk::devices() else { return Vec::new() };
    let mut changed = false;
    for d in &devices {
        if let Some(p) = half_duplex(d) {
            match sdk::set_profile(d.index, p) {
                Ok(()) => {
                    tracing::info!("decklink: {}: profile {} (each connector an input of its own)", d.display_name, sdk::fourcc(p));
                    changed = true;
                }
                Err(e) => tracing::warn!("decklink: {}: cannot switch to profile {}: {e}", d.display_name, sdk::fourcc(p)),
            }
        }
    }
    if changed {
        std::thread::sleep(Duration::from_millis(500));
        devices = sdk::devices().unwrap_or_default();
    }
    devices.iter().filter(|d| d.input).map(Input::of).collect()
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
            json!({ "number": i.number, "name": i.name, "card": names.get(&key).cloned().unwrap_or_else(|| i.model()), "input": *k, "present": here.contains(&i.number) })
        })
        .collect()
}

/// The input's connector on its card, from the driver's name: "DeckLink Quad 2 (5)" → 5 (SDI 5;
/// the driver lists them in another order: 1, 5, 2, 6… on a Quad 2), else its rank + 1.
fn sub_index(i: &Input) -> Option<u32> {
    let n = i.name.trim();
    let named = n.rfind(" (").filter(|_| n.ends_with(')')).and_then(|k| n[k + 2..n.len() - 1].parse().ok());
    named.or_else(|| i.sub.and_then(|s| u32::try_from(s + 1).ok()))
}

/// A card, for grouping its inputs and naming it: its device group (shared by its inputs), else
/// its model.
fn card_key(i: &Input) -> String {
    let model = i.model();
    if i.serial.is_empty() {
        model
    } else {
        format!("{model} {}", i.serial)
    }
}

/// Each input's card.
fn card_keys(inputs: &[Input]) -> std::collections::HashMap<u32, String> {
    inputs.iter().map(|i| (i.number, card_key(i))).collect()
}

/// The inputs in the order of the card's connectors, else the driver's.
fn connector_order(a: &Input, b: &Input) -> std::cmp::Ordering {
    (sub_index(a).unwrap_or(u32::MAX), a.number).cmp(&(sub_index(b).unwrap_or(u32::MAX), b.number))
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

/// A video format as the recorders name it ("1080i50", "720p5994", "2160p25", "4kdcip24",
/// "pal", "ntsc2398"…), from its size, scan and frame rate (frames per second).
pub fn mode_of(width: u32, height: u32, interlaced: bool, num: u32, den: u32) -> String {
    if den == 0 || num == 0 {
        return String::new();
    }
    let fps = num as f64 / den as f64;
    // Interlaced formats count fields (1080i50: 25 frames, 50 fields).
    let rate = if interlaced { fps * 2.0 } else { fps };
    let r = if (rate - rate.round()).abs() < 0.01 { format!("{}", rate.round() as u32) } else { format!("{}", (rate * 100.0).round() as u32) };
    match (width, height) {
        // SD: PAL (576 lines), NTSC (486 or 480).
        (_, 576) => if interlaced { "pal" } else { "pal-p" }.to_string(),
        (_, 486 | 480) => match (interlaced, r.as_str()) {
            (true, "2398") | (true, "4795") => "ntsc2398".to_string(),
            (true, _) => "ntsc".to_string(),
            (false, _) => "ntsc-p".to_string(),
        },
        (2048, 1556) => format!("1556p{r}"),
        (2048, 1080) => format!("2kdcip{r}"),
        (4096, 2160) => format!("4kdcip{r}"),
        (8192, 4320) => format!("8kdcip{r}"),
        (_, 4320) => format!("8kp{r}"),
        _ => format!("{height}{}{r}", if interlaced { 'i' } else { 'p' }),
    }
}

/// The card's format of a recorder's format name (the widescreen SD ones are the same picture).
fn find_mode<'a>(modes: &'a [sdk::Mode], name: &str) -> Option<&'a sdk::Mode> {
    let name = name.trim_end_matches("-widescreen");
    modes.iter().find(|m| m.format() == name)
}

/// The format an input opens in before the card detects the signal's: 1080i50, else the first.
fn start_mode(modes: &[sdk::Mode]) -> Option<&sdk::Mode> {
    find_mode(modes, "1080i50").or_else(|| modes.first())
}

/// The formats tried in turn on a card that does not detect its input's.
const CANDIDATES: [&str; 11] = ["1080i50", "1080p25", "720p50", "1080i5994", "1080p2997", "720p5994", "1080p50", "2160p25", "2160p50", "pal", "ntsc"];

/// One capture at a time on an input (the probes of Système › Matériel, the previews, the
/// opening of a recording).
fn input_lock(number: u32) -> Arc<std::sync::Mutex<()>> {
    static LOCKS: std::sync::Mutex<Vec<Arc<std::sync::Mutex<()>>>> = std::sync::Mutex::new(Vec::new());
    let mut l = LOCKS.lock().unwrap();
    while l.len() <= number as usize {
        l.push(Default::default());
    }
    l[number as usize].clone()
}

/// Watches an open capture until the card has a steady picture (`frames` with signal since its
/// format last changed), or `limit`: the format then, `None` without signal. The frame is left
/// in `p`.
fn settle(cap: &Capture, limit: Duration, frames: u32, p: &mut Packet) -> Option<sdk::Mode> {
    let start = Instant::now();
    let mut changes = cap.mode().map(|m| m.1).unwrap_or(0);
    let mut good = 0;
    while start.elapsed() < limit {
        if cap.next(Duration::from_millis(100), p) {
            good = if p.no_signal || p.video.is_empty() { 0 } else { good + 1 };
        }
        let Some((mode, c)) = cap.mode() else { continue };
        if c != changes {
            changes = c;
            good = 0;
        }
        if good >= frames && p.width == mode.width && p.height == mode.height {
            return Some(mode);
        }
    }
    None
}

/// Why an input cannot be opened: busy (another program, or another capture of this server).
fn busy(e: &str) -> bool {
    e.contains("in use")
}

/// What a free input receives now: (signal, format). The input is opened a moment, the card
/// detecting the format of the signal. `None`: busy (another program).
fn probe_now(number: u32, detection: bool) -> Option<(bool, String)> {
    let lock = input_lock(number);
    let _one = lock.lock().unwrap();
    let Ok(modes) = sdk::input_modes(number) else { return Some((false, String::new())) };
    let tries: Vec<&sdk::Mode> = if detection { start_mode(&modes).into_iter().collect() } else { CANDIDATES.iter().filter_map(|c| find_mode(&modes, c)).collect() };
    let limit = if detection { Duration::from_millis(1500) } else { Duration::from_millis(400) };
    let mut p = Packet::default();
    for m in tries {
        let cap = match Capture::open(&sdk::Open { index: number, mode: m.id, detect: detection, max_frames: 2, ..Default::default() }) {
            Ok(c) => c,
            Err(e) if busy(&e) => return None,
            Err(_) => return Some((false, String::new())),
        };
        if let Some(mode) = settle(&cap, limit, 2, &mut p) {
            return Some((true, mode.format()));
        }
    }
    Some((false, String::new()))
}

async fn probe(number: u32, detection: bool) -> Option<(bool, String)> {
    tokio::task::spawn_blocking(move || probe_now(number, detection)).await.unwrap_or(Some((false, String::new())))
}

/// A still of what an input receives (JPEG, 640 wide) and its format, for the recorder's form.
/// `None`: no picture within a few seconds (no signal, or the input is busy).
pub async fn preview(ffmpeg: &str, number: u32, connection: &str, mode: &str) -> Option<(Vec<u8>, String)> {
    let (connection, mode) = (connection.to_string(), mode.to_string());
    let shot = tokio::task::spawn_blocking(move || {
        let lock = input_lock(number);
        let _one = lock.lock().unwrap();
        let modes = sdk::input_modes(number).ok()?;
        let auto = mode == "auto";
        let detection = sdk::devices().ok().and_then(|d| d.into_iter().find(|d| d.index == number)).is_some_and(|d| d.detection);
        let first = if auto { start_mode(&modes)? } else { find_mode(&modes, &mode)? };
        let cap = Capture::open(&sdk::Open { index: number, mode: first.id, detect: auto && detection, connection: video_connection(&connection), max_frames: 2, ..Default::default() }).ok()?;
        let mut p = Packet::default();
        let m = settle(&cap, Duration::from_secs(3), 3, &mut p)?;
        Some((p.video, m))
    })
    .await
    .ok()
    .flatten()?;
    let (frame, m) = shot;
    let deinterlace = if m.interlaced() { "yadif," } else { "" };
    let args = [
        "-hide_banner", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "uyvy422", "-video_size", &format!("{}x{}", m.width, m.height), "-i", "pipe:0",
        "-frames:v", "1", "-vf", &format!("{deinterlace}scale=640:-2"), "-q:v", "4", "-f", "image2", "-c:v", "mjpeg", "pipe:1",
    ];
    let mut child = tokio::process::Command::new(ffmpeg).args(args).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(&frame).await;
    });
    let out = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await.ok()?.ok()?;
    (out.stdout.len() > 100).then(|| (out.stdout, m.format()))
}

/// An input opened for a recording, in the format it receives.
pub struct Recording {
    pub cap: Capture,
    pub mode: sdk::Mode,
    /// The format changes seen so far (another one ends the run: the encoder is made for one).
    changes: u64,
    pub signal: bool,
}

/// Opens a recorder's input: in its format, or in the one the card detects ("auto", waiting a
/// moment for the signal; without, the capture starts in 1080i50 and starts again when the
/// signal comes in another format).
pub fn open_recording(c: &RecorderConfig) -> Result<Recording, String> {
    let lock = input_lock(c.device);
    let _one = lock.lock().unwrap();
    let modes = sdk::input_modes(c.device)?;
    let auto = c.video_mode == "auto";
    let first = if auto { start_mode(&modes).ok_or("the card gives no video format")? } else { find_mode(&modes, &c.video_mode).ok_or("the card cannot capture this video format")? };
    let cap = Capture::open(&sdk::Open {
        index: c.device,
        mode: first.id,
        detect: auto,
        connection: video_connection(&c.connection),
        audio_connection: audio_connection(&c.audio_input),
        channels: c.audio_channels,
        max_frames: 50,
    })?;
    let mut p = Packet::default();
    let settled = if auto { settle(&cap, Duration::from_millis(2500), 3, &mut p) } else { None };
    let (mode, changes) = cap.mode().ok_or("the card gives no video format")?;
    Ok(Recording { signal: settled.is_some() || cap.signal() == Some(true), cap, mode, changes })
}

/// FFmpeg's command line (without the program) encoding a capture: the pictures (`video`, UYVY)
/// and the sound (`audio`, 32-bit at 48 kHz) into an MPEG-TS written in `out`.
pub fn encoder_args(c: &RecorderConfig, m: &sdk::Mode, video: &Path, audio: &Path, out: &Path) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let kbps = (c.bitrate * 1000.0).round() as u32;
    let mut a = s(&["-hide_banner", "-nostdin", "-nostats", "-loglevel", "warning"]);
    a.extend(s(&["-thread_queue_size", "256", "-f", "rawvideo", "-pix_fmt", "uyvy422", "-video_size", &format!("{}x{}", m.width, m.height), "-framerate", &format!("{}/{}", m.timescale, m.duration), "-i", &video.to_string_lossy()]));
    let n = c.audio_channels;
    a.extend(s(&["-thread_queue_size", "1024", "-f", "s32le", "-ar", "48000", "-ac", &n.to_string(), "-i", &audio.to_string_lossy()]));
    // Picture: 4:2:0 H.264; interlacing kept (fields coded as fields, in the card's order).
    let top = m.field != sdk::LOWER_FIELD_FIRST;
    let vf = if m.interlaced() { format!("setfield={},format=yuv420p", if top { "tff" } else { "bff" }) } else { "format=yuv420p".to_string() };
    a.extend(s(&["-map", "0:v", "-vf", &vf, "-c:v", "libx264", "-preset", "superfast", "-b:v", &format!("{kbps}k"), "-maxrate", &format!("{kbps}k"), "-bufsize", &format!("{kbps}k"), "-g", "25"]));
    if m.interlaced() {
        a.extend(s(&["-flags", "+ildct+ilme", "-top", if top { "1" } else { "0" }]));
    }
    // Sound: each stereo pair of the channels captured, a track of its own.
    let graph: Vec<String> = (0..n / 2).map(|k| format!("[1:a]pan=stereo|c0=c{}|c1=c{}[a{k}]", 2 * k, 2 * k + 1)).collect();
    a.extend(s(&["-filter_complex", &graph.join(";")]));
    for k in 0..n / 2 {
        a.extend(s(&["-map", &format!("[a{k}]")]));
    }
    a.extend(s(&["-c:a", "aac", "-b:a", "192k", "-ar", "48000"]));
    a.extend(s(&["-f", "mpegts", "-muxdelay", "0", "-y", &out.to_string_lossy()]));
    a
}

/// How a capture ended.
#[derive(Debug, PartialEq)]
pub enum End {
    Stopped,
    /// The signal's format changed (the new one): the recording starts again in it.
    FormatChanged(String),
    Failed(String),
}

/// Opens a named pipe for writing without blocking forever: the reader (FFmpeg) opens it in a
/// moment, or the capture is over.
fn open_writer(path: &Path, stop: &AtomicBool) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let start = Instant::now();
    loop {
        match std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(path) {
            Ok(f) => {
                use std::os::fd::AsRawFd;
                // Blocking writes from now on: the encoder sets the pace.
                // SAFETY: a valid descriptor; only its status flags change.
                unsafe {
                    let fd = f.as_raw_fd();
                    let flags = libc::fcntl(fd, libc::F_GETFL);
                    libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
                }
                return Some(f);
            }
            Err(_) if !stop.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(20) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
}

/// A thread writing what it receives into a named pipe (until the channel closes or the pipe
/// breaks: the encoder stopped).
fn writer(path: PathBuf, rx: std::sync::mpsc::Receiver<Vec<u8>>, stop: Arc<AtomicBool>, broken: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        use std::io::Write;
        let Some(mut f) = open_writer(&path, &stop) else {
            broken.store(true, Ordering::Relaxed);
            return;
        };
        for buf in rx {
            if f.write_all(&buf).is_err() {
                broken.store(true, Ordering::Relaxed);
                return;
            }
        }
    });
}

/// Moves the card's frames and sound into the encoder's pipes until `stop`, the encoder stops or
/// the signal's format changes. A frame the encoder cannot take in time is dropped with its
/// sound (the picture and the sound stay together); a frame without sound gets silence.
pub fn pump(r: Recording, video: PathBuf, audio: PathBuf, channels: u32, stop: Arc<AtomicBool>, on_signal: impl Fn(bool)) -> End {
    let broken = Arc::new(AtomicBool::new(false));
    let (vtx, vrx) = std::sync::mpsc::sync_channel::<Vec<u8>>(25);
    let (atx, arx) = std::sync::mpsc::sync_channel::<Vec<u8>>(250);
    writer(video, vrx, stop.clone(), broken.clone());
    writer(audio, arx, stop.clone(), broken.clone());
    let m = r.mode.clone();
    let size = m.width as usize * m.height as usize * 2;
    // A black UYVY picture, for a frame the card gave without one.
    let black: Vec<u8> = [128u8, 16].iter().copied().cycle().take(size).collect();
    let per_second = 48000.0 * m.duration as f64 / m.timescale.max(1) as f64;
    let (mut frames, mut written, mut dropped, mut idle) = (0u64, 0u64, 0u64, 0u32);
    let mut signal = r.signal;
    let mut p = Packet::default();
    let mut last_report = Instant::now();
    let mut shim_dropped = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            return End::Stopped;
        }
        if broken.load(Ordering::Relaxed) {
            return End::Failed("the encoder stopped".into());
        }
        if let Some((now, c)) = r.cap.mode() {
            if c != r.changes && (now.id != m.id || now.field != m.field) {
                return End::FormatChanged(now.format());
            }
        }
        if !r.cap.next(Duration::from_millis(500), &mut p) {
            idle += 1;
            // No frame at all for 10 s: the card is gone (frames come even without signal).
            if idle >= 20 {
                return End::Failed("the card gives no more pictures".into());
            }
            continue;
        }
        idle = 0;
        let on = !p.no_signal;
        if on != signal {
            signal = on;
            on_signal(on);
        }
        let picture = if p.video.len() == size { std::mem::take(&mut p.video) } else { black.clone() };
        let (sound, samples) = if channels == 0 {
            (Vec::new(), 0)
        } else if p.samples > 0 {
            (std::mem::take(&mut p.audio), p.samples as u64)
        } else {
            // The sound of this frame did not come: silence, as long as the frame lasts.
            let due = ((frames + 1) as f64 * per_second).round() as u64;
            let n = due.saturating_sub(written);
            (vec![0u8; n as usize * channels as usize * 4], n)
        };
        match vtx.try_send(picture) {
            Ok(()) => {
                frames += 1;
                written += samples;
                if atx.send(sound).is_err() {
                    return End::Failed("the encoder stopped".into());
                }
            }
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                // The encoder is behind: this frame and its sound go.
                dropped += 1;
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return End::Failed("the encoder stopped".into()),
        }
        if last_report.elapsed() > Duration::from_secs(60) {
            last_report = Instant::now();
            let lost = r.cap.dropped();
            if dropped > 0 || lost > shim_dropped {
                tracing::warn!("decklink {}: {} frame(s) dropped in the last minute (the server is short of processor)", r.mode.name, dropped + lost - shim_dropped);
            }
            dropped = 0;
            shim_dropped = lost;
        }
    }
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

/// The last report on the cards and their inputs, and when it was made: Système › Matériel shows
/// it at once; a newer one is made in the background (probing the free inputs takes a moment).
static SNAPSHOT: std::sync::Mutex<Option<(Instant, Value)>> = std::sync::Mutex::new(None);
static REFRESHING: AtomicBool = AtomicBool::new(false);
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn refresh_in_background(m: &Arc<super::Manager>) {
    if !REFRESHING.swap(true, Ordering::SeqCst) {
        let m = m.clone();
        tokio::spawn(async move {
            refresh(&m, false).await;
            REFRESHING.store(false, Ordering::SeqCst);
        });
    }
}

/// Système › Matériel: the cards adopted, each with its inputs: signal, format, the recorder
/// using it, and its reference input. `search`: look for new cards (else, only once after the
/// server starts).
pub async fn hardware(m: &Arc<super::Manager>, search: bool) -> Value {
    let last = SNAPSHOT.lock().unwrap().clone();
    let v = match last {
        // The first time: the cards at once, their free inputs being checked.
        None if !search => {
            let v = report(m, false, false).await;
            refresh_in_background(m);
            v
        }
        Some((at, v)) if !search => {
            if at.elapsed() > Duration::from_secs(4) {
                refresh_in_background(m);
            }
            v
        }
        _ => refresh(m, search).await,
    };
    live(with_names(v, m.data_dir()), m)
}

/// The signal of an input a recorder uses: the recorder knows it at once. `None`: no recorder
/// running on it.
fn running_state(m: &super::Manager, number: u32) -> Option<(&'static str, String)> {
    let r = m.recs().into_iter().find(|r| r.config().protocol == super::Protocol::Decklink && r.config().device == number)?;
    let s = r.status.lock().unwrap().clone();
    match s.state {
        super::State::Recording if r.no_signal.load(Ordering::Relaxed) => Some(("nosignal", String::new())),
        super::State::Recording => Some(("signal", s.media.as_ref().map(|md| mode_of(md.width, md.height, md.interlaced, md.fps_num, md.fps_den)).unwrap_or_default())),
        super::State::Nosignal => Some(("nosignal", String::new())),
        super::State::Starting => Some(("unknown", String::new())),
        _ => None,
    }
}

/// The recorder of an input, as sent.
fn recorder_of(m: &super::Manager, number: u32) -> Value {
    let r = m.recs().into_iter().find(|r| r.config().protocol == super::Protocol::Decklink && r.config().device == number);
    r.map(|r| json!({ "id": r.id, "name": r.config().name, "running": running_state(m, number).is_some(), "connection": r.config().connection })).unwrap_or(Value::Null)
}

/// The inputs' recorders and the inputs they capture, as they are now (the report may be a few
/// seconds old).
fn live(mut v: Value, m: &super::Manager) -> Value {
    if let Some(cards) = v.get_mut("cards").and_then(|c| c.as_array_mut()) {
        for port in cards.iter_mut().filter_map(|c| c.get_mut("inputs").and_then(|i| i.as_array_mut())).flatten() {
            let Some(n) = port["number"].as_u64() else { continue };
            port["recorder"] = recorder_of(m, n as u32);
            if port["state"] == "absent" {
                continue;
            }
            if let Some((state, format)) = running_state(m, n as u32) {
                port["state"] = json!(state);
                port["format"] = json!(format);
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
            if at.elapsed() < Duration::from_secs(2) {
                return v.clone();
            }
        }
    }
    let v = report(m, search, true).await;
    *SNAPSHOT.lock().unwrap() = Some((Instant::now(), v.clone()));
    v
}

/// The reference signal of a BMDDisplayMode: black burst (SD) or tri-level (HD), and its format
/// (`formats`: the card's, by BMDDisplayMode).
fn reference_of(mode: u32, formats: &std::collections::HashMap<u32, String>) -> (&'static str, String) {
    let sd = [b"pal ", b"ntsc", b"nt23", b"palp", b"ntsp"].iter().any(|c| u32::from_be_bytes(**c) == mode);
    let name = formats.get(&mode).cloned().unwrap_or_else(|| sdk::fourcc(mode).trim().to_string());
    (if sd { "blackBurst" } else { "triLevel" }, name)
}

/// A card of the report: its key, model and group, its inputs (as sent) and their numbers.
type CardRow = (String, String, String, Vec<Value>, Vec<u32>);

/// `probe_them`: probe the free inputs (else they are "checking").
async fn report(m: &super::Manager, search: bool, probe_them: bool) -> Value {
    let (inputs, here) = inputs(m.data_dir(), search).await;
    // The cards' state now (the reference input): Desktop Video answers at once.
    let (devices, formats) = tokio::task::spawn_blocking(|| {
        let devices = sdk::devices().ok();
        // The formats' names, for the reference signal.
        let formats: std::collections::HashMap<u32, String> = devices.as_ref().and_then(|d| d.iter().find(|d| d.input)).and_then(|d| sdk::input_modes(d.index).ok()).map(|ms| ms.iter().map(|m| (m.id, m.format())).collect()).unwrap_or_default();
        (devices, formats)
    })
    .await
    .unwrap_or_default();
    let driver = devices.is_some();
    let devices = devices.unwrap_or_default();
    // The recorders on each input: a running one tells its signal and format itself.
    let recs: Vec<_> = m.recs().into_iter().filter(|r| r.config().protocol == super::Protocol::Decklink).collect();
    let running_of = |n: u32| {
        recs.iter().find(|r| r.config().device == n).map(|r| r.status.lock().unwrap().clone()).filter(|s| matches!(s.state, super::State::Recording | super::State::Nosignal | super::State::Starting))
    };
    // The free inputs that are there are probed all at once (a second or so each).
    let mut probes = tokio::task::JoinSet::new();
    if driver && probe_them {
        for input in inputs.iter().filter(|i| here.contains(&i.number) && running_of(i.number).is_none()) {
            let (n, detection) = (input.number, input.detection);
            probes.spawn(async move { (n, probe(n, detection).await) });
        }
    }
    let mut probed = std::collections::HashMap::new();
    while let Some(Ok((n, p))) = probes.join_next().await {
        probed.insert(n, p);
    }
    let keys = card_keys(&inputs);
    let mut ordered: Vec<&Input> = inputs.iter().collect();
    ordered.sort_by(|a, b| connector_order(a, b));
    let mut cards: Vec<CardRow> = Vec::new();
    for input in ordered {
        let key = keys[&input.number].clone();
        let user = recs.iter().find(|r| r.config().device == input.number);
        let running = running_of(input.number);
        let (state, format) = match &running {
            Some(s) if s.state == super::State::Recording && user.is_some_and(|r| r.no_signal.load(Ordering::Relaxed)) => ("nosignal", String::new()),
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
            "recorder": Value::Null,
        });
        match cards.iter_mut().find(|c| c.0 == key) {
            Some(c) => {
                c.3.push(port);
                c.4.push(input.number);
            }
            None => cards.push((key, input.model(), input.serial.clone(), vec![port], vec![input.number])),
        }
    }
    let cards: Vec<Value> = cards
        .into_iter()
        .map(|(key, model, serial, ports, numbers)| {
            // Already in the order of the connectors.
            let present = ports.iter().any(|p| p["state"] != "absent");
            // Its reference input, as the card's devices see it now.
            let devs: Vec<&DeviceInfo> = devices.iter().filter(|d| numbers.contains(&d.index) && inputs.iter().any(|i| i.number == d.index && i.name == d.display_name)).collect();
            let locked = devs.iter().find_map(|d| d.reference_locked);
            let reference = devs.iter().any(|d| d.has_reference_input).then(|| {
                let kind = devs.iter().map(|d| d.reference_mode).find(|m| *m != 0).filter(|_| locked == Some(true)).map(|m| reference_of(m, &formats));
                json!({ "locked": locked, "kind": kind.as_ref().map(|k| k.0), "format": kind.map(|k| k.1) })
            });
            json!({ "key": key, "name": "", "model": model, "serial": serial, "connector": connector_of(&model), "present": present, "reference": reference, "inputs": ports })
        })
        .collect();
    let version = if driver { tokio::task::spawn_blocking(sdk::version).await.ok().flatten().unwrap_or_default() } else { String::new() };
    json!({ "sdk": true, "driver": driver, "driverVersion": version, "cards": cards, "pci": pci_devices().await, "at": super::now_ms() })
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

    fn hd(field: &[u8; 4]) -> sdk::Mode {
        sdk::Mode { id: u32::from_be_bytes(*b"Hi50"), width: 1920, height: 1080, field: u32::from_be_bytes(*field), duration: 1000, timescale: 25000, name: "1080i50".into() }
    }

    #[test]
    fn encoder() {
        let a = encoder_args(&cfg(), &hd(b"uppr"), Path::new("/d/capture/x.uyvy"), Path::new("/d/capture/x.pcm"), Path::new("/d/capture/x.ts"));
        let line = a.join(" ");
        assert!(line.contains("-f rawvideo -pix_fmt uyvy422 -video_size 1920x1080 -framerate 25000/1000 -i /d/capture/x.uyvy"), "{line}");
        assert!(line.contains("-f s32le -ar 48000 -ac 8 -i /d/capture/x.pcm"));
        assert!(line.contains("setfield=tff,format=yuv420p") && line.contains("-flags +ildct+ilme -top 1"));
        assert!(line.contains("-b:v 25000k"));
        // 8 channels: 4 stereo tracks, the second one made of channels 3 and 4.
        assert!(line.contains("[1:a]pan=stereo|c0=c2|c1=c3[a1]"));
        assert_eq!(a.iter().filter(|x| x.starts_with("[a")).count(), 4);
        assert!(line.ends_with("-f mpegts -muxdelay 0 -y /d/capture/x.ts"));
        let p = encoder_args(&RecorderConfig { audio_channels: 2, ..cfg() }, &hd(b"prog"), Path::new("v"), Path::new("a"), Path::new("o")).join(" ");
        assert!(!p.contains("ildct") && p.contains("-vf format=yuv420p") && p.contains("[1:a]pan=stereo|c0=c0|c1=c1[a0] -map [a0] -c:a"));
        assert!(validate(&cfg()).is_ok());
        assert!(validate(&RecorderConfig { audio_channels: 6, ..cfg() }).is_err());
        assert!(validate(&RecorderConfig { video_mode: "1080i49".into(), ..cfg() }).is_err());
    }

    #[test]
    fn cards() {
        // A Quad 2 as Desktop Video lists it: its inputs 1, 5, 2, 6… one card, in connector order.
        let devices: Vec<DeviceInfo> = [1, 5, 2, 6, 3, 7, 4, 8]
            .iter()
            .enumerate()
            .map(|(k, n)| DeviceInfo { index: k as u32, display_name: format!("DeckLink Quad 2 ({n})"), model_name: "DeckLink Quad 2".into(), group_id: Some(0x5848cb00), persistent_id: Some(0x5848cb00 + n), sub_device_index: Some(n - 1), input: true, detection: true, ..Default::default() })
            .collect();
        let quad: Vec<Input> = devices.iter().map(Input::of).collect();
        let keys = card_keys(&quad);
        assert!(keys.values().all(|k| k == "DeckLink Quad 2 5848cb00"), "{keys:?}");
        let mut order: Vec<&Input> = quad.iter().collect();
        order.sort_by(|a, b| connector_order(a, b));
        assert_eq!(order.iter().map(|i| i.number).collect::<Vec<_>>(), vec![0, 2, 4, 6, 1, 3, 5, 7]);
        // In full duplex, a pair is switched to half duplex: every connector an input.
        let full = DeviceInfo { profile: Some(u32::from_be_bytes(*b"1dfd")), profiles: vec![u32::from_be_bytes(*b"1dfd"), u32::from_be_bytes(*b"2dhd")], ..devices[0].clone() };
        assert_eq!(half_duplex(&full), Some(u32::from_be_bytes(*b"2dhd")));
        assert_eq!(half_duplex(&DeviceInfo { profile: Some(u32::from_be_bytes(*b"2dhd")), ..full.clone() }), None);
        // Adopted: an input not listed this time stays; one that moved takes its new number.
        let adopted = adopt(&quad, &quad[..3]);
        assert_eq!(adopted.len(), 8);
        let moved = Input { number: 9, ..quad[7].clone() };
        let again = adopt(&adopted, std::slice::from_ref(&moved));
        assert_eq!(again.len(), 8);
        assert!(again.contains(&moved) && !again.iter().any(|i| i.number == 7));
        // Another card on the number of an absent one: the absent one goes.
        let other = Input { number: 0, name: "DeckLink Mini Recorder".into(), serial: String::new(), persistent_id: None, model: String::new(), sub: None, detection: false };
        let mixed = adopt(&again, std::slice::from_ref(&other));
        assert!(mixed.contains(&other) && mixed.iter().filter(|i| i.number == 0).count() == 1);
        // An input adopted before (by GStreamer's listing): still read.
        let old: Vec<Input> = serde_json::from_str(r#"[{"number":0,"name":"DeckLink Duo (1)","serial":"A1","persistentId":null}]"#).unwrap();
        assert_eq!(old[0].model(), "DeckLink Duo");
        assert_eq!(model_of("Intensity Pro 4K"), "Intensity Pro 4K");
        assert_eq!(connector_of("Intensity Pro 4K"), "hdmi");
        assert_eq!(connector_of("DeckLink Quad 2"), "sdi");
    }

    #[test]
    fn formats() {
        assert_eq!(hd(b"uppr").format(), "1080i50");
        assert_eq!(hd(b"prog").format(), "1080p25");
        let modes = vec![hd(b"uppr"), sdk::Mode { id: 1, width: 720, height: 576, field: u32::from_be_bytes(*b"lowr"), duration: 1000, timescale: 25000, name: "PAL".into() }];
        assert_eq!(find_mode(&modes, "pal-widescreen").map(|m| m.id), Some(1));
        assert_eq!(start_mode(&modes).map(|m| m.width), Some(1920));
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
        let names: std::collections::HashMap<u32, String> = [(u32::from_be_bytes(*b"Hi50"), "1080i50".to_string())].into();
        assert_eq!(reference_of(u32::from_be_bytes(*b"pal "), &names).0, "blackBurst");
        assert_eq!(reference_of(u32::from_be_bytes(*b"Hi50"), &names), ("triLevel", "1080i50".to_string()));
    }
}
