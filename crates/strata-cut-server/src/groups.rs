//! Groups of users (Système › Groupes): a name, its members (accounts' ids) and what its members
//! may do (`crate::roles::PERMS`), kept in `groups.json`. Three base groups (operators, streamers,
//! readers) are always there: made at the first start from the accounts' former roles, they can
//! be changed but not deleted. An account gets what each of its groups gives; an admin, all.
//! Every account can read the groups' names (to share a clip with one); admins make them.
use std::{
    path::{Path, PathBuf},
    sync::RwLock,
};

use axum::{
    extract::{Path as UrlPath, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    api::{bad, not_found, require, ApiResult},
    auth::{CurrentUser, Role, User},
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
    /// What its members may do.
    #[serde(default)]
    pub rights: Vec<String>,
    /// One of the base groups (its name is fixed; it cannot be deleted).
    #[serde(default)]
    pub base: bool,
    /// The sources its members see (Control Room, Cut, Streamer).
    #[serde(default)]
    pub sources: Scope,
    /// The file servers its members may send clips to.
    #[serde(default)]
    pub sends: Scope,
    /// The Streamer's destinations its members may stream to.
    #[serde(default)]
    pub dests: Scope,
}

/// All of them, or these only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    #[serde(default = "yes")]
    pub all: bool,
    #[serde(default)]
    pub ids: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Default for Scope {
    fn default() -> Self {
        Scope { all: true, ids: Vec::new() }
    }
}

impl Scope {
    pub fn has(&self, id: &str) -> bool {
        self.all || self.ids.iter().any(|i| i == id)
    }
}

/// The base groups: their ids, and the former role each stands for.
pub const BASE: [(&str, Role); 3] = [("operators", Role::Operator), ("streamers", Role::Streamer), ("viewers", Role::Viewer)];
/// Their names (English, translated where shown).
fn base_name(id: &str) -> &'static str {
    match id {
        "operators" => "Operators",
        "streamers" => "Streamers",
        _ => "Viewers",
    }
}

/// The base group of a former role (none for an admin).
pub fn base_of(role: Role) -> Option<&'static str> {
    BASE.iter().find(|(_, r)| *r == role).map(|(id, _)| *id)
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("groups.json")
}

/// The groups as saved (read at start, kept up to date): what `User::can` reads.
static ALL: RwLock<Vec<Group>> = RwLock::new(Vec::new());
static FILE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Reads the groups at start; the first time, makes the base groups from the accounts' roles
/// (their members, and the rights their roles had).
pub fn load(data_dir: &Path, users: &[User]) {
    let file = path(data_dir);
    let mut all: Vec<Group> = crate::paths::read_json(&file).unwrap_or_default();
    let mut made = false;
    for (id, role) in BASE {
        if all.iter().any(|g| g.id == id) {
            if let Some(g) = all.iter_mut().find(|g| g.id == id) {
                g.base = true;
                g.name = base_name(id).into();
            }
            continue;
        }
        let members = users.iter().filter(|u| u.role == role).map(|u| u.id.clone()).collect();
        all.push(Group { id: id.into(), name: base_name(id).into(), members, rights: crate::roles::former(data_dir, role), base: true, ..Default::default() });
        made = true;
    }
    // Rights of an earlier version as today's.
    for g in &mut all {
        let up = crate::roles::upgraded(&g.rights);
        if up != g.rights {
            g.rights = up;
            made = true;
        }
    }
    if made {
        if let Err(e) = crate::paths::write_json(&file, &all, false) {
            tracing::warn!("cannot save the groups: {e}");
        } else {
            tracing::info!("the groups are saved with today's rights");
        }
    }
    *ALL.write().unwrap() = all;
    *FILE.write().unwrap() = Some(file);
}

/// The groups, their members limited to the accounts that still exist; the base ones first.
pub fn list(s: &Shared) -> Vec<Group> {
    let users: Vec<String> = s.auth.users().into_iter().map(|u| u.id).collect();
    let mut all = ALL.read().unwrap().clone();
    for g in &mut all {
        g.members.retain(|m| users.contains(m));
    }
    all.sort_by_key(|g| (!g.base, BASE.iter().position(|(id, _)| *id == g.id).unwrap_or(9), g.name.to_lowercase()));
    all
}

/// The groups an account is in.
pub fn of_id(user: &str) -> Vec<String> {
    ALL.read().unwrap().iter().filter(|g| g.members.iter().any(|m| m == user)).map(|g| g.id.clone()).collect()
}

/// The groups an account is in.
pub fn of(_s: &Shared, user: &str) -> Vec<String> {
    of_id(user)
}

/// What an account's groups let it do (an admin's not counted: it may do all).
pub fn rights_of(user: &str) -> Vec<String> {
    let mut out: Vec<String> = ALL.read().unwrap().iter().filter(|g| g.members.iter().any(|m| m == user)).flat_map(|g| g.rights.iter().cloned()).collect();
    let order: Vec<&str> = crate::roles::PERMS.iter().map(|p| p.0).collect();
    out.sort_by_key(|p| order.iter().position(|k| k == p).unwrap_or(usize::MAX));
    out.dedup();
    out
}

/// May one of the account's groups do this?
pub fn allows(user: &str, perm: &str) -> bool {
    any_of(user, |g| g.rights.iter().any(|r| r == perm))
}

/// Does one of the account's groups say so?
fn any_of(user: &str, f: impl Fn(&Group) -> bool) -> bool {
    ALL.read().unwrap().iter().any(|g| g.members.iter().any(|m| m == user) && f(g))
}

/// May this account see this source (an admin: all)?
pub fn sees_source(me: &User, id: &str) -> bool {
    me.is_admin() || any_of(&me.id, |g| g.sources.has(id))
}

/// The sources an account's groups give it (None: all of them).
pub fn sources_of(user: &str) -> Option<Vec<String>> {
    let all = ALL.read().unwrap();
    let mine: Vec<&Group> = all.iter().filter(|g| g.members.iter().any(|m| m == user)).collect();
    if mine.iter().any(|g| g.sources.all) {
        return None;
    }
    let mut ids: Vec<String> = mine.iter().flat_map(|g| g.sources.ids.iter().cloned()).collect();
    ids.sort();
    ids.dedup();
    Some(ids)
}

/// May this account send clips to this file server (the right to send apart)?
pub fn may_send_to(me: &User, target: &str) -> bool {
    me.is_admin() || any_of(&me.id, |g| g.sends.has(target))
}

/// May this account stream to this destination (the right to stream apart)?
pub fn may_stream_to(me: &User, dest: &str) -> bool {
    me.is_admin() || any_of(&me.id, |g| g.dests.has(dest))
}

/// The file servers' former rule (who may send there) as the groups' lists, once: a server
/// for every account that extracts goes to every group; one for some groups, to these.
pub fn adopt_send_rules(rules: &[(String, String, Vec<String>)]) {
    if !rules.iter().any(|(_, who, _)| who == "groups" || who == "admins") {
        return;
    }
    let _one = WRITE.lock().unwrap();
    let mut all = ALL.read().unwrap().clone();
    for g in &mut all {
        let ids: Vec<String> = rules.iter().filter(|(_, who, groups)| who == "extract" || (who == "groups" && groups.contains(&g.id))).map(|(id, _, _)| id.clone()).collect();
        g.sends = Scope { all: false, ids };
    }
    if let Err(e) = save(all) {
        tracing::warn!("cannot save the groups: {e}");
    }
}

static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn save(all: Vec<Group>) -> Result<(), String> {
    let file = FILE.read().unwrap().clone().ok_or("the groups are not loaded")?;
    crate::paths::write_json(&file, &all, false).map_err(|e| e.to_string())?;
    *ALL.write().unwrap() = all;
    Ok(())
}

/// Puts an account in these groups only (the others it leaves); unknown ids are left out.
pub fn set_member(user: &str, groups: &[String]) -> Result<bool, String> {
    let _one = WRITE.lock().unwrap();
    let mut all = ALL.read().unwrap().clone();
    let mut changed = false;
    for g in &mut all {
        let want = groups.contains(&g.id);
        let is = g.members.iter().any(|m| m == user);
        if want && !is {
            g.members.push(user.to_string());
            g.members.sort();
            changed = true;
        } else if !want && is {
            g.members.retain(|m| m != user);
            changed = true;
        }
    }
    if changed {
        save(all)?;
    }
    Ok(changed)
}

/// The base group of a role given by the directory (SSO): the account in it, out of the other
/// base groups (the groups made here are left as they are). An admin's are left too.
pub fn set_base(user: &str, role: Role) -> Result<bool, String> {
    let mut want: Vec<String> = of_id(user).into_iter().filter(|g| !BASE.iter().any(|(id, _)| id == g)).collect();
    if role == Role::Admin {
        want.extend(of_id(user).into_iter().filter(|g| BASE.iter().any(|(id, _)| id == g)));
    } else if let Some(b) = base_of(role) {
        want.push(b.into());
    }
    set_member(user, &want)
}

/// The rights known, in their order, without repeats.
fn clean_rights(list: &[String]) -> Vec<String> {
    let order: Vec<&str> = crate::roles::PERMS.iter().map(|p| p.0).collect();
    let mut out: Vec<String> = list.iter().filter(|p| order.contains(&p.as_str())).cloned().collect();
    out.sort_by_key(|p| order.iter().position(|k| k == p));
    out.dedup();
    out
}

#[derive(Deserialize)]
struct Body {
    #[serde(default)]
    name: String,
    #[serde(default)]
    members: Vec<String>,
    /// None: left as they are.
    #[serde(default)]
    rights: Option<Vec<String>>,
    #[serde(default)]
    sources: Option<Scope>,
    #[serde(default)]
    sends: Option<Scope>,
    #[serde(default)]
    dests: Option<Scope>,
}

fn check(s: &Shared, b: &Body, all: &[Group], id: &str, base: bool) -> Result<(String, Vec<String>), String> {
    let name = b.name.trim().to_string();
    if !base {
        if name.is_empty() {
            return Err("the name cannot be empty".into());
        }
        if name.chars().count() > 60 {
            return Err("the name has 60 characters at most".into());
        }
        if all.iter().any(|g| g.id != id && g.name.to_lowercase() == name.to_lowercase()) {
            return Err("a group already has this name".into());
        }
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

/// Every account sees the groups (to share a clip with one); only admins see their members and
/// what they give.
async fn get_all(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> Json<Vec<Group>> {
    let mut all = list(&s);
    if !me.is_admin() {
        for g in &mut all {
            g.members.retain(|m| *m == me.id);
            g.rights.clear();
            (g.sources, g.sends, g.dests) = Default::default();
        }
    }
    Json(all)
}

async fn create(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<Body>) -> ApiResult<Json<Group>> {
    require(me.is_admin())?;
    let _one = WRITE.lock().unwrap();
    let mut all = list(&s);
    let (name, members) = check(&s, &b, &all, "", false).map_err(bad)?;
    let g = Group {
        id: crate::api::new_id(),
        name,
        members,
        rights: clean_rights(&b.rights.clone().unwrap_or_default()),
        base: false,
        sources: b.sources.clone().unwrap_or_default(),
        sends: b.sends.clone().unwrap_or_default(),
        dests: b.dests.clone().unwrap_or_default(),
    };
    all.push(g.clone());
    save(all).map_err(bad)?;
    tracing::info!("{} created the group {}", me.username, g.name);
    s.changed("groups");
    s.changed("roles");
    Ok(Json(g))
}

async fn update(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<Body>) -> ApiResult<Json<Group>> {
    require(me.is_admin())?;
    let _one = WRITE.lock().unwrap();
    let mut all = list(&s);
    let base = all.iter().find(|g| g.id == id).ok_or_else(|| not_found("group"))?.base;
    let (name, members) = check(&s, &b, &all, &id, base).map_err(bad)?;
    let g = all.iter_mut().find(|g| g.id == id).ok_or_else(|| not_found("group"))?;
    if !base {
        g.name = name;
    }
    g.members = members;
    if let Some(r) = &b.rights {
        g.rights = clean_rights(r);
    }
    for (to, from) in [(&mut g.sources, &b.sources), (&mut g.sends, &b.sends), (&mut g.dests, &b.dests)] {
        if let Some(x) = from {
            *to = x.clone();
        }
    }
    let g = g.clone();
    save(all).map_err(bad)?;
    tracing::info!("{} changed the group {}", me.username, if g.name.is_empty() { &g.id } else { &g.name });
    s.changed("groups");
    // Every client asks again for its account (its rights) and redraws.
    s.changed("roles");
    Ok(Json(g))
}

async fn remove(State(s): State<Shared>, CurrentUser(me): CurrentUser, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    let _one = WRITE.lock().unwrap();
    let mut all = list(&s);
    match all.iter().find(|g| g.id == id) {
        None => return Err(not_found("group")),
        Some(g) if g.base => return Err(bad("a base group cannot be deleted")),
        _ => {}
    }
    all.retain(|g| g.id != id);
    save(all).map_err(bad)?;
    s.changed("groups");
    s.changed("roles");
    s.changed("clips");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_become_groups() {
        let dir = std::env::temp_dir().join(format!("sc-groups-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let op = User::without_password("op".into(), String::new(), Role::Operator, String::new());
        let st = User::without_password("st".into(), String::new(), Role::Streamer, String::new());
        let boss = User::without_password("boss".into(), String::new(), Role::Admin, String::new());
        load(&dir, &[op.clone(), st.clone(), boss.clone()]);
        assert_eq!(of_id(&op.id), vec!["operators"]);
        assert!(of_id(&boss.id).is_empty(), "an admin is in no base group");
        assert!(op.can("cut.make") && op.can("cut.send") && !op.can("control.rec.start"));
        assert!(st.can("stream.mixer") && !st.can("cut.make"));
        assert!(boss.can("clips.all.delete"));
        assert!(sees_source(&op, "any") && may_stream_to(&st, "any"), "all by default");
        // The directory makes the streamer a reader: out of the streamers, in the readers.
        assert!(set_base(&st.id, Role::Viewer).unwrap());
        assert_eq!(of_id(&st.id), vec!["viewers"]);
        assert!(!st.can("stream.run.own"));
        // Read again: the groups are kept, not made again.
        load(&dir, &[op.clone(), st.clone(), boss]);
        assert_eq!(of_id(&st.id), vec!["viewers"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
