//! Per-user choices: language, theme, display preferences (spec § 5.7–5.9).
use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

use crate::{
    api::{bad, blocking, internal, not_found, ApiResult},
    auth::{CurrentUser, PublicUser},
    state::Shared,
};

pub const LANGS: [&str; 2] = ["en", "fr"];
pub const THEMES: [&str; 3] = ["dark", "light", "system"];
const MAX_KEY: usize = 200;
const MAX_TOTAL: usize = 256 << 10;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/me/lang", put(put_lang))
        .route("/api/me/theme", put(put_theme))
        .route("/api/me/display", get(get_display).patch(patch_display))
}

pub fn path(data_dir: &Path, user_id: &str) -> PathBuf {
    data_dir.join("prefs").join(format!("{user_id}.json"))
}

#[derive(Deserialize)]
struct LangBody {
    #[serde(default)]
    lang: String,
}

async fn put_lang(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<LangBody>) -> ApiResult<Json<PublicUser>> {
    if !LANGS.contains(&b.lang.as_str()) {
        return Err(bad("unknown language"));
    }
    let a = s.auth.clone();
    let u = blocking(move || a.modify(&me.id, |u| { u.lang = b.lang; Ok(()) })).await?.map_err(|_| not_found("user"))?;
    Ok(Json(u.public()))
}

#[derive(Deserialize)]
struct ThemeBody {
    #[serde(default)]
    theme: String,
}

async fn put_theme(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<ThemeBody>) -> ApiResult<Json<PublicUser>> {
    if !THEMES.contains(&b.theme.as_str()) {
        return Err(bad("unknown theme"));
    }
    let a = s.auth.clone();
    let u = blocking(move || a.modify(&me.id, |u| { u.theme = b.theme; Ok(()) })).await?.map_err(|_| not_found("user"))?;
    Ok(Json(u.public()))
}

fn read(path: &Path) -> Map<String, Value> {
    crate::paths::read_json(path).unwrap_or_default()
}

async fn get_display(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Map<String, Value>>> {
    let p = path(&s.data_dir, &me.id);
    Ok(Json(blocking(move || read(&p)).await?))
}

/// Merges `changes` into `prefs` (`null` deletes); checks the limits.
pub fn merge(prefs: &mut Map<String, Value>, changes: Map<String, Value>) -> Result<(), String> {
    for (k, v) in changes {
        if k.is_empty() || k.chars().count() > MAX_KEY {
            return Err(format!("a key has 1 to {MAX_KEY} characters"));
        }
        if v.is_null() {
            prefs.remove(&k);
        } else {
            prefs.insert(k, v);
        }
    }
    if serde_json::to_vec(prefs).map(|d| d.len()).unwrap_or(0) > MAX_TOTAL {
        return Err("the display preferences are too large (256 KB at most)".into());
    }
    Ok(())
}

async fn patch_display(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(changes): Json<Map<String, Value>>) -> ApiResult<StatusCode> {
    // One writer at a time.
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _g = LOCK.lock().await;
    let p = path(&s.data_dir, &me.id);
    blocking(move || -> Result<(), (StatusCode, String)> {
        let mut prefs = read(&p);
        merge(&mut prefs, changes).map_err(bad)?;
        crate::paths::write_json(&p, &prefs, false).map_err(internal)
    })
    .await??;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merging() {
        let mut p = Map::new();
        let m = |v: Value| v.as_object().unwrap().clone();
        merge(&mut p, m(json!({"a": 1, "b": {"x": true}}))).unwrap();
        merge(&mut p, m(json!({"a": null, "c": "z"}))).unwrap();
        assert_eq!(Value::Object(p.clone()), json!({"b": {"x": true}, "c": "z"}));
        assert!(merge(&mut p, m(json!({ "k".repeat(201): 1 }))).is_err());
        assert!(merge(&mut p, m(json!({"big": "x".repeat(300_000)}))).is_err());
    }
}
