//! The recorders: one FFmpeg process per source, writing a loop of segments.
use axum::Router;
use std::{path::Path, sync::Arc};

use crate::{config::ServerConfig, state::{Events, Shared}};

pub struct Manager {}

impl Manager {
    pub fn start(_data_dir: &Path, _cfg: &ServerConfig, _events: Events) -> Arc<Self> {
        Arc::new(Manager {})
    }
    pub fn set_config(&self, _cfg: &ServerConfig) {}
    pub async fn shutdown(&self) {}
    /// Every recorder's settings (JSON array).
    pub fn list_json(&self) -> serde_json::Value {
        serde_json::json!([])
    }
    /// Every recorder's live state (JSON array).
    pub fn status_json(&self) -> serde_json::Value {
        serde_json::json!([])
    }
}

pub fn routes() -> Router<Shared> {
    Router::new()
}
