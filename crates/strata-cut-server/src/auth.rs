//! Accounts (`users.json`), passwords (Argon2), sessions, sign-in (spec § 5.1–5.3).
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{ConnectInfo, FromRequestParts, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{broadcast, Semaphore};

use crate::{
    api::{bad, ApiResult},
    guard,
    state::Shared,
};

pub const COOKIE: &str = "sc_session";
/// Sliding session length.
pub const SESSION_TTL: Duration = Duration::from_secs(14 * 24 * 3600);
pub const MIN_PASSWORD: usize = 8;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// Everything: recorders, users, system.
    Admin,
    /// Cuts clips and manages them.
    Operator,
    /// Sends the sources live to the destinations (Streamer).
    Streamer,
    /// Watches the loops and downloads clips.
    Viewer,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Admin, Role::Operator, Role::Streamer, Role::Viewer];

    /// Higher is more rights (admin > operator > streamer > viewer).
    pub fn rank(self) -> u8 {
        match self {
            Role::Admin => 4,
            Role::Operator => 3,
            Role::Streamer => 2,
            Role::Viewer => 1,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Operator => "operator",
            Role::Streamer => "streamer",
            Role::Viewer => "viewer",
        }
    }
}

impl std::str::FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Role::ALL.into_iter().find(|r| r.as_str().eq_ignore_ascii_case(s.trim())).ok_or_else(|| "unknown role".to_string())
    }
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
    /// "en" | "fr"; empty = the browser's.
    #[serde(default)]
    pub lang: String,
    /// "dark" | "light" | "system"; empty = dark.
    #[serde(default)]
    pub theme: String,
    /// "issuer|subject" of a directory (SSO) account.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sso: String,
    /// The role is set by hand, not by the directory groups.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub role_fixed: bool,
    /// Never sent to a client (see `PublicUser`); empty = no password sign-in.
    #[serde(default)]
    password_hash: String,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
    /// May this account do this (`crate::roles::PERMS`)? An admin may do everything; the others
    /// what their groups give.
    pub fn can(&self, perm: &str) -> bool {
        self.is_admin() || crate::groups::allows(&self.id, perm)
    }
    /// Makes clips.
    pub fn can_extract(&self) -> bool {
        self.can("cut.make")
    }
    pub fn name(&self) -> &str {
        if self.display_name.is_empty() { &self.username } else { &self.display_name }
    }
    pub fn public(&self) -> PublicUser {
        PublicUser::from(self)
    }
    /// An account without a password (directory sign-in).
    pub fn without_password(username: String, display_name: String, role: Role, sso: String) -> User {
        User {
            id: crate::api::new_id(),
            display_name: if display_name.trim().is_empty() { username.clone() } else { display_name.trim().to_string() },
            username,
            role,
            disabled: false,
            lang: String::new(),
            theme: String::new(),
            sso,
            role_fixed: false,
            password_hash: String::new(),
        }
    }
}

/// A user as the API sends it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PublicUser {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub disabled: bool,
    pub lang: String,
    pub theme: String,
    pub sso: bool,
    pub role_fixed: bool,
    /// What its groups let it do (an admin: everything); the interface shows only that.
    pub perms: Vec<String>,
    /// The groups it is in.
    pub groups: Vec<String>,
    /// The sources it sees (null: all).
    pub sources: Option<Vec<String>>,
}

impl From<&User> for PublicUser {
    fn from(u: &User) -> Self {
        PublicUser {
            id: u.id.clone(),
            username: u.username.clone(),
            display_name: u.name().to_string(),
            role: u.role,
            disabled: u.disabled,
            lang: u.lang.clone(),
            theme: u.theme.clone(),
            sso: !u.sso.is_empty(),
            role_fixed: u.role_fixed,
            perms: if u.is_admin() { crate::roles::all() } else { crate::groups::rights_of(&u.id) },
            groups: crate::groups::of_id(&u.id),
            sources: if u.is_admin() { None } else { crate::groups::sources_of(&u.id) },
        }
    }
}

/// A new account.
pub struct NewUser {
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub disabled: bool,
    /// None = no password (directory sign-in only).
    pub password: Option<String>,
    pub role_fixed: bool,
    pub sso: String,
}

/// Letters, digits and `. - _ @`, 64 at most.
pub fn validate_username(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("the user name cannot be empty".into());
    }
    if name.chars().count() > 64 {
        return Err("the user name is too long (64 characters at most)".into());
    }
    if !name.chars().all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '@')) {
        return Err("the user name can only have letters, digits and . - _ @".into());
    }
    Ok(())
}

pub fn validate_password(pw: &str) -> Result<(), String> {
    if pw.chars().count() < MIN_PASSWORD {
        return Err(format!("the password needs at least {MIN_PASSWORD} characters"));
    }
    Ok(())
}

/// Argon2 (default parameters), salt = 16 bytes of a v4 UUID.
pub fn hash_password(pw: &str) -> String {
    let salt = SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes()).expect("16-byte salt");
    Argon2::default().hash_password(pw.as_bytes(), &salt).expect("argon2").to_string()
}

pub fn verify_hash(hash: &str, pw: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

/// A hash checked for unknown names, so the answer time says nothing.
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("not a real password"))
}

/// A random password nobody knows (directory accounts).
pub fn random_password() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

pub fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

struct Session {
    user_id: String,
    expires: Instant,
}

struct Store {
    list: Vec<User>,
    /// users.json's (modified, len) when last read: it is read again when it changes (the CLI).
    stamp: Option<(SystemTime, u64)>,
}

pub struct Auth {
    path: PathBuf,
    store: Mutex<Store>,
    sessions: Mutex<HashMap<String, Session>>,
    /// Tokens of sessions ended because the user signed in elsewhere.
    pub ended: broadcast::Sender<String>,
    /// Ids of users that changed (role, disabled, password, deleted): their sockets close.
    pub changed: broadcast::Sender<String>,
    verify_slots: Semaphore,
    pub logins: guard::Logins,
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Is there an active admin in this list?
pub fn has_active_admin(list: &[User]) -> bool {
    list.iter().any(|u| u.is_admin() && !u.disabled)
}

fn check_list(list: &[User]) -> Result<(), String> {
    let mut names = HashSet::new();
    for u in list {
        if !names.insert(u.username.to_lowercase()) {
            return Err("this user name is already taken".into());
        }
    }
    if !list.is_empty() && !has_active_admin(list) {
        return Err("there must stay at least one active admin".into());
    }
    Ok(())
}

impl Auth {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("users.json");
        let auth = Auth {
            path,
            store: Mutex::new(Store { list: Vec::new(), stamp: None }),
            sessions: Mutex::new(HashMap::new()),
            ended: broadcast::channel(64).0,
            changed: broadcast::channel(64).0,
            verify_slots: Semaphore::new(4),
            logins: guard::Logins::default(),
        };
        auth.store.lock().unwrap().list = crate::paths::read_json(&auth.path).unwrap_or_default();
        auth.store.lock().unwrap().stamp = stamp(&auth.path);
        auth
    }

    /// Reads users.json again when it changed on disk; returns what to announce.
    fn refresh(&self, store: &mut Store) -> Changes {
        let now = stamp(&self.path);
        if now == store.stamp {
            return Changes::default();
        }
        store.stamp = now;
        let list: Vec<User> = if now.is_none() { Vec::new() } else {
            match crate::paths::read_json(&self.path) {
                Some(l) => l,
                None => return Changes::default(),
            }
        };
        let changes = diff(&store.list, &list);
        store.list = list;
        changes
    }

    fn with_store<R>(&self, f: impl FnOnce(&Store) -> R) -> R {
        let mut store = self.store.lock().unwrap();
        let changes = self.refresh(&mut store);
        let r = f(&store);
        drop(store);
        self.announce(changes);
        r
    }

    pub fn needs_setup(&self) -> bool {
        self.with_store(|s| s.list.is_empty())
    }

    pub fn users(&self) -> Vec<User> {
        self.with_store(|s| s.list.clone())
    }

    pub fn get(&self, id: &str) -> Option<User> {
        self.with_store(|s| s.list.iter().find(|u| u.id == id).cloned())
    }

    pub fn find_name(&self, name: &str) -> Option<User> {
        self.with_store(|s| {
            let name = name.to_lowercase();
            s.list.iter().find(|u| u.username.to_lowercase() == name).cloned()
        })
    }

    /// Changes the list atomically: invariants checked, saved (0600), changes announced.
    pub fn edit<R>(&self, f: impl FnOnce(&mut Vec<User>) -> Result<R, String>) -> Result<R, String> {
        let mut store = self.store.lock().unwrap();
        let mut changes = self.refresh(&mut store);
        let mut list = store.list.clone();
        let r = f(&mut list)?;
        check_list(&list)?;
        crate::paths::write_json(&self.path, &list, true).map_err(|e| format!("cannot save the users: {e}"))?;
        store.stamp = stamp(&self.path);
        changes.merge(diff(&store.list, &list));
        store.list = list;
        drop(store);
        self.announce(changes);
        Ok(r)
    }

    /// Adds an account (hashes the password: call from blocking code).
    pub fn add(&self, n: NewUser) -> Result<User, String> {
        let username = n.username.trim().to_string();
        validate_username(&username)?;
        let hash = match &n.password {
            Some(p) => {
                validate_password(p)?;
                hash_password(p)
            }
            None => String::new(),
        };
        let display_name = n.display_name.trim();
        let user = User {
            id: crate::api::new_id(),
            display_name: if display_name.is_empty() { username.clone() } else { display_name.to_string() },
            username,
            role: n.role,
            disabled: n.disabled,
            lang: String::new(),
            theme: String::new(),
            sso: n.sso,
            role_fixed: n.role_fixed,
            password_hash: hash,
        };
        let u = user.clone();
        self.edit(move |list| {
            list.push(u);
            Ok(())
        })?;
        Ok(user)
    }

    /// Changes one account.
    pub fn modify(&self, id: &str, f: impl FnOnce(&mut User) -> Result<(), String>) -> Result<User, String> {
        self.edit(|list| {
            let u = list.iter_mut().find(|u| u.id == id).ok_or_else(|| "no such user".to_string())?;
            f(u)?;
            Ok(u.clone())
        })
    }

    pub fn delete(&self, id: &str) -> Result<User, String> {
        self.edit(|list| {
            let i = list.iter().position(|u| u.id == id).ok_or_else(|| "no such user".to_string())?;
            let u = list.remove(i);
            if !has_active_admin(list) {
                return Err("you cannot delete the last active admin".into());
            }
            Ok(u)
        })
    }

    /// Sets a password (hashes: call from blocking code). Ends the user's sessions.
    pub fn set_password(&self, id: &str, pw: &str) -> Result<User, String> {
        validate_password(pw)?;
        let hash = hash_password(pw);
        self.modify(id, |u| {
            u.password_hash = hash;
            Ok(())
        })
    }

    /// The account matching a name and password (blocking: Argon2).
    pub fn check_password(&self, username: &str, password: &str) -> Option<User> {
        let user = self.find_name(username.trim());
        match &user {
            Some(u) if !u.password_hash.is_empty() => verify_hash(&u.password_hash, password).then_some(user.clone()).flatten(),
            _ => {
                let _ = verify_hash(dummy_hash(), password);
                None
            }
        }
    }

    /// `check_password` off the async threads, 4 at a time at most.
    pub async fn verify(self: &Arc<Self>, username: &str, password: &str) -> Option<User> {
        let _slot = self.verify_slots.acquire().await.ok()?;
        let (me, u, p) = (self.clone(), username.to_string(), password.to_string());
        tokio::task::spawn_blocking(move || me.check_password(&u, &p)).await.ok().flatten()
    }

    /// Opens a session; every other session of this user ends (one session per user).
    pub fn open_session(&self, user_id: &str) -> String {
        let token = new_token();
        let now = Instant::now();
        let mut ended = Vec::new();
        {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.retain(|t, s| {
                if s.expires <= now {
                    return false;
                }
                if s.user_id == user_id {
                    ended.push(t.clone());
                    return false;
                }
                true
            });
            sessions.insert(token.clone(), Session { user_id: user_id.to_string(), expires: now + SESSION_TTL });
        }
        for t in ended {
            let _ = self.ended.send(t);
        }
        token
    }

    pub fn close_session(&self, token: &str) {
        self.sessions.lock().unwrap().remove(token);
    }

    fn close_user_sessions(&self, user_id: &str) {
        self.sessions.lock().unwrap().retain(|_, s| s.user_id != user_id);
    }

    /// The user a session token belongs to (and extends the session).
    pub fn user_for_token(&self, token: &str) -> Option<User> {
        let now = Instant::now();
        let user_id = {
            let mut sessions = self.sessions.lock().unwrap();
            let s = sessions.get_mut(token)?;
            if s.expires <= now {
                sessions.remove(token);
                return None;
            }
            s.expires = now + SESSION_TTL;
            s.user_id.clone()
        };
        self.get(&user_id)
    }

    fn announce(&self, c: Changes) {
        for id in &c.end_sessions {
            self.close_user_sessions(id);
        }
        for id in c.changed {
            let _ = self.changed.send(id);
        }
    }
}

#[derive(Default)]
struct Changes {
    /// Users whose sessions end (deleted, disabled, new password).
    end_sessions: Vec<String>,
    /// Users whose sockets close (any change of rights).
    changed: Vec<String>,
}

impl Changes {
    fn merge(&mut self, o: Changes) {
        self.end_sessions.extend(o.end_sessions);
        self.changed.extend(o.changed);
    }
}

fn diff(old: &[User], new: &[User]) -> Changes {
    let mut c = Changes::default();
    for o in old {
        match new.iter().find(|n| n.id == o.id) {
            None => {
                c.end_sessions.push(o.id.clone());
                c.changed.push(o.id.clone());
            }
            Some(n) => {
                let ends = (n.disabled && !o.disabled) || n.password_hash != o.password_hash;
                if ends {
                    c.end_sessions.push(o.id.clone());
                }
                if ends || n.role != o.role || n.disabled != o.disabled || n.username != o.username {
                    c.changed.push(o.id.clone());
                }
            }
        }
    }
    c
}

/// `Authorization: Bearer <token>`, else the `sc_session` cookie.
pub fn token_from(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(t) = v.strip_prefix("Bearer ") {
            return Some(t.trim().to_string());
        }
    }
    cookie(headers, COOKIE)
}

/// A cookie's value (none when empty).
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all("cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(name).and_then(|v| v.strip_prefix('=')).map(str::to_string))
        .filter(|t| !t.is_empty())
}

/// Did the request come over HTTPS (directly or through a proxy)?
pub fn is_https(headers: &HeaderMap) -> bool {
    headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').next().unwrap_or("").trim().eq_ignore_ascii_case("https"))
}

pub fn session_cookie(token: &str, headers: &HeaderMap) -> HeaderValue {
    let max_age = if token.is_empty() { 0 } else { SESSION_TTL.as_secs() };
    let secure = if is_https(headers) { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")).expect("cookie")
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

/// The signed-in user, if any.
pub struct MaybeUser(pub Option<User>);

impl FromRequestParts<Shared> for MaybeUser {
    type Rejection = Infallible;
    async fn from_request_parts(parts: &mut Parts, s: &Shared) -> Result<Self, Self::Rejection> {
        Ok(MaybeUser(token_from(&parts.headers).and_then(|t| s.auth.user_for_token(&t)).filter(|u| !u.disabled)))
    }
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/auth/state", get(state))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/setup", post(setup))
}

/// Opens a session: `{user, token}` + the cookie.
pub fn signed_in(s: &Shared, user: &User, headers: &HeaderMap) -> Response {
    let token = s.auth.open_session(&user.id);
    let mut res = Json(json!({"user": user.public(), "token": token})).into_response();
    res.headers_mut().insert(header::SET_COOKIE, session_cookie(&token, headers));
    res
}

async fn state(State(s): State<Shared>, MaybeUser(user): MaybeUser) -> Json<serde_json::Value> {
    let cfg = s.config.load();
    let sso = &cfg.security.sso;
    Json(json!({
        "needsSetup": s.auth.needs_setup(),
        "user": user.as_ref().map(User::public),
        "serverName": cfg.name,
        // The live shown in the app: its fixed delay (s), or none (the player's own).
        "liveDelay": cfg.live_delay.enabled.then_some(cfg.live_delay.seconds),
        "sso": sso.active().then(|| sso.button_name()),
        "localAccounts": if sso.active() { sso.local_accounts } else { crate::sso::LocalAccounts::All },
    }))
}

#[derive(Deserialize)]
struct LoginBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

async fn login(State(s): State<Shared>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(b): Json<LoginBody>) -> ApiResult<Response> {
    let ip = guard::client_ip(peer, &headers);
    let name = b.username.trim().to_string();
    if let Err(wait) = s.auth.logins.begin(ip, &name) {
        return Err((StatusCode::TOO_MANY_REQUESTS, format!("too many tries: wait {wait} s")));
    }
    // An account that signs in through the SSO: said before its password is looked at (the
    // answer never tells whether a password was right).
    let cfg = s.config.load();
    let sso = &cfg.security.sso;
    if let Some(u) = s.auth.find_name(&name) {
        if !sso.local_allowed(u.role) && !guard::this_machine(peer, &headers) {
            return Err((StatusCode::FORBIDDEN, format!("sign in with {}", sso.button_name())));
        }
    }
    let user = s.auth.verify(&name, &b.password).await.filter(|u| !u.disabled);
    let Some(user) = user else {
        return Err((StatusCode::UNAUTHORIZED, "wrong user name or password".into()));
    };
    s.auth.logins.succeeded(ip, &name);
    tracing::info!("{} signed in from {ip}", user.username);
    Ok(signed_in(&s, &user, &headers))
}

async fn logout(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(t) = token_from(&headers) {
        s.auth.close_session(&t);
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(header::SET_COOKIE, session_cookie("", &headers));
    res
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    display_name: String,
}

async fn setup(State(s): State<Shared>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(b): Json<SetupBody>) -> ApiResult<Response> {
    if !s.auth.needs_setup() {
        return Err((StatusCode::CONFLICT, "already set up".into()));
    }
    if !guard::this_machine(peer, &headers) {
        return Err((StatusCode::FORBIDDEN, "the first account is created on the server itself (http://localhost)".into()));
    }
    let auth = s.auth.clone();
    let user = crate::api::blocking(move || {
        if !auth.needs_setup() {
            return Err((StatusCode::CONFLICT, "already set up".to_string()));
        }
        auth.add(NewUser { username: b.username, display_name: b.display_name, role: Role::Admin, disabled: false, password: Some(b.password), role_fixed: false, sso: String::new() }).map_err(bad)
    })
    .await??;
    tracing::info!("admin account {} created", user.username);
    s.changed("users");
    Ok(signed_in(&s, &user, &headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let h = hash_password("correct horse");
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_hash(&h, "correct horse"));
        assert!(!verify_hash(&h, "wrong horse"));
        assert!(!verify_hash("", "anything"));
        assert_ne!(h, hash_password("correct horse"), "salted");
    }

    #[test]
    fn usernames() {
        assert!(validate_username("admin").is_ok());
        assert!(validate_username("jean.dupont@example.com").is_ok());
        assert!(validate_username("a-b_c").is_ok());
        assert!(validate_username("éloïse").is_ok());
        assert!(validate_username("").is_err());
        assert!(validate_username("a b").is_err());
        assert!(validate_username("a/b").is_err());
        assert!(validate_username(&"x".repeat(64)).is_ok());
        assert!(validate_username(&"x".repeat(65)).is_err());
        assert!(validate_password("1234567").is_err());
        assert!(validate_password("12345678").is_ok());
    }

    #[test]
    fn accounts_and_sessions() {
        let dir = std::env::temp_dir().join(format!("sc-auth-{}", crate::api::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let auth = Auth::load(&dir);
        assert!(auth.needs_setup());
        let nu = |name: &str, role| NewUser { username: name.into(), display_name: String::new(), role, disabled: false, password: Some("password1".into()), role_fixed: false, sso: String::new() };
        let admin = auth.add(nu("admin", Role::Admin)).unwrap();
        assert!(!auth.needs_setup());
        assert_eq!(admin.name(), "admin");
        assert!(auth.add(nu("ADMIN", Role::Viewer)).is_err(), "unique without case");
        let v = auth.add(nu("viewer", Role::Viewer)).unwrap();
        assert!(auth.check_password("Admin", "password1").is_some());
        assert!(auth.check_password("admin", "nope").is_none());
        assert!(auth.check_password("ghost", "password1").is_none());
        // last active admin
        assert!(auth.modify(&admin.id, |u| { u.disabled = true; Ok(()) }).is_err());
        assert!(auth.delete(&admin.id).is_err());
        // one session per user
        let t1 = auth.open_session(&v.id);
        let mut ended = auth.ended.subscribe();
        let t2 = auth.open_session(&v.id);
        assert!(auth.user_for_token(&t1).is_none());
        assert_eq!(auth.user_for_token(&t2).unwrap().id, v.id);
        assert_eq!(ended.try_recv().unwrap(), t1);
        // a new password ends the sessions
        auth.set_password(&v.id, "password2").unwrap();
        assert!(auth.user_for_token(&t2).is_none());
        // reload from disk
        let again = Auth::load(&dir);
        assert_eq!(again.users().len(), 2);
        assert!(again.check_password("viewer", "password2").is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
