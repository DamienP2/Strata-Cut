//! Streamer: the mixer of a source's programme, a frame synchronizer. On its own clock it makes a
//! picture and its sound every frame, whatever comes in: the source's picture and sound as the
//! decoder gives them, the image over it by a fade, the overlay, the tracks on at their level, the
//! channels, the master, a limiter at -1 dBFS.
//!
//! The decoder gives the source at the source's pace, already in step: pictures at the
//! programme's rate and, for each track, the sound of each picture (`n` samples a picture, all
//! from the same moment). A few frames wait between it and the mixer (its unevenness absorbed);
//! more (a burst, the source's clock a little faster than ours), the oldest are dropped, picture
//! and sound together; none (the source's clock a little slower, a stall), the last picture is
//! held and the sound is silent until a few wait again; none for a while, the source is lost: the
//! slate. No timestamps, no clock to guess: what waits says it all.
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{Receiver, Sender},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use crate::program::{Change, Program, Shape};

/// Sound: 48 kHz, stereo, 32-bit float.
pub const RATE: usize = 48_000;
/// Tracks of a source mixed at most.
pub const TRACKS: usize = 8;
/// What goes out never passes -1 dBFS.
const LIMIT: f32 = 0.891;
/// The overlay's fade when it is put on or taken off (s).
const OVERLAY_FADE: f64 = 0.3;
/// Frames waiting before the mixer takes them (after a start or a lack).
const KEEP: usize = 5;
/// More waiting than this: the oldest dropped, down to `KEEP`.
const MOST: usize = 25;
/// Nothing from the source this long: lost (the slate).
const LOST: Duration = Duration::from_secs(3);

/// What the decoder gives, in step: picture `k` goes with samples `k·n` to `(k+1)·n` of each
/// track (interleaved stereo).
pub struct Inputs {
    /// Samples a picture.
    n: usize,
    q: Mutex<Queues>,
}

#[derive(Default)]
struct Queues {
    pictures: VecDeque<Vec<u8>>,
    sound: Vec<VecDeque<f32>>,
    /// Samples of each track played (as silence) before they came: dropped when they come.
    owed: Vec<usize>,
    /// Waiting for `KEEP` frames before taking them.
    filling: bool,
    /// When the last picture came.
    last: Option<Instant>,
}

impl Inputs {
    pub fn new(fps: u32) -> Inputs {
        let q = Queues { sound: vec![VecDeque::new(); TRACKS], owed: vec![0; TRACKS], filling: true, ..Default::default() };
        Inputs { n: RATE / fps.max(1) as usize, q: Mutex::new(q) }
    }

    /// A picture (YUV 4:2:0 at the programme's size).
    pub fn picture(&self, f: Vec<u8>) {
        let mut q = self.q.lock().unwrap();
        q.pictures.push_back(f);
        q.last = Some(Instant::now());
        // Far too many (the mixer stopped): the oldest dropped, with their sound.
        if q.pictures.len() > MOST * 4 {
            q.pictures.pop_front();
            let n2 = self.n * 2;
            for k in 0..TRACKS {
                let have = q.sound[k].len().min(n2);
                q.sound[k].drain(..have);
                q.owed[k] += n2 - have;
            }
        }
    }

    /// Sound of a track (interleaved stereo).
    pub fn sound(&self, track: usize, samples: &[f32]) {
        let mut q = self.q.lock().unwrap();
        let Some(owed) = q.owed.get(track).copied() else { return };
        // What was owed (played as silence) is of the past.
        let skip = owed.min(samples.len());
        q.owed[track] -= skip;
        q.sound[track].extend(&samples[skip..]);
    }

    /// The decoder starts again: what waits is of the run before (out of step with the next).
    pub fn restart(&self) {
        let mut q = self.q.lock().unwrap();
        q.pictures.clear();
        q.sound.iter_mut().for_each(VecDeque::clear);
        q.owed.iter_mut().for_each(|o| *o = 0);
        q.filling = true;
    }

    /// The next frame: a picture (none: hold the last) and each track's sound into `sound`
    /// (silence where it has none).
    fn next(&self, sound: &mut [Vec<f32>], stats: &mut Stats) -> Option<Vec<u8>> {
        let n2 = self.n * 2;
        let mut q = self.q.lock().unwrap();
        // Too many waiting: the oldest dropped, picture and sound together.
        if q.pictures.len() > MOST {
            let drop = q.pictures.len() - KEEP;
            q.pictures.drain(..drop);
            for k in 0..TRACKS {
                let have = q.sound[k].len().min(drop * n2);
                q.sound[k].drain(..have);
                q.owed[k] += drop * n2 - have;
            }
            stats.dropped += drop as u32;
        }
        if q.filling && q.pictures.len() >= KEEP {
            q.filling = false;
        }
        let picture = if q.filling { None } else { q.pictures.pop_front() };
        if picture.is_none() {
            // None: the sound waits with the picture (they stay in step), silent meanwhile.
            q.filling = true;
            sound.iter_mut().for_each(|s| s.iter_mut().for_each(|x| *x = 0.0));
            return None;
        }
        for (k, out) in sound.iter_mut().enumerate() {
            let have = q.sound[k].len().min(n2);
            for (o, s) in out.iter_mut().zip(q.sound[k].drain(..have)) {
                *o = s;
            }
            out[have..].iter_mut().for_each(|x| *x = 0.0);
            q.owed[k] += n2 - have;
        }
        picture
    }

    /// The source is lost: no picture waits, and none came for a while.
    pub fn lost(&self) -> bool {
        let q = self.q.lock().unwrap();
        q.pictures.is_empty() && q.last.is_none_or(|t| t.elapsed() > LOST)
    }
}

/// The pictures put over the source or instead of it, at the programme's size (YUVA 4:2:0: the
/// image and the slate opaque).
pub struct Pics {
    pub image: Arc<Vec<u8>>,
    pub overlay: Arc<Vec<u8>>,
    pub slate: Arc<Vec<u8>>,
}

/// Where the mixer sends what it makes: the encoder's two inputs, each written by its own thread
/// (`queued`: what waits to be written). The encoder late (starting, the server busy): the frame
/// is not sent, picture and sound alike (they stay in step), rather than piling up.
pub struct Sink {
    pub video: Sender<Vec<u8>>,
    pub audio: Sender<Vec<u8>>,
    pub queued: [Arc<AtomicUsize>; 2],
    /// Frames waiting at most.
    pub most: usize,
    /// One of its inputs broke (the encoder is made again).
    pub dead: Arc<AtomicBool>,
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

/// What went wrong in the last minute, said in the journal.
#[derive(Default)]
struct Stats {
    /// Frames the source's picture was held (none waiting).
    held: u32,
    /// Pictures of the source dropped (too many waiting).
    dropped: u32,
    /// Frames not sent (the encoder late).
    unsent: u32,
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
    /// A picture of the source was mixed.
    pub pictured: Arc<AtomicBool>,
}

impl Mixer {
    pub fn spawn(self) -> std::io::Result<()> {
        std::thread::Builder::new().name("program-mixer".into()).spawn(move || self.run()).map(|_| ())
    }

    fn run(mut self) {
        let sh = self.shape;
        let fps = sh.fps.max(1);
        let period = Duration::from_secs(1) / fps;
        let n = RATE / fps as usize;
        let yuv = (sh.w * sh.h) as usize * 3 / 2;
        let mut last: Vec<u8> = crate::program::plain(&sh, true)[..yuv].to_vec();
        let mut ever = false;
        let mut image = Ramp::at(if self.program.image_on { 1.0 } else { 0.0 });
        let mut overlay = Ramp::at(self.program.overlay_alpha());
        let mut levels = Levels::of(&self.program);
        let mut limiter = Limiter::default();
        let mut sound = vec![vec![0f32; n * 2]; TRACKS];
        let mut stats = Stats::default();
        let mut said = Instant::now();
        let start = Instant::now();
        let mut tick: u32 = 0;
        while !self.done.load(Ordering::Relaxed) {
            let now = Instant::now();
            // What changed: the image and the overlay by a fade, the sound smoothly.
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
            // The source's picture and sound; none: the last picture, silence.
            match self.inputs.next(&mut sound, &mut stats) {
                Some(f) if f.len() == yuv => {
                    last = f;
                    if !ever {
                        ever = true;
                        self.pictured.store(true, Ordering::Relaxed);
                    }
                }
                _ if ever => stats.held += 1,
                _ => {}
            }
            // The picture: the source's (lost: the slate), the image over it, the overlay.
            let pics = self.pics.lock().unwrap();
            let slate = ever && self.program.slate && self.inputs.lost();
            let mut out = if slate { pics.slate[..yuv].to_vec() } else { last.clone() };
            let k = image.value(now);
            blend(&mut out, &pics.image[..yuv], k);
            let a = overlay.value(now);
            if !slate && a > 0.0 {
                overlay_on(&mut out, &pics.overlay, &sh, a);
            }
            drop(pics);
            // The sound: none under the image (it fades with it).
            let next = Levels { under: 1.0 - image.value(now + period) as f32, ..Levels::of(&self.program) };
            let level = Levels { under: 1.0 - k as f32, ..levels };
            let samples = mix(&sound, &level, &next, &self.program.channels, &mut limiter);
            levels = next;
            if !self.send(out, samples) {
                stats.unsent += 1;
            }
            if said.elapsed() >= Duration::from_secs(60) {
                if stats.held > 0 || stats.dropped > 0 || stats.unsent > 0 {
                    tracing::info!("programme: the last minute, {} frames with the source's picture held (none came), {} of its pictures dropped (too many came), {} frames not sent (the encoder late)", stats.held, stats.dropped, stats.unsent);
                }
                stats = Stats::default();
                said = Instant::now();
            }
            // The next frame, on the clock.
            tick += 1;
            let due = start + period * tick;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
        }
    }

    /// False: not sent (the encoder late, or not there).
    fn send(&self, picture: Vec<u8>, sound: Vec<f32>) -> bool {
        let sink = self.sink.lock().unwrap();
        let Some(s) = sink.as_ref() else { return false };
        if s.dead.load(Ordering::Relaxed) || s.queued.iter().any(|q| q.load(Ordering::Relaxed) >= s.most) {
            return false;
        }
        let bytes: Vec<u8> = sound.iter().flat_map(|x| x.to_le_bytes()).collect();
        s.queued.iter().for_each(|q| {
            q.fetch_add(1, Ordering::Relaxed);
        });
        let _ = s.video.send(picture);
        let _ = s.audio.send(bytes);
        true
    }
}

/// The levels of a frame: each track's (0: off), the master, the programme's (none under the
/// image).
#[derive(Clone, Copy)]
struct Levels {
    tracks: [f32; TRACKS],
    master: f32,
    under: f32,
}

impl Levels {
    fn of(p: &Program) -> Levels {
        let db = |v: f64| 10f64.powf(v / 20.0) as f32;
        let mut tracks = [0f32; TRACKS];
        for (k, g) in tracks.iter_mut().enumerate() {
            let t = p.track(k);
            *g = if t.on { db(t.gain) } else { 0.0 };
        }
        Levels { tracks, master: db(p.master), under: 1.0 }
    }
}

/// The limiter's gain, from one frame to the next.
struct Limiter {
    gain: f32,
}

impl Default for Limiter {
    fn default() -> Self {
        Limiter { gain: 1.0 }
    }
}

/// A frame's sound (interleaved stereo): the tracks at their level (from `a`'s to `b`'s over the
/// frame: no click), on the channels chosen, the master, the programme's level, then never above
/// -1 dBFS (down at once, back up in 50 ms).
fn mix(sound: &[Vec<f32>], a: &Levels, b: &Levels, channels: &str, lim: &mut Limiter) -> Vec<f32> {
    let len = sound.first().map_or(0, Vec::len);
    let n = len / 2;
    let mut out = vec![0f32; len];
    for (k, s) in sound.iter().enumerate() {
        let (g0, g1) = (a.tracks[k], b.tracks[k]);
        if g0 == 0.0 && g1 == 0.0 {
            continue;
        }
        for i in 0..n {
            let g = g0 + (g1 - g0) * (i as f32 / n as f32);
            out[2 * i] += s[2 * i] * g;
            out[2 * i + 1] += s[2 * i + 1] * g;
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
        let v = (a.master + (b.master - a.master) * x) * (a.under + (b.under - a.under) * x);
        let (l, r) = (l * v, r * v);
        let peak = l.abs().max(r.abs());
        let want = if peak > LIMIT { LIMIT / peak } else { 1.0 };
        lim.gain = (lim.gain + (1.0 - lim.gain) * release).min(want);
        out[2 * i] = l * lim.gain;
        out[2 * i + 1] = r * lim.gain;
    }
    out
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

    /// Pictures saying their number, and their sound (each sample its picture's number).
    fn feed(inputs: &Inputs, from: u8, count: u8) {
        for k in from..from + count {
            inputs.picture(vec![k]);
            inputs.sound(0, &vec![k as f32; inputs.n * 2]);
        }
    }

    fn take(inputs: &Inputs, stats: &mut Stats) -> (Option<u8>, f32) {
        let mut sound = vec![vec![9f32; inputs.n * 2]; TRACKS];
        let p = inputs.next(&mut sound, stats).map(|f| f[0]);
        (p, sound[0][0])
    }

    #[test]
    fn in_step_after_a_few_wait() {
        let inputs = Inputs::new(25);
        let mut st = Stats::default();
        // Fewer than KEEP: nothing yet (silence).
        feed(&inputs, 0, KEEP as u8 - 1);
        assert_eq!(take(&inputs, &mut st), (None, 0.0));
        feed(&inputs, KEEP as u8 - 1, 1);
        // Then each picture with its sound, in order.
        for k in 0..KEEP as u8 {
            assert_eq!(take(&inputs, &mut st), (Some(k), k as f32));
        }
        // None left: held, silent, until KEEP wait again.
        assert_eq!(take(&inputs, &mut st), (None, 0.0));
        feed(&inputs, 10, 2);
        assert_eq!(take(&inputs, &mut st).0, None);
        feed(&inputs, 12, KEEP as u8);
        assert_eq!(take(&inputs, &mut st), (Some(10), 10.0));
    }

    #[test]
    fn too_many_dropped_together() {
        let inputs = Inputs::new(25);
        let mut st = Stats::default();
        feed(&inputs, 0, MOST as u8 + 5);
        // Down to KEEP: the newest, picture and sound alike.
        let first = (MOST + 5 - KEEP) as u8;
        assert_eq!(take(&inputs, &mut st), (Some(first), first as f32));
        assert_eq!(st.dropped as usize, MOST + 5 - KEEP);
    }

    #[test]
    fn a_track_late_stays_in_step() {
        let inputs = Inputs::new(25);
        let mut st = Stats::default();
        // The pictures come, track 0's sound not yet: silence, owed.
        for k in 0..KEEP as u8 + 2 {
            inputs.picture(vec![k]);
        }
        assert_eq!(take(&inputs, &mut st), (Some(0), 0.0));
        assert_eq!(take(&inputs, &mut st), (Some(1), 0.0));
        // Its sound comes now, from the start: the two frames played are dropped.
        for k in 0..KEEP as u8 + 2 {
            inputs.sound(0, &vec![k as f32; inputs.n * 2]);
        }
        assert_eq!(take(&inputs, &mut st), (Some(2), 2.0));
    }

    #[test]
    fn lost_after_a_while_without() {
        let inputs = Inputs::new(25);
        assert!(inputs.lost());
        inputs.picture(vec![0]);
        assert!(!inputs.lost());
    }

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
        let n = 1920;
        let mut sound = vec![vec![0f32; n * 2]; TRACKS];
        sound[0] = vec![0.5; n * 2];
        sound[1] = vec![0.8; n * 2];
        let mut lim = Limiter::default();
        let mut lv = Levels { tracks: [0.0; TRACKS], master: 1.0, under: 1.0 };
        lv.tracks[0] = 1.0;
        let out = mix(&sound, &lv, &lv, "lr", &mut lim);
        assert!(out.iter().all(|x| (x - 0.5).abs() < 1e-6));
        // Both on, +6 dB: limited to -1 dBFS.
        lv.tracks = [2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let out = mix(&sound, &lv, &lv, "lr", &mut lim);
        assert!(out.iter().all(|x| *x <= LIMIT + 1e-6));
        // Under the image: none.
        let under = Levels { under: 0.0, ..lv };
        let out = mix(&sound, &under, &under, "lr", &mut lim);
        assert!(out.iter().all(|x| *x == 0.0));
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
