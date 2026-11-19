//! The server's settings (`config.json`).
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::{Path, PathBuf}};

pub const BRAND: &str = "Strata Cut";

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub bind: Option<SocketAddr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_origins: Vec<String>,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub security: Security,
    /// Where the loops are written (empty = `<data>/loops`).
    #[serde(default)]
    pub recordings_dir: String,
    /// Where the clips are written (empty = `<data>/clips`).
    #[serde(default)]
    pub clips_dir: String,
    /// The loops take at most this many GB (0 = no limit but the disk).
    #[serde(default)]
    pub loops_max_gb: f64,
    /// The clips take at most this many GB (0 = no limit but the disk).
    #[serde(default)]
    pub clips_max_gb: f64,
    /// Clips older than this many days (since made) are deleted (0 = kept until deleted by hand).
    #[serde(default)]
    pub clips_keep_days: u32,
    /// Space always left free on the disks, % (the oldest recordings go first).
    #[serde(default = "default_keep_free")]
    pub keep_free_percent: u32,
    /// The machine's NTP servers (empty = the system's default).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ntp: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Security {
    #[serde(default)]
    pub sso: crate::sso::SsoConfig,
}

fn default_keep_free() -> u32 {
    5
}
fn default_name() -> String {
    BRAND.into()
}
fn default_timezone() -> String {
    jiff::tz::TimeZone::system().iana_name().unwrap_or("UTC").to_string()
}

impl Default for ServerConfig {
    fn default() -> Self {
        serde_json::from_str("{}").unwrap()
    }
}

impl ServerConfig {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("config.json")
    }

    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        let raw: Option<serde_json::Value> = std::fs::read(&path).ok().and_then(|d| serde_json::from_slice(&d).ok());
        let mut cfg: ServerConfig = raw.as_ref().and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_else(|| {
            if path.exists() {
                tracing::error!("{}: invalid, default settings used", path.display());
            }
            Self::default()
        });
        cfg.normalize();
        if raw.as_ref() != Some(&serde_json::to_value(&cfg).unwrap()) {
            if let Err(e) = cfg.save(data_dir) {
                tracing::error!("{}: {e}", path.display());
            }
        }
        cfg
    }

    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        crate::paths::write_json(&Self::path(data_dir), self, true)
    }

    pub fn normalize(&mut self) {
        self.name = self.name.trim().to_string();
        self.trusted_origins = self.trusted_origins.iter().map(|o| o.trim().trim_end_matches('/').to_string()).filter(|o| !o.is_empty()).collect();
        self.recordings_dir = self.recordings_dir.trim().to_string();
        self.clips_dir = self.clips_dir.trim().to_string();
        self.loops_max_gb = if self.loops_max_gb.is_finite() { self.loops_max_gb.max(0.0) } else { 0.0 };
        self.clips_max_gb = if self.clips_max_gb.is_finite() { self.clips_max_gb.max(0.0) } else { 0.0 };
        self.keep_free_percent = self.keep_free_percent.clamp(1, 50);
        self.clips_keep_days = self.clips_keep_days.min(3650);
        let mut ntp: Vec<String> = Vec::new();
        for s in self.ntp.iter().flat_map(|s| s.split([' ', ',', ';'])).map(str::trim).filter(|s| !s.is_empty()) {
            if !ntp.iter().any(|x| x.eq_ignore_ascii_case(s)) {
                ntp.push(s.to_string());
            }
        }
        self.ntp = ntp;
        if jiff::tz::TimeZone::get(&self.timezone).is_err() {
            self.timezone = default_timezone();
        }
        self.security.sso.normalize();
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("the server name cannot be empty".into());
        }
        if self.ntp.len() > 8 {
            return Err("8 ntp servers at most".into());
        }
        if let Some(bad) = self.ntp.iter().find(|s| !crate::ntp::valid(s)) {
            let _ = bad;
            return Err("invalid ntp server name".into());
        }
        for d in [&self.recordings_dir, &self.clips_dir] {
            if !d.is_empty() && !Path::new(d).is_absolute() {
                return Err("a folder is written as an absolute path".into());
            }
        }
        Ok(())
    }

    pub fn recordings_dir(&self, data_dir: &Path) -> PathBuf {
        if self.recordings_dir.is_empty() { data_dir.join("loops") } else { PathBuf::from(&self.recordings_dir) }
    }

    pub fn clips_dir(&self, data_dir: &Path) -> PathBuf {
        if self.clips_dir.is_empty() { data_dir.join("clips") } else { PathBuf::from(&self.clips_dir) }
    }
}
