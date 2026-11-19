//! HLS: a master playlist offers several streams (its variants): the same in several
//! qualities, or different ones (cameras, channels). FFmpeg opens each variant as a program,
//! numbered in the playlist's order (with the sounds it refers to); the recorder reads only the
//! program of the stream chosen, its other variants are not downloaded.

use std::time::Duration;

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Variant {
    /// Its playlist's address as written (without its query: tokens change), which names it.
    pub uri: String,
    /// Its name, when the playlist gives one (NAME, or its video's rendition).
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Bits per second, as the playlist says.
    pub bandwidth: u64,
    pub fps: f64,
}

/// The variants of a master playlist, in its order (none for a media playlist).
pub fn variants(playlist: &str) -> Vec<Variant> {
    let lines: Vec<&str> = playlist.lines().map(str::trim).collect();
    // The video renditions' names, by group (a camera's name, for instance).
    let video_names: Vec<(String, String)> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("#EXT-X-MEDIA:"))
        .filter(|a| attr(a, "TYPE").is_some_and(|t| t.eq_ignore_ascii_case("VIDEO")))
        .filter_map(|a| Some((attr(a, "GROUP-ID")?.to_string(), attr(a, "NAME")?.to_string())))
        .collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let Some(a) = l.strip_prefix("#EXT-X-STREAM-INF:") else { continue };
        let uri = lines[i + 1..].iter().find(|x| !x.is_empty() && !x.starts_with('#')).map(|x| x.split('?').next().unwrap_or(x).to_string()).unwrap_or_default();
        let (width, height) = attr(a, "RESOLUTION").and_then(|r| r.split_once(['x', 'X'])).map(|(w, h)| (w.parse().unwrap_or(0), h.parse().unwrap_or(0))).unwrap_or((0, 0));
        let name = attr(a, "NAME").map(str::to_string).or_else(|| attr(a, "VIDEO").and_then(|g| video_names.iter().find(|(id, _)| id == g)).map(|(_, n)| n.clone())).unwrap_or_default();
        out.push(Variant { uri, name, width, height, bandwidth: attr(a, "BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0), fps: attr(a, "FRAME-RATE").and_then(|f| f.parse().ok()).unwrap_or(0.0) });
    }
    out
}

/// An attribute of a tag's list (`KEY=value,KEY="a,b"`), without its quotes.
fn attr<'a>(list: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = list;
    while !rest.is_empty() {
        let (k, after) = rest.split_once('=')?;
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            let end = quoted.find('"').unwrap_or(quoted.len());
            (&quoted[..end], quoted.get(end + 1..).unwrap_or("").trim_start_matches(','))
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (&after[..end], after.get(end + 1..).unwrap_or(""))
        };
        if k.trim().eq_ignore_ascii_case(key) {
            return Some(value.trim());
        }
        rest = next;
    }
    None
}

/// The variant to read: the one chosen (named by its address), else (none chosen, or no
/// longer offered) the best picture. None without variants.
pub fn pick(variants: &[Variant], chosen: &str) -> Option<usize> {
    if !chosen.is_empty() {
        if let Some(i) = variants.iter().position(|v| v.uri == chosen) {
            return Some(i);
        }
    }
    variants.iter().enumerate().max_by_key(|(_, v)| (v.height, v.bandwidth)).map(|(i, _)| i)
}

/// The playlist at this address (1 MB at most).
pub async fn fetch(url: &str) -> Result<String, String> {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        let mut res = crate::update::http_agent(5, Duration::from_secs(15)).get(&url).call().map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("{}", res.status()));
        }
        res.body_mut().with_config().limit(1_000_000).read_to_string().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The variants offered at this address and the one to read (None: a media playlist, a
/// single variant, or a playlist that cannot be read here: FFmpeg's choice).
pub async fn choose(url: &str, chosen: &str) -> (Vec<Variant>, Option<usize>) {
    let Ok(text) = fetch(url).await else { return (Vec::new(), None) };
    let v = variants(&text);
    let program = if v.len() > 1 { pick(&v, chosen) } else { None };
    if program.is_some() && !chosen.is_empty() && v.iter().all(|x| x.uri != chosen) {
        tracing::warn!("hls: the stream chosen ({chosen}) is no longer offered by {url}: the best one is read");
    }
    (v, program)
}

/// FFmpeg's description of one program of its input (its streams), out of the whole.
pub fn program_text(text: &str, program: usize) -> String {
    let head = format!("Program {program}");
    let mut out = String::new();
    let mut inside = false;
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with("Program ") || t.starts_with("No Program") {
            inside = t == head;
        } else if t.starts_with("Output #") || t.starts_with("Stream mapping") {
            inside = false;
        }
        if inside {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: &str = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"fr\",DEFAULT=YES,URI=\"a.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1400000,RESOLUTION=842x480,CODECS=\"avc1.4d401f,mp4a.40.2\",FRAME-RATE=25,AUDIO=\"aac\"\n480p.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,FRAME-RATE=50.000,CODECS=\"avc1.640028,mp4a.40.2\"\n1080p.m3u8\n#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=200000,RESOLUTION=1920x1080,URI=\"if.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,NAME=\"Plateau\"\n720p.m3u8?token=1\n";

    #[test]
    fn reads_the_variants() {
        let v = variants(MASTER);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], Variant { uri: "480p.m3u8".into(), name: String::new(), width: 842, height: 480, bandwidth: 1_400_000, fps: 25.0 });
        assert_eq!((v[1].height, v[1].fps, v[1].uri.as_str()), (1080, 50.0, "1080p.m3u8"));
        assert_eq!((v[2].bandwidth, v[2].uri.as_str(), v[2].name.as_str()), (2_800_000, "720p.m3u8", "Plateau"));
        assert!(variants("#EXTM3U\n#EXTINF:6,\ns1.ts\n").is_empty());
    }

    #[test]
    fn picks_a_stream() {
        let v = variants(MASTER);
        assert_eq!(pick(&v, ""), Some(1)); // the best picture
        assert_eq!(pick(&v, "720p.m3u8"), Some(2));
        assert_eq!(pick(&v, "gone.m3u8"), Some(1));
        assert_eq!(pick(&[], "720p.m3u8"), None);
    }

    #[test]
    fn one_program_described() {
        let text = "Input #0, hls, from 'x':\n  Duration: N/A\n  Program 0\n    Metadata:\n      variant_bitrate : 1400000\n  Stream #0:0: Video: h264, 842x480, 25 fps\n  Stream #0:1: Audio: aac\n  Program 1\n  Stream #0:2: Video: h264, 1920x1080, 50 fps\n  Stream #0:1: Audio: aac\n";
        let p = program_text(text, 1);
        assert!(p.contains("1920x1080") && p.contains("Audio") && !p.contains("842x480"));
    }
}
