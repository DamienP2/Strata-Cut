//! The HTTP API: shared helpers and the router.
use axum::{http::StatusCode, Router};

use crate::state::Shared;

pub type ApiResult<T> = Result<T, (StatusCode, String)>;

pub fn bad(e: impl ToString) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}
pub fn internal(e: impl ToString) -> (StatusCode, String) {
    let e = e.to_string();
    tracing::error!("{e}");
    (StatusCode::INTERNAL_SERVER_ERROR, e)
}
pub fn not_found(what: &str) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, format!("no such {what}"))
}
pub fn require(cond: bool) -> ApiResult<()> {
    if cond { Ok(()) } else { Err((StatusCode::FORBIDDEN, "you do not have the rights for this".into())) }
}

/// Runs blocking work (Argon2, files, outgoing HTTP) off the async threads.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(internal)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

pub fn router() -> Router<Shared> {
    Router::new()
        .merge(crate::auth::routes())
        .merge(crate::users::routes())
        .merge(crate::prefs::routes())
        .merge(crate::system::routes())
        .merge(crate::health::routes())
        .merge(crate::logs::routes())
        .merge(crate::update::routes())
        .merge(crate::sso::routes())
        .merge(crate::ws::routes())
        .merge(crate::ntp::routes())
        .merge(crate::recorder::routes())
        .merge(crate::clips::routes())
}
