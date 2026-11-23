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
        atomic::AtomicUsize,
        mpsc::{Receiver, Sender},
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
/// No picture from the source this long (ms): lost.
const LOST_MS: i64 = 1000;
/// The overlay's fade when it is put on or taken off (s).
const OVERLAY_FADE: f64 = 0.3;

/// What the decoders give the mixer, with their timestamps (the source's own, in seconds): each
/// is played at its moment, the source's time mapped to ours: a margin (`DELAY`) after the
/// latest any came lately. A source that comes in bursts (HLS: a segment of some seconds at a
/// time) plays as smoothly as one that comes evenly, only later; neither faster nor out of step.
pub struct Inputs {
    clock: Mutex<Clock>,
    /// The last picture's timestamp: the others' turn of the clock taken from it.
    reference: Mutex<Option<f64>>,
    pictures: Mutex<VecDeque<(f64, Vec<u8>)>>,
    /// When the last picture came (ms; 0: never).
    last_picture: AtomicI64,
    tracks: Vec<Mutex<Sound>>,
}

/// The margin after the latest a picture came after its timestamp (s).
const DELAY: f64 = 0.5;
/// Pictures waiting at most: the decoder waits meanwhile (what comes waits before it, compressed).
pub const PICTURES_MOST: usize = 75;
/// The delays looked at (s).
const WINDOW: f64 = 30.0;

/// The source's time and ours: the greatest delay (our time minus its timestamp) of the last
/// 30 s: everything that came since was there before its moment.
#[derive(Default)]
struct Clock {
    delays: VecDeque<(f64, f64)>,
    most: Option<f64>,
}

impl Clock {
    fn seen(&mut self, pts: f64, now: f64) {
        let d = now - pts;
        // Another timeline (the source started again, its clock jumped): from now on.
        if self.most.is_some_and(|l| (d - l).abs() > 25.0) {
            self.delays.clear();
        }
        // Kept: the delays that may still be the greatest (a smaller one before a greater is
        // never again).
        while self.delays.back().is_some_and(|x| x.1 <= d) {
            self.delays.pop_back();
        }
        self.delays.push_back((now, d));
        while self.delays.front().is_some_and(|x| x.0 < now - WINDOW) {
            self.delays.pop_front();
        }
        self.most = self.delays.front().map(|x| x.1);
    }

    /// The source's moment to play now.
    fn play(&self, now: f64) -> Option<f64> {
        Some(now - self.most? - DELAY)
    }

    /// Played `by` s sooner (the delay was taken too great).
    fn sooner(&mut self, by: f64) {
        self.delays.iter_mut().for_each(|x| x.1 -= by);
        self.most = self.most.map(|m| m - by);
    }
}

/// A track's sound: interleaved stereo samples, from the moment `head` (the source's time).
#[derive(Default)]
struct Sound {
    buf: VecDeque<f32>,
    head: f64,
}

/// MPEG-TS timestamps turn every 2^33 / 90 kHz (26.5 h): each decoder counts its turns from its
/// own start, so one started after a turn is a turn away from one started before it.
const TURN: f64 = 8_589_934_592.0 / 90_000.0;

/// Our time, s: a steady clock (the system's time may be set, by NTP).
fn now_s() -> f64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// The moment played follows the source's clock by going a little faster or slower (at most
/// this much: unheard), never by a jump (a cut in the sound, a picture held or skipped): a
/// source's clock is never quite ours.
const PULL_MOST: f64 = 0.003;
/// How slowly it follows (s): a gap of this many seconds' worth of `PULL_MOST`… made up over it.
const PULL_TIME: f64 = 30.0;
/// A gap of more than this (s): the source started again, the moment played jumps to it.
const JUMP: f64 = 1.0;

impl Default for Inputs {
    fn default() -> Self {
        Inputs { clock: Mutex::default(), reference: Mutex::new(None), pictures: Mutex::new(VecDeque::new()), last_picture: AtomicI64::new(0), tracks: (0..TRACKS).map(|_| Mutex::new(Sound::default())).collect() }
    }
}

impl Inputs {
    /// A picture of the source (YUV 4:2:0 at the programme's size), of the moment `pts`.
    pub fn picture(&self, pts: f64, f: Vec<u8>) {
        let pts = self.same_turn(pts);
        *self.reference.lock().unwrap() = Some(pts);
        // Back after it was lost: its time again from what comes now (not from what was late).
        if self.lost() {
            *self.clock.lock().unwrap() = Clock::default();
        }
        self.clock.lock().unwrap().seen(pts, now_s());
        self.last_picture.store(now_ms(), Ordering::Relaxed);
        let mut q = self.pictures.lock().unwrap();
        match q.back().map(|b| pts - b.0) {
            // Well before the last one (the source started again): those waiting are of before.
            Some(d) if d < -1.0 => q.clear(),
            // The same moment, or just before (out of order): one is enough.
            Some(d) if d <= 0.0 => return,
            _ => {}
        }
        q.push_back((pts, f));
        // The decoder waits for room (`room`): more, only if it did not.
        while q.len() > PICTURES_MOST * 2 {
            q.pop_front();
        }
    }

    /// A timestamp on the same turn of the clock as the pictures'.
    fn same_turn(&self, pts: f64) -> f64 {
        match *self.reference.lock().unwrap() {
            Some(r) => pts + ((r - pts) / TURN).round() * TURN,
            None => pts,
        }
    }

    /// Sound of a track (interleaved stereo), from the moment `pts`.
    pub fn sound(&self, track: usize, pts: f64, samples: &[f32]) {
        let pts = self.same_turn(pts);
        // A source without pictures: its sound gives the time.
        if self.lost() {
            self.clock.lock().unwrap().seen(pts, now_s());
        }
        let Some(t) = self.tracks.get(track) else { return };
        let mut t = t.lock().unwrap();
        let end = t.head + (t.buf.len() / 2) as f64 / RATE as f64;
        let gap = pts - end;
        if t.buf.is_empty() || !(-0.1..1.0).contains(&gap) {
            // The first, or another timeline: from this one.
            t.buf.clear();
            t.head = pts;
        } else if gap > 0.01 {
            // A hole (the track stopped a moment): silence in it.
            let n = (gap * RATE as f64).round() as usize * 2;
            t.buf.extend(std::iter::repeat_n(0.0, n));
        }
        t.buf.extend(samples);
        // 30 s at most ahead.
        let most = RATE * 2 * 30;
        if t.buf.len() > most {
            let drop = (t.buf.len() - most) & !1;
            t.buf.drain(..drop);
            t.head += (drop / 2) as f64 / RATE as f64;
        }
    }

    /// Room for another picture (else the decoder waits).
    pub fn room(&self) -> bool {
        self.pictures.lock().unwrap().len() < PICTURES_MOST
    }

    /// The source's moment to play now (none before anything came).
    fn play(&self) -> Option<f64> {
        self.clock.lock().unwrap().play(now_s())
    }

    /// `n` stereo samples of a track from the moment `at`, read `ratio` times as fast (a little
    /// more or less than 1: the source's clock followed), silence where it has none.
    fn take(&self, track: usize, at: f64, n: usize, ratio: f64, out: &mut [f32]) -> bool {
        out.iter_mut().for_each(|x| *x = 0.0);
        let mut t = self.tracks[track].lock().unwrap();
        let frames = t.buf.len() / 2;
        if frames == 0 {
            return false;
        }
        // Where `at` is in what it has (samples; before its start: silence first).
        let x0 = (at - t.head) * RATE as f64;
        let mut any = false;
        for i in 0..n {
            let x = x0 + i as f64 * ratio;
            if x < 0.0 {
                continue;
            }
            let k = x.floor() as usize;
            if k + 1 >= frames {
                break;
            }
            let f = (x - k as f64) as f32;
            for c in 0..2 {
                out[2 * i + c] = t.buf[2 * k + c] * (1.0 - f) + t.buf[2 * (k + 1) + c] * f;
            }
            any = true;
        }
        // What was read (and what was too late) dropped: the next starts where this one ended.
        let end = x0 + n as f64 * ratio;
        let drop = if end > 0.0 { (end.floor() as usize).min(frames) } else { 0 };
        t.buf.drain(..drop * 2);
        t.head += drop as f64 / RATE as f64;
        any
    }

    /// A moment on the pictures' beat (the nearest), for pictures and frames to fall together.
    fn on_beat(&self, at: f64, period: f64) -> f64 {
        match *self.reference.lock().unwrap() {
            Some(r) => r + ((at - r) / period).round() * period,
            None => at,
        }
    }

    /// Pictures waiting, none of them yet due at `at`, and all the room taken or none shown
    /// for a while (`stalled`): by how much the first is ahead (the delay was taken too great:
    /// a picture that came very late, from before a loss).
    fn too_late(&self, at: f64, period: f64, stalled: bool) -> Option<f64> {
        let q = self.pictures.lock().unwrap();
        let first = q.front()?.0;
        // Due this frame or the next: on time (a source in bursts keeps them all waiting).
        ((q.len() >= PICTURES_MOST || stalled) && first > at + 1.5 * period).then_some(first - at)
    }

    /// Played sooner by `by` s.
    fn sooner(&self, by: f64) {
        self.clock.lock().unwrap().sooner(by);
    }

    /// The source's picture of the moment `at` (the last one up to it), the older ones dropped.
    fn picture_at(&self, at: f64) -> Option<Vec<u8>> {
        let mut q = self.pictures.lock().unwrap();
        let mut found = None;
        while q.front().is_some_and(|f| f.0 <= at) {
            found = q.pop_front().map(|f| f.1);
        }
        found
    }

    /// The source is lost: none of its pictures waits to be played, and none came for a second
    /// (a source in bursts has some waiting between them).
    pub fn lost(&self) -> bool {
        now_ms() - self.last_picture.load(Ordering::Relaxed) > LOST_MS && self.pictures.lock().unwrap().is_empty()
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
/// is not sent, picture and sound alike (they stay together), rather than piling up.
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
        // What went wrong in the last minute, said in the journal (where to look when the
        // programme does not flow): pictures held (none new from the source while it comes),
        // frames not sent (the encoder late), the clock started again (the mixer late), the
        // longest frame's making.
        let mut held = 0u32;
        let mut unsent = 0u32;
        let mut late = 0u32;
        let mut longest = Duration::ZERO;
        let mut said = Instant::now();
        // The moment played moved at once (pictures skipped or held, a cut in the sound).
        let mut jumps = 0u32;
        let mut jumped = 0.0f64;
        let mut pos: Option<f64> = None;
        let began = Instant::now();
        // When a picture of the source was last shown.
        let mut shown = Instant::now();
        let step = n as f64 / RATE as f64;
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
            // The source's moment to play: ours, on from frame to frame (the clock's estimate
            // followed when it moves away, not its every jitter).
            let p_secs = period.as_secs_f64();
            let mut ratio = 1.0;
            if let Some(p) = self.inputs.play() {
                pos = Some(match pos {
                    // Followed: a little faster when behind, slower when ahead.
                    Some(x) if (x - p).abs() < JUMP => {
                        ratio = 1.0 + ((p - x) / PULL_TIME).clamp(-PULL_MOST, PULL_MOST);
                        x
                    }
                    Some(x) => {
                        jumps += 1;
                        jumped += (p - x).abs();
                        tracing::info!("programme: the moment played jumps {} ms (the source's time moved), {} s after the start", ((p - x) * 1000.0) as i64, began.elapsed().as_secs());
                        self.inputs.on_beat(p, p_secs)
                    }
                    None => self.inputs.on_beat(p, p_secs),
                });
            }
            let stalled = ever && shown.elapsed() > Duration::from_millis(LOST_MS as u64);
            if let Some(by) = pos.and_then(|at| self.inputs.too_late(at, p_secs, stalled)) {
                self.inputs.sooner(by);
                pos = pos.map(|x| self.inputs.on_beat(x + by, p_secs));
                jumps += 1;
                jumped += by;
                tracing::info!("programme: the moment played jumps {} ms (pictures waiting, none due), {} s after the start", (by * 1000.0) as i64, began.elapsed().as_secs());
            }
            match pos.and_then(|at| self.inputs.picture_at(at + period.as_secs_f64() / 2.0)) {
                Some(f) if f.len() == yuv => {
                    last = f;
                    ever = true;
                    shown = Instant::now();
                }
                _ if ever && !self.inputs.lost() => held += 1,
                _ => {}
            }
            if ever && !self.pictured.load(Ordering::Relaxed) {
                self.pictured.store(true, Ordering::Relaxed);
            }
            // The slate once the source was seen and is lost (before its first picture: the dark
            // ground, the live waits for it).
            let pics = self.pics.lock().unwrap();
            let slate = ever && self.inputs.lost() && self.program.slate;
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
            let samples = sound.mix(&self.inputs, pos, n, ratio, &gains, &next_gains, &self.program.channels, (1.0 - k) as f32, (1.0 - k_end) as f32, master, next_master);
            gains = next_gains;
            master = next_master;
            pos = pos.map(|x| x + step * ratio);
            if !self.send(out, samples) {
                unsent += 1;
            }
            longest = longest.max(now.elapsed());
            if said.elapsed() >= Duration::from_secs(60) {
                if held > 0 || unsent > 0 || late > 0 || jumps > 0 {
                    tracing::info!("programme: the last minute, {held} pictures held (the source late), {unsent} frames not sent (the encoder late), {late} times late, {jumps} jumps ({} ms in all), {} ms at most for a frame", (jumped * 1000.0) as i64, longest.as_millis());
                }
                (held, unsent, late, longest, said, jumps, jumped) = (0, 0, 0, Duration::ZERO, Instant::now(), 0, 0.0);
            }
            // The next frame, on the clock (late by more than a second: the clock starts again).
            tick += 1;
            let due = start + period * tick as u32;
            let now = Instant::now();
            if now > due + Duration::from_secs(1) {
                late += 1;
                start = now;
                tick = 0;
            } else if due > now {
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
    /// `n` stereo samples from the source's moment `at`: each track (all of them read: what is
    /// late dropped), those on mixed at
    /// their gain (from `g0` to `g1` over the frame), the channels, the programme's level (from
    /// `v0` to `v1`: none under the image), the master, the limiter.
    #[allow(clippy::too_many_arguments)]
    fn mix(&mut self, inputs: &Inputs, at: Option<f64>, n: usize, ratio: f64, g0: &[f32], g1: &[f32], channels: &str, v0: f32, v1: f32, m0: f32, m1: f32) -> Vec<f32> {
        let mut out = vec![0f32; n * 2];
        self.take.resize(n * 2, 0.0);
        for k in 0..TRACKS {
            let got = at.is_some_and(|at| inputs.take(k, at, n, ratio, &mut self.take));
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
        // Track 0: 0.5 on both channels; track 1: 0.8 (off); 0.2 s of each, from the moment 10.
        let n = 1920;
        inputs.sound(0, 10.0, &vec![0.5; RATE / 5 * 2]);
        inputs.sound(1, 10.0, &vec![0.8; RATE / 5 * 2]);
        let mut st = Sound2::default();
        let g = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let out = st.mix(&inputs, Some(10.0), n, 1.0, &g, &g, "lr", 1.0, 1.0, 1.0, 1.0);
        assert!(out.iter().all(|x| (x - 0.5).abs() < 1e-6));
        // Both on, +6 dB: limited to -1 dBFS.
        let g = [2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let out = st.mix(&inputs, Some(10.04), n, 1.0, &g, &g, "lr", 1.0, 1.0, 1.0, 1.0);
        assert!(out.iter().all(|x| *x <= LIMIT + 1e-6));
        // Under the image: none.
        let out = st.mix(&inputs, Some(10.08), n, 1.0, &g, &g, "lr", 0.0, 0.0, 1.0, 1.0);
        assert!(out.iter().all(|x| *x == 0.0));
        // Nothing to play yet: silence.
        let out = st.mix(&inputs, None, n, 1.0, &g, &g, "lr", 1.0, 1.0, 1.0, 1.0);
        assert!(out.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn sound_at_its_moment() {
        let inputs = Inputs::default();
        let mut out = vec![1f32; 200];
        assert!(!inputs.take(0, 5.0, 100, 1.0, &mut out));
        assert!(out.iter().all(|x| *x == 0.0));
        // 0.1 s from the moment 5 (a ramp: each sample says its index).
        let s: Vec<f32> = (0..4800).flat_map(|i| [i as f32, i as f32]).collect();
        inputs.sound(0, 5.0, &s);
        // From 5.01: the first 480 dropped (late).
        assert!(inputs.take(0, 5.01, 100, 1.0, &mut out));
        assert_eq!((out[0], out[198]), (480.0, 579.0));
        // Then on, without a hole.
        assert!(inputs.take(0, 5.01 + 100.0 / RATE as f64, 100, 1.0, &mut out));
        assert_eq!(out[0], 580.0);
        // Ahead of what it has (from 4.999): silence first.
        let inputs = Inputs::default();
        inputs.sound(0, 5.0, &s);
        assert!(inputs.take(0, 5.0 - 48.0 / RATE as f64, 100, 1.0, &mut out));
        // 48 samples of silence (96 values), then the first ones: 0, 1, 2…
        assert_eq!((out[95], out[98], out[100]), (0.0, 1.0, 2.0));
        // A hole in what came: silence in it, the rest at its moment.
        let inputs = Inputs::default();
        inputs.sound(0, 1.0, &vec![0.5; 960]);
        inputs.sound(0, 1.02, &vec![0.7; 960]);
        let mut out = vec![0f32; 2880];
        inputs.take(0, 1.0, 1440, 1.0, &mut out);
        assert_eq!((out[0], out[958], out[960], out[1918], out[1920]), (0.5, 0.5, 0.0, 0.0, 0.7));
    }

    #[test]
    fn sound_read_a_little_faster_without_a_break() {
        let inputs = Inputs::default();
        // A ramp: each sample says its index.
        let s: Vec<f32> = (0..9600).flat_map(|i| [i as f32, i as f32]).collect();
        inputs.sound(0, 2.0, &s);
        let mut out = vec![0f32; 200];
        let ratio = 1.01;
        let mut at = 2.0;
        let mut last = -1.0f32;
        for _ in 0..20 {
            assert!(inputs.take(0, at, 100, ratio, &mut out));
            // Each sample a little further than the one before (1.01), from one call to the next too.
            for v in out.iter().step_by(2) {
                if last >= 0.0 {
                    assert!((v - last - 1.01).abs() < 0.02, "{last} → {v}");
                }
                last = *v;
            }
            at += 100.0 * ratio / RATE as f64;
        }
    }

    #[test]
    fn pictures_at_their_moment() {
        let inputs = Inputs::default();
        // Four pictures of 0.04 s, in a burst.
        for k in 0..4 {
            inputs.picture(100.0 + k as f64 * 0.04, vec![k as u8]);
        }
        // Played at their moment, one a frame, not all at once.
        assert_eq!(inputs.picture_at(100.0), Some(vec![0]));
        assert_eq!(inputs.picture_at(100.04), Some(vec![1]));
        assert_eq!(inputs.picture_at(100.05), None);
        // Late: the last up to the moment, those before it dropped.
        assert_eq!(inputs.picture_at(100.2), Some(vec![3]));
        // The moment: the latest after its timestamp, plus the margin.
        let play = inputs.play().unwrap();
        assert!((now_s() - play - DELAY - (now_s() - 100.0)).abs() < 1.0);
        // The same moment twice (timestamps said to the tenth of a second): one kept, the rest not
        // dropped.
        inputs.picture(100.4, vec![5]);
        inputs.picture(100.4, vec![6]);
        inputs.picture(100.44, vec![7]);
        assert_eq!(inputs.picture_at(100.4), Some(vec![5]));
        assert_eq!(inputs.picture_at(100.44), Some(vec![7]));
        // The source started again (timestamps back): from the new ones.
        inputs.picture(3.0, vec![9]);
        assert_eq!(inputs.picture_at(3.0), Some(vec![9]));
        // A sound decoder a turn of the clock away: on the pictures' turn.
        assert!((inputs.same_turn(3.5 - TURN) - 3.5).abs() < 1e-6);
        assert!((inputs.same_turn(3.5 + TURN) - 3.5).abs() < 1e-6);
        // All waiting, none due: the delay was too great.
        let inputs = Inputs::default();
        for k in 0..PICTURES_MOST {
            inputs.picture(50.0 + k as f64 * 0.04, vec![0]);
        }
        assert_eq!(inputs.too_late(49.0, 0.04, false), Some(1.0));
        assert_eq!(inputs.too_late(50.0, 0.04, false), None);
        // Due at the next frame: on time.
        assert_eq!(inputs.too_late(49.97, 0.04, false), None);
        // A few waiting, none shown for a while: the same.
        let inputs = Inputs::default();
        inputs.picture(80.0, vec![0]);
        assert_eq!(inputs.too_late(78.0, 0.04, false), None);
        assert_eq!(inputs.too_late(78.0, 0.04, true), Some(2.0));
    }

    #[test]
    fn the_latest_delay_of_the_last_30_s() {
        let mut c = Clock::default();
        // A segment of 6 s at once (its first picture 6 s late), then the pictures of the next.
        c.seen(0.0, 6.0);
        c.seen(5.96, 6.0);
        assert_eq!(c.play(6.0), Some(6.0 - 6.0 - DELAY));
        c.seen(6.0, 12.1);
        assert!((c.play(12.1).unwrap() - (12.1 - 6.1 - DELAY)).abs() < 1e-9);
        // Even for 30 s: then the delays since.
        c.seen(40.0, 41.0);
        assert!((c.play(41.0).unwrap() - (41.0 - 6.1 - DELAY)).abs() < 1e-9);
        c.seen(70.0, 71.0);
        assert!((c.play(71.0).unwrap() - (71.0 - 1.0 - DELAY)).abs() < 1e-9);
        // Another timeline: from it.
        c.seen(5.0, 200.0);
        assert!((c.play(200.0).unwrap() - (5.0 - DELAY)).abs() < 1e-9);
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
