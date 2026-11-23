//! Streamer: the mixer of a source's programme, a frame synchronizer. On its own clock it makes a
//! picture and its sound every frame, whatever comes in: the source's picture and sound as the
//! decoder gives them, the image over it by a fade, the overlay, the tracks on at their level, the
//! channels, the master, a limiter at -1 dBFS.
//!
//! The decoder gives the source as it comes, already in step and numbered: pictures at the
//! programme's rate from the moment 0, and the sound of each (`n` samples a picture: picture `k`
//! goes with samples `k·n` to `(k+1)·n` of each track). A frame synchronizer plays picture `k`
//! and its sound at frame `start + k` of the mixer's clock: `start` is the first picture's coming
//! plus a steady delay (1 s; a source in segments, HLS or the web: 10 s), so what comes unevenly,
//! in bursts or a little late is played evenly. A picture not there at its frame: the one before
//! is held, with its sound, and the rest is played a frame later (the source's clock a little
//! slower than ours; a lack); a source that faltered longer is played again at the delay. None
//! for a while: lost, the slate. What waits is kept small: beyond, the decoder waits (what comes
//! waits before it, compressed).
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
/// Nothing from the source this long: lost (the slate).
const LOST: Duration = Duration::from_secs(3);
/// Frames over which the fewest waiting is looked at (a source a little quicker than our clock).
const LOOK: u32 = 250;

/// What the decoder gives, and when each is played.
pub struct Inputs {
    /// Samples a picture.
    n: usize,
    /// Frames between a picture's coming and its playing.
    delay: usize,
    /// Pictures waiting at most: beyond, the decoder waits for room.
    most: usize,
    q: Mutex<Queues>,
}

#[derive(Default)]
struct Queues {
    pictures: VecDeque<Vec<u8>>,
    /// The number of the first picture waiting (from the decoder's start).
    first: i64,
    /// Each track's samples (interleaved stereo), and the number of the first waiting.
    sound: Vec<VecDeque<f32>>,
    sound_first: Vec<i64>,
    /// The mixer's frame at which picture 0 is played (none: not yet known).
    start: Option<i64>,
    /// Frames held in a row (the source late).
    held: usize,
    /// The fewest pictures waiting these last frames, and the frames left to look.
    fewest: usize,
    look: u32,
    /// The last frame's sound, held with its picture.
    last_sound: Vec<Vec<f32>>,
    /// When the last picture came.
    last: Option<Instant>,
}

/// What the synchronizer gives for a frame.
#[derive(Debug, PartialEq)]
enum Frame {
    /// The source's picture of this frame (its sound given).
    Picture(Vec<u8>),
    /// None in time: the last one held.
    Held,
    /// None to play yet (the delay filling).
    Waiting,
}

impl Inputs {
    /// `delay`: seconds between a picture's coming and its playing.
    pub fn new(fps: u32, delay: u32) -> Inputs {
        let fps = fps.max(1) as usize;
        let n = RATE / fps;
        let q = Queues {
            sound: vec![VecDeque::new(); TRACKS],
            sound_first: vec![0; TRACKS],
            last_sound: vec![vec![0.0; n * 2]; TRACKS],
            fewest: usize::MAX,
            look: LOOK,
            ..Default::default()
        };
        Inputs { n, delay: fps * delay as usize, most: 2 * fps, q: Mutex::new(q) }
    }

    /// The next picture (YUV 4:2:0 at the programme's size).
    pub fn picture(&self, f: Vec<u8>) {
        let mut q = self.q.lock().unwrap();
        q.pictures.push_back(f);
        q.last = Some(Instant::now());
    }

    /// Enough pictures wait: the decoder waits for room (what comes waits before it, compressed).
    pub fn full(&self) -> bool {
        self.q.lock().unwrap().pictures.len() >= self.most
    }

    /// The next sound of a track (interleaved stereo).
    pub fn sound(&self, track: usize, samples: &[f32]) {
        let most = (self.most + self.delay) * self.n * 2;
        let mut q = self.q.lock().unwrap();
        let Some(s) = q.sound.get_mut(track) else { return };
        s.extend(samples);
        // Sound without pictures to play it with: the oldest dropped.
        let over = s.len().saturating_sub(most) / 2;
        s.drain(..over * 2);
        q.sound_first[track] += over as i64;
    }

    /// The decoder starts again: its numbers start again (what waits is dropped, the delay fills).
    pub fn restart(&self) {
        let mut q = self.q.lock().unwrap();
        q.pictures.clear();
        q.first = 0;
        q.sound.iter_mut().for_each(VecDeque::clear);
        q.sound_first.iter_mut().for_each(|f| *f = 0);
        q.start = None;
    }

    /// Frame `tick` of the mixer's clock: its picture, and each track's sound into `sound`.
    fn frame(&self, tick: u64, sound: &mut [Vec<f32>]) -> Frame {
        let (n, n2, tick) = (self.n as i64, self.n * 2, tick as i64);
        let mut q = self.q.lock().unwrap();
        // The first picture of a run, or the first after a long lack: played after the delay.
        if q.start.is_none() {
            if q.pictures.is_empty() {
                sound.iter_mut().for_each(|s| s.fill(0.0));
                return Frame::Waiting;
            }
            q.start = Some(tick + self.delay as i64 - q.first);
            q.held = 0;
            q.fewest = usize::MAX;
            q.look = LOOK;
        }
        let start = q.start.unwrap_or(0);
        let k = tick - start;
        if k < q.first {
            sound.iter_mut().for_each(|s| s.fill(0.0));
            return Frame::Waiting;
        }
        // Older ones (played late, or one dropped): dropped.
        while q.first < k && q.pictures.pop_front().is_some() {
            q.first += 1;
        }
        if q.first == k {
            if let Some(f) = q.pictures.pop_front() {
                q.first += 1;
                q.held = 0;
                for (t, out) in sound.iter_mut().enumerate() {
                    // Its samples: those before them dropped, those not come yet silence.
                    let before = (k * n - q.sound_first[t]).clamp(0, q.sound[t].len() as i64 / 2);
                    q.sound[t].drain(..before as usize * 2);
                    q.sound_first[t] += before;
                    let have = if q.sound_first[t] == k * n { q.sound[t].len().min(n2) } else { 0 };
                    for (o, x) in out.iter_mut().zip(q.sound[t].drain(..have)) {
                        *o = x;
                    }
                    out[have..].fill(0.0);
                    q.sound_first[t] += (have / 2) as i64;
                    q.last_sound[t].copy_from_slice(out);
                }
                // A source a little quicker than our clock: more and more wait. When even the
                // fewest of a while is half a second over the delay, one is dropped.
                q.fewest = q.fewest.min(q.pictures.len());
                q.look -= 1;
                if q.look == 0 {
                    if q.fewest > self.delay + self.most / 4 {
                        q.start = Some(start - 1);
                    }
                    q.fewest = usize::MAX;
                    q.look = LOOK;
                }
                return Frame::Picture(f);
            }
        }
        // Not there in time: the one before held, with its sound (once: then silence), and the
        // rest played a frame later. Held longer than the delay: played again at the delay.
        q.held += 1;
        q.start = Some(start + 1);
        for (t, out) in sound.iter_mut().enumerate() {
            if q.held == 1 {
                out.copy_from_slice(&q.last_sound[t]);
            } else {
                out.fill(0.0);
            }
        }
        if q.held > self.delay.max(1) {
            q.start = None;
        }
        Frame::Held
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
        let mut tick: u64 = 0;
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
            // The source's picture and sound of this frame; none: the last picture.
            match self.inputs.frame(tick, &mut sound) {
                Frame::Picture(f) if f.len() == yuv => {
                    last = f;
                    if !ever {
                        ever = true;
                        self.pictured.store(true, Ordering::Relaxed);
                    }
                }
                Frame::Held if ever && !self.inputs.lost() => stats.held += 1,
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
                if stats.held > 0 || stats.unsent > 0 {
                    tracing::info!("programme: the last minute, {} frames with the source's picture held (it came late), {} frames not sent (the encoder late)", stats.held, stats.unsent);
                }
                stats = Stats::default();
                said = Instant::now();
            }
            // The next frame, on the clock.
            tick += 1;
            let due = start + period * tick as u32;
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
    fn feed(inputs: &Inputs, from: u32, count: u32) {
        for k in from..from + count {
            inputs.picture(vec![k as u8]);
            inputs.sound(0, &vec![(k % 256) as f32; inputs.n * 2]);
        }
    }

    /// Frame `tick`: its picture's number (or how it is not), and its sound.
    fn at(inputs: &Inputs, tick: u64) -> (Result<u8, Frame>, f32) {
        let mut sound = vec![vec![9f32; inputs.n * 2]; TRACKS];
        let f = match inputs.frame(tick, &mut sound) {
            Frame::Picture(p) => Ok(p[0]),
            other => Err(other),
        };
        (f, sound[0][0])
    }

    #[test]
    fn played_after_the_delay_each_at_its_frame() {
        // 1 s at 25 fps: 25 frames.
        let inputs = Inputs::new(25, 1);
        assert_eq!(at(&inputs, 0), (Err(Frame::Waiting), 0.0));
        // The first comes at frame 10: played at 35.
        feed(&inputs, 0, 3);
        assert_eq!(at(&inputs, 10), (Err(Frame::Waiting), 0.0));
        assert_eq!(at(&inputs, 34), (Err(Frame::Waiting), 0.0));
        assert_eq!(at(&inputs, 35), (Ok(0), 0.0));
        assert_eq!(at(&inputs, 36), (Ok(1), 1.0));
        // Some in a burst, some late: each at its frame all the same.
        feed(&inputs, 3, 20);
        assert_eq!(at(&inputs, 37), (Ok(2), 2.0));
        assert_eq!(at(&inputs, 38), (Ok(3), 3.0));
    }

    #[test]
    fn one_late_held_then_on() {
        let inputs = Inputs::new(25, 1);
        feed(&inputs, 0, 2);
        assert_eq!(at(&inputs, 0), (Err(Frame::Waiting), 0.0));
        assert_eq!(at(&inputs, 25), (Ok(0), 0.0));
        assert_eq!(at(&inputs, 26), (Ok(1), 1.0));
        // 2 not there: 1 held with its sound, once.
        assert_eq!(at(&inputs, 27), (Err(Frame::Held), 1.0));
        assert_eq!(at(&inputs, 28), (Err(Frame::Held), 0.0));
        // It comes: played now, the rest after it, a frame later each.
        feed(&inputs, 2, 2);
        assert_eq!(at(&inputs, 29), (Ok(2), 2.0));
        assert_eq!(at(&inputs, 30), (Ok(3), 3.0));
    }

    #[test]
    fn the_sound_at_its_picture_whenever_it_comes() {
        let inputs = Inputs::new(25, 1);
        // Pictures first, the sound later (or sooner): each with its own.
        for k in 0..4u8 {
            inputs.picture(vec![k]);
        }
        assert_eq!(at(&inputs, 0), (Err(Frame::Waiting), 0.0));
        assert_eq!(at(&inputs, 25).0, Ok(0));
        for k in 0..4u8 {
            inputs.sound(0, &vec![k as f32; inputs.n * 2]);
        }
        // Picture 1 with sound 1 (sound 0, come late, dropped).
        assert_eq!(at(&inputs, 26), (Ok(1), 1.0));
    }

    #[test]
    fn a_long_lack_played_again_at_the_delay() {
        let inputs = Inputs::new(25, 1);
        feed(&inputs, 0, 1);
        assert_eq!(at(&inputs, 0), (Err(Frame::Waiting), 0.0));
        assert_eq!(at(&inputs, 25).0, Ok(0));
        // More than the delay without: then the next waits the delay again.
        for t in 26..52 {
            assert_eq!(at(&inputs, t).0, Err(Frame::Held));
        }
        assert_eq!(at(&inputs, 52).0, Err(Frame::Waiting));
        feed(&inputs, 1, 1);
        assert_eq!(at(&inputs, 60).0, Err(Frame::Waiting));
        assert_eq!(at(&inputs, 85).0, Ok(1));
    }

    #[test]
    fn a_new_run_starts_again() {
        let inputs = Inputs::new(25, 1);
        feed(&inputs, 0, 3);
        assert_eq!(at(&inputs, 0).0, Err(Frame::Waiting));
        assert_eq!(at(&inputs, 25).0, Ok(0));
        inputs.restart();
        feed(&inputs, 50, 2);
        assert_eq!(at(&inputs, 26).0, Err(Frame::Waiting));
        assert_eq!(at(&inputs, 51), (Ok(50), 50.0));
    }

    #[test]
    fn a_quicker_source_one_dropped_now_and_then() {
        let inputs = Inputs::new(25, 1);
        // Always 45 waiting (the delay is 25): after a while, one dropped.
        feed(&inputs, 0, 45);
        assert_eq!(at(&inputs, 0).0, Err(Frame::Waiting));
        for t in 25..25 + LOOK as u64 {
            feed(&inputs, t as u32 + 20, 1);
            assert_eq!(at(&inputs, t), (Ok((t - 25) as u8), ((t - 25) % 256) as f32));
        }
        let t = 25 + LOOK as u64;
        feed(&inputs, t as u32 + 20, 1);
        assert_eq!(at(&inputs, t), (Ok((t - 24) as u8), ((t - 24) % 256) as f32));
    }

    #[test]
    fn as_many_as_the_delay_none_dropped() {
        let inputs = Inputs::new(25, 1);
        feed(&inputs, 0, 1);
        assert_eq!(at(&inputs, 0).0, Err(Frame::Waiting));
        // Each comes a frame after the one before: 25 wait, as they should.
        for t in 1..25 + 3 * LOOK as u64 {
            feed(&inputs, t as u32, 1);
            if t >= 25 {
                assert_eq!(at(&inputs, t).0, Ok((t - 25) as u8));
            } else {
                assert_eq!(at(&inputs, t).0, Err(Frame::Waiting));
            }
        }
    }

    #[test]
    fn full_at_most() {
        let inputs = Inputs::new(25, 1);
        feed(&inputs, 0, 49);
        assert!(!inputs.full());
        feed(&inputs, 100, 1);
        assert!(inputs.full());
    }

    #[test]
    fn lost_after_a_while_without() {
        let inputs = Inputs::new(25, 1);
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
