//! Request guards. STUB — to be completed (spec § 2.5).
use axum::{extract::{Request, State}, middleware::Next, response::Response};
use crate::state::Shared;

/// Refuses state-changing requests sent by another site with only the cookie.
pub async fn csrf(State(_s): State<Shared>, req: Request, next: Next) -> Response {
    next.run(req).await
}

/// Headers on every answer.
pub async fn every_answer(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("x-content-type-options", axum::http::HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", axum::http::HeaderValue::from_static("same-origin"));
    res
}
