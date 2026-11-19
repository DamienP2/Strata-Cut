//! Groups of users (Système › Groupes), to share clips with: a name and its members (accounts'
//! ids), kept in `groups.json`. Admins make them; everyone can read their names (to share).
use std::path::{Path, PathBuf};

use axum::{
    extract::{Path as UrlPath, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    api::{bad, not_found, require, ApiResult},
    auth::CurrentUser,
    state::Shared,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: String,
    pub name: String,
    /// The accounts' ids.
    #[serde(default)]
    pub members: Vec<String>,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("groups.json")
}

/// The groups, their members limited to the accounts that still exist.
pub fn list(s: &Shared) -> Vec<Group> {
    let users: Vec<String> = s.auth.users().into_iter().map(|u| u.id).collect();
    let mut all: Vec<Group> = crate::paths::read_json(&path(&s.data_dir)).unwrap_or_default();
    for g in &mut all {
        g.members.retain(|m| users.contains(m));
    }
    all.sort_by_key(|g| g.name.to_lowercase());
    all
}

/// The groups an account is in.
pub fn of(s: &Shared, user: &str) -> Vec<String> {
    list(s).into_iter().filter(|g| g.members.iter().any(|m| m == user)).map(|g| g.id).collect()
}

static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn save(s: &Shared, all: &[Group]) -> Result<(), String> {
    crate::paths::write_json(&path(&s.data_dir), all, false).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct Body {
    name: String,
    #[serde(default)]
    members: Vec<String>,
}

fn check(s: &Shared, b: &Body, all: &[Group], id: &str) -> Result<(String, Vec<String>), String> {
    let name = b.name.trim().to_string();
    if name.is_empty() {
        return Err("the name cannot be empty".into());
    }
    if name.chars().count() > 60 {
        return Err("the name has 60 characters at most".into());
    }
    if all.iter().any(|g| g.id != id && g.name.to_lowercase() == name.to_lowercase()) {
        return Err("a group already has this name".into());
    }
    let users: Vec<String> = s.auth.users().into_iter().map(|u| u.id).collect();
    let mut members: Vec<String> = b.members.iter().filter(|m| users.contains(m)).cloned().collect();
    members.sort();
    members.dedup();
    Ok((name, members))
}

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/groups", get(get_all).post(create)).route("/api/groups/{id}", put(update).delete(remove))
}

/// Every account sees the groups (to share a clip with one); only admins see their members.
async fn get_all(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> Json<Vec<Group>> {
    let mut all = list(&s);
    if !me.is_admin() {
        for g in &mut all {
            g.members.retain(|m| *m == me.id);
        }
    }
    Json(all)
}

async fn create(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<Body>) -> ApiResult<Json<Group>> {
    require(me.is_admin())?;
    let _one = WRITE.lock().unwrap();
    let mut all = list(&s);
    let (name, members) = check(&s, &b, &all, "").map_err(bad)?;
    let g = Group { id: crate::api::new_id(), name, members };
    all.push(g.clone());
    save(&s, &all).map_err(bad)?;
    tracing::info!("{} created the group {}", me.username, g.name);
    s.changed("groups");
    Ok(Json(g))
}

async fn update(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<Body>) -> ApiResult<Json<Group>> {
    require(me.is_admin())?;
    let _one = WRITE.lock().unwrap();
    let mut all = list(&s);
    let (name, members) = check(&s, &b, &all, &id).map_err(bad)?;
    let g = all.iter_mut().find(|g| g.id == id).ok_or_else(|| not_found("group"))?;
    g.name = name;
    g.members = members;
    let g = g.clone();
    save(&s, &all).map_err(bad)?;
    s.changed("groups");
    Ok(Json(g))
}

async fn remove(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    let _one = WRITE.lock().unwrap();
    let mut all = list(&s);
    let before = all.len();
    all.retain(|g| g.id != id);
    if all.len() == before {
        return Err(not_found("group"));
    }
    save(&s, &all).map_err(bad)?;
    s.changed("groups");
    s.changed("clips");
    Ok(StatusCode::NO_CONTENT)
}
