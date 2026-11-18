//! Clips cut from the loops, at the frame.
use axum::Router;
use std::{path::Path, sync::Arc};

use crate::{config::ServerConfig, recorder::Manager, state::{Events, Shared}};

pub struct Clips {}

impl Clips {
    pub fn start(_data_dir: &Path, _cfg: &ServerConfig, _events: Events, _recorders: Arc<Manager>) -> Arc<Self> {
        Arc::new(Clips {})
    }
    pub fn set_config(&self, _cfg: &ServerConfig) {}
    /// The extraction queue (JSON array).
    pub fn jobs_json(&self) -> serde_json::Value {
        serde_json::json!([])
    }
}

pub fn routes() -> Router<Shared> {
    Router::new()
}
