//! Strata Cut server: records streams in loops, cuts clips at the frame.
use clap::{Args, Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::{Duration, Instant}};
use tokio::sync::{watch, Notify};
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod api;
mod auth;
mod cli;
mod clips;
mod config;
mod guard;
mod health;
mod logs;
mod ntp;
mod paths;
mod perf;
mod prefs;
mod recorder;
mod service;
mod sso;
mod state;
mod system;
mod update;
mod users;
mod web;
mod ws;

use state::{AppState, Events, Shared};

pub const DEFAULT_BIND: &str = "0.0.0.0:8080";
pub const BIN: &str = "strata-cut";

#[derive(Parser)]
#[command(name = "strata-cut", version, args_conflicts_with_subcommands = true)]
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
    rt.block_on(async move {
        let (stop_tx, stop_rx) = watch::channel(false);
        tokio::spawn(async move {
            shutdown_signal().await;
            let _ = stop_tx.send(true);
        });
        serve(RunArgs { data_dir: Some(data_dir), ..args }, stop_rx).await
    })
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

async fn until_true(mut rx: watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            return;
        }
    }
}

pub async fn serve(args: RunArgs, stop_rx: watch::Receiver<bool>) -> anyhow::Result<()> {
    let data_dir = args.data_dir.clone().unwrap_or_else(paths::user_data_dir);
    health::start(&data_dir);
    update::startup_guard(&data_dir);
    update::confirm_after(data_dir.clone(), Duration::from_secs(60));
    update::spawn_auto_check(data_dir.clone());
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
    let clips = clips::Clips::start(&data_dir, &first, events.clone(), recorders.clone());
    loop {
        let config = config::ServerConfig::load(&data_dir);
        recorders.set_config(&config);
        ntp::request(&data_dir, &config.ntp);
        clips.set_config(&config);
        let wanted = args.bind.or(config.bind).or(args.default_bind).unwrap_or(DEFAULT_BIND.parse()?);
        let listener = bind_retry(wanted).await?;
        let bind = listener.local_addr()?;
        tracing::info!("listening on http://{bind}");
        let _ = paths::write_json(&data_dir.join("runtime.json"), &serde_json::json!({"bind": bind, "version": env!("CARGO_PKG_VERSION"), "pid": std::process::id()}), false);
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
    recorders.shutdown().await;
    health::stopped();
    Ok(())
}
