//! systemd service. STUB — to be completed (spec § 2.9, Linux only).
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ServiceCmd {
    /// Installs and starts the service.
    Install {
        #[arg(long, default_value = "0.0.0.0:8080")]
        bind: std::net::SocketAddr,
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
    },
    Uninstall,
    Start,
    Stop,
    Status,
}

pub fn run(_cmd: ServiceCmd) -> anyhow::Result<()> {
    anyhow::bail!("not implemented yet")
}
