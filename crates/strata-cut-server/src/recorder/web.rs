//! Live streams of web pages (YouTube, Facebook, Instagram, X, LinkedIn…), through yt-dlp.
//!
//! Before each recording yt-dlp reads the page: is it live, and where is the stream?
//! One stream with sound and picture (HLS, most lives) goes straight to FFmpeg with the
//! page's HTTP headers; picture and sound apart (DASH) come through yt-dlp, which joins them.
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde_json::Value;
use tokio::process::Command;

use super::RecorderConfig;

/// The best stream up to 1080p, with sound; else picture and sound apart.
const FORMAT: &str = "b[height<=1080][vcodec!=none][acodec!=none]/bv*[height<=1080]+ba/b";

/// The yt-dlp program: the one the install script puts in the data folder (kept up to date), else the system's.
pub fn ytdlp(data_dir: &Path) -> String {
    let own = data_dir.join("bin/yt-dlp");
    if own.is_file() {
        return own.to_string_lossy().into_owned();
    }
    std::env::var("SC_YTDLP").unwrap_or_else(|_| "yt-dlp".into())
}

/// The platform of a page, for the interface.
pub fn platform(url: &str) -> &'static str {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
    let is = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    if is("youtube.com") || is("youtu.be") {
        "youtube"
    } else if is("facebook.com") || is("fb.watch") || is("fb.com") {
        "facebook"
    } else if is("instagram.com") {
        "instagram"
    } else if is("x.com") || is("twitter.com") {
        "x"
    } else if is("linkedin.com") {
        "linkedin"
    } else {
        "web"
    }
}

/// Where FFmpeg reads the live from.
pub enum Source {
    /// One URL (picture and sound), with the HTTP headers the platform wants.
    Direct { url: String, user_agent: String, headers: String },
    /// Picture and sound apart: yt-dlp joins them and writes MPEG-TS on its output.
    Piped,
}

/// The cookies of a signed-in account (Netscape cookies.txt), in a file for yt-dlp.
fn cookies_file(data_dir: &Path, c: &RecorderConfig) -> Option<PathBuf> {
    if c.cookies.trim().is_empty() {
        return None;
    }
    let dir = data_dir.join("cookies");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.txt", c.id));
    std::fs::write(&path, c.cookies.as_bytes()).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Some(path)
}

/// The options common to every yt-dlp call for this recorder.
fn common(data_dir: &Path, c: &RecorderConfig) -> Vec<String> {
    let mut a: Vec<String> = ["--no-warnings", "--no-playlist", "--no-progress", "-f", FORMAT].map(String::from).to_vec();
    if let Some(f) = cookies_file(data_dir, c) {
        a.push("--cookies".into());
        a.push(f.to_string_lossy().into_owned());
    }
    // YouTube needs a JavaScript runtime: Deno by default, else Node (installed with Strata Cut).
    if !on_path("deno") && on_path("node") {
        a.extend(["--js-runtimes".into(), "node".into()]);
    }
    a
}

fn on_path(prog: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
}

/// Reads the page: live or not, and where its stream is. Err = a reason for the interface.
pub async fn resolve(data_dir: &Path, c: &RecorderConfig) -> Result<Source, String> {
    let prog = ytdlp(data_dir);
    let mut cmd = Command::new(&prog);
    cmd.args(common(data_dir, c)).arg("-J").arg("--").arg(&c.url).stdin(Stdio::null()).kill_on_drop(true);
    let out = match tokio::time::timeout(Duration::from_secs(90), cmd.output()).await {
        Err(_) => return Err("web: the page does not answer".into()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => return Err("web: yt-dlp is not installed (run the install script again)".into()),
        Ok(Err(e)) => return Err(format!("web: cannot start {prog}: {e}")),
        Ok(Ok(o)) => o,
    };
    if !out.status.success() {
        return Err(reason(&String::from_utf8_lossy(&out.stderr)));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| "web: unexpected answer from yt-dlp".to_string())?;
    source(&v)
}

/// The stream to record from yt-dlp's description of the page.
pub fn source(v: &Value) -> Result<Source, String> {
    match v["live_status"].as_str() {
        Some("is_live") => {}
        Some("is_upcoming") => return Err("web: the live has not started".into()),
        Some("was_live") | Some("post_live") => return Err("web: the live is over".into()),
        // A page without the field (some platforms) is taken as live when it says so.
        None if v["is_live"].as_bool() == Some(true) => {}
        _ => return Err("web: this page is not a live".into()),
    }
    if v["requested_formats"].as_array().is_some_and(|f| f.len() > 1) {
        return Ok(Source::Piped);
    }
    let Some(url) = v["url"].as_str().filter(|u| u.starts_with("http")) else { return Ok(Source::Piped) };
    let mut user_agent = String::new();
    let mut headers = String::new();
    if let Some(h) = v["http_headers"].as_object() {
        for (k, val) in h {
            let Some(val) = val.as_str() else { continue };
            if k.eq_ignore_ascii_case("user-agent") {
                user_agent = val.to_string();
            } else if !val.contains(['\r', '\n']) {
                headers.push_str(&format!("{k}: {val}\r\n"));
            }
        }
    }
    Ok(Source::Direct { url: url.to_string(), user_agent, headers })
}

/// FFmpeg's input options for a live read straight from its URL.
pub fn input_args(s: &Source) -> Vec<String> {
    match s {
        Source::Direct { url, user_agent, headers } => {
            let mut a: Vec<String> = vec!["-rw_timeout".into(), "20000000".into()];
            if !user_agent.is_empty() {
                a.extend(["-user_agent".into(), user_agent.clone()]);
            }
            if !headers.is_empty() {
                a.extend(["-headers".into(), headers.clone()]);
            }
            a.extend(["-i".into(), url.clone()]);
            a
        }
        Source::Piped => vec!["-f".into(), "mpegts".into(), "-i".into(), "pipe:0".into()],
    }
}

/// yt-dlp writing the live (picture and sound joined) as MPEG-TS on its output.
pub fn pipe_command(data_dir: &Path, c: &RecorderConfig) -> Command {
    let mut cmd = Command::new(ytdlp(data_dir));
    cmd.args(common(data_dir, c)).args(["--quiet", "--no-part", "--hls-use-mpegts", "-o", "-", "--"]).arg(&c.url);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    cmd
}

/// yt-dlp's error in a few words for the interface.
pub fn reason(stderr: &str) -> String {
    let line = stderr.lines().rev().find(|l| l.contains("ERROR")).or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("").trim();
    let low = line.to_ascii_lowercase();
    let r = if low.contains("will begin") || low.contains("premieres") || low.contains("upcoming") || low.contains("scheduled") {
        "web: the live has not started"
    } else if low.contains("not currently live") || low.contains("is not live") || low.contains("no live") || low.contains("not live") {
        "web: this page is not a live"
    } else if low.contains("sign in") || low.contains("log in") || low.contains("login") || low.contains("logged-in") || low.contains("cookies") || low.contains("age-restricted") {
        "web: this page needs a signed-in account (cookies)"
    } else if low.contains("private") || low.contains("unavailable") || low.contains("removed") || low.contains("does not exist") || low.contains("404") {
        "web: this live is unavailable or private"
    } else if low.contains("unsupported url") {
        "web: this address is not supported"
    } else if low.contains("429") || low.contains("rate-limit") || low.contains("rate limit") || low.contains("too many requests") {
        "web: the platform limits this server (too many requests)"
    } else if low.contains("javascript") || low.contains("js runtime") || low.contains("challenge") {
        "web: yt-dlp needs a javascript runtime (run the install script again)"
    } else {
        let msg = line.trim_start_matches("ERROR:").trim();
        // "[youtube] abc: message" → "message"
        let msg = msg.split_once("]").map(|(_, m)| m.trim()).unwrap_or(msg);
        let msg = msg.split_once(": ").map(|(_, m)| m).unwrap_or(msg);
        return if msg.is_empty() { "web: the live cannot be read".into() } else { format!("web: {}", msg.chars().take(160).collect::<String>()) };
    };
    r.into()
}

/// Keeps the data folder's yt-dlp up to date (the platforms change often): at start, then every day.
pub fn keep_updated(data_dir: PathBuf) {
    tokio::spawn(async move {
        loop {
            let own = data_dir.join("bin/yt-dlp");
            if own.is_file() {
                let run = Command::new(&own).args(["-U", "--no-warnings"]).stdin(Stdio::null()).kill_on_drop(true).output();
                match tokio::time::timeout(Duration::from_secs(300), run).await {
                    Ok(Ok(o)) => {
                        let text = String::from_utf8_lossy(&o.stdout);
                        if let Some(l) = text.lines().rev().find(|l| l.contains("Updated") || l.contains("up to date") || l.contains("ERROR")) {
                            tracing::info!("yt-dlp: {}", l.trim());
                        }
                    }
                    _ => tracing::warn!("yt-dlp: the update did not finish"),
                }
            }
            tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn platforms() {
        assert_eq!(platform("https://www.youtube.com/watch?v=abc"), "youtube");
        assert_eq!(platform("https://youtu.be/abc"), "youtube");
        assert_eq!(platform("https://m.facebook.com/page/videos/1"), "facebook");
        assert_eq!(platform("https://www.instagram.com/someone/live/"), "instagram");
        assert_eq!(platform("https://x.com/i/broadcasts/1"), "x");
        assert_eq!(platform("https://twitter.com/i/broadcasts/1"), "x");
        assert_eq!(platform("https://www.linkedin.com/events/123/"), "linkedin");
        assert_eq!(platform("https://notyoutube.com/x"), "web");
    }

    #[test]
    fn sources() {
        let live = json!({"live_status": "is_live", "url": "https://h/master.m3u8", "http_headers": {"User-Agent": "UA", "Referer": "https://r/"}});
        match source(&live) {
            Ok(Source::Direct { url, user_agent, headers }) => {
                assert_eq!(url, "https://h/master.m3u8");
                assert_eq!(user_agent, "UA");
                assert_eq!(headers, "Referer: https://r/\r\n");
            }
            _ => panic!("direct expected"),
        }
        let apart = json!({"live_status": "is_live", "requested_formats": [{"url": "a"}, {"url": "b"}]});
        assert!(matches!(source(&apart), Ok(Source::Piped)));
        assert_eq!(source(&json!({"live_status": "is_upcoming"})).err().unwrap(), "web: the live has not started");
        assert_eq!(source(&json!({"live_status": "was_live"})).err().unwrap(), "web: the live is over");
        assert_eq!(source(&json!({"live_status": "not_live"})).err().unwrap(), "web: this page is not a live");
    }

    #[test]
    fn reasons() {
        assert_eq!(reason("ERROR: [youtube] abc: This live event will begin in 2 hours."), "web: the live has not started");
        assert_eq!(reason("ERROR: [youtube:tab] @chan: The channel is not currently live"), "web: this page is not a live");
        assert_eq!(reason("ERROR: [instagram] x: Requested content is not available, rate-limit reached or login required. Use --cookies"), "web: this page needs a signed-in account (cookies)");
        assert_eq!(reason("ERROR: Unsupported URL: https://example.com/"), "web: this address is not supported");
        assert_eq!(reason("ERROR: [x] 1: Something odd happened"), "web: Something odd happened");
    }
}
