//! `strata-cut user …`: accounts from the command line. Acts on the installed service's folder if
//! it exists, else the user's; run with sudo, gives users.json back to the data folder's owner.
use anyhow::{anyhow, bail, Context};
use clap::Subcommand;
use std::{
    io::{BufRead, IsTerminal},
    path::{Path, PathBuf},
};

use crate::auth::{self, Auth, NewUser, Role};

#[derive(Subcommand)]
pub enum UserCmd {
    /// Adds a user (asks for the password twice).
    Add {
        name: String,
        /// admin, operator or viewer.
        #[arg(long, default_value = "admin")]
        role: String,
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Lists the users.
    List {
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Changes a user's password.
    Passwd {
        name: String,
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Deletes a user.
    Delete {
        name: String,
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
}

/// Asks for a new password twice (from the terminal; from stdin, one per line, when piped).
fn ask_new_password() -> anyhow::Result<String> {
    let (a, b) = if std::io::stdin().is_terminal() {
        (rpassword::prompt_password("Password: ")?, rpassword::prompt_password("Repeat the password: ")?)
    } else {
        let mut lines = std::io::stdin().lock().lines();
        let mut next = || lines.next().transpose().map(|l| l.unwrap_or_default());
        (next()?, next()?)
    };
    if a != b {
        bail!("the passwords do not match");
    }
    auth::validate_password(&a).map_err(|e| anyhow!(e))?;
    Ok(a)
}

/// Run with sudo: users.json goes back to the owner of the data folder.
fn give_back(dir: &Path) {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    if let Ok(m) = std::fs::metadata(dir) {
        if m.uid() != 0 {
            let _ = std::os::unix::fs::chown(dir.join("users.json"), Some(m.uid()), Some(m.gid()));
        }
    }
}

fn open(given: Option<PathBuf>) -> anyhow::Result<(PathBuf, Auth)> {
    let dir = crate::paths::cli_data_dir(given);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let auth = Auth::load(&dir);
    Ok((dir, auth))
}

fn find(auth: &Auth, name: &str) -> anyhow::Result<auth::User> {
    auth.find_name(name).ok_or_else(|| anyhow!("no such user: {name}"))
}

pub fn user(cmd: UserCmd) -> anyhow::Result<()> {
    match cmd {
        UserCmd::Add { name, role, data_dir } => {
            let role: Role = role.parse().map_err(|e: String| anyhow!("{e} (admin, operator or viewer)"))?;
            auth::validate_username(name.trim()).map_err(|e| anyhow!(e))?;
            let (dir, auth) = open(data_dir)?;
            if auth.find_name(name.trim()).is_some() {
                bail!("this user name is already taken");
            }
            let password = ask_new_password()?;
            let u = auth
                .add(NewUser { username: name, display_name: String::new(), role, disabled: false, password: Some(password), role_fixed: false, sso: String::new() })
                .map_err(|e| anyhow!(e))?;
            give_back(&dir);
            println!("user {} added ({}) in {}", u.username, u.role.as_str(), dir.display());
        }
        UserCmd::List { data_dir } => {
            let (dir, auth) = open(data_dir)?;
            let mut users = auth.users();
            users.sort_by_key(|u| u.username.to_lowercase());
            if users.is_empty() {
                println!("no user yet in {}", dir.display());
                return Ok(());
            }
            let w = users.iter().map(|u| u.username.chars().count()).max().unwrap_or(4).max(4);
            println!("{:w$}  {:8}  {:24}  STATE", "USER", "ROLE", "DISPLAY NAME");
            for u in users {
                let mut state = Vec::new();
                if u.disabled {
                    state.push("disabled");
                }
                if !u.sso.is_empty() {
                    state.push("sso");
                }
                println!("{:w$}  {:8}  {:24}  {}", u.username, u.role.as_str(), u.display_name, state.join(", "));
            }
        }
        UserCmd::Passwd { name, data_dir } => {
            let (dir, auth) = open(data_dir)?;
            let u = find(&auth, &name)?;
            let password = ask_new_password()?;
            auth.set_password(&u.id, &password).map_err(|e| anyhow!(e))?;
            give_back(&dir);
            println!("password of {} changed", u.username);
        }
        UserCmd::Delete { name, data_dir } => {
            let (dir, auth) = open(data_dir)?;
            let u = find(&auth, &name)?;
            auth.delete(&u.id).map_err(|e| anyhow!(e))?;
            let _ = std::fs::remove_file(crate::prefs::path(&dir, &u.id));
            give_back(&dir);
            println!("user {} deleted", u.username);
        }
    }
    Ok(())
}
