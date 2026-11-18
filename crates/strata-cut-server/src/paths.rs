//! Where the server keeps its data.
use std::path::{Path, PathBuf};

pub const SERVICE_DATA_DIR: &str = "/var/lib/strata-cut";

/// The data folder of an interactive run.
pub fn user_data_dir() -> PathBuf {
    directories::ProjectDirs::from("com", "Strata", "Strata Cut")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("data"))
}

/// The installed service's folder if it exists, else the user's (for the `user` commands).
pub fn cli_data_dir(given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| {
        let svc = PathBuf::from(SERVICE_DATA_DIR);
        if svc.join("users.json").exists() { svc } else { user_data_dir() }
    })
}

/// Writes JSON atomically (`.tmp` then rename); `private` = 0600.
pub fn write_json<T: serde::Serialize + ?Sized>(path: &Path, value: &T, private: bool) -> std::io::Result<()> {
    let data = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    write_atomic(path, &data, private)
}

pub fn write_atomic(path: &Path, data: &[u8], private: bool) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, data)?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = private;
    std::fs::rename(&tmp, path)
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let data = std::fs::read(path).ok()?;
    match serde_json::from_slice(&data) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::error!("{}: {e}", path.display());
            None
        }
    }
}
