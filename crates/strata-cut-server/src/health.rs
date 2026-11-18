//! STUB — to be completed.
use axum::Router;
use crate::state::Shared;

pub fn routes() -> Router<Shared> {
    Router::new()
}

/// Starts the on-disk journal and crash reports.
pub fn start(_data_dir: &std::path::Path) {}
/// Marks a clean stop (not counted as a crash).
pub fn stopped() {}
