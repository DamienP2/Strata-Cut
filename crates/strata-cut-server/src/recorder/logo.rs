//! A recorder's logo (the channel's, the show's): PNG, JPEG, WebP, GIF or SVG, at most 2 MB,
//! kept as `<data>/logos/<recorder>.<ext>`. Sent by the browser, or fetched from an address.
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub const MAX: usize = 2 << 20;
const EXTS: [&str; 5] = ["png", "jpg", "webp", "gif", "svg"];

fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logos")
}

/// The file of a logo: only a plain id (letters, digits, - and _) names one, never a path.
fn file(data_dir: &Path, id: &str, ext: &str) -> Option<PathBuf> {
    let plain = !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    plain.then(|| dir(data_dir).join(format!("{id}.{ext}")))
}

/// The type of an image from its first bytes: (extension, MIME type).
pub fn kind(b: &[u8]) -> Option<(&'static str, &'static str)> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("png", "image/png"))
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(("webp", "image/webp"))
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(("gif", "image/gif"))
    } else {
        let head = String::from_utf8_lossy(&b[..b.len().min(2048)]).to_ascii_lowercase();
        let text = head.trim_start_matches('\u{feff}').trim_start();
        (text.starts_with('<') && head.contains("<svg")).then_some(("svg", "image/svg+xml"))
    }
}

/// The logo's file and its MIME type.
pub fn find(data_dir: &Path, id: &str) -> Option<(PathBuf, &'static str)> {
    EXTS.iter().filter_map(|e| file(data_dir, id, e)).find(|p| p.is_file()).and_then(|p| {
        let head = std::fs::read(&p).ok()?;
        kind(&head).map(|(_, mime)| (p, mime))
    })
}

/// When the logo last changed (ms): the interface asks for it again when it changes.
pub fn version(data_dir: &Path, id: &str) -> Option<i64> {
    let (p, _) = find(data_dir, id)?;
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
}

pub fn remove(data_dir: &Path, id: &str) {
    for p in EXTS.iter().filter_map(|e| file(data_dir, id, e)) {
        let _ = std::fs::remove_file(p);
    }
}

pub fn save(data_dir: &Path, id: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX {
        return Err("the logo is 2 MB at most".into());
    }
    let (ext, _) = kind(bytes).ok_or("the logo is a PNG, JPEG, WebP, GIF or SVG image")?;
    let path = file(data_dir, id, ext).ok_or("no such recorder")?;
    std::fs::create_dir_all(dir(data_dir)).map_err(|e| e.to_string())?;
    remove(data_dir, id);
    crate::paths::write_atomic(&path, bytes, false).map_err(|e| e.to_string())
}

/// Not an address of this machine, nor the cloud's metadata (link-local): a logo comes from the web.
fn allowed(host: &str, port: u16) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<_> = (host, port).to_socket_addrs().map_err(|_| format!("unknown address: {host}"))?.collect();
    if addrs.is_empty() {
        return Err(format!("unknown address: {host}"));
    }
    for a in addrs {
        let ip = crate::guard::plain_ip(a.ip());
        let local = match ip {
            std::net::IpAddr::V4(v) => v.is_loopback() || v.is_link_local() || v.is_unspecified() || v.is_broadcast(),
            std::net::IpAddr::V6(v) => v.is_loopback() || v.is_unspecified() || (v.segments()[0] & 0xffc0) == 0xfe80,
        };
        if local {
            return Err("this address is not allowed for a logo".into());
        }
    }
    Ok(())
}

/// Fetches a logo from an address (http or https, 3 redirections at most, 2 MB at most).
pub fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let mut url = url.trim().to_string();
    for _ in 0..4 {
        let uri: ureq::http::Uri = url.parse().map_err(|_| "this address is not valid".to_string())?;
        let https = match uri.scheme_str() {
            Some("https") => true,
            Some("http") => false,
            _ => return Err("the address starts with https://".into()),
        };
        let host = uri.host().ok_or("this address is not valid")?.trim_matches(['[', ']']).to_string();
        allowed(&host, uri.port_u16().unwrap_or(if https { 443 } else { 80 }))?;
        let res = crate::update::http_agent(0, Duration::from_secs(20)).get(&url).header("Accept", "image/*").call();
        let mut res = match res {
            Ok(r) => r,
            Err(e) => return Err(format!("cannot fetch the logo: {e}")),
        };
        if res.status().is_redirection() {
            let to = res.headers().get("location").and_then(|v| v.to_str().ok()).ok_or("cannot fetch the logo")?;
            url = if to.starts_with("http") { to.to_string() } else { format!("{}://{}{}", if https { "https" } else { "http" }, uri.authority().map(|a| a.as_str()).unwrap_or(&host), to) };
            continue;
        }
        if !res.status().is_success() {
            return Err(format!("cannot fetch the logo ({})", res.status()));
        }
        return res.body_mut().with_config().limit(MAX as u64 + 1).read_to_vec().map_err(|_| "the logo is 2 MB at most".to_string());
    }
    Err("too many redirections".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind(b"\x89PNG\r\n\x1a\nxxxx").map(|k| k.0), Some("png"));
        assert_eq!(kind(&[0xFF, 0xD8, 0xFF, 0xE0]).map(|k| k.0), Some("jpg"));
        assert_eq!(kind(b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>").map(|k| k.0), Some("svg"));
        assert_eq!(kind(b"<html><body>no</body></html>"), None);
        assert!(allowed("127.0.0.1", 80).is_err());
        assert!(allowed("169.254.169.254", 80).is_err());
        assert!(allowed("::ffff:127.0.0.1", 80).is_err());
        let d = Path::new("/data");
        assert!(file(d, "../clips/abc", "jpg").is_none());
        assert!(file(d, "..", "jpg").is_none());
        assert!(file(d, "", "jpg").is_none());
        assert_eq!(file(d, "0a1b2c3d4e5f", "png"), Some(PathBuf::from("/data/logos/0a1b2c3d4e5f.png")));
    }
}
