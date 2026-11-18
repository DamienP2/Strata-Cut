//! `strata-cut user …`. STUB — to be completed.
use clap::Subcommand;

#[derive(Subcommand)]
pub enum UserCmd {
    /// Adds a user (asks for the password twice).
    Add {
        name: String,
        #[arg(long, default_value = "admin")]
        role: String,
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
    },
    List {
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
    },
    Passwd {
        name: String,
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
    },
    Delete {
        name: String,
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
    },
}

pub fn user(_cmd: UserCmd) -> anyhow::Result<()> {
    anyhow::bail!("not implemented yet")
}
