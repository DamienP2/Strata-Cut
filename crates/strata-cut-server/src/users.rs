//! Accounts, for admins (spec § 5.4).
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;

use crate::{
    api::{bad, blocking, not_found, require, ApiResult},
    auth::{self, CurrentUser, NewUser, PublicUser, Role},
    state::Shared,
};

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/users", get(list).post(create)).route("/api/users/{id}", put(update).delete(remove))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    display_name: String,
    /// An admin (else what its groups give). None: the role given, as before.
    #[serde(default)]
    admin: Option<bool>,
    #[serde(default)]
    role: Option<Role>,
    /// The groups it is in (None: left as they are).
    #[serde(default)]
    groups: Option<Vec<String>>,
    #[serde(default)]
    disabled: bool,
    /// Empty: random (SSO on) on creation; unchanged on edit.
    #[serde(default)]
    password: String,
    #[serde(default)]
    role_fixed: bool,
}

/// The role kept for an account: admin or not (a former role stays as it was: it only says which
/// base group the directory puts it in).
fn role_of(b: &UserBody, was: Role) -> Role {
    match (b.admin, b.role) {
        (Some(true), _) => Role::Admin,
        (Some(false), _) if was == Role::Admin => Role::Viewer,
        (Some(false), _) => was,
        (None, Some(r)) => r,
        (None, None) => was,
    }
}

/// Its groups set; told to every client (the rights may have changed).
fn set_groups(s: &Shared, id: &str, groups: &Option<Vec<String>>) -> Result<(), String> {
    if let Some(g) = groups {
        if crate::groups::set_member(id, g)? {
            s.changed("groups");
            s.changed("roles");
        }
    }
    Ok(())
}

fn no_such(e: String) -> (StatusCode, String) {
    if e == "no such user" { not_found("user") } else { bad(e) }
}

async fn list(State(s): State<Shared>, CurrentUser(me): CurrentUser) -> ApiResult<Json<Vec<PublicUser>>> {
    require(me.is_admin())?;
    let mut users: Vec<PublicUser> = s.auth.users().iter().map(PublicUser::from).collect();
    users.sort_by_key(|u| u.username.to_lowercase());
    Ok(Json(users))
}

async fn create(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<UserBody>) -> ApiResult<Json<PublicUser>> {
    require(me.is_admin())?;
    let sso = s.config.load().security.sso.active();
    let role = role_of(&b, Role::Viewer);
    let password = if b.password.is_empty() && sso { auth::random_password() } else { b.password };
    let a = s.auth.clone();
    let groups = b.groups.clone();
    let user = blocking(move || a.add(NewUser { username: b.username, display_name: b.display_name, role, disabled: b.disabled, password: Some(password), role_fixed: b.role_fixed, sso: String::new() })).await?.map_err(bad)?;
    set_groups(&s, &user.id, &groups).map_err(bad)?;
    tracing::info!("{} created the account {} ({})", me.username, user.username, user.role.as_str());
    s.changed("users");
    Ok(Json(user.public()))
}

async fn update(State(s): State<Shared>, CurrentUser(me): CurrentUser, Path(id): Path<String>, Json(b): Json<UserBody>) -> ApiResult<Json<PublicUser>> {
    require(me.is_admin())?;
    let a = s.auth.clone();
    let groups = b.groups.clone();
    let uid = id.clone();
    let user = blocking(move || -> Result<auth::User, String> {
        let username = b.username.trim().to_string();
        auth::validate_username(&username)?;
        if !b.password.is_empty() {
            auth::validate_password(&b.password)?;
        }
        let user = a.modify(&id, |u| {
            u.username = username;
            let dn = b.display_name.trim();
            u.display_name = if dn.is_empty() { u.username.clone() } else { dn.to_string() };
            u.role = role_of(&b, u.role);
            u.disabled = b.disabled;
            u.role_fixed = b.role_fixed;
            Ok(())
        })?;
        if b.password.is_empty() { Ok(user) } else { a.set_password(&id, &b.password) }
    })
    .await?
    .map_err(no_such)?;
    set_groups(&s, &uid, &groups).map_err(bad)?;
    tracing::info!("{} changed the account {}", me.username, user.username);
    s.changed("users");
    Ok(Json(user.public()))
}

async fn remove(State(s): State<Shared>, CurrentUser(me): CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    require(me.is_admin())?;
    if id == me.id {
        return Err(bad("you cannot delete your own account"));
    }
    let a = s.auth.clone();
    let i = id.clone();
    let user = blocking(move || a.delete(&i)).await?.map_err(no_such)?;
    let _ = std::fs::remove_file(crate::prefs::path(&s.data_dir, &id));
    tracing::info!("{} deleted the account {}", me.username, user.username);
    s.changed("users");
    Ok(StatusCode::NO_CONTENT)
}
