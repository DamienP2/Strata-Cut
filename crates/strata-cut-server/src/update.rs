//! STUB — to be completed.
use axum::Router;
use crate::state::Shared;

pub fn routes() -> Router<Shared> {
    Router::new()
}

pub fn startup_guard(_data_dir: &std::path::Path) {}
pub fn confirm_after(_data_dir: std::path::PathBuf, _after: std::time::Duration) {}
pub fn spawn_auto_check(_data_dir: std::path::PathBuf) {}
