//! What each role may do (Système › Accès › Rôles), function by function. An admin may do
//! everything (and alone the Système pages); the operators, the streamers and the readers get
//! what is ticked for their role, kept in `roles.json` (each role's defaults until changed).
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::RwLock,
};

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};

use crate::{
    api::{bad, require, ApiResult},
    auth::{CurrentUser, Role},
    state::Shared,
};

/// The rights, in the order shown, each with its part of the app and what it lets do (English).
pub const PERMS: [(&str, &str, &str); 9] = [
    ("extract.view", "Extraction", "Watch and listen to the loops"),
    ("extract.cut", "Extraction", "Make clips"),
    ("clips.view", "Clips", "See and download the clips (theirs and those shared with them)"),
    ("clips.share", "Clips", "Share their clips"),
    ("clips.delete", "Clips", "Delete their clips"),
    ("schedule.view", "Schedule", "See what is recorded, and when"),
    ("schedule.edit", "Schedule", "Start and stop recordings, program slots"),
    ("stream.view", "Streamer", "See the streams"),
    ("stream.run", "Streamer", "Start, stop and test streams"),
];

/// The roles whose rights are set here (an admin has them all).
pub const EDITABLE: [Role; 3] = [Role::Operator, Role::Streamer, Role::Viewer];

fn defaults(role: Role) -> Vec<&'static str> {
    match role {
        Role::Admin => PERMS.iter().map(|p| p.0).collect(),
        Role::Operator => vec!["extract.view", "extract.cut", "clips.view", "clips.share", "clips.delete", "schedule.view", "stream.view", "stream.run"],
        Role::Streamer => vec!["extract.view", "clips.view", "schedule.view", "stream.view", "stream.run"],
        Role::Viewer => vec!["extract.view", "clips.view", "schedule.view"],
    }
}

static TABLE: RwLock<Option<BTreeMap<Role, Vec<String>>>> = RwLock::new(None);
static FILE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Reads `roles.json` (once, at start).
pub fn load(data_dir: &Path) {
    let path = data_dir.join("roles.json");
    let saved: BTreeMap<Role, Vec<String>> = crate::paths::read_json(&path).unwrap_or_default();
    *TABLE.write().unwrap() = Some(saved);
    *FILE.write().unwrap() = Some(path);
}

/// The rights of a role.
pub fn of(role: Role) -> Vec<String> {
    if role == Role::Admin {
        return defaults(Role::Admin).into_iter().map(String::from).collect();
    }
    let table = TABLE.read().unwrap();
    match table.as_ref().and_then(|t| t.get(&role)) {
        Some(list) => list.clone(),
        None => defaults(role).into_iter().map(String::from).collect(),
    }
}

pub fn allowed(role: Role, perm: &str) -> bool {
    role == Role::Admin || of(role).iter().any(|p| p == perm)
}

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/roles", get(get_roles).put(put_roles))
}

fn table_json() -> Value {
    json!({
        "perms": PERMS.iter().map(|(key, part, label)| json!({"key": key, "part": part, "label": label})).collect::<Vec<_>>(),
        "roles": EDITABLE.iter().map(|r| (r.as_str().to_string(), json!(of(*r)))).collect::<serde_json::Map<String, Value>>(),
    })
}

async fn get_roles(CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    Ok(Json(table_json()))
}

async fn put_roles(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<BTreeMap<String, Vec<String>>>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let known: Vec<&str> = PERMS.iter().map(|p| p.0).collect();
    let mut table = BTreeMap::new();
    for role in EDITABLE {
        let mut list: Vec<String> = b.get(role.as_str()).cloned().unwrap_or_else(|| of(role)).into_iter().filter(|p| known.contains(&p.as_str())).collect();
        list.sort_by_key(|p| known.iter().position(|k| k == p));
        list.dedup();
        table.insert(role, list);
    }
    let path = FILE.read().unwrap().clone().ok_or_else(|| bad("the roles are not loaded"))?;
    crate::paths::write_json(&path, &table, false).map_err(bad)?;
    *TABLE.write().unwrap() = Some(table);
    tracing::info!("{} changed the roles' rights", me.username);
    // Every client asks again for its account (its rights) and redraws.
    s.changed("roles");
    Ok(Json(table_json()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rights() {
        assert!(allowed(Role::Admin, "stream.run"));
        assert!(allowed(Role::Streamer, "stream.run") && !allowed(Role::Streamer, "extract.cut"));
        assert!(allowed(Role::Viewer, "extract.view") && !allowed(Role::Viewer, "schedule.edit"));
        assert!(!allowed(Role::Operator, "schedule.edit"));
    }
}
