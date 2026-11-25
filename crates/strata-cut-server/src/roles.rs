//! What an account may do, function by function (Système › Accès › Autorisations). An admin may
//! do everything (and alone the Système pages); the others get what their groups give
//! (`crate::groups`). The former roles' rights (`roles.json`) become the base groups' at the first
//! start (`former`).
use std::path::Path;

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

use crate::{
    api::{require, ApiResult},
    auth::{CurrentUser, Role},
    state::Shared,
};

/// The rights, in the order shown, each with its part of the app and what it lets do (English).
pub const PERMS: [(&str, &str, &str); 14] = [
    ("schedule.view", "Schedule", "See the Régie"),
    ("schedule.rec", "Schedule", "Start and stop recordings"),
    ("schedule.edit", "Schedule", "Program recordings"),
    ("extract.view", "Extraction", "Watch and listen to the loops"),
    ("extract.cut", "Extraction", "Make clips"),
    ("extract.send", "Extraction", "Send to the file servers"),
    ("clips.view", "Clips", "See and download their clips"),
    ("clips.all", "Clips", "See every clip"),
    ("clips.share", "Clips", "Share their clips"),
    ("clips.delete", "Clips", "Delete their clips"),
    ("stream.view", "Streamer", "See the Streamer"),
    ("stream.program", "Streamer", "Prepare the output"),
    ("stream.pilot", "Streamer", "Pilot the web pages"),
    ("stream.run", "Streamer", "Start and stop the streams"),
];

/// Every right (an admin's).
pub fn all() -> Vec<String> {
    PERMS.iter().map(|p| p.0.to_string()).collect()
}

/// The former roles' rights, before they could be changed.
fn former_defaults(role: Role) -> Vec<&'static str> {
    match role {
        Role::Admin => PERMS.iter().map(|p| p.0).collect(),
        Role::Operator => vec!["extract.view", "extract.cut", "clips.view", "clips.share", "clips.delete", "schedule.view", "stream.view", "stream.run"],
        Role::Streamer => vec!["extract.view", "clips.view", "schedule.view", "stream.view", "stream.run"],
        Role::Viewer => vec!["extract.view", "clips.view", "schedule.view"],
    }
}

/// A former right as the rights it was split into: nothing that was allowed is taken away.
fn split(p: &str) -> &'static [&'static str] {
    match p {
        "schedule.view" => &["schedule.view"],
        "schedule.edit" => &["schedule.rec", "schedule.edit"],
        "extract.view" => &["extract.view"],
        "extract.cut" => &["extract.cut", "extract.send"],
        "clips.view" => &["clips.view"],
        "clips.share" => &["clips.share"],
        "clips.delete" => &["clips.delete"],
        "stream.view" => &["stream.view"],
        "stream.run" => &["stream.program", "stream.pilot", "stream.run"],
        _ => &[],
    }
}

/// What a former role could do (as set in `roles.json`, else its defaults), in today's rights.
pub fn former(data_dir: &Path, role: Role) -> Vec<String> {
    let saved: std::collections::BTreeMap<Role, Vec<String>> = crate::paths::read_json(&data_dir.join("roles.json")).unwrap_or_default();
    let old: Vec<String> = saved.get(&role).cloned().unwrap_or_else(|| former_defaults(role).into_iter().map(String::from).collect());
    let mut out: Vec<String> = old.iter().flat_map(|p| split(p).iter().map(|s| s.to_string())).collect();
    let order: Vec<&str> = PERMS.iter().map(|p| p.0).collect();
    out.sort_by_key(|p| order.iter().position(|k| k == p));
    out.dedup();
    out
}

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/roles", get(catalogue))
}

/// The rights there are (the Autorisations page; each group's are in `/api/groups`).
async fn catalogue(CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    Ok(Json(json!({ "perms": PERMS.iter().map(|(key, part, label)| json!({"key": key, "part": part, "label": label})).collect::<Vec<_>>() })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn former_rights_kept() {
        let dir = std::env::temp_dir().join(format!("sc-roles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let op = former(&dir, Role::Operator);
        assert!(op.contains(&"extract.send".to_string()) && op.contains(&"stream.pilot".to_string()) && !op.contains(&"schedule.rec".to_string()));
        assert_eq!(former(&dir, Role::Viewer), vec!["schedule.view", "extract.view", "clips.view"]);
        std::fs::write(dir.join("roles.json"), r#"{"viewer":["schedule.view","schedule.edit"]}"#).unwrap();
        assert_eq!(former(&dir, Role::Viewer), vec!["schedule.view", "schedule.rec", "schedule.edit"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
