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
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Security {
    #[serde(default)]
    pub sso: crate::sso::SsoConfig,
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
        if jiff::tz::TimeZone::get(&self.timezone).is_err() {
            self.timezone = default_timezone();
        }
        self.security.sso.normalize();
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("the server name cannot be empty".into());
        }
        for d in [&self.recordings_dir, &self.clips_dir] {
            if !d.is_empty() && !Path::new(d).is_absolute() {
                return Err("a folder is written as an absolute path".into());
            }
        }
        Ok(())
    }

    #[allow(dead_code)] // used by the recording engine
    pub fn recordings_dir(&self, data_dir: &Path) -> PathBuf {
        if self.recordings_dir.is_empty() { data_dir.join("loops") } else { PathBuf::from(&self.recordings_dir) }
    }

    #[allow(dead_code)] // used by the recording engine
    pub fn clips_dir(&self, data_dir: &Path) -> PathBuf {
        if self.clips_dir.is_empty() { data_dir.join("clips") } else { PathBuf::from(&self.clips_dir) }
    }
}
