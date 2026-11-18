//! The disks the loops and the clips can go to.
//!
//! The service runs without privileges: on another disk it usually cannot
//! create its folder. It writes the folder wanted to `<data>/storage.request`;
//! a root path unit installed by the install script (`strata-cut-storage.path`)
//! checks it, creates it and gives it to the service account.
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub const REQUEST: &str = "storage.request";
const PATH_UNIT: &str = "/etc/systemd/system/strata-cut-storage.path";

/// File systems that hold data (not /proc, tmpfs, squashfs, overlay…).
const DATA_FS: &[&str] = &["ext4", "ext3", "ext2", "xfs", "btrfs", "zfs", "f2fs", "jfs", "nfs", "nfs4", "cifs", "smb3", "fuseblk", "exfat", "ntfs3", "vfat"];
/// Mount points that are never a place for recordings.
const SKIP: &[&str] = &["/boot", "/snap", "/proc", "/sys", "/dev", "/run", "/var/lib/docker", "/var/snap", "/tmp"];

/// The root helper that prepares folders is installed.
pub fn managed() -> bool {
    Path::new(PATH_UNIT).exists()
}

/// "/mnt/My\\040Disk" in /proc/mounts → "/mnt/My Disk".
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The mounted data disks: `[{mount, device, fs, total, free}]`, one per device.
pub fn disks() -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string("/proc/self/mounts") else { return Vec::new() };
    let mut seen: Vec<(String, String)> = Vec::new(); // (device, mount)
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(dev), Some(mount), Some(fs)) = (f.first(), f.get(1), f.get(2)) else { continue };
        let mount = unescape(mount);
        if !DATA_FS.contains(fs) || SKIP.iter().any(|s| mount == *s || mount.starts_with(&format!("{s}/"))) {
            continue;
        }
        match seen.iter_mut().find(|(d, _)| d == dev) {
            // The same disk mounted twice: keep the shortest path.
            Some(e) if mount.len() < e.1.len() => e.1 = mount,
            Some(_) => {}
            None => seen.push((dev.to_string(), mount)),
        }
    }
    let mut out: Vec<Value> = seen
        .into_iter()
        .filter_map(|(dev, mount)| {
            let fs = text.lines().find(|l| l.split_whitespace().nth(1).map(unescape).as_deref() == Some(mount.as_str())).and_then(|l| l.split_whitespace().nth(2)).unwrap_or("").to_string();
            let (total, free) = crate::recorder::disk_space(Path::new(&mount))?;
            (total >= 1 << 30).then(|| json!({"mount": mount, "device": dev, "fs": fs, "total": total, "free": free}))
        })
        .collect();
    out.sort_by(|a, b| a["mount"].as_str().cmp(&b["mount"].as_str()));
    out
}

/// The service can write in `dir` (creating it if its parent allows).
fn writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".strata-cut-write-test");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Makes sure the service can write in `dir`: creates it, else asks the root helper and waits for it.
pub async fn ensure_writable(data_dir: &Path, dir: &Path) -> Result<(), String> {
    let d = dir.to_path_buf();
    if tokio::task::spawn_blocking(move || writable(&d)).await.unwrap_or(false) {
        return Ok(());
    }
    if !managed() {
        return Err("the service cannot write in this folder".into());
    }
    let req = data_dir.join(REQUEST);
    let mut text = dir.to_string_lossy().into_owned();
    text.push('\n');
    crate::paths::write_atomic(&req, text.as_bytes(), false).map_err(|e| e.to_string())?;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let d: PathBuf = dir.to_path_buf();
        if tokio::task::spawn_blocking(move || writable(&d)).await.unwrap_or(false) {
            tracing::info!("{} prepared for the service", dir.display());
            return Ok(());
        }
    }
    Err("the service cannot write in this folder".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn mount_names() {
        assert_eq!(super::unescape("/mnt/My\\040Disk"), "/mnt/My Disk");
        assert_eq!(super::unescape("/srv/data"), "/srv/data");
    }
}
