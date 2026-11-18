//! OpenID Connect sign-in. STUB — to be completed (spec § 5.5).
use axum::Router;
use serde::{Deserialize, Serialize};
use crate::state::Shared;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SsoConfig {
    #[serde(default)]
    pub enabled: bool,
}

impl SsoConfig {
    pub fn normalize(&mut self) {}
}

pub fn routes() -> Router<Shared> {
    Router::new()
}
