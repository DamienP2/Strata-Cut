//! Strata Cut server: records streams in loops, cuts clips at the frame.
use clap::{Args, Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::{Duration, Instant}};
use tokio::sync::{watch, Notify};
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod alerts;
mod api;
mod auth;
mod cli;
mod clips;
mod config;
mod groups;
mod roles;
mod limits;
mod streams;
mod guard;
mod health;
mod logs;
mod mixer;
mod ntp;
mod paths;
mod perf;
mod prefs;
mod recorder;
mod service;
mod program;
mod slates;
mod sso;
mod state;
mod storage;
mod system;
mod update;
mod users;
mod web;
mod ws;

use state::{AppState, Events, Shared};

pub const DEFAULT_BIND: &str = "0.0.0.0:8080";
pub const BIN: &str = "strata-cut";
/// The version: the release's (set by the release workflow, or by the install script from
/// the newest release tag when it builds from the source), else Cargo.toml's.
pub const VERSION: &str = match option_env!("STRATA_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};

#[derive(Parser)]
#[command(name = "strata-cut", version = VERSION, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand)]
enum Cmd {
    /// Runs the server (the default).
    Run(RunArgs),
    /// Installs or controls the system service.
    #[command(subcommand)]
    Service(service::ServiceCmd),
    /// Manages the user accounts.
    #[command(subcommand)]
    User(cli::UserCmd),
}

#[derive(Args, Clone, Default)]
pub struct RunArgs {
    #[arg(long, env = "SC_BIND")]
    pub bind: Option<SocketAddr>,
    #[arg(long, env = "SC_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    #[arg(long, env = "SC_WEB_DIR")]
    pub web_dir: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub default_bind: Option<SocketAddr>,
    #[arg(long, hide = true)]
    pub service: bool,
}

/// True when a service manager restarts the server (systemd, or SC_SUPERVISED=1).
pub fn supervised() -> bool {
    std::env::var_os("INVOCATION_ID").is_some() || std::env::var("SC_SUPERVISED").is_ok_and(|v| v == "1")
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Service(c)) => service::run(c),
        Some(Cmd::User(c)) => cli::user(c),
        Some(Cmd::Run(a)) => run(a),
        None => run(cli.run),
    }
}

fn run(args: RunArgs) -> anyhow::Result<()> {
    let data_dir = args.data_dir.clone().unwrap_or_else(paths::user_data_dir);
    std::fs::create_dir_all(&data_dir)?;
    // Absolute: FFmpeg runs in each run's folder.
    let data_dir = data_dir.canonicalize()?;
    init_logging(&data_dir);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let done = rt.block_on(async move {
        let (stop_tx, stop_rx) = watch::channel(false);
        tokio::spawn(async move {
            shutdown_signal().await;
            tracing::info!("stopping");
            let _ = stop_tx.send(true);
        });
        serve(RunArgs { data_dir: Some(data_dir), ..args }, stop_rx).await
    });
    // The recorders are stopped and their files closed: a task still running in the
    // background (a still being made, a probe, a check asleep) no longer holds the exit
    // back (it made each restart, after an update, wait several seconds).
    rt.shutdown_timeout(Duration::from_millis(500));
    tracing::info!("stopped");
    // Stopped to start again (an update installed, a version put back).
    if update::RESTART.load(std::sync::atomic::Ordering::SeqCst) {
        update::relaunch();
    }
    done
}

fn init_logging(data_dir: &std::path::Path) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,tower_http=warn"));
    let ring = logs::Ring::layer();
    if supervised() {
        let dir = data_dir.join("logs");
        let _ = std::fs::create_dir_all(&dir);
        let file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{BIN}.log")));
        if let Ok(file) = file {
            tracing_subscriber::registry().with(filter).with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(std::sync::Mutex::new(file))).with(ring).init();
            return;
        }
    }
    tracing_subscriber::registry().with(filter).with(tracing_subscriber::fmt::layer().with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))).with(ring).init();
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
        tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}

/// Binds, retrying while the old process frees the port (after an update);
/// falls back to 0.0.0.0:8080 to keep the interface reachable.
async fn bind_retry(wanted: SocketAddr) -> anyhow::Result<tokio::net::TcpListener> {
    for _ in 0..20 {
        match tokio::net::TcpListener::bind(wanted).await {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => tokio::time::sleep(Duration::from_millis(250)).await,
            Err(e) => {
                tracing::error!("cannot listen on {wanted}: {e}; falling back to {DEFAULT_BIND}");
                return Ok(tokio::net::TcpListener::bind(DEFAULT_BIND).await?);
            }
        }
    }
    let fallback: SocketAddr = DEFAULT_BIND.parse()?;
    if fallback != wanted {
        tracing::error!("{wanted} is in use; falling back to {DEFAULT_BIND}");
        return Ok(tokio::net::TcpListener::bind(fallback).await?);
    }
    anyhow::bail!("{wanted} is in use")
}

/// Other Strata Cut servers running on the same data folder (same program, `run`).
fn others(data_dir: &std::path::Path) -> Vec<i32> {
    let me = std::process::id() as i32;
    let Ok(exe) = std::env::current_exe() else { return Vec::new() };
    let strip = |p: std::path::PathBuf| std::path::PathBuf::from(p.to_string_lossy().trim_end_matches(" (deleted)"));
    let exe = strip(exe);
    let data = data_dir.to_string_lossy().into_owned();
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&pid| pid != me)
        .filter(|&pid| std::fs::read_link(format!("/proc/{pid}/exe")).map(strip).is_ok_and(|p| p == exe))
        .filter(|&pid| {
            let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let args: Vec<String> = cmd.split(|&b| b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
            // A server (not `user …` or `service …`) on this very folder.
            let server = !args.iter().skip(1).any(|a| a == "user" || a == "service");
            let env = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
            let env_dir = env.split(|&b| b == 0).find_map(|v| v.strip_prefix(b"SC_DATA_DIR=").map(|d| String::from_utf8_lossy(d).into_owned()));
            server && (args.contains(&data) || env_dir.as_deref() == Some(data.as_str()))
        })
        .collect()
}

/// Stops them (cleanly, then by force after 10 s).
fn stop_others(data_dir: &std::path::Path) {
    let pids = others(data_dir);
    if pids.is_empty() {
        return;
    }
    for &pid in &pids {
        tracing::warn!("another Strata Cut (pid {pid}) was still running on {}: stopped", data_dir.display());
        // SAFETY: kill has no memory effects.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    for _ in 0..100 {
        if others(data_dir).is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    for pid in others(data_dir) {
        tracing::warn!("pid {pid} did not stop: killed");
        // SAFETY: as above.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// Recorders' FFmpeg left by a Strata Cut that died without stopping them (a receiver goes on
/// writing an old run, a preview waits for a stream that no longer comes). Adopted by init,
/// they are known by their command line.
fn stop_orphan_ffmpeg() {
    let Ok(dir) = std::fs::read_dir("/proc") else { return };
    for pid in dir.flatten().filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok()) {
        // "pid (name) state ppid …": the name may hold spaces and brackets.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let ppid = stat.rsplit_once(')').and_then(|(_, r)| r.split_whitespace().nth(1)?.parse::<i32>().ok());
        if ppid != Some(1) {
            continue;
        }
        let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let cmd = cmd.split(|&b| b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect::<Vec<_>>().join(" ");
        if cmd.contains("ffmpeg") && (cmd.contains("/master/list.csv") || cmd.contains("/video/p%08d.m4s")) {
            tracing::warn!("a recorder's FFmpeg left by an earlier run (pid {pid}): stopped");
            // SAFETY: kill has no memory effects.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

async fn until_true(mut rx: watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            return;
        }
    }
}

pub async fn serve(args: RunArgs, stop_rx: watch::Receiver<bool>) -> anyhow::Result<()> {
    let data_dir = args.data_dir.clone().unwrap_or_else(paths::user_data_dir);
    // One Strata Cut per data folder: an older one still running (an update or a restart
    // that did not end it) would go on recording with its old list of recorders.
    stop_others(&data_dir);
    stop_orphan_ffmpeg();
    health::start(&data_dir);
    update::startup_guard(&data_dir);
    update::confirm_after(data_dir.clone(), Duration::from_secs(60));
    update::spawn_auto_check(data_dir.clone());
    roles::load(&data_dir);
    let auth = Arc::new(auth::Auth::load(&data_dir)); // outside the loop: sessions survive
    if auth.needs_setup() {
        let port = args.bind.map(|b| b.port()).unwrap_or(8080);
        tracing::warn!("no user yet: open http://localhost:{port}/ on this machine, or run `{BIN} user add admin`");
    }
    let started_at = Instant::now();
    // Outside the loop too: recording never stops when the settings change.
    let events = Events::new(256);
    let first = config::ServerConfig::load(&data_dir);
    let recorders = recorder::Manager::start(&data_dir, &first, events.clone());
    // The capture cards are searched once now (then only when asked, in Système › Matériel).
    tokio::spawn(recorder::decklink::search_at_start(data_dir.clone()));
    let clips = clips::Clips::start(&data_dir, &first, events.clone(), recorders.clone());
    let programs = program::Programs::start(&data_dir, recorders.clone(), events.clone());
    let streams = streams::Streams::start(&data_dir, recorders.clone(), programs, events.clone());
    // The lives under way before the restart start again.
    streams.resume();
    alerts::start(data_dir.clone(), recorders.clone(), streams.clone());
    loop {
        let config = config::ServerConfig::load(&data_dir);
        alerts::configure(&config.alerts, &config.name);
        recorders.set_config(&config);
        ntp::request(&data_dir, &config.ntp);
        clips.set_config(&config);
        recorder::web::configure(&data_dir, &config.youtube);
        let wanted = args.bind.or(config.bind).or(args.default_bind).unwrap_or(DEFAULT_BIND.parse()?);
        let listener = bind_retry(wanted).await?;
        let bind = listener.local_addr()?;
        tracing::info!("listening on http://{bind}");
        let _ = paths::write_json(&data_dir.join("runtime.json"), &serde_json::json!({"bind": bind, "version": VERSION, "pid": std::process::id()}), false);
        let shared: Shared = Arc::new(AppState {
            data_dir: data_dir.clone(),
            auth: auth.clone(),
            config: arc_swap::ArcSwap::from_pointee(config),
            events: events.clone(),
            bind,
            started_at,
            restart: Notify::new(),
            closing: watch::channel(false).0,
            recorders: recorders.clone(),
            clips: clips.clone(),
            streams: streams.clone(),
        });
        let mut app = api::router();
        app = match &args.web_dir {
            Some(dir) => app.fallback_service(tower_http::services::ServeDir::new(dir).fallback(tower_http::services::ServeFile::new(dir.join("index.html")))),
            None => app.fallback(web::embedded),
        };
        let app = app
            .layer(axum::middleware::from_fn(web::compress))
            .layer(axum::middleware::from_fn_with_state(shared.clone(), guard::csrf))
            .layer(axum::middleware::from_fn(guard::every_answer))
            .layer(CorsLayer::permissive())
            .with_state(shared.clone());
        let (cycle, stop) = (shared.clone(), until_true(stop_rx.clone()));
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async move {
                tokio::select! { _ = stop => {}, _ = cycle.restart.notified() => {} }
                let _ = cycle.closing.send(true);
            })
            .await?;
        if *stop_rx.borrow() {
            break;
        }
        tracing::info!("settings changed: restarting the web server");
    }
    tracing::info!("stopping: web server closed, the streams and the recorders stop");
    streams.shutdown().await;
    recorders.shutdown().await;
    tracing::info!("stopping: recorders stopped");
    health::stopped();
    Ok(())
}
