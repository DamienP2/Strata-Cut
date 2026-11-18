//! The log in memory: the last 5000 lines, read by the System page (spec § 5.11).
use axum::{
    extract::{Query, State},
    http::header,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::VecDeque, fmt::Write as _, sync::Mutex};
use tracing::{field::Field, Level};

use crate::{
    api::{require, ApiResult},
    auth::CurrentUser,
    state::Shared,
};

const CAPACITY: usize = 5000;

#[derive(Serialize, Clone, Debug)]
pub struct Entry {
    pub seq: u64,
    /// ms since the epoch.
    pub at: i64,
    pub level: &'static str,
    pub target: String,
    pub message: String,
    /// "k=v k=v"
    pub fields: String,
}

impl Entry {
    pub fn line(&self) -> String {
        let at = jiff::Timestamp::from_millisecond(self.at).map(|t| t.to_string()).unwrap_or_default();
        let mut s = format!("{at} {:5} {}: {}", self.level, self.target, self.message);
        if !self.fields.is_empty() {
            s.push(' ');
            s.push_str(&self.fields);
        }
        s
    }
}

struct Buf {
    next: u64,
    entries: VecDeque<Entry>,
}

static RING: Mutex<Buf> = Mutex::new(Buf { next: 1, entries: VecDeque::new() });

fn rank(level: &str) -> u8 {
    match level.to_ascii_uppercase().as_str() {
        "ERROR" => 5,
        "WARN" | "WARNING" => 4,
        "INFO" => 3,
        "DEBUG" => 2,
        _ => 1,
    }
}

/// The lines after `after`, at `level` or more severe, `limit` at most (the newest).
/// Empties the log kept in memory (the numbering goes on).
pub fn clear() {
    RING.lock().unwrap_or_else(|p| p.into_inner()).entries.clear();
}

pub fn read(after: u64, level: Option<&str>, limit: usize) -> (Vec<Entry>, u64) {
    let min = level.map(rank).unwrap_or(0);
    let ring = RING.lock().unwrap();
    let mut out: Vec<Entry> = ring.entries.iter().filter(|e| e.seq > after && rank(e.level) >= min).cloned().collect();
    if out.len() > limit {
        out.drain(..out.len() - limit);
    }
    (out, ring.next - 1)
}

/// The tracing layer that fills the ring (installed by `main`).
pub struct Ring;

impl Ring {
    pub fn layer() -> Ring {
        Ring
    }
}

#[derive(Default)]
struct Visitor {
    message: String,
    fields: String,
}

impl tracing::field::Visit for Visitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.push(field, &value);
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            self.push(field, value);
        }
    }
}

impl Visitor {
    fn push(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name().starts_with("log.") {
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        let _ = write!(self.fields, "{}={value:?}", field.name());
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Ring {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let meta = event.metadata();
        let mut v = Visitor::default();
        event.record(&mut v);
        let level = match *meta.level() {
            Level::ERROR => "ERROR",
            Level::WARN => "WARN",
            Level::INFO => "INFO",
            Level::DEBUG => "DEBUG",
            Level::TRACE => "TRACE",
        };
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        let entry = {
            let mut ring = RING.lock().unwrap();
            let e = Entry { seq: ring.next, at, level, target: meta.target().to_string(), message: v.message, fields: v.fields };
            ring.next += 1;
            if ring.entries.len() >= CAPACITY {
                ring.entries.pop_front();
            }
            ring.entries.push_back(e.clone());
            e
        };
        if *meta.level() <= Level::WARN {
            crate::health::record_log(&entry);
        }
    }
}

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/system/logs", get(logs)).route("/api/system/logs/download", get(download))
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    after: u64,
    level: Option<String>,
    limit: Option<usize>,
}

/// The log file of a service run, if any.
pub fn file(s: &Shared) -> Option<String> {
    let f = s.data_dir.join("logs").join(format!("{}.log", crate::BIN));
    (crate::supervised() && f.exists()).then(|| f.display().to_string())
}

async fn logs(State(s): State<Shared>, CurrentUser(me): CurrentUser, Query(q): Query<LogQuery>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let limit = q.limit.unwrap_or(1000).clamp(1, CAPACITY);
    let (entries, last) = read(q.after, q.level.as_deref().filter(|l| !l.is_empty()), limit);
    Ok(Json(json!({"entries": entries, "last": last, "file": file(&s)})))
}

/// Every line in memory, as text.
pub fn text() -> String {
    let (entries, _) = read(0, None, CAPACITY);
    let mut out = String::new();
    for e in entries {
        out.push_str(&e.line());
        out.push('\n');
    }
    out
}

async fn download(CurrentUser(me): CurrentUser) -> ApiResult<impl IntoResponse> {
    require(me.is_admin())?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"stratacut.log\"")], text()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert!(rank("ERROR") > rank("WARN") && rank("warn") > rank("INFO") && rank("INFO") > rank("DEBUG"));
        let e = Entry { seq: 1, at: 0, level: "INFO", target: "t".into(), message: "m".into(), fields: "a=1".into() };
        assert_eq!(e.line(), "1970-01-01T00:00:00Z INFO  t: m a=1");
    }
}
