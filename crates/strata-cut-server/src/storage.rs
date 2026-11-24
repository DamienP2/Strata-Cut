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
const RESULT: &str = "storage.result";
const HELPER: &str = "/usr/local/lib/strata-cut/prepare-folder";

/// File systems that hold data (not /proc, tmpfs, squashfs, overlay…).
const DATA_FS: &[&str] = &["ext4", "ext3", "ext2", "xfs", "btrfs", "zfs", "f2fs", "jfs", "nfs", "nfs4", "cifs", "smb3", "fuseblk", "exfat", "ntfs3", "vfat"];
/// Mount points that are never a place for recordings.
const SKIP: &[&str] = &["/boot", "/snap", "/proc", "/sys", "/dev", "/run", "/var/lib/docker", "/var/snap", "/tmp"];

/// The root helper that prepares folders is installed.
pub fn managed() -> bool {
    Path::new(PATH_UNIT).exists()
}

/// The root helper also extends LVM volumes (installed by a recent install script).
fn extends() -> bool {
    std::fs::read_to_string(HELPER).is_ok_and(|s| s.contains("storage.result"))
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

/// The mount point holding `dir` (the longest one that starts it).
pub fn mount_of(dir: &Path) -> String {
    let mut p = dir.to_path_buf();
    while !p.exists() && p.pop() {}
    let p = p.canonicalize().unwrap_or(p);
    let Ok(text) = std::fs::read_to_string("/proc/self/mounts") else { return "/".into() };
    text.lines()
        .filter_map(|l| l.split_whitespace().nth(1).map(unescape))
        .filter(|m| p.starts_with(m))
        .max_by_key(|m| m.len())
        .unwrap_or_else(|| "/".into())
}

fn num(v: &Value) -> u64 {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0)
}

/// What is under a mount point: the physical disk, its size, and the part
/// not usable yet (free space of an LVM volume group, or unpartitioned).
pub fn physical(mount: &str) -> Value {
    let Ok(out) = std::process::Command::new("lsblk").args(["-b", "-J", "-o", "NAME,TYPE,SIZE,FSTYPE,MOUNTPOINTS"]).output() else { return Value::Null };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else { return Value::Null };
    fn find<'a>(nodes: &'a [Value], mount: &str, chain: &mut Vec<&'a Value>) -> bool {
        for n in nodes {
            chain.push(n);
            let here = n["mountpoints"].as_array().is_some_and(|m| m.iter().any(|x| x.as_str() == Some(mount))) || n["mountpoint"].as_str() == Some(mount);
            if here || n["children"].as_array().is_some_and(|c| find(c, mount, chain)) {
                return true;
            }
            chain.pop();
        }
        false
    }
    let mut chain = Vec::new();
    if !find(v["blockdevices"].as_array().map(|a| a.as_slice()).unwrap_or(&[]), mount, &mut chain) {
        return Value::Null;
    }
    let disk = chain[0];
    let node = chain[chain.len() - 1];
    let size = num(&disk["size"]);
    let kids = |n: &Value, t: &str| n["children"].as_array().map(|c| c.iter().filter(|x| x["type"] == t).map(|x| num(&x["size"])).sum::<u64>()).unwrap_or(0);
    let lvm = node["type"] == "lvm" && chain.len() >= 2;
    let lvm_free = if lvm { num(&chain[chain.len() - 2]["size"]).saturating_sub(kids(chain[chain.len() - 2], "lvm")) } else { 0 };
    let parts = kids(disk, "part");
    let unpartitioned = if parts > 0 { size.saturating_sub(parts) } else { 0 };
    json!({
        "disk": disk["name"], "size": size, "lvmFree": lvm_free, "unpartitioned": unpartitioned,
        // What extending gives: the group's free space, and the end of the disk outside any partition (growpart).
        "growable": if lvm { lvm_free + unpartitioned } else { 0 },
        "lv": if lvm { Value::String(format!("/dev/mapper/{}", node["name"].as_str().unwrap_or(""))) } else { Value::Null },
    })
}

/// The disk holding `dir`, for System › Storage: total, used by other files,
/// the system's reserve, and what Strata LiveCut may use (loops + clips).
pub fn usage(dir: &Path, loops_here: u64, clips_here: u64) -> Value {
    let Some((total, free)) = crate::recorder::disk_space(dir) else { return Value::Null };
    let mount = mount_of(dir);
    let others = total.saturating_sub(free).saturating_sub(loops_here + clips_here);
    let reserve = crate::recorder::system_reserve(dir, total);
    json!({
        "mount": mount, "total": total, "free": free, "others": others, "reserve": reserve,
        "available": total.saturating_sub(others).saturating_sub(reserve),
        "loops": loops_here, "clips": clips_here, "physical": physical(&mount),
    })
}

/// The space given to the loops and to the clips must really be there:
/// within what their disks leave available, and not below what the loops already reserve.
pub fn check_allocation(s: &crate::state::Shared, cfg: &crate::config::ServerConfig) -> Result<(), String> {
    let gb = 1e9;
    let (loops_max, clips_max) = ((cfg.loops_max_gb * gb) as u64, (cfg.clips_max_gb * gb) as u64);
    if loops_max == 0 && clips_max == 0 {
        return Ok(());
    }
    let rec_dir = cfg.recordings_dir(&s.data_dir);
    let clips_dir = cfg.clips_dir(&s.data_dir);
    let same = crate::recorder::same_disk(&rec_dir, &clips_dir);
    let loops_here = s.recorders.loops_bytes_under(&rec_dir);
    let clips_here = crate::recorder::dir_size(&clips_dir);
    let avail = |dir: &Path, loops: u64, clips: u64| usage(dir, loops, clips)["available"].as_u64().unwrap_or(0);
    // 1 % of slack for what moved since the page was read.
    let fits = |want: u64, have: u64| want as f64 <= have as f64 * 1.01;
    if same {
        let have = avail(&rec_dir, loops_here, clips_here);
        if !fits(loops_max + clips_max, have) {
            return Err("the space given is more than the disk has available".into());
        }
    } else {
        if loops_max > 0 && !fits(loops_max, avail(&rec_dir, loops_here, 0)) {
            return Err("the space given is more than the disk has available".into());
        }
        if clips_max > 0 && !fits(clips_max, avail(&clips_dir, 0, clips_here)) {
            return Err("the space given is more than the disk has available".into());
        }
    }
    if loops_max > 0 && s.recorders.reserved() > loops_max {
        return Err("the recorders already reserve more than this for their loops".into());
    }
    Ok(())
}

pub fn routes() -> axum::Router<crate::state::Shared> {
    axum::Router::new().route("/api/system/storage/extend", axum::routing::post(extend))
}

#[derive(serde::Deserialize)]
struct Extend {
    mount: String,
}

/// Gives the volume of a mount point all the free space of its LVM group (root helper: lvextend -r).
async fn extend(
    axum::extract::State(s): axum::extract::State<crate::state::Shared>,
    crate::auth::CurrentUser(me): crate::auth::CurrentUser,
    axum::Json(b): axum::Json<Extend>,
) -> crate::api::ApiResult<axum::Json<Value>> {
    crate::api::require(me.is_admin())?;
    if !managed() || !extends() {
        return Err(crate::api::bad("the install script must be run again (it installs the helper)"));
    }
    let mount = b.mount.clone();
    let p = tokio::task::spawn_blocking(move || physical(&mount)).await.map_err(crate::api::internal)?;
    let (Some(lv), Some(free)) = (p["lv"].as_str(), p["growable"].as_u64()) else { return Err(crate::api::bad("this disk is not an lvm volume")) };
    if free < 1 << 30 {
        return Err(crate::api::bad("no free space left in the volume group"));
    }
    let before = crate::recorder::disk_space(Path::new(&b.mount)).map(|(t, _)| t).unwrap_or(0);
    let result = s.data_dir.join(RESULT);
    let _ = std::fs::remove_file(&result);
    let text = format!("extend {lv}\n");
    crate::paths::write_atomic(&s.data_dir.join(REQUEST), text.as_bytes(), false).map_err(crate::api::internal)?;
    // The helper writes "ok" or "error: <why>" when done (growing a partition and a file system takes a while).
    for _ in 0..450 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let Ok(r) = std::fs::read_to_string(&result) else { continue };
        let r = r.trim();
        if let Some(why) = r.strip_prefix("error:") {
            tracing::warn!("{}: extending {lv} failed: {}", b.mount, why.trim());
            return Err(crate::api::bad(format!("the volume could not be extended: {}", why.trim())));
        }
        let now = crate::recorder::disk_space(Path::new(&b.mount)).map(|(t, _)| t).unwrap_or(0);
        tracing::warn!("{} extended by {}: {} GB → {} GB", b.mount, me.username, before / 1_000_000_000, now / 1_000_000_000);
        return Ok(axum::Json(json!({"total": now})));
    }
    Err(crate::api::bad("the volume could not be extended"))
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
