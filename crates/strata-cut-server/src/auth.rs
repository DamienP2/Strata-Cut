//! Accounts, passwords and sessions. STUB: the interface the other modules rely on.
//! (to be completed: users.json, Argon2, sessions, login/logout/setup — spec § 5.1–5.3)
use axum::{extract::FromRequestParts, http::{request::Parts, HeaderMap, StatusCode}, Router};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::state::Shared;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// Everything: recorders, users, system.
    Admin,
    /// Cuts clips and manages them.
    Operator,
    /// Watches the loops and downloads clips.
    Viewer,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    pub role: Role,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub lang: String,
    #[serde(default)]
    pub theme: String,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
    /// Admins and operators cut clips.
    pub fn can_extract(&self) -> bool {
        matches!(self.role, Role::Admin | Role::Operator)
    }
    pub fn name(&self) -> &str {
        if self.display_name.is_empty() { &self.username } else { &self.display_name }
    }
}

pub struct Auth {}

impl Auth {
    pub fn load(_data_dir: &Path) -> Self {
        Auth {}
    }
    pub fn needs_setup(&self) -> bool {
        true
    }
    /// The user a session token belongs to (and extends the session).
    pub fn user_for_token(&self, _token: &str) -> Option<User> {
        None
    }
}

/// `Authorization: Bearer <token>`, else the `sc_session` cookie.
pub fn token_from(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(t) = v.strip_prefix("Bearer ") {
            return Some(t.trim().to_string());
        }
    }
    headers.get_all("cookie").iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|c| c.trim().strip_prefix("sc_session=").map(str::to_string))
}

/// The signed-in user, else 401 "login required".
pub struct CurrentUser(pub User);

impl FromRequestParts<Shared> for CurrentUser {
    type Rejection = (StatusCode, String);
    async fn from_request_parts(parts: &mut Parts, s: &Shared) -> Result<Self, Self::Rejection> {
        token_from(&parts.headers)
            .and_then(|t| s.auth.user_for_token(&t))
            .filter(|u| !u.disabled)
            .map(CurrentUser)
            .ok_or((StatusCode::UNAUTHORIZED, "login required".into()))
    }
}

pub fn routes() -> Router<Shared> {
    Router::new()
}
