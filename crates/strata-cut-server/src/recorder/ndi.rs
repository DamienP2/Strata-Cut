//! NDI sources: found on the network and received with the NDI library (`libndi.so`, loaded
//! when the server runs: NDI's runtime is not part of this program, it is installed beside it),
//! then encoded as a card's input is: the pictures (UYVY) and the sound go to an FFmpeg, whose
//! MPEG-TS the recorder reads.
//!
//! `url`: the source's NDI name ("MACHINE (Channel 1)"); `host`: an address where to look for it
//! when the network does not announce it (another subnet, no mDNS).
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use super::{decklink::End, RecorderConfig};

pub const DEFAULT_BITRATE: f64 = 25.0;
pub const BITRATES: std::ops::RangeInclusive<f64> = 5.0..=200.0;

pub fn validate(c: &RecorderConfig) -> Result<(), String> {
    if c.url.trim().is_empty() {
        return Err("choose the ndi source".into());
    }
    if !BITRATES.contains(&c.bitrate) {
        return Err("the recording bitrate is 5 to 200 mb/s".into());
    }
    if !c.host.is_empty() && !c.host.split(',').all(|h| h.trim().parse::<std::net::Ipv4Addr>().is_ok()) {
        return Err("the ndi address is an ip address".into());
    }
    Ok(())
}

// ---------------------------------------------------------------- the library

#[repr(C)]
struct SourceT {
    name: *const c_char,
    url: *const c_char,
}

#[repr(C)]
struct FindCreate {
    show_local_sources: bool,
    groups: *const c_char,
    extra_ips: *const c_char,
}

#[repr(C)]
struct RecvCreate {
    source: SourceT,
    color_format: c_int,
    bandwidth: c_int,
    allow_video_fields: bool,
    name: *const c_char,
}

#[repr(C)]
struct VideoFrame {
    xres: c_int,
    yres: c_int,
    fourcc: u32,
    frame_rate_n: c_int,
    frame_rate_d: c_int,
    picture_aspect_ratio: f32,
    frame_format_type: c_int,
    timecode: i64,
    data: *const u8,
    line_stride_in_bytes: c_int,
    metadata: *const c_char,
    timestamp: i64,
}

#[repr(C)]
struct AudioFrame {
    sample_rate: c_int,
    no_channels: c_int,
    no_samples: c_int,
    timecode: i64,
    data: *const f32,
    channel_stride_in_bytes: c_int,
    metadata: *const c_char,
    timestamp: i64,
}

const FRAME_NONE: c_int = 0;
const FRAME_VIDEO: c_int = 1;
const FRAME_AUDIO: c_int = 2;
const FRAME_ERROR: c_int = 4;
/// UYVY for a picture without transparency, BGRA with.
const COLOR_UYVY_BGRA: c_int = 1;
const BANDWIDTH_HIGHEST: c_int = 100;
const FORMAT_PROGRESSIVE: c_int = 1;
const TIMESTAMP_UNDEFINED: i64 = i64::MAX;

struct Lib {
    find_create: unsafe extern "C" fn(*const FindCreate) -> *mut c_void,
    find_wait: unsafe extern "C" fn(*mut c_void, u32) -> bool,
    find_sources: unsafe extern "C" fn(*mut c_void, *mut u32) -> *const SourceT,
    recv_create: unsafe extern "C" fn(*const RecvCreate) -> *mut c_void,
    recv_destroy: unsafe extern "C" fn(*mut c_void),
    recv_capture: unsafe extern "C" fn(*mut c_void, *mut VideoFrame, *mut AudioFrame, *mut c_void, u32) -> c_int,
    recv_free_video: unsafe extern "C" fn(*mut c_void, *const VideoFrame),
    recv_free_audio: unsafe extern "C" fn(*mut c_void, *const AudioFrame),
    version: Option<unsafe extern "C" fn() -> *const c_char>,
}

// SAFETY: plain function pointers into a library that stays loaded.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

/// Where the library may be: named by `STRATA_NDI_LIB`, in NDI's own folders, or installed in the system.
fn candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("STRATA_NDI_LIB") {
        v.push(p.into());
    }
    let names = ["libndi.so.6", "libndi.so.5", "libndi.so.4", "libndi.so"];
    for var in ["NDI_RUNTIME_DIR_V6", "NDI_RUNTIME_DIR_V5", "NDI_RUNTIME_DIR_V4"] {
        if let Some(dir) = std::env::var_os(var) {
            v.extend(names.iter().map(|n| Path::new(&dir).join(n)));
        }
    }
    // By name alone: the system's library folders.
    v.extend(names.iter().map(PathBuf::from));
    for dir in ["/usr/local/lib", "/usr/lib", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/opt/strata-cut"] {
        v.extend(names.iter().map(|n| Path::new(dir).join(n)));
    }
    v
}

fn load() -> Result<Lib, String> {
    for path in candidates() {
        let Ok(c) = CString::new(path.to_string_lossy().as_bytes()) else { continue };
        // SAFETY: dlopen of a library by its path; its symbols are looked up by name and have
        // the signatures of NDI's SDK (Processing.NDI.Lib.h, version 4 and later).
        unsafe {
            let h = libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
            if h.is_null() {
                continue;
            }
            let sym = |name: &str| -> Result<*mut c_void, String> {
                let n = CString::new(name).unwrap();
                let p = libc::dlsym(h, n.as_ptr());
                if p.is_null() { Err(format!("ndi: {} lacks {name}", path.display())) } else { Ok(p) }
            };
            let initialize: unsafe extern "C" fn() -> bool = std::mem::transmute(sym("NDIlib_initialize")?);
            let lib = Lib {
                find_create: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*const FindCreate) -> *mut c_void>(sym("NDIlib_find_create_v2")?),
                find_wait: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void, u32) -> bool>(sym("NDIlib_find_wait_for_sources")?),
                find_sources: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void, *mut u32) -> *const SourceT>(sym("NDIlib_find_get_current_sources")?),
                recv_create: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*const RecvCreate) -> *mut c_void>(sym("NDIlib_recv_create_v3")?),
                recv_destroy: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void)>(sym("NDIlib_recv_destroy")?),
                recv_capture: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void, *mut VideoFrame, *mut AudioFrame, *mut c_void, u32) -> c_int>(sym("NDIlib_recv_capture_v2")?),
                recv_free_video: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void, *const VideoFrame)>(sym("NDIlib_recv_free_video_v2")?),
                recv_free_audio: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void, *const AudioFrame)>(sym("NDIlib_recv_free_audio_v2")?),
                version: sym("NDIlib_version").ok().map(|p| std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> *const c_char>(p)),
            };
            if !initialize() {
                return Err("ndi: this processor cannot run the ndi library".into());
            }
            tracing::info!("ndi: library loaded ({})", path.display());
            return Ok(lib);
        }
    }
    Err("ndi: the ndi library is not installed".into())
}

/// The library, loaded at the first need (looked for again while it is not found).
fn lib() -> Result<Arc<Lib>, String> {
    static LIB: Mutex<Option<Arc<Lib>>> = Mutex::new(None);
    let mut l = LIB.lock().unwrap();
    if let Some(lib) = l.as_ref() {
        return Ok(lib.clone());
    }
    let lib = Arc::new(load()?);
    *l = Some(lib.clone());
    Ok(lib)
}

pub fn installed() -> bool {
    lib().is_ok()
}

/// "NDI SDK LINUX … 6.3.2.0" → the library's version.
pub fn version() -> Option<String> {
    let lib = lib().ok()?;
    let f = lib.version?;
    // SAFETY: the library returns a static, NUL-terminated string.
    let s = unsafe { CStr::from_ptr(f()) }.to_string_lossy().into_owned();
    Some(s.split_whitespace().last().unwrap_or(&s).to_string())
}

// ---------------------------------------------------------------- the sources on the network

/// A source the network announces: its name, and where it sends from.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Source {
    pub name: String,
    pub address: String,
}

struct Finder(*mut c_void);
// SAFETY: the finder is only used under the lock of `FINDERS`.
unsafe impl Send for Finder {}

/// One finder for each list of extra addresses, kept: it learns the sources as they come.
static FINDERS: Mutex<Vec<(String, Finder, Instant)>> = Mutex::new(Vec::new());

/// The sources found now. `hosts`: addresses to ask too (comma separated); `wait`: how long a
/// finder just made may take to hear the network.
pub fn sources(hosts: &str, wait: Duration) -> Result<Vec<Source>, String> {
    let lib = lib()?;
    let hosts: String = hosts.split(',').map(str::trim).filter(|h| !h.is_empty()).collect::<Vec<_>>().join(",");
    let mut finders = FINDERS.lock().unwrap();
    let i = match finders.iter().position(|f| f.0 == hosts) {
        Some(i) => i,
        None => {
            let extra = CString::new(hosts.clone()).map_err(|_| "ndi: bad address")?;
            let create = FindCreate { show_local_sources: true, groups: std::ptr::null(), extra_ips: if hosts.is_empty() { std::ptr::null() } else { extra.as_ptr() } };
            // SAFETY: the settings live for the call; the library copies them.
            let f = unsafe { (lib.find_create)(&create) };
            if f.is_null() {
                return Err("ndi: cannot look for the sources".into());
            }
            finders.push((hosts, Finder(f), Instant::now()));
            finders.len() - 1
        }
    };
    let (finder, made) = (finders[i].1 .0, finders[i].2);
    // A new finder hears the announcements over its first second or two.
    let left = wait.saturating_sub(made.elapsed());
    let until = Instant::now() + left;
    // SAFETY: a finder made above, used under the lock; the list it returns is read before
    // the next call on it.
    unsafe {
        while Instant::now() < until {
            (lib.find_wait)(finder, 200);
        }
        let mut n: u32 = 0;
        let list = (lib.find_sources)(finder, &mut n);
        let mut out = Vec::new();
        if !list.is_null() {
            for s in std::slice::from_raw_parts(list, n as usize) {
                let text = |p: *const c_char| if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() };
                out.push(Source { name: text(s.name), address: text(s.url) });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}

// ---------------------------------------------------------------- receiving

/// What a source sends: fixed for a recording (the encoder is made for it).
#[derive(Debug, Clone, PartialEq)]
pub struct Format {
    pub width: u32,
    pub height: u32,
    pub fps_n: u32,
    pub fps_d: u32,
    pub interlaced: bool,
    /// FFmpeg's name of the pixels ("uyvy422", "bgra").
    pub pix: &'static str,
    /// Bytes a pixel.
    bpp: usize,
    pub rate: u32,
    pub channels: u32,
    /// The source sends no sound: silence is written.
    pub silent: bool,
}

impl Format {
    /// "1080i50", "720p5994"…
    pub fn name(&self) -> String {
        super::decklink::mode_of(self.width, self.height, self.interlaced, self.fps_n, self.fps_d)
    }

    fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * self.bpp
    }
}

fn pixels(fourcc: u32) -> Option<(&'static str, usize)> {
    match &fourcc.to_le_bytes() {
        b"UYVY" => Some(("uyvy422", 2)),
        b"BGRA" => Some(("bgra", 4)),
        b"BGRX" => Some(("bgr0", 4)),
        b"RGBA" => Some(("rgba", 4)),
        b"RGBX" => Some(("rgb0", 4)),
        _ => None,
    }
}

/// A connection to a source. Used by one thread at a time.
pub struct Receiver {
    lib: Arc<Lib>,
    recv: *mut c_void,
}

// SAFETY: the receiver is moved to the thread that uses it, never shared.
unsafe impl Send for Receiver {}

impl Drop for Receiver {
    fn drop(&mut self) {
        // SAFETY: a receiver made by this library, not used any more.
        unsafe { (self.lib.recv_destroy)(self.recv) };
    }
}

/// What a call to the receiver gave.
enum Got {
    Nothing,
    /// The connection is lost.
    Lost,
    /// Rows packed, its picture format, its time (100 ns units; None: not given).
    Video(Vec<u8>, VideoShape, Option<i64>),
    /// Samples interleaved (32-bit float), their rate and channels.
    Audio(Vec<u8>, u32, u32),
}

#[derive(Debug, Clone, PartialEq)]
struct VideoShape {
    width: u32,
    height: u32,
    fps_n: u32,
    fps_d: u32,
    interlaced: bool,
    pix: &'static str,
    bpp: usize,
}

impl Receiver {
    /// Connects to the source named `name` (looked for at `hosts` too).
    pub fn connect(name: &str, hosts: &str) -> Result<Receiver, String> {
        let lib = lib()?;
        // Its address, when a finder knows it (needed off the local network).
        let address = sources(hosts, Duration::from_millis(if hosts.is_empty() { 0 } else { 2500 })).ok().and_then(|l| l.into_iter().find(|s| s.name == name)).map(|s| s.address).unwrap_or_default();
        let c_name = CString::new(name).map_err(|_| "ndi: bad source name")?;
        let c_addr = CString::new(address.clone()).map_err(|_| "ndi: bad address")?;
        let me = CString::new(crate::config::BRAND).unwrap();
        let create = RecvCreate {
            source: SourceT { name: c_name.as_ptr(), url: if address.is_empty() { std::ptr::null() } else { c_addr.as_ptr() } },
            color_format: COLOR_UYVY_BGRA,
            bandwidth: BANDWIDTH_HIGHEST,
            // Interlaced pictures come whole (both fields in a frame).
            allow_video_fields: false,
            name: me.as_ptr(),
        };
        // SAFETY: the settings and their strings live for the call; the library copies them.
        let recv = unsafe { (lib.recv_create)(&create) };
        if recv.is_null() {
            return Err("ndi: cannot open a receiver".into());
        }
        Ok(Receiver { lib, recv })
    }

    fn next(&self, wait: Duration) -> Got {
        // SAFETY: zeroed frames are what the library fills; each frame it gives is copied, then
        // given back to it with its own free function before the next call.
        unsafe {
            let mut v: VideoFrame = std::mem::zeroed();
            let mut a: AudioFrame = std::mem::zeroed();
            match (self.lib.recv_capture)(self.recv, &mut v, &mut a, std::ptr::null_mut(), wait.as_millis() as u32) {
                FRAME_VIDEO => {
                    let got = (|| {
                        let (pix, bpp) = pixels(v.fourcc)?;
                        let (w, h, stride) = (v.xres.max(0) as usize, v.yres.max(0) as usize, v.line_stride_in_bytes.max(0) as usize);
                        let row = w * bpp;
                        if v.data.is_null() || w == 0 || h == 0 || stride < row {
                            return None;
                        }
                        let mut out = Vec::with_capacity(row * h);
                        for y in 0..h {
                            out.extend_from_slice(std::slice::from_raw_parts(v.data.add(y * stride), row));
                        }
                        let shape = VideoShape { width: w as u32, height: h as u32, fps_n: v.frame_rate_n.max(1) as u32, fps_d: v.frame_rate_d.max(1) as u32, interlaced: v.frame_format_type != FORMAT_PROGRESSIVE, pix, bpp };
                        Some(Got::Video(out, shape, (v.timestamp != TIMESTAMP_UNDEFINED).then_some(v.timestamp)))
                    })();
                    (self.lib.recv_free_video)(self.recv, &v);
                    got.unwrap_or(Got::Nothing)
                }
                FRAME_AUDIO => {
                    let got = (|| {
                        let (ch, n, stride) = (a.no_channels.max(0) as usize, a.no_samples.max(0) as usize, a.channel_stride_in_bytes.max(0) as usize);
                        if a.data.is_null() || ch == 0 || n == 0 || stride < n * 4 {
                            return None;
                        }
                        // Planar (a channel after the other) → interleaved.
                        let planes: Vec<&[f32]> = (0..ch).map(|c| std::slice::from_raw_parts(a.data.cast::<u8>().add(c * stride).cast::<f32>(), n)).collect();
                        let mut out = Vec::with_capacity(n * ch * 4);
                        for i in 0..n {
                            for p in &planes {
                                out.extend_from_slice(&p[i].clamp(-1.0, 1.0).to_le_bytes());
                            }
                        }
                        Some(Got::Audio(out, a.sample_rate.max(1) as u32, ch as u32))
                    })();
                    (self.lib.recv_free_audio)(self.recv, &a);
                    got.unwrap_or(Got::Nothing)
                }
                FRAME_ERROR => Got::Lost,
                FRAME_NONE => Got::Nothing,
                _ => Got::Nothing,
            }
        }
    }
}

/// A source opened for a recording, in the format it sends.
pub struct Recording {
    rx: Receiver,
    pub format: Format,
    /// What came while its format was read: written first.
    first_video: Vec<u8>,
    first_audio: Vec<u8>,
}

/// Connects and waits for the source's first picture (8 s), then a moment for its sound.
pub fn open(c: &RecorderConfig) -> Result<Recording, String> {
    let rx = Receiver::connect(c.url.trim(), &c.host)?;
    let start = Instant::now();
    let mut video: Option<(Vec<u8>, VideoShape)> = None;
    let mut audio: Option<(u32, u32)> = None;
    let mut first_audio = Vec::new();
    let mut video_at = None;
    loop {
        match rx.next(Duration::from_millis(250)) {
            Got::Video(data, shape, _) => {
                // The latest picture: the first ones may come before the sound.
                if video.is_none() {
                    video_at = Some(Instant::now());
                }
                video = Some((data, shape));
            }
            Got::Audio(data, rate, channels) => {
                if audio.is_some_and(|a| a != (rate, channels)) {
                    first_audio.clear();
                }
                audio = Some((rate, channels));
                // Kept from the first picture on (what comes before has no picture to go with).
                if video.is_some() {
                    first_audio.extend_from_slice(&data);
                }
            }
            Got::Lost => return Err("ndi: the connection to the source was lost".into()),
            Got::Nothing => {}
        }
        if video.is_some() && (audio.is_some() || video_at.is_some_and(|t| t.elapsed() > Duration::from_millis(1500))) {
            break;
        }
        if video.is_none() && start.elapsed() > Duration::from_secs(8) {
            return Err("ndi: nothing received from this source".into());
        }
    }
    let (first_video, s) = video.unwrap();
    let (rate, channels) = audio.unwrap_or((48000, 2));
    let format = Format { width: s.width, height: s.height, fps_n: s.fps_n, fps_d: s.fps_d, interlaced: s.interlaced, pix: s.pix, bpp: s.bpp, rate, channels: channels.min(16), silent: audio.is_none() };
    if channels > 16 {
        first_audio.clear();
    }
    Ok(Recording { rx, format, first_video, first_audio })
}

/// FFmpeg's command line (without the program) encoding a source: the pictures (`video`) and the
/// sound (`audio`, 32-bit float) into an MPEG-TS written in `out`.
pub fn encoder_args(c: &RecorderConfig, f: &Format, video: &Path, audio: &Path, out: &Path) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let kbps = (c.bitrate * 1000.0).round() as u32;
    let mut a = s(&["-hide_banner", "-nostdin", "-nostats", "-loglevel", "warning"]);
    a.extend(s(&["-thread_queue_size", "256", "-f", "rawvideo", "-pix_fmt", f.pix, "-video_size", &format!("{}x{}", f.width, f.height), "-framerate", &format!("{}/{}", f.fps_n, f.fps_d), "-i", &video.to_string_lossy()]));
    let n = f.channels.max(1);
    a.extend(s(&["-thread_queue_size", "1024", "-f", "f32le", "-ar", &f.rate.to_string(), "-ac", &n.to_string(), "-i", &audio.to_string_lossy()]));
    // Picture: 4:2:0 H.264; interlacing kept (NDI's first field is the top one).
    let vf = if f.interlaced { "setfield=tff,format=yuv420p" } else { "format=yuv420p" };
    let gop = ((f.fps_n as f64 / f.fps_d.max(1) as f64).round() as u32).clamp(1, 120);
    a.extend(s(&["-map", "0:v", "-vf", vf, "-c:v", "libx264", "-preset", "superfast", "-b:v", &format!("{kbps}k"), "-maxrate", &format!("{kbps}k"), "-bufsize", &format!("{kbps}k"), "-g", &gop.to_string()]));
    if f.interlaced {
        a.extend(s(&["-flags", "+ildct+ilme", "-top", "1"]));
    }
    // Sound: each stereo pair a track of its own; a single channel, one track.
    if n >= 2 {
        let graph: Vec<String> = (0..n / 2).map(|k| format!("[1:a]pan=stereo|c0=c{}|c1=c{}[a{k}]", 2 * k, 2 * k + 1)).collect();
        a.extend(s(&["-filter_complex", &graph.join(";")]));
        for k in 0..n / 2 {
            a.extend(s(&["-map", &format!("[a{k}]")]));
        }
    } else {
        a.extend(s(&["-map", "1:a"]));
    }
    a.extend(s(&["-c:a", "aac", "-b:a", "192k", "-ar", "48000"]));
    a.extend(s(&["-f", "mpegts", "-muxdelay", "0", "-y", &out.to_string_lossy()]));
    a
}

/// Moves the source's pictures and sound into the encoder's pipes until `stop`, the encoder
/// stops, the source is gone or its format changes. The pictures keep the source's pace: one
/// that did not come (by the times the source gives) or that the encoder could not take in
/// time is made up by the next one, repeated; sound that does not come is silence.
pub fn pump(r: Recording, video: PathBuf, audio: PathBuf, stop: Arc<AtomicBool>) -> End {
    let broken = Arc::new(AtomicBool::new(false));
    let (vtx, vrx) = std::sync::mpsc::sync_channel::<Vec<u8>>(25);
    let (atx, arx) = std::sync::mpsc::sync_channel::<Vec<u8>>(500);
    super::decklink::writer(video, vrx, stop.clone(), broken.clone());
    super::decklink::writer(audio, arx, stop.clone(), broken.clone());
    let f = r.format.clone();
    let frame = f.fps_d as f64 / f.fps_n.max(1) as f64;
    let sample = f.channels.max(1) as usize * 4;
    // Pictures written (counting those owed), sound samples written, pictures owed to the encoder.
    let (mut frames, mut samples, mut owed) = (0u64, 0u64, 0u64);
    let mut last_time: Option<i64> = None;
    let mut last_video = Instant::now();
    let (mut repeated, mut last_report) = (0u64, Instant::now());
    let begun = Instant::now();
    let failed = || End::Failed("the encoder stopped".into());
    let mut pending = Some(Got::Video(r.first_video, VideoShape { width: f.width, height: f.height, fps_n: f.fps_n, fps_d: f.fps_d, interlaced: f.interlaced, pix: f.pix, bpp: f.bpp }, None));
    let mut first_audio = Some(r.first_audio).filter(|a| !a.is_empty());
    loop {
        if stop.load(Ordering::Relaxed) {
            return End::Stopped;
        }
        if broken.load(Ordering::Relaxed) {
            return failed();
        }
        let got = match pending.take() {
            Some(g) => g,
            None => match first_audio.take() {
                Some(a) => Got::Audio(a, f.rate, f.channels),
                None => r.rx.next(Duration::from_millis(250)),
            },
        };
        match got {
            Got::Video(picture, shape, time) => {
                last_video = Instant::now();
                if (shape.width, shape.height, shape.fps_n, shape.fps_d, shape.interlaced, shape.pix) != (f.width, f.height, f.fps_n, f.fps_d, f.interlaced, f.pix) || picture.len() != f.frame_bytes() {
                    return End::FormatChanged(super::decklink::mode_of(shape.width, shape.height, shape.interlaced, shape.fps_n, shape.fps_d));
                }
                // Pictures that did not come, by the source's times: this one stands for them.
                let mut copies = 1u64;
                if let (Some(t), Some(prev)) = (time, last_time) {
                    let gap = ((t - prev) as f64 / 1e7 / frame).round();
                    if (2.0..=50.0).contains(&gap) {
                        copies = gap as u64;
                    }
                }
                last_time = time.or(last_time);
                frames += copies;
                owed += copies - 1;
                let copy = (owed > 0).then(|| picture.clone());
                match vtx.try_send(picture) {
                    Ok(()) => {
                        if let Some(copy) = copy {
                            while owed > 0 && vtx.try_send(copy.clone()).is_ok() {
                                owed -= 1;
                                if begun.elapsed() > Duration::from_secs(5) {
                                    repeated += 1;
                                }
                            }
                        }
                    }
                    Err(std::sync::mpsc::TrySendError::Full(_)) => owed += 1,
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return failed(),
                }
                // The sound as far as the pictures: silence for what did not come (all of it
                // for a source without sound; half a second late for one with).
                let due = (frames as f64 * frame * f.rate as f64) as u64;
                let slack = if f.silent { 0 } else { f.rate as u64 / 2 };
                if samples + slack < due {
                    let n = due - samples;
                    samples += n;
                    if atx.send(vec![0u8; n as usize * sample]).is_err() {
                        return failed();
                    }
                }
            }
            Got::Audio(data, rate, channels) => {
                if f.silent {
                    // Sound after all: the recording starts again with it.
                    return End::FormatChanged(f.name());
                }
                if (rate, channels.min(16)) != (f.rate, f.channels) {
                    return End::FormatChanged(f.name());
                }
                samples += (data.len() / sample) as u64;
                if atx.send(data).is_err() {
                    return failed();
                }
            }
            Got::Lost => return End::Failed("the connection to the source was lost".into()),
            Got::Nothing => {
                // 10 s without a picture: the source is gone (the recorder looks for it again).
                if last_video.elapsed() > Duration::from_secs(10) {
                    return End::Failed("the source sends no more pictures".into());
                }
            }
        }
        if last_report.elapsed() > Duration::from_secs(60) {
            last_report = Instant::now();
            if repeated > 0 {
                tracing::warn!("ndi {}: {repeated} picture(s) repeated in the last minute (not received, or the encoder could not keep up: the server is short of processor)", f.name());
            }
            repeated = 0;
        }
    }
}

/// A still of what a source sends (JPEG, 640 wide) and its format, for the source's form.
pub async fn preview(ffmpeg: &str, name: &str, hosts: &str) -> Result<(Vec<u8>, Format), String> {
    let (name, hosts) = (name.to_string(), hosts.to_string());
    let shot = tokio::task::spawn_blocking(move || {
        let c = RecorderConfig { url: name, host: hosts, ..super::blank_config() };
        open(&c).map(|r| (r.first_video, r.format))
    })
    .await
    .map_err(|e| e.to_string())??;
    let (frame, f) = shot;
    let deinterlace = if f.interlaced { "yadif," } else { "" };
    let args = ["-hide_banner", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", f.pix, "-video_size", &format!("{}x{}", f.width, f.height), "-i", "pipe:0", "-frames:v", "1", "-vf", &format!("{deinterlace}scale=640:-2"), "-q:v", "4", "-f", "image2", "-c:v", "mjpeg", "pipe:1"];
    let mut child = tokio::process::Command::new(ffmpeg).args(args).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn().map_err(|e| format!("cannot start FFmpeg: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no input")?;
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(&frame).await;
    });
    let out = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await.map_err(|_| "ndi: no picture")?.map_err(|e| e.to_string())?;
    if out.stdout.len() <= 100 {
        return Err("ndi: no picture".into());
    }
    Ok((out.stdout, f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        // As in NDI's headers (64-bit): a mistake here reads garbage from the library.
        assert_eq!(std::mem::size_of::<SourceT>(), 16);
        assert_eq!(std::mem::size_of::<FindCreate>(), 24);
        assert_eq!(std::mem::size_of::<RecvCreate>(), 40);
        assert_eq!(std::mem::size_of::<VideoFrame>(), 72);
        assert_eq!(std::mem::offset_of!(VideoFrame, data), 40);
        assert_eq!(std::mem::offset_of!(VideoFrame, timestamp), 64);
        assert_eq!(std::mem::size_of::<AudioFrame>(), 56);
        assert_eq!(std::mem::offset_of!(AudioFrame, data), 24);
    }

    #[test]
    fn encoder() {
        let c = RecorderConfig { bitrate: 25.0, ..crate::recorder::blank_config() };
        let f = Format { width: 1920, height: 1080, fps_n: 25, fps_d: 1, interlaced: true, pix: "uyvy422", bpp: 2, rate: 48000, channels: 4, silent: false };
        assert_eq!(f.name(), "1080i50");
        let a = encoder_args(&c, &f, Path::new("/v"), Path::new("/a"), Path::new("/o")).join(" ");
        assert!(a.contains("-pix_fmt uyvy422 -video_size 1920x1080 -framerate 25/1 -i /v"), "{a}");
        assert!(a.contains("-f f32le -ar 48000 -ac 4 -i /a") && a.contains("[a1]") && a.contains("+ildct+ilme"), "{a}");
        assert_eq!(pixels(u32::from_le_bytes(*b"UYVY")), Some(("uyvy422", 2)));
        assert!(validate(&RecorderConfig { url: "PC (Cam)".into(), bitrate: 25.0, host: "10.0.0.5".into(), ..crate::recorder::blank_config() }).is_ok());
        assert!(validate(&RecorderConfig { url: "PC (Cam)".into(), bitrate: 25.0, host: "studio".into(), ..crate::recorder::blank_config() }).is_err());
    }
}
