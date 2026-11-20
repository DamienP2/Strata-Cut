//! A test pattern as a source (Système › Sources: "Mire"): bars (SMPTE HD, EBU 75 %) or an
//! animated pattern, in the format chosen, with something moving, a flash and a beep together
//! once a second (lip sync), the source's name and format, a clock to the frame, and the
//! source's logo. FFmpeg makes it and encodes it (H.264 / AAC, MPEG-TS) into the receiver.
use std::{path::Path, process::Stdio};

use super::RecorderConfig;

/// The formats offered, as named elsewhere ("1080i50": 25 frames a second, interlaced).
pub const FORMATS: [&str; 12] = ["2160p50", "2160p25", "1080p50", "1080p25", "1080i50", "1080p5994", "1080p2997", "1080i5994", "720p50", "720p5994", "576i50", "480i5994"];

/// The patterns: SMPTE HD bars, EBU bars, an animated pattern.
pub const STYLES: [&str; 4] = ["smpte", "ebu", "animated", "lipsync"];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Format {
    pub width: u32,
    pub height: u32,
    pub interlaced: bool,
    /// Frames a second, as a fraction.
    pub num: u32,
    pub den: u32,
}

impl Format {
    pub fn parse(name: &str) -> Option<Format> {
        if !FORMATS.contains(&name) {
            return None;
        }
        let at = name.find(['i', 'p'])?;
        let height: u32 = name[..at].parse().ok()?;
        let interlaced = &name[at..at + 1] == "i";
        let rate = &name[at + 1..];
        // "5994" is 59.94 (fields for an interlaced one), "50" is 50.
        let (mut num, mut den) = if rate.len() == 4 { (rate.parse::<u32>().ok()? * 10 + 4, 1001) } else { (rate.parse::<u32>().ok()? * 1000, 1000) };
        if rate.len() == 4 {
            num = ((num as f64) / 1000.0).round() as u32 * 1000; // 59944 → 60000
        }
        if interlaced {
            num /= 2;
        }
        let g = gcd(num, den);
        (num, den) = (num / g, den / g);
        let width = match height {
            2160 => 3840,
            1080 => 1920,
            720 => 1280,
            _ => 720,
        };
        Some(Format { width, height, interlaced, num, den })
    }
    pub fn fps(&self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

pub fn validate(c: &RecorderConfig) -> Result<(), String> {
    if Format::parse(&c.video_mode).is_none() {
        return Err("choose the format of the test pattern".into());
    }
    if !STYLES.contains(&c.pattern.as_str()) {
        return Err("choose the test pattern".into());
    }
    if !(1.0..=200.0).contains(&c.bitrate) {
        return Err("the bitrate is 1 to 200 mb/s".into());
    }
    Ok(())
}

/// The text written on the pattern (in a file: no escaping in the filter).
fn text_file(data_dir: &Path, c: &RecorderConfig) -> std::path::PathBuf {
    let dir = data_dir.join("patterns");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}.txt", if c.id.is_empty() { "probe" } else { &c.id }));
    let _ = std::fs::write(&path, format!("{}  ·  {}", c.name.trim(), c.video_mode));
    path
}

/// The arguments of the FFmpeg making the pattern, its MPEG-TS on its output.
pub fn args(data_dir: &Path, c: &RecorderConfig) -> Vec<String> {
    let f = Format::parse(&c.video_mode).unwrap_or(Format { width: 1920, height: 1080, interlaced: true, num: 25, den: 1 });
    let (w, h) = (f.width, f.height);
    let rate = format!("{}/{}", f.num, f.den);
    let fps = f.fps().round().max(1.0) as u32;
    let base = match c.pattern.as_str() {
        "ebu" => format!("pal75bars=size={w}x{h}:rate={rate}"),
        "animated" => format!("testsrc2=size={w}x{h}:rate={rate}"),
        "lipsync" => format!("color=c=0x15161c:size={w}x{h}:rate={rate}"),
        _ => format!("smptehdbars=size={w}x{h}:rate={rate}"),
    };
    // A beep of one frame, once a second, on both channels: with the flash. Half a second after
    // the key frames (one a second from the first frame, as the preview's): the stills of the
    // timelines, taken on key frames, never show the flash (a white picture with the lip sync one).
    let beep_len = 1.0 / f.fps();
    let phase = 1.0 - (fps / 2) as f64 / f.fps() + 0.0005;
    let beep = format!("if(lt(mod(t+{phase:.5}\\,1)\\,{beep_len:.4})\\,0.5*sin(2*PI*1000*t)\\,0)");
    let mut a: Vec<String> = ["-hide_banner", "-nostdin", "-nostats", "-loglevel", "error", "-re", "-f", "lavfi", "-i"].map(String::from).to_vec();
    a.push(base);
    a.extend(["-f", "lavfi", "-i"].map(String::from));
    a.push(format!("aevalsrc='{beep}|{beep}':s=48000"));
    let s = h / 12; // the moving square, the flash
    let band = h * 14 / 100;
    let gap = h / 60;
    let font = (h as f64 * 0.05).round() as u32;
    let lipsync = c.pattern == "lipsync";
    // What moves is laid over the picture (FFmpeg 6 does not move a drawn box): a square crossing
    // the picture in 4 s; with the lip sync pattern, a bar sweeping it once a second.
    a.extend(["-f".into(), "lavfi".into(), "-i".into(), format!("color=c=white:size={s}x{s}:rate={rate}")]);
    let bar = lipsync.then(|| {
        a.extend(["-f".into(), "lavfi".into(), "-i".into(), format!("color=c=0x8fa3ff:size={}x{h}:rate={rate}", (w / 60).max(4))]);
        3
    });
    // The source's logo (a picture FFmpeg reads: not SVG).
    let logo = super::logo::find(data_dir, &c.id).map(|(p, _)| p).filter(|p| p.extension().is_some_and(|e| e != "svg"));
    let logo_in = if bar.is_some() { 4 } else { 3 };
    if let Some(p) = &logo {
        a.extend(["-loop", "1", "-i"].map(String::from));
        a.push(p.to_string_lossy().into_owned());
    }
    let text = text_file(data_dir, c);
    let text = text.to_string_lossy().replace('\\', "/").replace(':', "\\\\:").replace('\'', "");
    let sar = match (w, h) {
        (720, 576) => ",setsar=64/45",
        (720, 480) => ",setsar=32/27",
        _ => ",setsar=1",
    };
    // The frame count: 00 on the flash.
    let clock = format!("%{{localtime\\:%H\\\\\\:%M\\\\\\:%S}}\\:%{{eif\\:mod(n+{}\\,{fps})\\:d\\:2}}", fps - fps / 2);
    let flash = format!("enable='lt(mod(t+{phase:.5}\\,1)\\,{beep_len:.4})'");
    // 1. The picture, and what moves over it.
    let mut v = if lipsync { "[0:v]drawgrid=w=iw/10:h=ih:t=2:c=gray@0.55[g];[g][2:v]".to_string() } else { "[0:v][2:v]".to_string() };
    v.push_str(&format!("overlay=x='mod(t*W/4\\,W-w)':y=H-{band}-h-{gap}:shortest=1[m]"));
    if let Some(i) = bar {
        v.push_str(&format!(";[m][{i}:v]overlay=x='mod(t\\,1)*W':y=0:shortest=1[m2]"));
    }
    // 2. The flash (a square, the whole picture for the lip sync one), the timecode, the band.
    v.push_str(&format!(";[{}]", if bar.is_some() { "m2" } else { "m" }));
    if lipsync {
        let tc = (h as f64 * 0.16).round() as u32;
        v.push_str(&format!(
            "drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:{flash},\
             drawtext=text='{clock}':fontcolor=white:fontsize={tc}:x=(w-tw)/2:y=(h-th)/2-h*0.06:box=1:boxcolor=black@0.7:boxborderw={},",
            tc / 6
        ));
    }
    v.push_str(&format!(
        "drawbox=x=0:y=ih-{band}:w=iw:h={band}:color=black@0.78:t=fill,\
         drawbox=x=iw*0.03:y=ih-{band}-{s}-{gap}:w={s}:h={s}:color=white:t=fill:{flash},\
         drawtext=textfile='{text}':fontcolor=white:fontsize={font}:x=w*0.03:y=h-{band}/2-th/2,\
         drawtext=text='{clock}':fontcolor=white:fontsize={font}:x=w-tw-w*0.03:y=h-{band}/2-th/2{sar}"
    ));
    if logo.is_some() {
        let lh = (h / 8).max(16);
        v.push_str(&format!("[bg];[{logo_in}:v]scale=-2:{lh}[logo];[bg][logo]overlay=W-w-W*0.03:H*0.04:shortest=0"));
    }
    if f.interlaced {
        v.push_str(",setfield=tff");
    }
    v.push_str("[v]");
    a.push("-filter_complex".into());
    a.push(v);
    let br = c.bitrate.clamp(1.0, 200.0);
    a.extend(["-map", "[v]", "-map", "1:a", "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p"].map(String::from));
    a.extend(["-b:v".into(), format!("{br}M"), "-maxrate".into(), format!("{br}M"), "-bufsize".into(), format!("{}M", br * 2.0), "-g".into(), fps.to_string()]);
    if f.interlaced {
        a.extend(["-flags", "+ildct+ilme", "-top", "1"].map(String::from));
    }
    a.extend(["-c:a", "aac", "-b:a", "192k", "-ar", "48000"].map(String::from));
    a.extend(["-metadata".into(), format!("service_name={}", c.name.trim()), "-metadata".into(), "service_provider=Strata Cut".into()]);
    a.extend(["-f", "mpegts", "-muxdelay", "0", "pipe:1"].map(String::from));
    a
}

/// The FFmpeg making the pattern (its output: to the receiver's input).
pub fn command(ffmpeg: &str, data_dir: &Path, c: &RecorderConfig) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.args(args(data_dir, c)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(Format::parse("1080i50"), Some(Format { width: 1920, height: 1080, interlaced: true, num: 25, den: 1 }));
        assert_eq!(Format::parse("1080p5994"), Some(Format { width: 1920, height: 1080, interlaced: false, num: 60000, den: 1001 }));
        assert_eq!(Format::parse("480i5994"), Some(Format { width: 720, height: 480, interlaced: true, num: 30000, den: 1001 }));
        assert_eq!(Format::parse("2160p25").map(|f| (f.width, f.fps())), Some((3840, 25.0)));
        assert!(Format::parse("1080p51").is_none());
        for f in FORMATS {
            assert!(Format::parse(f).is_some(), "{f}");
        }
    }
}
