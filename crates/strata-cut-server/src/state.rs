//! What every request can reach.
use arc_swap::ArcSwap;
use serde_json::{json, Value};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Instant};
use tokio::sync::{broadcast, watch, Notify};

use crate::{auth::Auth, config::ServerConfig};

pub type Shared = Arc<AppState>;

pub struct AppState {
    pub data_dir: PathBuf,
    pub auth: Arc<Auth>,
    pub config: ArcSwap<ServerConfig>,
    /// Lives across hot restarts (recording never stops when the settings change).
    pub events: Events,
    pub bind: SocketAddr,
    pub started_at: Instant,
    /// Wakes the serve loop: rebuild state and listener without leaving the process.
    pub restart: Notify,
    /// Set to true when this cycle ends (WebSockets close).
    pub closing: watch::Sender<bool>,
    pub recorders: Arc<crate::recorder::Manager>,
    pub clips: Arc<crate::clips::Clips>,
    pub streams: Arc<crate::streams::Streams>,
}

impl AppState {
    /// Tells every client that a list changed: `{"type":"changed","what":…}`.
    pub fn changed(&self, what: &str) {
        self.events.send(Audience::All, json!({"type": "changed", "what": what}));
    }

    /// The app's lists, added to the WebSocket `hello`: `recorders`, `status`, `jobs`.
    pub fn app_hello(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("recorders".into(), self.recorders.list_json());
        m.insert("status".into(), self.recorders.status_json());
        m.insert("jobs".into(), self.clips.jobs_json());
        m
    }
}

/// Who receives an event.
#[allow(dead_code)] // Admins / User: used by the recording engine
#[derive(Clone, Debug, PartialEq)]
pub enum Audience {
    All,
    Admins,
    User(String),
}

#[derive(Clone)]
pub struct Event {
    pub audience: Audience,
    /// Serialised once.
    pub json: Arc<str>,
}

#[derive(Clone)]
pub struct Events {
    tx: broadcast::Sender<Event>,
}

impl Events {
    pub fn new(capacity: usize) -> Self {
        Self { tx: broadcast::channel(capacity).0 }
    }
    pub fn send(&self, audience: Audience, value: Value) {
        let _ = self.tx.send(Event { audience, json: value.to_string().into() });
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
