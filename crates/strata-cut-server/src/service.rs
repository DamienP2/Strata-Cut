//! The systemd service (spec § 2.9, Linux only).
use anyhow::{bail, Context};
use clap::Subcommand;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command,
};

const UNIT: &str = "strata-cut";
const UNIT_FILE: &str = "/etc/systemd/system/strata-cut.service";
/// The dedicated account the service runs as.
const SERVICE_USER: &str = "strata-cut";

#[derive(Subcommand)]
pub enum ServiceCmd {
    /// Installs and starts the service.
    Install {
        #[arg(long, default_value = "0.0.0.0:8080")]
        bind: SocketAddr,
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Stops and removes the service (the data stays).
    Uninstall,
    /// Starts the service.
    Start,
    /// Stops the service.
    Stop,
    /// Shows the service's state.
    Status,
}

/// The unit file.
pub fn unit(exe: &Path, bind: SocketAddr, data: &Path) -> String {
    format!(
        "[Unit]
Description={brand} server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User={user}
Group={user}
ExecStart=\"{exe}\" run --default-bind {bind} --data-dir \"{data}\"
Restart=always
RestartSec=2
TimeoutStopSec=15
NoNewPrivileges=yes
ProtectSystem=full
ReadWritePaths={data}
AmbientCapabilities=CAP_NET_BIND_SERVICE
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
",
        brand = crate::config::BRAND,
        user = SERVICE_USER,
        exe = exe.display(),
        data = data.display(),
    )
}

fn sh(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    let st = Command::new(cmd).args(args).status().with_context(|| format!("cannot run {cmd}"))?;
    if !st.success() {
        bail!("{cmd} {} failed", args.join(" "));
    }
    Ok(())
}

fn need_root() -> anyhow::Result<()> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        bail!("run this as root (sudo {} service …)", crate::BIN);
    }
    if !Path::new("/run/systemd/system").exists() {
        bail!("systemd is not running on this machine");
    }
    Ok(())
}

fn ensure_user(data: &Path) -> anyhow::Result<()> {
    let exists = Command::new("id").args(["-u", SERVICE_USER]).output().is_ok_and(|o| o.status.success());
    if !exists {
        let home = data.display().to_string();
        sh("useradd", &["--system", "--no-create-home", "--home-dir", &home, "--shell", "/usr/sbin/nologin", SERVICE_USER])?;
    }
    Ok(())
}

pub fn run(cmd: ServiceCmd) -> anyhow::Result<()> {
    match cmd {
        ServiceCmd::Install { bind, data_dir } => {
            need_root()?;
            let exe = std::env::current_exe()?.canonicalize()?;
            let data = data_dir.unwrap_or_else(|| PathBuf::from(crate::paths::SERVICE_DATA_DIR));
            std::fs::create_dir_all(&data).with_context(|| format!("cannot create {}", data.display()))?;
            let data = data.canonicalize()?;
            ensure_user(&data)?;
            sh("chown", &["-R", &format!("{SERVICE_USER}:{SERVICE_USER}"), &data.display().to_string()])?;
            std::fs::write(UNIT_FILE, unit(&exe, bind, &data)).with_context(|| format!("cannot write {UNIT_FILE}"))?;
            sh("systemctl", &["daemon-reload"])?;
            sh("systemctl", &["enable", "--now", UNIT])?;
            println!("{} runs as a service: http://localhost:{}/", crate::config::BRAND, bind.port());
            println!("data: {}", data.display());
        }
        ServiceCmd::Uninstall => {
            need_root()?;
            let _ = sh("systemctl", &["disable", "--now", UNIT]);
            if Path::new(UNIT_FILE).exists() {
                std::fs::remove_file(UNIT_FILE)?;
            }
            sh("systemctl", &["daemon-reload"])?;
            println!("service removed; the data stays in {}", crate::paths::SERVICE_DATA_DIR);
        }
        ServiceCmd::Start => {
            need_root()?;
            sh("systemctl", &["start", UNIT])?;
        }
        ServiceCmd::Stop => {
            need_root()?;
            sh("systemctl", &["stop", UNIT])?;
        }
        ServiceCmd::Status => {
            let _ = Command::new("systemctl").args(["status", "--no-pager", UNIT]).status().context("cannot run systemctl")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn unit_file() {
        let u = super::unit(std::path::Path::new("/opt/strata-cut/strata-cut"), "0.0.0.0:8080".parse().unwrap(), std::path::Path::new("/var/lib/strata-cut"));
        assert!(u.contains("ExecStart=\"/opt/strata-cut/strata-cut\" run --default-bind 0.0.0.0:8080 --data-dir \"/var/lib/strata-cut\""));
        assert!(u.contains("Restart=always\nRestartSec=2"));
        assert!(u.contains("ReadWritePaths=/var/lib/strata-cut"));
    }
}
