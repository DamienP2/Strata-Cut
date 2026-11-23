//! Streamer: the levels of a source's sound, every track and each of its channels (a mono track:
//! one, a 5.1 track: six…), 10 times a second, for the Streamer's meters. Measured by an FFmpeg of
//! its own on the source's relay, while a page asks for them; a few minutes after the last asking,
//! it stops (a page opened again finds them at once).
//!
//! A measure is kept with its timestamp (the source's own, as the preview's): the page asks for
//! those of the moment its picture shows. A timestamp becomes a server time by the least delay
//! seen to the server's clock over the last half minute. The first seconds, before the moment the
//! picture shows was measured: the newest.
use std::{
    collections::{HashMap, VecDeque},
    process::Stdio,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use tokio::{io::AsyncBufReadExt, process::Command};

use crate::{
    engine::{free_udp, in_segments, rand_key, wait_picks, Picks, Untap, Watch},
    recorder::{now_ms, Manager},
};

/// Asked for no more for this long: the measures stop.
const IDLE_MS: i64 = 300_000;

/// The meters of the sources whose levels are asked for.
pub struct Meters {
    recorders: Arc<Manager>,
    all: Mutex<HashMap<String, Arc<Meter>>>,
}

struct Meter {
    peaks: Mutex<Peaks>,
    /// When its levels were last asked for (server ms).
    asked: AtomicI64,
}

impl Meters {
    pub fn new(recorders: Arc<Manager>) -> Arc<Meters> {
        Arc::new(Meters { recorders, all: Mutex::new(HashMap::new()) })
    }

    /// The peaks of each of the source's `tracks` tracks, each of its channels (dB, -90: silence;
    /// none yet: none), at `at` (server ms; none: the newest). Measured from now on, if they were
    /// not.
    pub fn read(self: &Arc<Self>, source: &str, tracks: usize, at: Option<i64>) -> Vec<Vec<f32>> {
        let m = {
            let mut all = self.all.lock().unwrap();
            all.entry(source.to_string())
                .or_insert_with(|| {
                    let m = Arc::new(Meter { peaks: Mutex::new(Peaks::default()), asked: AtomicI64::new(now_ms()) });
                    tokio::spawn(self.clone().measure(source.to_string(), m.clone()));
                    m
                })
                .clone()
        };
        m.asked.store(now_ms(), Ordering::Relaxed);
        let p = m.peaks.lock().unwrap();
        let at = at.or_else(|| p.newest_at()).unwrap_or_else(now_ms);
        (0..tracks).map(|k| p.read(k, at)).collect()
    }

    /// Measures while asked for: an FFmpeg on the source's relay, made again when it stops or
    /// when a track begins to send.
    async fn measure(self: Arc<Self>, source: String, m: Arc<Meter>) {
        let idle = || now_ms() - m.asked.load(Ordering::Relaxed) > IDLE_MS;
        loop {
            let Some(rec) = self.recorders.get(&source).filter(|_| !idle()) else { break };
            if !rec.flowing() {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            let Ok(watch) = Watch::open() else {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            };
            let key = rand_key();
            rec.taps.lock().unwrap().push((key, watch.port));
            let _untap = Untap(rec.clone(), key);
            let guess = rec.status.lock().unwrap().media.as_ref().map_or(0, |x| x.audio_tracks as usize);
            let picks = wait_picks(&watch, &rec, guess).await;
            let Ok(port) = free_udp() else { continue };
            let segments = in_segments(&rec);
            let mut cmd = Command::new(&self.recorders.ffmpeg);
            cmd.args(meter_args(&picks, port, segments)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
            let Ok(mut child) = cmd.spawn() else {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            };
            let key = rand_key();
            rec.taps.lock().unwrap().push((key, port));
            let _untap2 = Untap(rec.clone(), key);
            let mut lines = child.stderr.take().map(|e| tokio::io::BufReader::new(e).lines());
            let mut look = tokio::time::interval(Duration::from_secs(1));
            let mut looks = 0u32;
            loop {
                tokio::select! {
                    _ = child.wait() => break,
                    _ = look.tick() => {
                        looks += 1;
                        // Not asked for any more; or a track that sent nothing sends now: again.
                        let more = looks.is_multiple_of(5) && matches!((&picks, watch.picks(if segments { 30_000 } else { 2_000 })), (Picks::Known { tracks: a, .. }, Some(Picks::Known { tracks: b, .. })) if b.iter().any(|t| !a.contains(t)));
                        if idle() || more {
                            break;
                        }
                    }
                    line = async { match lines.as_mut() { Some(l) => l.next_line().await, None => std::future::pending().await } } => {
                        let Ok(Some(l)) = line else { lines = None; continue };
                        let now = now_ms();
                        if let Some((k, ch, db)) = level_line(&l) {
                            m.peaks.lock().unwrap().measured(k, ch, db);
                        } else if let Some((k, pts)) = level_moment(&l) {
                            m.peaks.lock().unwrap().moment(k, pts, now);
                        }
                    }
                }
            }
            let _ = child.kill().await;
            if !idle() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        let mut all = self.all.lock().unwrap();
        if all.get(&source).is_some_and(|x| Arc::ptr_eq(x, &m)) {
            all.remove(&source);
        }
    }
}

/// The meter: each track's peaks, each channel, every 0.1 s (`astats@m<track>`), the timestamps in
/// microseconds (`asettb=AVTB`: `pts_time` has 6 digits only before FFmpeg 7), printed.
fn meter_args(picks: &Picks, port: u16, segments: bool) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let wait = if segments { 30_000_000 } else { 5_000_000 };
    let mut a = s(&["-hide_banner", "-nostdin", "-nostats", "-loglevel", "info", "-copyts", "-thread_queue_size", "4096", "-analyzeduration", "300000", "-probesize", "1000000", "-f", "mpegts", "-i"]);
    a.push(format!("udp://127.0.0.1:{port}?fifo_size=250000&overrun_nonfatal=1&buffer_size=16777216&timeout={wait}"));
    for (k, spec) in picks.tracks() {
        a.extend(s(&["-map", &spec, "-vn", "-af"]));
        a.push(format!("asetnsamples=n=4800,astats@m{k}=metadata=1:reset=1:measure_overall=none:measure_perchannel=Peak_level,asettb=AVTB,ametadata@m{k}=mode=print"));
        a.extend(s(&["-f", "null", "-"]));
    }
    a
}

/// The track of a meter's line (`[ametadata@m<track> @ 0x…]`).
fn track_of(line: &str) -> Option<usize> {
    let rest = line.split_once("[ametadata@m")?.1;
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

/// A measure's timestamp line (`frame:.. pts:<µs> pts_time:..`): (track, ms).
fn level_moment(line: &str) -> Option<(usize, i64)> {
    let k = track_of(line)?;
    let us: i64 = line.split_once(" pts:")?.1.split_whitespace().next()?.parse().ok()?;
    Some((k, us / 1000))
}

/// A measure's level line (`lavfi.astats.<channel>.Peak_level=<dB>`): (track, channel from 0,
/// dB; silence at -90).
fn level_line(line: &str) -> Option<(usize, usize, f32)> {
    let k = track_of(line)?;
    let (ch, v) = line.split_once("lavfi.astats.")?.1.split_once(".Peak_level=")?;
    let ch: usize = ch.parse().ok()?;
    let db = v.trim().parse::<f32>().unwrap_or(-90.0);
    (1..=64).contains(&ch).then_some((k, ch - 1, if db.is_finite() { db.max(-90.0) } else { -90.0 }))
}

/// A measure being read: its timestamp and when it came (ms), and its channels read so far.
type Reading = (Option<(i64, i64)>, Vec<f32>);

/// The measures of a source: each track's, with their timestamp.
#[derive(Default)]
struct Peaks {
    /// Each track's measures: (their timestamp, ms; each channel's peak).
    kept: Vec<VecDeque<(i64, Vec<f32>)>>,
    /// The measure being read, of each track: its timestamp and when it came (ms), and the
    /// channels read.
    reading: Vec<Reading>,
    /// The delays (server ms minus timestamp) of the last measures, with their timestamp.
    delays: VecDeque<(i64, i64)>,
    /// The newest timestamp (ms).
    newest: Option<i64>,
}

impl Peaks {
    /// How long the measures are kept (ms): more than the picture's delay may be.
    const KEEP: i64 = 70_000;
    /// The delays looked at (ms of timestamps).
    const DELAYS: i64 = 30_000;

    fn grow(&mut self, track: usize) {
        if self.kept.len() <= track {
            self.kept.resize_with(track + 1, VecDeque::new);
            self.reading.resize_with(track + 1, Default::default);
        }
    }

    /// A measure's timestamp (ms), its line before its levels: the one before it is complete.
    fn moment(&mut self, track: usize, pts_ms: i64, now: i64) {
        self.grow(track);
        let (was, ch) = std::mem::replace(&mut self.reading[track], (Some((pts_ms, now)), Vec::new()));
        let Some((pts, now)) = was.filter(|_| !ch.is_empty()) else { return };
        // The timestamps jumped (back, or far ahead): what was kept is of another timeline.
        if self.newest.is_some_and(|n| pts < n - 2_000 || pts > n + 20_000) {
            self.kept.iter_mut().for_each(VecDeque::clear);
            self.delays.clear();
        }
        self.newest = Some(self.newest.map_or(pts, |n| n.max(pts)));
        self.delays.push_back((pts, now - pts));
        while self.delays.front().is_some_and(|x| x.0 < pts - Self::DELAYS) {
            self.delays.pop_front();
        }
        let q = &mut self.kept[track];
        q.push_back((pts, ch));
        while q.front().is_some_and(|x| x.0 < pts - Self::KEEP) {
            q.pop_front();
        }
    }

    fn measured(&mut self, track: usize, ch: usize, db: f32) {
        self.grow(track);
        let r = &mut self.reading[track].1;
        if r.len() <= ch {
            r.resize(ch + 1, -90.0);
        }
        r[ch] = db;
    }

    /// The server time of the newest measure.
    fn newest_at(&self) -> Option<i64> {
        Some(self.newest? + self.delays.iter().map(|x| x.1).min()?)
    }

    /// A track's peaks of the moment `at` (server ms): the highest of the quarter second before,
    /// each channel (none measured: none).
    fn read(&self, track: usize, at: i64) -> Vec<f32> {
        let Some(q) = self.kept.get(track) else { return Vec::new() };
        let mut p = vec![-90.0f32; q.back().map_or(0, |x| x.1.len())];
        let Some(off) = self.delays.iter().map(|x| x.1).min() else { return p };
        let at = at - off;
        // Before the first measured (the measures just began): the newest.
        if q.front().is_some_and(|x| at < x.0) {
            return q.back().map(|x| x.1.clone()).unwrap_or(p);
        }
        for (t, v) in q.iter().rev() {
            if *t > at {
                continue;
            }
            if *t <= at - 250 {
                break;
            }
            for (a, b) in p.iter_mut().zip(v) {
                *a = a.max(*b);
            }
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_read() {
        assert_eq!(level_moment("[ametadata@m12 @ 0x5] frame:3    pts:80001450022 pts_time:80001.5"), Some((12, 80_001_450)));
        assert_eq!(level_line("[ametadata@m3 @ 0x55c1] lavfi.astats.6.Peak_level=-24.5"), Some((3, 5, -24.5)));
        assert_eq!(level_line("[ametadata@m0 @ 0x55c1] lavfi.astats.1.Peak_level=-inf"), Some((0, 0, -90.0)));
        assert_eq!(level_line("[ametadata@lv0 @ 0x55c1] lavfi.astats.1.Peak_level=-3"), None);
        let a = meter_args(&Picks::Known { video: true, tracks: vec![(0, 0x101), (5, 0x106)] }, 7000, false);
        assert!(a.windows(2).any(|w| w == ["-map", "0:i:262"]) && a.iter().any(|x| x.contains("astats@m5") && x.contains("ametadata@m5")));
    }

    #[test]
    fn peaks_of_each_channel_at_a_moment() {
        let mut p = Peaks::default();
        // Track 1, a 5.1: six channels; read at the next moment.
        for (i, t) in [1000i64, 1100, 1200, 1300].iter().enumerate() {
            p.moment(1, *t, 5000 + *t);
            for ch in 0..6 {
                p.measured(1, ch, -60.0 + i as f32 * 10.0 + ch as f32);
            }
        }
        p.moment(1, 1400, 6400);
        assert_eq!(p.read(0, 6300), Vec::<f32>::new());
        // At 1300 (+5000): the highest of 1100..1300.
        assert_eq!(p.read(1, 6300), vec![-30.0, -29.0, -28.0, -27.0, -26.0, -25.0]);
        assert_eq!(p.newest_at(), Some(6300));
        // Long after: silent, its channels kept.
        assert_eq!(p.read(1, 60_000), vec![-90.0; 6]);
        // Before the first measured: the newest.
        assert_eq!(p.read(1, 2000), vec![-30.0, -29.0, -28.0, -27.0, -26.0, -25.0]);
    }
}
