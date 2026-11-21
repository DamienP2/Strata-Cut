//! A loop on disk: runs (one per FFmpeg process) made of segments.
//!
//! Every timestamp `t` is the source's own presentation time in seconds
//! (FFmpeg runs with `-copyts`), the same in the original segments and in the
//! proxies, so a frame has one `t` everywhere. `wall` is the server clock (ms)
//! when that frame arrived, anchored segment by segment.
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

/// An original segment (MPEG-TS, the stream as received).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Seg {
    pub n: u32,
    pub t0: f64,
    pub t1: f64,
    /// Server time of `t0`, ms.
    pub wall0: f64,
    pub bytes: u64,
}

impl Seg {
    pub fn wall1(&self) -> f64 {
        self.wall0 + (self.t1 - self.t0) * 1000.0
    }
}

/// A proxy segment (fragmented MP4 for the browser).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PSeg {
    pub n: u32,
    pub t0: f64,
    pub t1: f64,
    #[serde(default)]
    pub bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub interlaced: bool,
    pub audio_tracks: u32,
    /// Each sound track's language ("fra", "eng"…; "" when not said).
    #[serde(default)]
    pub audio_langs: Vec<String>,
    /// The subtitle tracks' codecs ("dvb_subtitle", "dvb_teletext"…).
    #[serde(default)]
    pub subtitles: Vec<String>,
}

impl MediaInfo {
    pub fn fps(&self) -> f64 {
        if self.fps_den == 0 { 25.0 } else { self.fps_num as f64 / self.fps_den as f64 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RunMeta {
    pub id: String,
    /// Server time when the run started, ms.
    pub started: i64,
    #[serde(default)]
    pub media: Option<MediaInfo>,
}

/// One proxy track: its fragments' times are `offset + tfdt / timescale`.
#[derive(Clone, Debug, Default)]
pub struct Track {
    pub offset: Option<f64>,
    pub timescale: u32,
    pub segs: VecDeque<PSeg>,
    /// Audio only: the sound level of each fragment, one byte per 10 ms (see `wave`).
    pub waves: std::collections::HashMap<u32, std::sync::Arc<Vec<u8>>>,
}

pub struct Run {
    pub meta: RunMeta,
    pub dir: PathBuf,
    pub master: VecDeque<Seg>,
    pub video: Track,
    pub audio: Track,
    /// FFmpeg is still writing it.
    pub live: bool,
}

pub const MASTER: &str = "master";
pub const VIDEO: &str = "video";
pub const AUDIO: &str = "audio";

impl Run {
    pub fn new(dir: PathBuf, meta: RunMeta) -> Self {
        Run { meta, dir, master: VecDeque::new(), video: Track::default(), audio: Track::default(), live: true }
    }

    pub fn track(&self, video: bool) -> &Track {
        if video { &self.video } else { &self.audio }
    }

    pub fn track_mut(&mut self, video: bool) -> &mut Track {
        if video { &mut self.video } else { &mut self.audio }
    }

    pub fn track_dir(&self, video: bool) -> PathBuf {
        self.dir.join(if video { VIDEO } else { AUDIO })
    }

    pub fn first_wall(&self) -> Option<f64> {
        self.master.front().map(|s| s.wall0)
    }
    pub fn last_wall(&self) -> Option<f64> {
        self.master.back().map(Seg::wall1)
    }

    /// Server time (ms) of the instant `t`.
    pub fn wall_of(&self, t: f64) -> Option<f64> {
        let i = self.master.partition_point(|s| s.t1 <= t);
        let s = self.master.get(i).or(self.master.back())?;
        Some(s.wall0 + (t - s.t0) * 1000.0)
    }

    /// The instant `t` shown at server time `wall` (ms).
    pub fn t_of(&self, wall: f64) -> Option<f64> {
        let i = self.master.partition_point(|s| s.wall1() <= wall);
        let s = self.master.get(i).or(self.master.back())?;
        Some(s.t0 + (wall - s.wall0) / 1000.0)
    }

    /// The original segments covering [t0, t1], if they follow each other without a hole.
    pub fn covering(&self, t0: f64, t1: f64) -> Option<Vec<Seg>> {
        let first = self.master.iter().position(|s| s.t1 > t0)?;
        if self.master[first].t0 > t0 + 0.001 {
            return None;
        }
        let mut out = Vec::new();
        let mut end = self.master[first].t0;
        for s in self.master.iter().skip(first) {
            if (s.t0 - end).abs() > 0.5 {
                return None; // a hole
            }
            out.push(s.clone());
            end = s.t1;
            if s.t1 >= t1 {
                return Some(out);
            }
        }
        None
    }

    /// What the run takes on disk: the original segments and the proxies.
    pub fn bytes(&self) -> u64 {
        self.master.iter().map(|s| s.bytes).sum::<u64>() + self.video.segs.iter().chain(self.audio.segs.iter()).map(|p| p.bytes).sum::<u64>()
    }

    pub fn master_path(&self, n: u32) -> PathBuf {
        self.dir.join(MASTER).join(format!("s{n:08}.ts"))
    }
    pub fn proxy_path(&self, video: bool, n: u32) -> PathBuf {
        self.track_dir(video).join(format!("p{n:08}.m4s"))
    }
    pub fn wave_path(&self, n: u32) -> PathBuf {
        self.track_dir(false).join(format!("w{n:08}.bin"))
    }

    pub fn append_master(&mut self, s: Seg) {
        append_line(&self.dir.join("master.jsonl"), &s);
        self.master.push_back(s);
    }

    pub fn append_proxy(&mut self, video: bool, s: PSeg) {
        append_line(&self.dir.join(if video { "video.jsonl" } else { "audio.jsonl" }), &s);
        self.track_mut(video).segs.push_back(s);
    }

    pub fn save_meta(&self) {
        let _ = crate::paths::write_json(&self.dir.join("run.json"), &self.meta, false);
    }

    /// Loads a finished run from its folder; None if it holds nothing.
    pub fn load(dir: &Path) -> Option<Run> {
        let meta: RunMeta = crate::paths::read_json(&dir.join("run.json"))?;
        let mut run = Run::new(dir.to_path_buf(), meta);
        run.live = false;
        run.master = read_lines::<Seg>(&dir.join("master.jsonl")).into_iter().filter(|s| run.master_path(s.n).exists()).collect();
        for video in [true, false] {
            let segs: VecDeque<PSeg> = read_lines::<PSeg>(&dir.join(if video { "video.jsonl" } else { "audio.jsonl" })).into_iter().filter(|s| run.proxy_path(video, s.n).exists()).collect();
            if let Ok(init) = std::fs::read(run.track_dir(video).join("init.mp4")) {
                if let Some((offset, ts)) = mp4::track_offset(&init) {
                    // On the recording's turn of 26 h 30 (see `same_turn`), its fragments too.
                    let near = run.master.front().map(|s| s.t0);
                    let shift = near.map_or(0.0, |near| super::same_turn(offset, near) - offset);
                    let t = run.track_mut(video);
                    t.offset = Some(offset + shift);
                    t.timescale = ts;
                    t.segs = segs
                        .into_iter()
                        .map(|mut s| {
                            if shift != 0.0 && (s.t0 - offset).abs() < super::PTS_WRAP / 2.0 {
                                s.t0 += shift;
                                s.t1 += shift;
                            }
                            s
                        })
                        .collect();
                }
            }
        }
        let waves: Vec<(u32, Vec<u8>)> = run.audio.segs.iter().filter_map(|s| std::fs::read(run.wave_path(s.n)).ok().map(|w| (s.n, w))).collect();
        for (n, w) in waves {
            run.audio.waves.insert(n, std::sync::Arc::new(w));
        }
        if run.master.is_empty() { None } else { Some(run) }
    }
}

fn append_line<T: Serialize>(path: &Path, v: &T) {
    let r = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| {
        let mut line = serde_json::to_vec(v).map_err(std::io::Error::other)?;
        line.push(b'\n');
        f.write_all(&line)
    });
    if let Err(e) = r {
        tracing::error!("{}: {e}", path.display());
    }
}

fn read_lines<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    let Ok(f) = std::fs::File::open(path) else { return Vec::new() };
    std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()).collect()
}

/// Rewrites a run's index files after old segments were removed.
pub fn rewrite_index(run: &Run) {
    let write = |name: &str, lines: Vec<String>| {
        let mut data = lines.join("\n");
        if !data.is_empty() {
            data.push('\n');
        }
        let _ = crate::paths::write_atomic(&run.dir.join(name), data.as_bytes(), false);
    };
    write("master.jsonl", run.master.iter().filter_map(|s| serde_json::to_string(s).ok()).collect());
    write("video.jsonl", run.video.segs.iter().filter_map(|s| serde_json::to_string(s).ok()).collect());
    write("audio.jsonl", run.audio.segs.iter().filter_map(|s| serde_json::to_string(s).ok()).collect());
}

/// The sound level drawn under the timeline: 100 values per second, the
/// peak of each 10 ms in dBFS mapped to 0 (−60 dB or less) … 255 (0 dB).
pub mod wave {
    use std::path::Path;

    pub const PER_SECOND: f64 = 100.0;

    /// Proxy audio fragments (decoded mono, 8 kHz) measured. Several fragments that follow each other, decoded by one FFmpeg (one per fragment loads
    /// a server receiving many sources: their fragments last a second), then cut by their
    /// durations (s). A single fragment: all its levels, whatever its duration.
    pub fn compute_many(ffmpeg: &str, init: &Path, segs: &[(&Path, f64)]) -> Option<Vec<Vec<u8>>> {
        if segs.is_empty() {
            return None;
        }
        let mut input = format!("concat:{}", init.display());
        for (seg, _) in segs {
            input.push('|');
            input.push_str(&seg.display().to_string());
        }
        let mut cmd = std::process::Command::new(ffmpeg);
        super::super::background_std(&mut cmd);
        let out = cmd
            .args(["-v", "error", "-i", &input, "-map", "0:a:0", "-ac", "1", "-ar", "8000", "-f", "s16le", "pipe:1"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if out.stdout.len() < 2 {
            return None;
        }
        Some(split(levels(&out.stdout), segs.iter().map(|s| s.1)))
    }

    /// The levels of several fragments, each its share by its duration (the last takes the rest).
    pub fn split(all: Vec<u8>, durations: impl ExactSizeIterator<Item = f64>) -> Vec<Vec<u8>> {
        let n = durations.len();
        let mut out = Vec::with_capacity(n);
        let (mut at, mut t) = (0usize, 0.0f64);
        for (i, d) in durations.enumerate() {
            t += d;
            let end = if i + 1 == n { all.len() } else { ((t * PER_SECOND).round() as usize).clamp(at, all.len()) };
            out.push(all[at..end].to_vec());
            at = end;
        }
        out
    }

    /// 16-bit little-endian samples at 8 kHz → one level per 80 samples.
    pub fn levels(pcm: &[u8]) -> Vec<u8> {
        pcm.chunks(160)
            .map(|c| {
                let peak = c.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b).unsigned_abs()).max().unwrap_or(0);
                level(peak as f64 / 32768.0)
            })
            .collect()
    }

    pub fn level(peak: f64) -> u8 {
        if peak <= 0.0 {
            return 0;
        }
        let db = 20.0 * peak.log10();
        (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 255.0).round() as u8
    }
}

/// Just enough ISO-BMFF reading for the proxies.
pub mod mp4 {
    fn be32(b: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
    }
    fn be64(b: &[u8], at: usize) -> Option<u64> {
        Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
    }

    /// The boxes directly inside `b[start..end]`: (type, payload start, payload end).
    fn children(b: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
        let mut out = Vec::new();
        let mut at = start;
        while at + 8 <= end {
            let Some(size32) = be32(b, at) else { break };
            let typ: [u8; 4] = b[at + 4..at + 8].try_into().unwrap();
            let (size, hdr) = match size32 {
                1 => match be64(b, at + 8) {
                    Some(s) => (s as usize, 16),
                    None => break,
                },
                0 => (end - at, 8),
                s => (s as usize, 8),
            };
            if size < hdr || at + size > end {
                break;
            }
            out.push((typ, at + hdr, at + size));
            at += size;
        }
        out
    }

    fn find(b: &[u8], start: usize, end: usize, path: &[&[u8; 4]]) -> Option<(usize, usize)> {
        let (first, rest) = path.split_first()?;
        let (_, s, e) = children(b, start, end).into_iter().find(|(t, _, _)| t == *first)?;
        if rest.is_empty() { Some((s, e)) } else { find(b, s, e, rest) }
    }

    /// From an init segment: (time of media time 0 in seconds, media timescale),
    /// read from the edit list FFmpeg writes instead of shifting the fragments.
    pub fn track_offset(init: &[u8]) -> Option<(f64, u32)> {
        let (ms, _) = find(init, 0, init.len(), &[b"moov", b"mvhd"])?;
        let movie_ts = if init[ms] == 1 { be32(init, ms + 20)? } else { be32(init, ms + 12)? };
        let (ts_start, ts_end) = find(init, 0, init.len(), &[b"moov", b"trak"])?;
        let (md, _) = find(init, ts_start, ts_end, &[b"mdia", b"mdhd"])?;
        let media_ts = if init[md] == 1 { be32(init, md + 20)? } else { be32(init, md + 12)? };
        if movie_ts == 0 || media_ts == 0 {
            return None;
        }
        let mut offset = 0.0;
        if let Some((es, _)) = find(init, ts_start, ts_end, &[b"edts", b"elst"]) {
            let v1 = init[es] == 1;
            let count = be32(init, es + 4)?;
            let mut at = es + 8;
            for _ in 0..count {
                let (dur, media_time, len) = if v1 { (be64(init, at)? as f64, be64(init, at + 8)? as i64, 20) } else { (be32(init, at)? as f64, be32(init, at + 4)? as i32 as i64, 12) };
                if media_time == -1 {
                    offset += dur / movie_ts as f64; // empty edit: the track starts later
                } else {
                    offset -= media_time as f64 / media_ts as f64;
                    break;
                }
                at += len;
            }
        }
        Some((offset, media_ts))
    }

    /// The decode time of a fragment's first sample (first `tfdt`), in its timescale.
    pub fn first_tfdt(seg: &[u8]) -> Option<u64> {
        let (s, _) = find(seg, 0, seg.len(), &[b"moof", b"traf", b"tfdt"])?;
        if seg[s] == 1 { be64(seg, s + 4) } else { be32(seg, s + 4).map(u64::from) }
    }

    /// The codec string of an init segment for MediaSource (`avc1.4d401f`, `mp4a.40.2`).
    pub fn codec(init: &[u8]) -> Option<String> {
        let (ts, te) = find(init, 0, init.len(), &[b"moov", b"trak"])?;
        let (ss, se) = find(init, ts, te, &[b"mdia", b"minf", b"stbl", b"stsd"])?;
        let entries = children(init, ss + 8, se);
        let (typ, es, ee) = *entries.first()?;
        match &typ {
            b"avc1" | b"avc3" => {
                // VisualSampleEntry: 78 bytes before its child boxes.
                let (cs, _) = children(init, es + 78, ee).into_iter().find(|(t, _, _)| t == b"avcC").map(|(_, s, e)| (s, e))?;
                Some(format!("avc1.{:02x}{:02x}{:02x}", init.get(cs + 1)?, init.get(cs + 2)?, init.get(cs + 3)?))
            }
            b"mp4a" => Some("mp4a.40.2".into()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_split_by_durations() {
        // 3 fragments of 1 s, 1.02 s and the rest: 100 levels a second.
        let parts = wave::split((0..310u32).map(|x| x as u8).collect(), [1.0, 1.02, 1.0].into_iter());
        assert_eq!(parts.iter().map(Vec::len).collect::<Vec<_>>(), vec![100, 102, 108]);
        assert_eq!(parts[1][0], 100);
        // One fragment: all its levels.
        assert_eq!(wave::split(vec![1; 40], [0.0].into_iter())[0].len(), 40);
    }

    fn run_with(segs: &[(f64, f64, f64)]) -> Run {
        let mut r = Run::new(PathBuf::from("/nonexistent"), RunMeta::default());
        for (i, (t0, t1, w)) in segs.iter().enumerate() {
            r.master.push_back(Seg { n: i as u32, t0: *t0, t1: *t1, wall0: *w, bytes: 1 });
        }
        r
    }

    #[test]
    fn wall_and_t_are_inverse() {
        let r = run_with(&[(100.0, 106.0, 1_000_000.0), (106.0, 112.0, 1_006_000.5)]);
        assert_eq!(r.wall_of(103.0), Some(1_003_000.0));
        assert_eq!(r.wall_of(107.0), Some(1_007_000.5));
        assert_eq!(r.t_of(1_007_000.5), Some(107.0));
        assert_eq!(r.t_of(1_002_000.0), Some(102.0));
    }

    #[test]
    fn wave_levels() {
        assert_eq!(wave::level(1.0), 255);
        assert_eq!(wave::level(0.001), 0); // -60 dB
        assert_eq!(wave::level(0.0), 0);
        // 160 bytes = 80 samples = 10 ms; a full-scale sample in the second slice.
        let mut pcm = vec![0u8; 320];
        pcm[200..202].copy_from_slice(&i16::MIN.to_le_bytes());
        assert_eq!(wave::levels(&pcm), vec![0, 255]);
    }

    #[test]
    fn covering_refuses_holes() {
        let r = run_with(&[(0.0, 6.0, 0.0), (6.0, 12.0, 6000.0), (20.0, 26.0, 20000.0)]);
        assert_eq!(r.covering(1.0, 11.0).map(|v| v.len()), Some(2));
        assert_eq!(r.covering(7.0, 8.0).map(|v| v.len()), Some(1));
        assert!(r.covering(5.0, 21.0).is_none());
        assert!(r.covering(-1.0, 2.0).is_none());
        assert!(r.covering(24.0, 30.0).is_none());
    }
}
