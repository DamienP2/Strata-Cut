//! The slates' images (Streamer): what a live sends while its source is lost. The images are
//! dropped in the Streamer (PNG, JPEG or WebP, 10 MB at most), kept as `<data>/slates/<id>.<ext>`,
//! and each source may have its own; without one, a dark picture with the source's name.
use std::path::{Path, PathBuf};

use axum::{
    body::Bytes,
    extract::{Path as UrlPath, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::Response,
    routing::{delete, get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    api::{bad, not_found, require, ApiResult},
    auth::CurrentUser,
    state::Shared,
};

pub const MAX: usize = 10 << 20;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub id: String,
    pub name: String,
    pub ext: String,
    pub added: i64,
    pub by: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct Index {
    images: Vec<Image>,
    /// The image of each source (recorder id → image id).
    sources: std::collections::BTreeMap<String, String>,
}

fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("slates")
}

fn index_path(data_dir: &Path) -> PathBuf {
    data_dir.join("slates.json")
}

fn load(data_dir: &Path) -> Index {
    crate::paths::read_json(&index_path(data_dir)).unwrap_or_default()
}

fn save(data_dir: &Path, i: &Index) -> Result<(), String> {
    crate::paths::write_json(&index_path(data_dir), i, true).map_err(|e| e.to_string())
}

/// One change at a time (the index read, changed, written).
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The image a source's lives send while it is lost, if it has one.
pub fn image_for(data_dir: &Path, source: &str) -> Option<PathBuf> {
    let i = load(data_dir);
    let id = i.sources.get(source)?;
    let img = i.images.iter().find(|x| &x.id == id)?;
    let p = dir(data_dir).join(format!("{}.{}", img.id, img.ext));
    p.is_file().then_some(p)
}

/// PNG, JPEG or WebP, from its first bytes: (extension, MIME type).
fn kind(b: &[u8]) -> Option<(&'static str, &'static str)> {
    match crate::recorder::logo::kind(b)? {
        k @ ("png" | "jpg" | "webp", _) => Some(k),
        _ => None,
    }
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/slates", get(list).post(add).layer(axum::extract::DefaultBodyLimit::max(MAX + 4096)))
        .route("/api/slates/{id}", delete(remove))
        .route("/api/slates/{id}/image", get(image))
        .route("/api/slates/source/{recorder}", put(choose))
}

async fn list(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.can("stream.view") || me.is_admin())?;
    let i = load(&s.data_dir);
    Ok(Json(json!({ "images": i.images, "sources": i.sources })))
}

#[derive(Deserialize)]
struct Named {
    #[serde(default)]
    name: String,
}

async fn add(State(s): State<Shared>, CurrentUser(me): CurrentUser, Query(q): Query<Named>, body: Bytes) -> ApiResult<Json<Image>> {
    require(me.can("stream.run"))?;
    if body.len() > MAX {
        return Err(bad("an image is 10 MB at most"));
    }
    let (ext, _) = kind(&body).ok_or_else(|| bad("the image is a PNG, a JPEG or a WebP"))?;
    let name: String = q.name.trim().chars().filter(|c| !c.is_control()).take(80).collect();
    let img = Image {
        id: crate::api::new_id(),
        name: if name.is_empty() { "image".into() } else { name },
        ext: ext.into(),
        added: crate::recorder::now_ms(),
        by: me.username.clone(),
    };
    let data_dir = s.data_dir.clone();
    let saved = img.clone();
    crate::api::blocking(move || -> Result<(), String> {
        let _g = LOCK.lock().unwrap();
        std::fs::create_dir_all(dir(&data_dir)).map_err(|e| e.to_string())?;
        std::fs::write(dir(&data_dir).join(format!("{}.{}", saved.id, saved.ext)), &body).map_err(|e| e.to_string())?;
        let mut i = load(&data_dir);
        i.images.push(saved);
        save(&data_dir, &i)
    })
    .await?
    .map_err(bad)?;
    tracing::info!("{} added the slate image {}", me.username, img.name);
    s.changed("slates");
    Ok(Json(img))
}

async fn remove(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.can("stream.run"))?;
    let data_dir = s.data_dir.clone();
    let gone = crate::api::blocking(move || -> Result<Option<Image>, String> {
        let _g = LOCK.lock().unwrap();
        let mut i = load(&data_dir);
        let Some(pos) = i.images.iter().position(|x| x.id == id) else { return Ok(None) };
        let img = i.images.remove(pos);
        // The sources that had it go back to the default slate.
        i.sources.retain(|_, v| *v != img.id);
        save(&data_dir, &i)?;
        let _ = std::fs::remove_file(dir(&data_dir).join(format!("{}.{}", img.id, img.ext)));
        Ok(Some(img))
    })
    .await?
    .map_err(bad)?
    .ok_or_else(|| not_found("image"))?;
    tracing::info!("{} deleted the slate image {}", me.username, gone.name);
    s.changed("slates");
    Ok(StatusCode::NO_CONTENT)
}

async fn image(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<Response> {
    require(me.can("stream.view") || me.is_admin())?;
    let img = load(&s.data_dir).images.into_iter().find(|x| x.id == id).ok_or_else(|| not_found("image"))?;
    let data = tokio::fs::read(dir(&s.data_dir).join(format!("{}.{}", img.id, img.ext))).await.map_err(|_| not_found("image"))?;
    let mime = kind(&data).map(|k| k.1).unwrap_or("application/octet-stream");
    let mut r = Response::new(axum::body::Body::from(data));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    Ok(r)
}

#[derive(Deserialize)]
struct Choice {
    /// The image, or none: the default slate.
    image: Option<String>,
}

async fn choose(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(recorder): UrlPath<String>, Json(c): Json<Choice>) -> ApiResult<StatusCode> {
    require(me.can("stream.run"))?;
    s.recorders.get(&recorder).ok_or_else(|| not_found("recorder"))?;
    let data_dir = s.data_dir.clone();
    crate::api::blocking(move || -> Result<(), String> {
        let _g = LOCK.lock().unwrap();
        let mut i = load(&data_dir);
        match c.image {
            Some(id) => {
                if !i.images.iter().any(|x| x.id == id) {
                    return Err("this image no longer exists".into());
                }
                i.sources.insert(recorder, id);
            }
            None => {
                i.sources.remove(&recorder);
            }
        }
        save(&data_dir, &i)
    })
    .await?
    .map_err(bad)?;
    s.changed("slates");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind(b"\x89PNG\r\n\x1a\n...."), Some(("png", "image/png")));
        assert!(kind(b"GIF89a....").is_none());
        assert!(kind(b"<svg xmlns=...>").is_none());
    }
}
