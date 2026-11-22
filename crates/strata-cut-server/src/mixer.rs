//! Streamer: the mixer of a source's programme. Its own clock (a picture every frame, 48 kHz
//! sound), whatever comes in: the source's last picture (held while it falters, the slate once it
//! is lost), the image over it (a fade), the overlay, and the sound of the tracks on at their
//! level (silence where a track sends nothing). What it makes never stops, so neither does the
//! encoding, nor the lives sending it.
//!
//! Its inputs are filled by the decoders (raw pictures at the programme's size, raw sound of each
//! track) and its pictures by the server (`Pics`); what it makes goes to the encoder (`Sink`).
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use crate::{
    program::{Change, Program, Shape},
    recorder::now_ms,
};

/// Sound: 48 kHz, stereo, 32-bit float.
pub const RATE: usize = 48_000;
/// Tracks of a source mixed at most.
pub const TRACKS: usize = 8;
/// What goes out never passes -1 dBFS.
const LIMIT: f32 = 0.891;
/// A track's sound kept ahead before it is played (s): its jitter absorbed.
const AHEAD: f64 = 0.1;
/// More than this ahead (s): the oldest dropped (the source's clock runs faster than ours).
const AHEAD_MAX: f64 = 0.4;
/// No picture from the source this long (ms): lost.
const LOST_MS: i64 = 1000;
/// The overlay's fade when it is put on or taken off (s).
const OVERLAY_FADE: f64 = 0.3;

/// What the decoders give the mixer.
pub struct Inputs {
    pictures: Mutex<VecDeque<Vec<u8>>>,
    /// When the last picture came (ms; 0: never).
    last_picture: AtomicI64,
    tracks: Vec<Mutex<Sound>>,
}

#[derive(Default)]
struct Sound {
    /// Interleaved stereo samples.
    buf: VecDeque<f32>,
    /// Enough came to play it (after a start or a lack).
    primed: bool,
}

impl Default for Inputs {
    fn default() -> Self {
        Inputs { pictures: Mutex::new(VecDeque::new()), last_picture: AtomicI64::new(0), tracks: (0..TRACKS).map(|_| Mutex::new(Sound::default())).collect() }
    }
}

impl Inputs {
    /// A picture of the source (YUV 4:2:0 at the programme's size).
    pub fn picture(&self, f: Vec<u8>) {
        let mut q = self.pictures.lock().unwrap();
        q.push_back(f);
        // A few at most: late is worse than a picture skipped.
        while q.len() > 3 {
            q.pop_front();
        }
        self.last_picture.store(now_ms(), Ordering::Relaxed);
    }

    /// Sound of a track (interleaved stereo).
    pub fn sound(&self, track: usize, samples: &[f32]) {
        let Some(t) = self.tracks.get(track) else { return };
        let mut t = t.lock().unwrap();
        t.buf.extend(samples);
        let max = (AHEAD_MAX * RATE as f64) as usize * 2;
        if t.buf.len() > max {
            let keep = (AHEAD * RATE as f64) as usize * 2;
            let drop = t.buf.len() - keep;
            t.buf.drain(..drop);
        }
        if t.buf.len() >= (AHEAD * RATE as f64) as usize * 2 {
            t.primed = true;
        }
    }

    /// `n` stereo samples of a track: silence while it lacks (then it fills again first).
    fn take(&self, track: usize, n: usize, out: &mut [f32]) -> bool {
        let mut t = self.tracks[track].lock().unwrap();
        if !t.primed || t.buf.len() < n * 2 {
            t.primed = false;
            out.iter_mut().for_each(|x| *x = 0.0);
            return false;
        }
        for (o, s) in out.iter_mut().zip(t.buf.drain(..n * 2)) {
            *o = s;
        }
        true
    }

    fn next_picture(&self) -> Option<Vec<u8>> {
        self.pictures.lock().unwrap().pop_front()
    }

    /// The source's pictures come (one in the last second).
    pub fn lost(&self) -> bool {
        now_ms() - self.last_picture.load(Ordering::Relaxed) > LOST_MS
    }
}

/// The pictures put over the source or instead of it, at the programme's size (YUVA 4:2:0: the
/// image and the slate opaque).
pub struct Pics {
    pub image: Arc<Vec<u8>>,
    pub overlay: Arc<Vec<u8>>,
    pub slate: Arc<Vec<u8>>,
}

/// Where the mixer sends what it makes: the encoder's two inputs. Broken: the encoder does not
/// keep up (or is gone), it is made again.
pub struct Sink {
    pub video: SyncSender<Vec<u8>>,
    pub audio: SyncSender<Vec<u8>>,
    pub broken: Arc<AtomicBool>,
}

/// A value going from one to another in some time.
#[derive(Clone, Copy)]
struct Ramp {
    from: f64,
    to: f64,
    start: Instant,
    secs: f64,
}

impl Ramp {
    fn at(v: f64) -> Ramp {
        Ramp { from: v, to: v, start: Instant::now(), secs: 0.0 }
    }
    fn value(&self, now: Instant) -> f64 {
        if self.secs <= 0.0 {
            return self.to;
        }
        let x = (now.duration_since(self.start).as_secs_f64() / self.secs).min(1.0);
        self.from + (self.to - self.from) * x
    }
    fn go(&mut self, to: f64, secs: f64, now: Instant) {
        *self = Ramp { from: self.value(now), to, start: now, secs };
    }
}

/// The mixer: runs until `done`, one frame at a time.
pub struct Mixer {
    pub shape: Shape,
    pub program: Program,
    pub inputs: Arc<Inputs>,
    pub pics: Arc<Mutex<Pics>>,
    pub changes: Receiver<Change>,
    pub sink: Arc<Mutex<Option<Sink>>>,
    pub done: Arc<AtomicBool>,
}

impl Mixer {
    pub fn spawn(self) -> std::io::Result<()> {
        std::thread::Builder::new().name("program-mixer".into()).spawn(move || self.run()).map(|_| ())
    }

    fn run(mut self) {
        let sh = self.shape;
        let fps = sh.fps.max(1) as usize;
        let n = RATE / fps;
        let period = Duration::from_secs_f64(1.0 / fps as f64);
        let yuv = (sh.w * sh.h) as usize * 3 / 2;
        let mut last: Vec<u8> = crate::program::plain(&sh, true)[..yuv].to_vec();
        let mut ever = false;
        let now = Instant::now();
        let mut image = Ramp::at(if self.program.image_on { 1.0 } else { 0.0 });
        let mut overlay = Ramp::at(self.program.overlay_alpha());
        let mut gains: Vec<f32> = (0..TRACKS).map(|k| track_gain(&self.program, k)).collect();
        let mut master = master_gain(&self.program);
        let mut sound = Sound2::default();
        let mut start = now;
        let mut tick: u64 = 0;
        while !self.done.load(Ordering::Relaxed) {
            let now = Instant::now();
            // What changed: the image and the overlay by a fade, the sound at once (smoothly).
            while let Ok(c) = self.changes.try_recv() {
                let next = c.program;
                if next.image_on != self.program.image_on {
                    image.go(if next.image_on { 1.0 } else { 0.0 }, c.image_fade, now);
                }
                if next.overlay_alpha() != self.program.overlay_alpha() {
                    overlay.go(next.overlay_alpha(), if next.overlay_on != self.program.overlay_on { OVERLAY_FADE } else { 0.0 }, now);
                }
                self.program = next;
            }
            // The picture: the source's next one, else its last; lost, the slate (or its last).
            if let Some(f) = self.inputs.next_picture() {
                if f.len() == yuv {
                    last = f;
                    ever = true;
                }
            }
            let lost = !ever || self.inputs.lost();
            let pics = self.pics.lock().unwrap();
            let slate = lost && self.program.slate;
            let mut out = if slate { pics.slate[..yuv].to_vec() } else { last.clone() };
            let k = image.value(now);
            blend(&mut out, &pics.image[..yuv], k);
            let a = overlay.value(now);
            if !slate && a > 0.0 {
                overlay_on(&mut out, &pics.overlay, &sh, a);
            }
            drop(pics);
            // The sound: the tracks on at their level, on the channels chosen, none under the
            // image, the master, then never above -1 dBFS.
            let next_gains: Vec<f32> = (0..TRACKS).map(|k| track_gain(&self.program, k)).collect();
            let next_master = master_gain(&self.program);
            let k_end = image.value(now + period);
            let samples = sound.mix(&self.inputs, n, &gains, &next_gains, &self.program.channels, (1.0 - k) as f32, (1.0 - k_end) as f32, master, next_master);
            gains = next_gains;
            master = next_master;
            self.send(out, samples);
            // The next frame, on the clock (late by more than a second: the clock starts again).
            tick += 1;
            let due = start + period * tick as u32;
            let now = Instant::now();
            if now > due + Duration::from_secs(1) {
                start = now;
                tick = 0;
            } else if due > now {
                std::thread::sleep(due - now);
            }
        }
    }

    fn send(&self, picture: Vec<u8>, sound: Vec<f32>) {
        let sink = self.sink.lock().unwrap();
        let Some(s) = sink.as_ref() else { return };
        let bytes: Vec<u8> = sound.iter().flat_map(|x| x.to_le_bytes()).collect();
        let full = |r: Result<(), TrySendError<Vec<u8>>>| matches!(r, Err(TrySendError::Full(_)));
        if full(s.video.try_send(picture)) || full(s.audio.try_send(bytes)) {
            s.broken.store(true, Ordering::Relaxed);
        }
    }
}

fn track_gain(p: &Program, k: usize) -> f32 {
    let t = p.track(k);
    if t.on { 10f64.powf(t.gain / 20.0) as f32 } else { 0.0 }
}

fn master_gain(p: &Program) -> f32 {
    10f64.powf(p.master / 20.0) as f32
}

/// The sound's state from one frame to the next.
struct Sound2 {
    /// The limiter's gain.
    env: f32,
    take: Vec<f32>,
}

impl Default for Sound2 {
    fn default() -> Self {
        Sound2 { env: 1.0, take: Vec::new() }
    }
}

impl Sound2 {
    /// `n` stereo samples: each track (all of them read, to keep their pace), those on mixed at
    /// their gain (from `g0` to `g1` over the frame), the channels, the programme's level (from
    /// `v0` to `v1`: none under the image), the master, the limiter.
    #[allow(clippy::too_many_arguments)]
    fn mix(&mut self, inputs: &Inputs, n: usize, g0: &[f32], g1: &[f32], channels: &str, v0: f32, v1: f32, m0: f32, m1: f32) -> Vec<f32> {
        let mut out = vec![0f32; n * 2];
        self.take.resize(n * 2, 0.0);
        for k in 0..TRACKS {
            let got = inputs.take(k, n, &mut self.take);
            if !got || (g0[k] == 0.0 && g1[k] == 0.0) {
                continue;
            }
            for i in 0..n {
                let g = g0[k] + (g1[k] - g0[k]) * (i as f32 / n as f32);
                out[2 * i] += self.take[2 * i] * g;
                out[2 * i + 1] += self.take[2 * i + 1] * g;
            }
        }
        let release = 1.0 - (-1.0f32 / (0.05 * RATE as f32)).exp();
        for i in 0..n {
            let x = i as f32 / n as f32;
            let (l, r) = (out[2 * i], out[2 * i + 1]);
            let (l, r) = match channels {
                "l" => (l, l),
                "r" => (r, r),
                "mono" => ((l + r) * 0.5, (l + r) * 0.5),
                _ => (l, r),
            };
            let v = (v0 + (v1 - v0) * x) * (m0 + (m1 - m0) * x);
            let (l, r) = (l * v, r * v);
            let peak = l.abs().max(r.abs());
            let want = if peak > LIMIT { LIMIT / peak } else { 1.0 };
            // At once down to what is needed, back up slowly (50 ms).
            self.env = (self.env + (1.0 - self.env) * release).min(want);
            out[2 * i] = l * self.env;
            out[2 * i + 1] = r * self.env;
        }
        out
    }
}

/// `out` = `out` × (1 − k) + `img` × k (YUV, same size).
pub fn blend(out: &mut [u8], img: &[u8], k: f64) {
    if k <= 0.0 {
        return;
    }
    if k >= 1.0 {
        out.copy_from_slice(&img[..out.len()]);
        return;
    }
    let w = (k * 256.0).round() as u32;
    for (o, &i) in out.iter_mut().zip(img) {
        *o = ((*o as u32 * (256 - w) + i as u32 * w) >> 8) as u8;
    }
}

/// The overlay (YUVA 4:2:0) over `out` (YUV 4:2:0), its alpha times `a`.
pub fn overlay_on(out: &mut [u8], ovl: &[u8], sh: &Shape, a: f64) {
    let (w, h) = (sh.w as usize, sh.h as usize);
    let n = w * h;
    if ovl.len() < n * 5 / 2 || out.len() < n * 3 / 2 {
        return;
    }
    let g = (a.clamp(0.0, 1.0) * 256.0).round() as u32;
    let alpha = &ovl[n * 3 / 2..];
    let mix = |o: &mut u8, v: u8, al: u8| {
        let x = (al as u32 * g) >> 8;
        if x == 0 {
            return;
        }
        let x = x + (x >> 7);
        *o = ((*o as u32 * (256 - x) + v as u32 * x) >> 8) as u8;
    };
    for i in 0..n {
        mix(&mut out[i], ovl[i], alpha[i]);
    }
    let (cw, ch) = (w / 2, h / 2);
    for cy in 0..ch {
        for cx in 0..cw {
            let al = alpha[(2 * cy) * w + 2 * cx];
            if al == 0 {
                continue;
            }
            let c = cy * cw + cx;
            mix(&mut out[n + c], ovl[n + c], al);
            mix(&mut out[n + n / 4 + c], ovl[n + n / 4 + c], al);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blends() {
        let mut o = vec![0u8, 100, 200];
        blend(&mut o, &[200, 100, 0], 0.5);
        assert_eq!(o, vec![100, 100, 100]);
        let mut o = vec![10u8; 3];
        blend(&mut o, &[50; 3], 1.0);
        assert_eq!(o, vec![50; 3]);
        let mut o = vec![10u8; 3];
        blend(&mut o, &[50; 3], 0.0);
        assert_eq!(o, vec![10; 3]);
    }

    #[test]
    fn overlays() {
        let sh = Shape { w: 4, h: 2, fps: 25, tracks: 0, kbps: 1 };
        let mut out = vec![16u8; 8];
        out.extend([128u8; 4]);
        // White where opaque (the first column), nothing elsewhere.
        let mut ovl = vec![235u8; 8];
        ovl.extend([128u8; 4]);
        ovl.extend([255, 0, 0, 0, 255, 0, 0, 0]);
        overlay_on(&mut out, &ovl, &sh, 1.0);
        assert_eq!(&out[..8], &[235, 16, 16, 16, 235, 16, 16, 16]);
        let mut half = vec![16u8; 8];
        half.extend([128u8; 4]);
        overlay_on(&mut half, &ovl, &sh, 0.5);
        assert!((120..=130).contains(&half[0]), "{}", half[0]);
    }

    #[test]
    fn sound_mixes_and_limits() {
        let inputs = Inputs::default();
        // Track 0: 0.5 on both channels; track 1: 0.8 (off); 0.2 s of each.
        let n = 1920;
        let s: Vec<f32> = vec![0.5; RATE / 5 * 2];
        inputs.sound(0, &s);
        inputs.sound(1, &vec![0.8; RATE / 5 * 2]);
        let mut st = Sound2::default();
        let g = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let out = st.mix(&inputs, n, &g, &g, "lr", 1.0, 1.0, 1.0, 1.0);
        assert!(out.iter().all(|x| (x - 0.5).abs() < 1e-6));
        // Both on, +6 dB: limited to -1 dBFS.
        let g = [2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let out = st.mix(&inputs, n, &g, &g, "lr", 1.0, 1.0, 1.0, 1.0);
        assert!(out.iter().all(|x| *x <= LIMIT + 1e-6));
        // Under the image: none.
        let out = st.mix(&inputs, n, &g, &g, "lr", 0.0, 0.0, 1.0, 1.0);
        assert!(out.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn a_track_that_lacks_is_silent_then_fills_again() {
        let inputs = Inputs::default();
        let mut out = vec![1f32; 200];
        assert!(!inputs.take(0, 100, &mut out));
        assert!(out.iter().all(|x| *x == 0.0));
        // Less than what is kept ahead: not yet.
        inputs.sound(0, &vec![0.1; 2000]);
        assert!(!inputs.take(0, 100, &mut out));
        inputs.sound(0, &vec![0.1; 10_000]);
        assert!(inputs.take(0, 100, &mut out));
    }

    #[test]
    fn ramps() {
        let t = Instant::now();
        let mut r = Ramp::at(0.0);
        r.go(1.0, 1.0, t);
        assert!((r.value(t + Duration::from_millis(500)) - 0.5).abs() < 1e-9);
        assert_eq!(r.value(t + Duration::from_secs(2)), 1.0);
        r.go(0.0, 0.0, t);
        assert_eq!(r.value(t), 0.0);
    }
}
