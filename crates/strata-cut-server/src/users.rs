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
    role: Role,
    #[serde(default)]
    disabled: bool,
    /// Empty: random (SSO on) on creation; unchanged on edit.
    #[serde(default)]
    password: String,
    #[serde(default)]
    role_fixed: bool,
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
    let password = if b.password.is_empty() && sso { auth::random_password() } else { b.password };
    let a = s.auth.clone();
    let user = blocking(move || a.add(NewUser { username: b.username, display_name: b.display_name, role: b.role, disabled: b.disabled, password: Some(password), role_fixed: b.role_fixed, sso: String::new() })).await?.map_err(bad)?;
    tracing::info!("{} created the account {} ({})", me.username, user.username, user.role.as_str());
    s.changed("users");
    Ok(Json(user.public()))
}

async fn update(State(s): State<Shared>, CurrentUser(me): CurrentUser, Path(id): Path<String>, Json(b): Json<UserBody>) -> ApiResult<Json<PublicUser>> {
    require(me.is_admin())?;
    let a = s.auth.clone();
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
            u.role = b.role;
            u.disabled = b.disabled;
            u.role_fixed = b.role_fixed;
            Ok(())
        })?;
        if b.password.is_empty() { Ok(user) } else { a.set_password(&id, &b.password) }
    })
    .await?
    .map_err(no_such)?;
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
