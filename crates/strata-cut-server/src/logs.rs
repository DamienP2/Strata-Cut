//! STUB — to be completed.
use axum::Router;
use crate::state::Shared;

pub fn routes() -> Router<Shared> {
    Router::new()
}

/// Keeps the last 5000 log lines in memory.
pub struct Ring;
impl Ring {
    pub fn layer() -> Ring {
        Ring
    }
}
impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Ring {}
