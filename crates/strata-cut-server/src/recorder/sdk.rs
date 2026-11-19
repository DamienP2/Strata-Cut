//! Blackmagic's DeckLink SDK, through the shim of `decklink/shim.cpp` (built by build.rs): the
//! devices Desktop Video lists, their input formats, and the capture of an input (pictures in
//! UYVY, sound in 32-bit at 48 kHz), the way the CG server does it.
//!
//! `STRATA_FAKE_DECKLINK=quad2` (also `duo2`, `mini4k`, comma separated) makes up cards instead
//! of asking the driver, to work without them: test pictures and sound on some inputs, none on
//! the others, one input "in use by another program".

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// A device of a card, as Desktop Video lists it (a sub-device: one connector, in half duplex).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceInfo {
    /// In Desktop Video's order: the recorder's `device`.
    pub index: u32,
    /// "DeckLink Quad 2 (5)".
    pub display_name: String,
    /// "DeckLink Quad 2".
    pub model_name: String,
    pub persistent_id: Option<i64>,
    /// Shared by the sub-devices of a card.
    pub group_id: Option<i64>,
    pub sub_device_index: Option<i64>,
    pub has_reference_input: bool,
    pub reference_locked: Option<bool>,
    /// The reference signal's BMDDisplayMode (0: none).
    pub reference_mode: u32,
    /// The active profile and those it can switch to (BMDProfileID: '2dhd'…).
    pub profile: Option<u32>,
    pub profiles: Vec<u32>,
    pub input: bool,
    /// The card detects the format of its input.
    pub detection: bool,
}

/// A video format of an input.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mode {
    /// BMDDisplayMode ('Hi50'…).
    pub id: u32,
    pub width: u32,
    pub height: u32,
    /// BMDFieldDominance ('uppr', 'lowr', 'prog', 'psf ').
    pub field: u32,
    /// A frame lasts `duration` / `timescale` s.
    pub duration: i64,
    pub timescale: i64,
    pub name: String,
}

pub const UPPER_FIELD_FIRST: u32 = u32::from_be_bytes(*b"uppr");
pub const LOWER_FIELD_FIRST: u32 = u32::from_be_bytes(*b"lowr");

impl Mode {
    pub fn interlaced(&self) -> bool {
        self.field == UPPER_FIELD_FIRST || self.field == LOWER_FIELD_FIRST
    }

    /// The format as the recorders name it ("1080i50", "720p5994", "pal"…).
    pub fn format(&self) -> String {
        super::decklink::mode_of(self.width, self.height, self.interlaced(), self.timescale.max(0) as u32, self.duration.max(0) as u32)
    }
}

/// A frame of the card and the sound that came with it.
#[derive(Debug, Default)]
pub struct Packet {
    /// UYVY, rows packed (empty: the card gave no picture).
    pub video: Vec<u8>,
    /// Signed 32-bit, channels interleaved.
    pub audio: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub samples: u32,
    /// The card had no input signal (the picture is black).
    pub no_signal: bool,
}

pub fn fourcc(v: u32) -> String {
    String::from_utf8_lossy(&v.to_be_bytes()).into_owned()
}

#[cfg(decklink_shim)]
mod ffi {
    use std::os::raw::{c_char, c_int, c_void};

    #[repr(C)]
    pub struct RawDevice {
        pub persistent_id: i64,
        pub group_id: i64,
        pub topological_id: i64,
        pub sub_device_index: i64,
        pub sub_devices: i64,
        pub has_persistent_id: i32,
        pub has_group_id: i32,
        pub has_topological_id: i32,
        pub has_sub_device_index: i32,
        pub has_sub_devices: i32,
        pub has_reference_input: i32,
        pub reference_locked: i32,
        pub reference_mode: u32,
        pub profile: u32,
        pub profiles: [u32; 8],
        pub profile_count: i32,
        pub caps: i32,
        pub display_name: [c_char; 128],
        pub model_name: [c_char; 128],
    }

    #[repr(C)]
    pub struct RawMode {
        pub id: u32,
        pub width: i32,
        pub height: i32,
        pub field_dominance: u32,
        pub duration: i64,
        pub timescale: i64,
        pub name: [c_char; 64],
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct RawPacket {
        pub video_bytes: i64,
        pub audio_bytes: i64,
        pub width: i32,
        pub height: i32,
        pub samples: i32,
        pub no_signal: i32,
    }

    extern "C" {
        pub fn dl_runtime_present() -> c_int;
        pub fn dl_devices(out: *mut RawDevice, cap: c_int) -> c_int;
        pub fn dl_api_version(buf: *mut c_char, len: c_int) -> c_int;
        pub fn dl_set_profile(index: c_int, profile: u32, err: *mut c_char, errlen: c_int) -> c_int;
        pub fn dl_input_modes(index: c_int, out: *mut RawMode, cap: c_int, err: *mut c_char, errlen: c_int) -> c_int;
        pub fn dl_capture_open(index: c_int, mode: u32, detect: c_int, connection: i64, audio_connection: i64, channels: c_int, max_frames: c_int, err: *mut c_char, errlen: c_int) -> *mut c_void;
        pub fn dl_capture_next(h: *mut c_void, timeout_ms: c_int, video: *mut u8, video_cap: i64, audio: *mut u8, audio_cap: i64, info: *mut RawPacket) -> c_int;
        pub fn dl_capture_mode(h: *mut c_void, out: *mut RawMode, changes: *mut u64) -> c_int;
        pub fn dl_capture_signal(h: *mut c_void) -> c_int;
        pub fn dl_capture_dropped(h: *mut c_void) -> u64;
        pub fn dl_capture_close(h: *mut c_void);
    }

    pub fn text(buf: &[c_char]) -> String {
        let bytes: Vec<u8> = buf.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn mode(m: &RawMode) -> super::Mode {
        super::Mode { id: m.id, width: m.width.max(0) as u32, height: m.height.max(0) as u32, field: m.field_dominance, duration: m.duration, timescale: m.timescale, name: text(&m.name) }
    }

    pub fn zeroed<T>() -> T {
        // SAFETY: plain C structs of integers and arrays: all zeros is a valid value.
        unsafe { std::mem::zeroed() }
    }
}

/// The fake cards asked for (`STRATA_FAKE_DECKLINK`).
fn fake() -> Option<Vec<String>> {
    let v = std::env::var("STRATA_FAKE_DECKLINK").ok()?;
    let v: Vec<String> = v.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
    (!v.is_empty()).then_some(v)
}

/// Blackmagic Desktop Video is there (its library loads).
pub fn runtime() -> bool {
    if fake().is_some() {
        return true;
    }
    #[cfg(decklink_shim)]
    {
        // SAFETY: no argument; the shim loads the library and releases what it made.
        unsafe { ffi::dl_runtime_present() == 1 }
    }
    #[cfg(not(decklink_shim))]
    false
}

/// The version of Desktop Video ("12.9").
pub fn version() -> Option<String> {
    if fake().is_some() {
        return Some("14.2".into());
    }
    #[cfg(decklink_shim)]
    {
        let mut buf = [0 as std::os::raw::c_char; 64];
        // SAFETY: the buffer and its length.
        let r = unsafe { ffi::dl_api_version(buf.as_mut_ptr(), buf.len() as i32) };
        (r == 0).then(|| ffi::text(&buf)).filter(|s| !s.is_empty())
    }
    #[cfg(not(decklink_shim))]
    None
}

/// Every device Desktop Video lists, in its order. `Err`: Desktop Video is not installed.
pub fn devices() -> Result<Vec<DeviceInfo>, String> {
    if let Some(models) = fake() {
        return Ok(fake_devices(&models));
    }
    #[cfg(decklink_shim)]
    {
        let mut raw: Vec<ffi::RawDevice> = (0..64).map(|_| ffi::zeroed()).collect();
        // SAFETY: `raw` holds 64 entries.
        let n = unsafe { ffi::dl_devices(raw.as_mut_ptr(), raw.len() as i32) };
        if n < 0 {
            return Err("Blackmagic Desktop Video is not installed".into());
        }
        Ok(raw
            .iter()
            .take(n.min(64) as usize)
            .enumerate()
            .map(|(i, d)| DeviceInfo {
                index: i as u32,
                display_name: ffi::text(&d.display_name),
                model_name: ffi::text(&d.model_name),
                persistent_id: (d.has_persistent_id != 0).then_some(d.persistent_id),
                group_id: (d.has_group_id != 0).then_some(d.group_id),
                sub_device_index: (d.has_sub_device_index != 0).then_some(d.sub_device_index),
                has_reference_input: d.has_reference_input != 0,
                reference_locked: match d.reference_locked {
                    1 => Some(true),
                    0 => Some(false),
                    _ => None,
                },
                reference_mode: d.reference_mode,
                profile: (d.profile != 0).then_some(d.profile),
                profiles: d.profiles.iter().take(d.profile_count.clamp(0, 8) as usize).copied().collect(),
                input: d.caps & 2 != 0,
                detection: d.caps & 16 != 0,
            })
            .collect())
    }
    #[cfg(not(decklink_shim))]
    Err("this server was built without DeckLink support".into())
}

/// Switches a device to a profile ('2dhd'…: how its connectors are shared).
pub fn set_profile(index: u32, profile: u32) -> Result<(), String> {
    if fake().is_some() {
        return Ok(());
    }
    #[cfg(decklink_shim)]
    {
        let mut err = [0 as std::os::raw::c_char; 256];
        // SAFETY: the error buffer and its length.
        let r = unsafe { ffi::dl_set_profile(index as i32, profile, err.as_mut_ptr(), err.len() as i32) };
        if r == 0 {
            Ok(())
        } else {
            Err(ffi::text(&err))
        }
    }
    #[cfg(not(decklink_shim))]
    {
        let _ = (index, profile);
        Err("this server was built without DeckLink support".into())
    }
}

/// The video formats an input can capture.
pub fn input_modes(index: u32) -> Result<Vec<Mode>, String> {
    if fake().is_some() {
        return Ok(fake_modes());
    }
    #[cfg(decklink_shim)]
    {
        let mut raw: Vec<ffi::RawMode> = (0..256).map(|_| ffi::zeroed()).collect();
        let mut err = [0 as std::os::raw::c_char; 256];
        // SAFETY: `raw` holds 256 entries; the error buffer and its length.
        let n = unsafe { ffi::dl_input_modes(index as i32, raw.as_mut_ptr(), raw.len() as i32, err.as_mut_ptr(), err.len() as i32) };
        if n < 0 {
            return Err(ffi::text(&err));
        }
        Ok(raw.iter().take(n.min(256) as usize).map(ffi::mode).collect())
    }
    #[cfg(not(decklink_shim))]
    {
        let _ = index;
        Err("this server was built without DeckLink support".into())
    }
}

/// How a capture is opened.
#[derive(Debug, Clone, Default)]
pub struct Open {
    pub index: u32,
    /// The BMDDisplayMode to start in.
    pub mode: u32,
    /// Follow the signal's format (when the card detects it).
    pub detect: bool,
    /// BMDVideoConnection / BMDAudioConnection: 0 = as the card is set.
    pub connection: i64,
    pub audio_connection: i64,
    /// Sound channels captured (0: none).
    pub channels: u32,
    /// Frames kept when the reader falls behind (the oldest go).
    pub max_frames: u32,
}

/// An input being captured: closed when dropped.
pub struct Capture {
    inner: Inner,
}

enum Inner {
    #[cfg(decklink_shim)]
    Sdk(*mut std::os::raw::c_void),
    Fake(Arc<FakeCapture>),
}

// SAFETY: every entry point of the shim may be called from any thread (its state is behind a mutex).
unsafe impl Send for Capture {}
unsafe impl Sync for Capture {}

impl Capture {
    pub fn open(o: &Open) -> Result<Capture, String> {
        if let Some(models) = fake() {
            return FakeCapture::open(&models, o).map(|f| Capture { inner: Inner::Fake(f) });
        }
        #[cfg(decklink_shim)]
        {
            let mut err = [0 as std::os::raw::c_char; 256];
            // SAFETY: the error buffer and its length; the handle is closed by Drop.
            let h = unsafe { ffi::dl_capture_open(o.index as i32, o.mode, o.detect as i32, o.connection, o.audio_connection, o.channels as i32, o.max_frames as i32, err.as_mut_ptr(), err.len() as i32) };
            if h.is_null() {
                return Err(ffi::text(&err));
            }
            Ok(Capture { inner: Inner::Sdk(h) })
        }
        #[cfg(not(decklink_shim))]
        Err("this server was built without DeckLink support".into())
    }

    /// The oldest frame captured and its sound, waiting up to `timeout`. False: nothing came.
    pub fn next(&self, timeout: Duration, p: &mut Packet) -> bool {
        match &self.inner {
            #[cfg(decklink_shim)]
            Inner::Sdk(h) => {
                let ms = timeout.as_millis().min(60_000) as i32;
                let mut info = ffi::RawPacket::default();
                for _ in 0..2 {
                    // SAFETY: the buffers and their capacities; `info` is written by the shim.
                    let r = unsafe { ffi::dl_capture_next(*h, ms, p.video.as_mut_ptr(), p.video.len() as i64, p.audio.as_mut_ptr(), p.audio.len() as i64, &mut info) };
                    match r {
                        1 => {
                            p.video.truncate(info.video_bytes.max(0) as usize);
                            p.audio.truncate(info.audio_bytes.max(0) as usize);
                            p.width = info.width.max(0) as u32;
                            p.height = info.height.max(0) as u32;
                            p.samples = info.samples.max(0) as u32;
                            p.no_signal = info.no_signal != 0;
                            return true;
                        }
                        -1 => {
                            // Room for it, then again.
                            p.video.resize(info.video_bytes.max(0) as usize, 0);
                            p.audio.resize(info.audio_bytes.max(0) as usize, 0);
                        }
                        _ => return false,
                    }
                }
                false
            }
            Inner::Fake(f) => f.next(timeout, p),
        }
    }

    /// The format captured now (the detected one once the card saw the signal), and how many
    /// times it changed since the capture opened.
    pub fn mode(&self) -> Option<(Mode, u64)> {
        match &self.inner {
            #[cfg(decklink_shim)]
            Inner::Sdk(h) => {
                let mut raw: ffi::RawMode = ffi::zeroed();
                let mut changes = 0u64;
                // SAFETY: `raw` and `changes` are written by the shim.
                let r = unsafe { ffi::dl_capture_mode(*h, &mut raw, &mut changes) };
                (r == 0).then(|| (ffi::mode(&raw), changes))
            }
            Inner::Fake(f) => Some(f.mode()),
        }
    }

    /// The input has a picture (true), none (false), not known yet (None).
    pub fn signal(&self) -> Option<bool> {
        let s = match &self.inner {
            #[cfg(decklink_shim)]
            // SAFETY: a handle open until Drop.
            Inner::Sdk(h) => unsafe { ffi::dl_capture_signal(*h) },
            Inner::Fake(f) => f.signal(),
        };
        match s {
            1 => Some(true),
            0 => Some(false),
            _ => None,
        }
    }

    /// Frames dropped because the reader fell behind.
    pub fn dropped(&self) -> u64 {
        match &self.inner {
            #[cfg(decklink_shim)]
            // SAFETY: a handle open until Drop.
            Inner::Sdk(h) => unsafe { ffi::dl_capture_dropped(*h) },
            Inner::Fake(f) => f.dropped.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        match &self.inner {
            #[cfg(decklink_shim)]
            // SAFETY: the handle dl_capture_open gave, closed once.
            Inner::Sdk(h) => unsafe { ffi::dl_capture_close(*h) },
            Inner::Fake(f) => f.close(),
        }
    }
}

// ------------------------------------------------------------------ fake cards

const fn cc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// (id, width, height, field, duration, timescale, name): the usual formats.
const FAKE_MODES: [(u32, u32, u32, u32, i64, i64, &str); 16] = [
    (cc(b"pal "), 720, 576, LOWER_FIELD_FIRST, 1000, 25000, "PAL"),
    (cc(b"ntsc"), 720, 486, LOWER_FIELD_FIRST, 1001, 30000, "NTSC"),
    (cc(b"Hi50"), 1920, 1080, UPPER_FIELD_FIRST, 1000, 25000, "1080i50"),
    (cc(b"Hi59"), 1920, 1080, UPPER_FIELD_FIRST, 1001, 30000, "1080i59.94"),
    (cc(b"Hp25"), 1920, 1080, cc(b"prog"), 1000, 25000, "1080p25"),
    (cc(b"Hp29"), 1920, 1080, cc(b"prog"), 1001, 30000, "1080p29.97"),
    (cc(b"Hp50"), 1920, 1080, cc(b"prog"), 1000, 50000, "1080p50"),
    (cc(b"Hp59"), 1920, 1080, cc(b"prog"), 1001, 60000, "1080p59.94"),
    (cc(b"24ps"), 1920, 1080, cc(b"prog"), 1000, 24000, "1080p24"),
    (cc(b"hp50"), 1280, 720, cc(b"prog"), 1000, 50000, "720p50"),
    (cc(b"hp59"), 1280, 720, cc(b"prog"), 1001, 60000, "720p59.94"),
    (cc(b"4k25"), 3840, 2160, cc(b"prog"), 1000, 25000, "2160p25"),
    (cc(b"4k50"), 3840, 2160, cc(b"prog"), 1000, 50000, "2160p50"),
    (cc(b"4k59"), 3840, 2160, cc(b"prog"), 1001, 60000, "2160p59.94"),
    (cc(b"Hi60"), 1920, 1080, UPPER_FIELD_FIRST, 1000, 30000, "1080i60"),
    (cc(b"hp60"), 1280, 720, cc(b"prog"), 1000, 60000, "720p60"),
];

fn fake_modes() -> Vec<Mode> {
    FAKE_MODES.iter().map(|&(id, width, height, field, duration, timescale, name)| Mode { id, width, height, field, duration, timescale, name: name.into() }).collect()
}

/// The fake cards' devices: a Quad 2 lists its inputs 1, 5, 2, 6… as the driver does.
fn fake_devices(models: &[String]) -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    for (k, m) in models.iter().enumerate() {
        let group = 0x5848_cb00_i64 + ((k as i64) << 8);
        let (model, subs): (&str, Vec<u32>) = match m.as_str() {
            "quad2" => ("DeckLink Quad 2", vec![1, 5, 2, 6, 3, 7, 4, 8]),
            "duo2" => ("DeckLink Duo 2", vec![1, 3, 2, 4]),
            "mini4k" => ("DeckLink Mini Recorder 4K", vec![0]),
            _ => continue,
        };
        for s in subs {
            let index = out.len() as u32;
            out.push(DeviceInfo {
                index,
                display_name: if s == 0 { model.to_string() } else { format!("{model} ({s})") },
                model_name: model.to_string(),
                persistent_id: Some(group + s as i64),
                group_id: Some(group),
                sub_device_index: (s > 0).then_some(s as i64 - 1),
                has_reference_input: model != "DeckLink Mini Recorder 4K",
                reference_locked: (model != "DeckLink Mini Recorder 4K").then_some(k == 0),
                reference_mode: if k == 0 { cc(b"pal ") } else { 0 },
                profile: (s > 0).then_some(cc(b"2dhd")),
                profiles: if s > 0 { vec![cc(b"1dfd"), cc(b"2dhd")] } else { Vec::new() },
                input: true,
                detection: true,
            });
        }
    }
    out
}

/// What the fake inputs receive, by device: a format, nothing, or busy.
fn fake_signal(index: u32) -> Result<Option<u32>, String> {
    match index {
        0 => Ok(Some(cc(b"Hi50"))),
        2 => Ok(Some(cc(b"hp50"))),
        4 => Ok(Some(cc(b"Hp25"))),
        7 => Err("cannot enable the video input (in use by another program?)".into()),
        _ => Ok(None),
    }
}

struct FakeState {
    queue: std::collections::VecDeque<Packet>,
    mode: u32,
    changes: u64,
    signal: i32,
}

pub struct FakeCapture {
    state: Mutex<FakeState>,
    arrived: Condvar,
    stop: AtomicBool,
    dropped: AtomicU64,
}

impl FakeCapture {
    fn open(models: &[String], o: &Open) -> Result<Arc<FakeCapture>, String> {
        let devices = fake_devices(models);
        if o.index as usize >= devices.len() {
            return Err(format!("no DeckLink device #{} ({} found)", o.index + 1, devices.len()));
        }
        let incoming = fake_signal(o.index)?;
        let modes = fake_modes();
        if !modes.iter().any(|m| m.id == o.mode) {
            return Err("the device cannot capture this video format".into());
        }
        let f = Arc::new(FakeCapture {
            state: Mutex::new(FakeState { queue: Default::default(), mode: o.mode, changes: 0, signal: -1 }),
            arrived: Condvar::new(),
            stop: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
        });
        let (g, o) = (f.clone(), o.clone());
        std::thread::spawn(move || g.run(&o, incoming, &modes));
        Ok(f)
    }

    /// The card's frames at its rate: a moving test picture and a tone when the input has a
    /// signal in the format captured; black and silence (no input source) otherwise.
    fn run(&self, o: &Open, incoming: Option<u32>, modes: &[Mode]) {
        let mut started = Instant::now();
        let mut n: u64 = 0;
        let mut samples_sent: u64 = 0;
        while !self.stop.load(Ordering::Relaxed) {
            let mode_id = self.state.lock().unwrap().mode;
            let mode = modes.iter().find(|m| m.id == mode_id).cloned().unwrap_or_default();
            // The card sees another format: it switches to it after a few frames.
            if let Some(i) = incoming {
                if i != mode_id && o.detect && n >= 3 {
                    let mut s = self.state.lock().unwrap();
                    s.mode = i;
                    s.changes += 1;
                    drop(s);
                    self.arrived.notify_all();
                    // The new format's frames, from now on.
                    started = Instant::now();
                    n = 0;
                    samples_sent = 0;
                    continue;
                }
            }
            let period = Duration::from_secs_f64(mode.duration as f64 / mode.timescale.max(1) as f64);
            let due = started + period.mul_f64(n as f64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            let on = incoming == Some(mode_id);
            let (w, h) = (mode.width as usize, mode.height as usize);
            let mut video = vec![0u8; w * h * 2];
            for (i, px) in video.chunks_exact_mut(4).enumerate() {
                let x = (i * 2) % w.max(1);
                let (y, u, v) = if !on {
                    (16, 128, 128)
                } else {
                    // Eight colour bars, and a white bar that moves.
                    let bar = x * 8 / w.max(1);
                    let moving = (x + w - (n as usize * 8) % w.max(1)) % w.max(1) < 16;
                    if moving {
                        (235, 128, 128)
                    } else {
                        [(180, 128, 128), (168, 44, 136), (145, 147, 44), (133, 63, 52), (63, 193, 204), (51, 109, 212), (28, 212, 120), (16, 128, 128)][bar.min(7)]
                    }
                };
                px.copy_from_slice(&[u, y, v, y]);
            }
            // The sound of the frame: 48 kHz, as many samples as the frame lasts.
            let total = ((n + 1) as f64 * period.as_secs_f64() * 48000.0).round() as u64;
            let count = (total - samples_sent) as usize;
            let mut audio = Vec::with_capacity(count * o.channels as usize * 4);
            for k in 0..count {
                let t = (samples_sent + k as u64) as f64 / 48000.0;
                let s = if on { ((t * 1000.0 * std::f64::consts::TAU).sin() * 0.25 * i32::MAX as f64) as i32 } else { 0 };
                for _ in 0..o.channels {
                    audio.extend_from_slice(&s.to_le_bytes());
                }
            }
            samples_sent = total;
            n += 1;
            let p = Packet { video, audio, width: w as u32, height: h as u32, samples: count as u32, no_signal: !on };
            let mut s = self.state.lock().unwrap();
            s.signal = on as i32;
            s.queue.push_back(p);
            while s.queue.len() > o.max_frames.max(1) as usize {
                s.queue.pop_front();
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            drop(s);
            self.arrived.notify_all();
        }
    }

    fn next(&self, timeout: Duration, p: &mut Packet) -> bool {
        let s = self.state.lock().unwrap();
        let (mut s, _) = self.arrived.wait_timeout_while(s, timeout, |s| s.queue.is_empty()).unwrap();
        match s.queue.pop_front() {
            Some(q) => {
                *p = q;
                true
            }
            None => false,
        }
    }

    fn mode(&self) -> (Mode, u64) {
        let s = self.state.lock().unwrap();
        (fake_modes().into_iter().find(|m| m.id == s.mode).unwrap_or_default(), s.changes)
    }

    fn signal(&self) -> i32 {
        self.state.lock().unwrap().signal
    }

    fn close(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
