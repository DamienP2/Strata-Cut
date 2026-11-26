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

/// The rights, in the order shown, each with its part of the app, the clips' "theirs" / "the
/// others'" (else empty), and what it lets do (English).
pub static PERMS: [(&str, &str, &str, &str); 25] = [
    ("control.access", "Schedule", "", "Access"),
    ("control.rec.start", "Schedule", "", "Start a recording"),
    ("control.rec.stop", "Schedule", "", "Stop any recording"),
    ("control.rec.stop.own", "Schedule", "", "Stop only their recordings"),
    ("control.slots.own", "Schedule", "", "Program or change their recordings"),
    ("control.slots.all", "Schedule", "", "Change or delete the others' recordings"),
    ("cut.access", "Extraction", "", "Access"),
    ("cut.view", "Extraction", "", "Watch the recordings"),
    ("cut.make", "Extraction", "", "Make clips"),
    ("cut.send", "Extraction", "", "Send to the file servers"),
    ("stream.access", "Streamer", "", "Access"),
    ("stream.mixer", "Streamer", "", "Control the mixer"),
    ("stream.run.own", "Streamer", "", "Start and stop their streams"),
    ("stream.run.all", "Streamer", "", "Start and stop the others' streams"),
    ("clips.access", "Clips", "", "Access"),
    ("clips.own.view", "Clips", "Theirs", "See"),
    ("clips.own.download", "Clips", "Theirs", "Download"),
    ("clips.own.share", "Clips", "Theirs", "Share"),
    ("clips.own.upload", "Clips", "Theirs", "Upload"),
    ("clips.own.delete", "Clips", "Theirs", "Delete"),
    ("clips.all.view", "Clips", "The others'", "See"),
    ("clips.all.download", "Clips", "The others'", "Download"),
    ("clips.all.share", "Clips", "The others'", "Share"),
    ("clips.all.upload", "Clips", "The others'", "Upload"),
    ("clips.all.delete", "Clips", "The others'", "Delete"),
];

/// A right of an earlier version as today's (today's as itself): nothing allowed is taken away.
pub fn upgrade(p: &str) -> &'static [&'static str] {
    match p {
        "schedule.view" => &["control.access"],
        "schedule.rec" => &["control.rec.start", "control.rec.stop"],
        "schedule.edit" => &["control.slots.own", "control.slots.all"],
        "extract.view" => &["cut.access", "cut.view"],
        "extract.cut" => &["cut.make"],
        "extract.send" => &["cut.send", "clips.own.upload"],
        "clips.view" => &["clips.access", "clips.own.view", "clips.own.download"],
        "clips.all" => &["clips.all.view", "clips.all.download"],
        "clips.share" => &["clips.own.share"],
        "clips.delete" => &["clips.own.delete"],
        "stream.view" => &["stream.access"],
        "stream.program" | "stream.pilot" => &["stream.mixer"],
        "stream.run" => &["stream.run.own", "stream.run.all"],
        _ => match PERMS.iter().find(|x| x.0 == p) {
            Some(x) => std::slice::from_ref(&x.0),
            None => &[],
        },
    }
}

/// Rights of any version as today's, in their order, without repeats.
pub fn upgraded(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = list.iter().flat_map(|p| upgrade(p).iter().map(|s| s.to_string())).collect();
    let order: Vec<&str> = PERMS.iter().map(|p| p.0).collect();
    out.sort_by_key(|p| order.iter().position(|k| k == p));
    out.dedup();
    out
}

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

/// A first-version right as the 14 rights it was split into (then `upgrade`d).
fn split(p: &str) -> &'static [&'static str] {
    match p {
        "schedule.edit" => &["schedule.rec", "schedule.edit"],
        "extract.cut" => &["extract.cut", "extract.send"],
        "stream.run" => &["stream.program", "stream.pilot", "stream.run"],
        "schedule.view" => &["schedule.view"],
        "extract.view" => &["extract.view"],
        "clips.view" => &["clips.view"],
        "clips.share" => &["clips.share"],
        "clips.delete" => &["clips.delete"],
        "stream.view" => &["stream.view"],
        _ => &[],
    }
}

/// What a former role could do (as set in `roles.json`, else its defaults), in today's rights.
pub fn former(data_dir: &Path, role: Role) -> Vec<String> {
    let saved: std::collections::BTreeMap<Role, Vec<String>> = crate::paths::read_json(&data_dir.join("roles.json")).unwrap_or_default();
    let old: Vec<String> = saved.get(&role).cloned().unwrap_or_else(|| former_defaults(role).into_iter().map(String::from).collect());
    let mid: Vec<String> = old.iter().flat_map(|p| split(p).iter().map(|s| s.to_string())).collect();
    upgraded(&mid)
}

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/roles", get(catalogue))
}

/// The rights there are (the Autorisations page; each group's are in `/api/groups`).
async fn catalogue(CurrentUser(me): CurrentUser) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    Ok(Json(json!({ "perms": PERMS.iter().map(|(key, part, sub, label)| json!({"key": key, "part": part, "sub": sub, "label": label})).collect::<Vec<_>>() })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn former_rights_kept() {
        let dir = std::env::temp_dir().join(format!("sc-roles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let op = former(&dir, Role::Operator);
        assert!(op.contains(&"cut.send".to_string()) && op.contains(&"stream.mixer".to_string()) && !op.contains(&"control.rec.start".to_string()));
        assert_eq!(former(&dir, Role::Viewer), vec!["control.access", "cut.access", "cut.view", "clips.access", "clips.own.view", "clips.own.download"]);
        std::fs::write(dir.join("roles.json"), r#"{"viewer":["schedule.view","schedule.edit"]}"#).unwrap();
        assert_eq!(former(&dir, Role::Viewer), vec!["control.access", "control.rec.start", "control.rec.stop", "control.slots.own", "control.slots.all"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upgrades() {
        assert_eq!(upgraded(&["stream.pilot".into(), "stream.program".into(), "cut.make".into()]), vec!["cut.make", "stream.mixer"]);
        assert!(upgraded(&["nonsense".into()]).is_empty());
    }
}
