//! The machine's NTP servers (the timecode is the server's clock).
//!
//! The service runs without privileges: it writes the wanted servers to
//! `<data>/ntp.conf`; a root path unit installed by the install script
//! (`strata-cut-ntp.path`) applies them to systemd-timesyncd or chrony.
use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};
use std::{path::Path, process::Command};

use crate::{
    api::{blocking, require, ApiResult},
    auth::CurrentUser,
    state::Shared,
};

pub const REQUEST: &str = "ntp.conf";
const PATH_UNIT: &str = "/etc/systemd/system/strata-cut-ntp.path";

/// The install script set up the root helper that applies the servers.
pub fn managed() -> bool {
    Path::new(PATH_UNIT).exists()
}

/// A host name or an IP address, nothing else.
pub fn valid(server: &str) -> bool {
    !server.is_empty() && server.len() <= 253 && server.chars().all(|c| c.is_ascii_alphanumeric() || ".:-".contains(c)) && !server.starts_with('-')
}

/// Writes the wanted servers for the root helper (only when they change).
pub fn request(data_dir: &Path, servers: &[String]) {
    let path = data_dir.join(REQUEST);
    let mut text = servers.join(" ");
    text.push('\n');
    if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
        return;
    }
    if !path.exists() && servers.is_empty() {
        return;
    }
    if let Err(e) = crate::paths::write_atomic(&path, text.as_bytes(), false) {
        tracing::error!("{}: {e}", path.display());
    } else {
        tracing::info!("ntp servers asked: {}", if servers.is_empty() { "system default" } else { text.trim() });
    }
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `{synchronized, service, server, offsetMs, managed}`.
pub fn status() -> Value {
    let synchronized = run("timedatectl", &["show", "-p", "NTPSynchronized", "--value"]).map(|v| v == "yes");
    let chrony = run("systemctl", &["is-active", "chrony"]).is_some_and(|v| v == "active");
    let (service, server, offset) = if chrony {
        // chronyc -c tracking: ref id, name, stratum, ref time, system time (s), …
        let t = run("chronyc", &["-c", "tracking"]).unwrap_or_default();
        let f: Vec<&str> = t.split(',').collect();
        ("chrony", f.get(1).map(|s| s.to_string()), f.get(4).and_then(|s| s.parse::<f64>().ok()).map(|s| s * 1000.0))
    } else {
        let server = run("timedatectl", &["show-timesync", "-p", "ServerName", "--value"]).filter(|s| !s.is_empty());
        // "Offset: +1.234ms" / "-12us" / "+1.5s"
        let offset = run("timedatectl", &["timesync-status"]).and_then(|t| {
            let v = t.lines().find_map(|l| l.trim().strip_prefix("Offset:"))?.trim().to_string();
            let (num, mul) = if let Some(n) = v.strip_suffix("ms") { (n, 1.0) } else if let Some(n) = v.strip_suffix("us") { (n, 0.001) } else if let Some(n) = v.strip_suffix('s') { (n, 1000.0) } else { (v.as_str(), 1.0) };
            num.trim().parse::<f64>().ok().map(|x| x * mul)
        });
        ("timesyncd", server, offset)
    };
    json!({"synchronized": synchronized, "service": service, "server": server, "offsetMs": offset, "managed": managed()})
}

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/system/time", get(time))
}

async fn time(State(_s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    Ok(Json(blocking(status).await?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn server_names() {
        assert!(super::valid("ntp.ubuntu.com"));
        assert!(super::valid("192.168.1.10"));
        assert!(super::valid("2001:db8::1"));
        assert!(!super::valid("a;rm -rf /"));
        assert!(!super::valid("-x"));
        assert!(!super::valid(""));
    }
}
