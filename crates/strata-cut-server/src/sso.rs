//! OpenID Connect sign-in (spec § 5.5).
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::rand::SecureRandom;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    api::{require, ApiResult},
    auth::{self, CurrentUser, Role, User},
    state::Shared,
};

/// A secret as the API sends it back.
pub const MASK: &str = "••••••••";
pub const PROVIDERS: [&str; 5] = ["entra", "okta", "keycloak", "google", "other"];

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LocalAccounts {
    /// Nobody (except from the server itself, to repair).
    #[serde(rename = "none")]
    Off,
    /// Admins only (a way back in).
    #[default]
    #[serde(rename = "adminOnly")]
    AdminOnly,
    #[serde(rename = "all")]
    All,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GroupRole {
    #[serde(default)]
    pub group: String,
    pub role: Role,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SsoConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub issuer: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default = "default_groups_claim")]
    pub groups_claim: String,
    #[serde(default)]
    pub groups: Vec<GroupRole>,
    /// The role of a user in no listed group; None = refused.
    #[serde(default)]
    pub default_role: Option<Role>,
    #[serde(default)]
    pub local_accounts: LocalAccounts,
    /// The e-mail domains allowed to sign in (empty: any). Google needs them: any Google account
    /// in the world would sign in otherwise.
    #[serde(default)]
    pub domains: Vec<String>,
}

fn default_provider() -> String {
    "entra".into()
}
fn default_groups_claim() -> String {
    "groups".into()
}

impl Default for SsoConfig {
    fn default() -> Self {
        serde_json::from_str("{}").unwrap()
    }
}

impl SsoConfig {
    pub fn normalize(&mut self) {
        self.provider = self.provider.trim().to_lowercase();
        if !PROVIDERS.contains(&self.provider.as_str()) {
            self.provider = "other".into();
        }
        self.issuer = self.issuer.trim().trim_end_matches('/').to_string();
        self.client_id = self.client_id.trim().to_string();
        self.client_secret = self.client_secret.trim().to_string();
        self.groups_claim = self.groups_claim.trim().to_string();
        if self.groups_claim.is_empty() {
            self.groups_claim = default_groups_claim();
        }
        for g in &mut self.groups {
            g.group = g.group.trim().to_string();
        }
        self.groups.retain(|g| !g.group.is_empty());
    }

    /// On, with an issuer and a client id.
    pub fn active(&self) -> bool {
        self.enabled && !self.issuer.is_empty() && !self.client_id.is_empty()
    }

    /// The name on the sign-in button.
    pub fn button_name(&self) -> &'static str {
        match self.provider.as_str() {
            "entra" => "Microsoft",
            "okta" => "Okta",
            "keycloak" => "Keycloak",
            "google" => "Google",
            _ => "SSO",
        }
    }

    /// May an account of this role sign in with a password?
    pub fn local_allowed(&self, role: Role) -> bool {
        if !self.active() {
            return true;
        }
        match self.local_accounts {
            LocalAccounts::Off => false,
            LocalAccounts::AdminOnly => role == Role::Admin,
            LocalAccounts::All => true,
        }
    }

    /// Is this identity's guaranteed address in an allowed domain? (No list: yes, but for Google.)
    pub fn domain_allowed(&self, id: &Identity) -> bool {
        let domains: Vec<String> = self.domains.iter().map(|d| d.trim().trim_start_matches('@').to_lowercase()).filter(|d| !d.is_empty()).collect();
        if domains.is_empty() {
            return self.provider != "google";
        }
        id.address.as_deref().and_then(|a| a.rsplit_once('@')).is_some_and(|(_, d)| domains.contains(&d.to_lowercase()))
    }

    /// The highest role given by the user's groups, else the default role.
    pub fn role_for(&self, groups: &[String]) -> Option<Role> {
        map_role(&self.groups, groups).or(self.default_role)
    }
}

fn clean_group(g: &str) -> String {
    g.trim().trim_start_matches('/').to_lowercase()
}

/// The highest role among the listed groups the user is in (no case, leading `/` ignored).
pub fn map_role(table: &[GroupRole], groups: &[String]) -> Option<Role> {
    let mine: Vec<String> = groups.iter().map(|g| clean_group(g)).collect();
    table.iter().filter(|r| mine.contains(&clean_group(&r.group))).map(|r| r.role).max_by_key(|r| r.rank())
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/auth/sso/start", get(start))
        .route("/api/auth/sso/callback", get(callback))
        .route("/api/security/sso/test", post(test))
}

// ---------------------------------------------------------------- discovery

#[derive(Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

fn agent() -> ureq::Agent {
    crate::update::http_agent(5, Duration::from_secs(30))
}

fn get_json(url: &str) -> Result<Value, String> {
    let mut res = agent().get(url).header("Accept", "application/json").call().map_err(|e| format!("{url}: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("{url}: HTTP {}", res.status().as_u16()));
    }
    let text = res.body_mut().read_to_string().map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{url}: {e}"))
}

fn discover(issuer: &str, fresh: bool) -> Result<Discovery, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Discovery)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if !fresh {
        if let Some((at, d)) = cache.lock().unwrap().get(issuer) {
            if at.elapsed() < Duration::from_secs(3600) {
                return Ok(d.clone());
            }
        }
    }
    let url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    let d: Discovery = serde_json::from_value(get_json(&url)?).map_err(|e| format!("{url}: {e}"))?;
    cache.lock().unwrap().insert(issuer.to_string(), (Instant::now(), d.clone()));
    Ok(d)
}

fn jwks(uri: &str, fresh: bool) -> Result<Vec<Value>, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<Value>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if !fresh {
        if let Some(k) = cache.lock().unwrap().get(uri) {
            return Ok(k.clone());
        }
    }
    let keys = get_json(uri)?.get("keys").and_then(Value::as_array).cloned().ok_or_else(|| format!("{uri}: no keys"))?;
    cache.lock().unwrap().insert(uri.to_string(), keys.clone());
    Ok(keys)
}

// ---------------------------------------------------------------- start

struct Pending {
    nonce: String,
    verifier: String,
    redirect_uri: String,
    next: String,
    created: Instant,
}

const PENDING_TTL: Duration = Duration::from_secs(600);
/// Sign-ins started and not finished, kept at most (the oldest go first).
const PENDING_MAX: usize = 1000;
/// The browser that started a sign-in: only it can finish it.
const STATE_COOKIE: &str = "sc_sso";

fn state_cookie(state: &str, headers: &HeaderMap) -> axum::http::HeaderValue {
    let max_age = if state.is_empty() { 0 } else { PENDING_TTL.as_secs() };
    let secure = if auth::is_https(headers) { "; Secure" } else { "" };
    axum::http::HeaderValue::from_str(&format!("{STATE_COOKIE}={state}; Path=/api/auth/sso; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")).expect("cookie")
}


fn pending() -> &'static Mutex<HashMap<String, Pending>> {
    static P: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn random() -> String {
    let mut b = [0u8; 32];
    ring::rand::SystemRandom::new().fill(&mut b).expect("random");
    URL_SAFE_NO_PAD.encode(b)
}

/// Percent-encodes everything but the unreserved characters.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Only a local path: starts with `/`, no `//` nor `\`, no space nor control character (a browser
/// drops a tab: `/\t/evil.com` would become `//evil.com`).
pub fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.contains("//") && !n.contains('\\') && !n.chars().any(|c| c.is_control() || c.is_whitespace()) => n.to_string(),
        _ => "/".into(),
    }
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok()).map(|v| v.split(',').next().unwrap_or("").trim()).filter(|v| !v.is_empty())
}

fn fail(code: &str, detail: &str) -> Response {
    if !detail.is_empty() {
        tracing::warn!("SSO sign-in failed ({code}): {detail}");
    }
    Redirect::to(&format!("/?sso_error={code}&sso_detail={}", enc(detail))).into_response()
}

#[derive(Deserialize)]
struct StartQuery {
    next: Option<String>,
}

async fn start(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<StartQuery>) -> Response {
    let cfg = s.config.load().security.sso.clone();
    if !cfg.active() {
        return fail("failed", "SSO is not set up");
    }
    let issuer = cfg.issuer.clone();
    let disc = match tokio::task::spawn_blocking(move || discover(&issuer, false)).await {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => return fail("failed", &e),
        Err(e) => return fail("failed", &e.to_string()),
    };
    let scheme = if auth::is_https(&headers) { "https" } else { "http" };
    let host = header(&headers, "x-forwarded-host").or_else(|| header(&headers, "host")).unwrap_or("localhost");
    let (state, nonce, verifier) = (random(), random(), random());
    let challenge = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let redirect_uri = format!("{scheme}://{host}/api/auth/sso/callback");
    let url = format!(
        "{}{}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
        disc.authorization_endpoint,
        if disc.authorization_endpoint.contains('?') { '&' } else { '?' },
        enc(&cfg.client_id),
        enc(&redirect_uri),
        enc("openid profile email"),
        state,
        nonce,
        challenge
    );
    let mut p = pending().lock().unwrap();
    p.retain(|_, v| v.created.elapsed() < PENDING_TTL);
    while p.len() >= PENDING_MAX {
        let Some(old) = p.iter().min_by_key(|(_, v)| v.created).map(|(k, _)| k.clone()) else { break };
        p.remove(&old);
    }
    p.insert(state.clone(), Pending { nonce, verifier, redirect_uri, next: safe_next(q.next.as_deref()), created: Instant::now() });
    let mut res = Redirect::to(&url).into_response();
    res.headers_mut().append(header::SET_COOKIE, state_cookie(&state, &headers));
    res
}

// ---------------------------------------------------------------- callback

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Who signed in, from the id_token's claims.
#[derive(Debug, PartialEq)]
pub struct Identity {
    /// "issuer|subject".
    pub key: String,
    pub login: String,
    /// A verified email, or the upn.
    pub address: Option<String>,
    pub name: String,
    pub groups: Vec<String>,
}

fn claim_str<'a>(c: &'a Value, k: &str) -> Option<&'a str> {
    c.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

pub fn identity(claims: &Value, groups_claim: &str) -> Result<Identity, String> {
    let iss = claim_str(claims, "iss").ok_or("no issuer in the token")?;
    let sub = claim_str(claims, "sub").ok_or("no subject in the token")?;
    let verified = claims.get("email_verified").and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true")));
    let email = claim_str(claims, "email").filter(|_| verified != Some(false));
    let upn = claim_str(claims, "upn");
    let login = email.or(claim_str(claims, "preferred_username")).or(upn).ok_or("the directory sent no user name")?;
    let address = if verified == Some(true) { email } else { None }.or(upn).map(str::to_string);
    let groups = match claims.get(groups_claim) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    };
    Ok(Identity { key: format!("{iss}|{sub}"), login: login.to_string(), address, name: claim_str(claims, "name").unwrap_or("").to_string(), groups })
}

/// `iss`, `aud`, `exp` (+120 s) and `nonce`.
pub fn check_claims(c: &Value, issuer: &str, client_id: &str, nonce: &str, now: u64) -> Result<(), String> {
    if claim_str(c, "iss").map(|i| i.trim_end_matches('/')) != Some(issuer.trim_end_matches('/')) {
        return Err("the token comes from another issuer".into());
    }
    let aud_ok = match c.get("aud") {
        Some(Value::String(a)) => a == client_id,
        Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err("the token is for another application".into());
    }
    match c.get("exp").and_then(Value::as_u64) {
        Some(exp) if exp + 120 >= now => {}
        _ => return Err("the token has expired".into()),
    }
    if claim_str(c, "nonce") != Some(nonce) {
        return Err("the token does not match this sign-in".into());
    }
    Ok(())
}

fn b64(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).map_err(|_| "bad base64 in the token".to_string())
}

fn verify_signature(alg: &str, key: &Value, msg: &[u8], sig: &[u8]) -> bool {
    use ring::signature;
    let field = |k: &str| key.get(k).and_then(Value::as_str).and_then(|v| b64(v).ok());
    match alg {
        "RS256" | "PS256" => {
            let (Some(n), Some(e)) = (field("n"), field("e")) else { return false };
            let params: &signature::RsaParameters = if alg == "RS256" { &signature::RSA_PKCS1_2048_8192_SHA256 } else { &signature::RSA_PSS_2048_8192_SHA256 };
            signature::RsaPublicKeyComponents { n, e }.verify(params, msg, sig).is_ok()
        }
        "ES256" => {
            let (Some(x), Some(y)) = (field("x"), field("y")) else { return false };
            let mut point = vec![4u8];
            point.extend(x);
            point.extend(y);
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point).verify(msg, sig).is_ok()
        }
        _ => false,
    }
}

/// Checks the id_token's signature with the issuer's keys (loaded again once if the key is unknown).
fn verified_claims(token: &str, jwks_uri: &str) -> Result<Value, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("the id token is malformed".into());
    }
    let head: Value = serde_json::from_slice(&b64(parts[0])?).map_err(|_| "the id token is malformed")?;
    let alg = head.get("alg").and_then(Value::as_str).unwrap_or("");
    if !matches!(alg, "RS256" | "PS256" | "ES256") {
        return Err(format!("unsupported token signature {alg}"));
    }
    let kid = head.get("kid").and_then(Value::as_str);
    let msg = format!("{}.{}", parts[0], parts[1]);
    let sig = b64(parts[2])?;
    let want_kty = if alg == "ES256" { "EC" } else { "RSA" };
    for fresh in [false, true] {
        let keys = jwks(jwks_uri, fresh)?;
        let candidates: Vec<&Value> = keys
            .iter()
            .filter(|k| k.get("kty").and_then(Value::as_str) == Some(want_kty))
            .filter(|k| kid.is_none() || k.get("kid").and_then(Value::as_str) == kid)
            .collect();
        if candidates.is_empty() {
            continue;
        }
        if candidates.iter().any(|k| verify_signature(alg, k, msg.as_bytes(), &sig)) {
            return serde_json::from_slice(&b64(parts[1])?).map_err(|_| "the id token is malformed".to_string());
        }
        return Err("the token signature is wrong".into());
    }
    Err("the token is signed with an unknown key".into())
}

fn exchange(cfg: &SsoConfig, disc: &Discovery, code: &str, p: &Pending) -> Result<String, String> {
    let mut form = vec![("grant_type", "authorization_code"), ("code", code), ("redirect_uri", p.redirect_uri.as_str()), ("code_verifier", p.verifier.as_str())];
    let mut req = agent().post(&disc.token_endpoint).header("Accept", "application/json");
    if cfg.client_secret.is_empty() {
        form.push(("client_id", cfg.client_id.as_str()));
    } else {
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", enc(&cfg.client_id), enc(&cfg.client_secret)));
        req = req.header("Authorization", &format!("Basic {basic}"));
    }
    let mut res = req.send_form(form).map_err(|e| e.to_string())?;
    let body: Value = res.body_mut().read_to_string().ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    if !res.status().is_success() {
        let why = claim_str(&body, "error_description").or(claim_str(&body, "error")).map(str::to_string).unwrap_or_else(|| format!("HTTP {}", res.status().as_u16()));
        return Err(why);
    }
    claim_str(&body, "id_token").map(str::to_string).ok_or_else(|| "the directory sent no id token".into())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

async fn callback(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<CallbackQuery>) -> Response {
    if let Some(e) = q.error.as_deref() {
        return fail("failed", q.error_description.as_deref().unwrap_or(e));
    }
    // The state this browser was given at the start: a callback link sent from elsewhere does not sign it in.
    let mine = auth::cookie(&headers, STATE_COOKIE);
    let p = q.state.as_deref().filter(|st| mine.as_deref() == Some(*st)).and_then(|st| pending().lock().unwrap().remove(st));
    let Some(p) = p.filter(|p| p.created.elapsed() < PENDING_TTL) else { return fail("expired", "") };
    let Some(code) = q.code else { return fail("failed", "no code") };
    let cfg = s.config.load().security.sso.clone();
    if !cfg.active() {
        return fail("failed", "SSO is not set up");
    }
    let c = cfg.clone();
    let next = p.next.clone();
    let got = tokio::task::spawn_blocking(move || -> Result<Identity, String> {
        let disc = discover(&c.issuer, false)?;
        let token = exchange(&c, &disc, &code, &p)?;
        let claims = verified_claims(&token, &disc.jwks_uri)?;
        check_claims(&claims, &disc.issuer, &c.client_id, &p.nonce, now_secs())?;
        identity(&claims, &c.groups_claim)
    })
    .await;
    let id = match got {
        Ok(Ok(id)) => id,
        Ok(Err(e)) => return fail("failed", &e),
        Err(e) => return fail("failed", &e.to_string()),
    };
    if !cfg.domain_allowed(&id) {
        tracing::warn!("sign-in refused with {}: {} is not in an allowed domain", cfg.button_name(), id.login);
        return fail("domain", "");
    }
    let role = cfg.role_for(&id.groups);
    let user = match sign_in_account(&s.auth, &id, role) {
        Ok(u) => u,
        Err(e) if e == "noGroup" || e == "disabled" => return fail(&e, ""),
        Err(e) => return fail("failed", &e),
    };
    // The directory's role: its base group (unless the account's are set by hand).
    if !user.role_fixed {
        match crate::groups::set_base(&user.id, user.role) {
            Ok(true) => {
                s.changed("groups");
                s.changed("roles");
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("cannot put {} in its group: {e}", user.username),
        }
    }
    s.changed("users");
    tracing::info!("{} signed in with {}", user.username, cfg.button_name());
    let token = s.auth.open_session(&user.id);
    let mut res = Redirect::to(&next).into_response();
    res.headers_mut().insert(header::SET_COOKIE, auth::session_cookie(&token, &headers));
    res.headers_mut().append(header::SET_COOKIE, state_cookie("", &headers));
    res
}

/// A user name from the directory login: allowed characters, unique (`-2`, `-3`… when taken).
fn free_username(login: &str, list: &[User]) -> String {
    let mut base: String = login.chars().filter(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '@')).take(60).collect();
    if base.is_empty() {
        base = "user".into();
    }
    let taken = |n: &str| list.iter().any(|u| u.username.to_lowercase() == n.to_lowercase());
    if !taken(&base) {
        return base;
    }
    (2..).map(|i| format!("{base}-{i}")).find(|n| !taken(n)).unwrap()
}

/// Finds (by SSO key, or a local account named after the guaranteed address) or creates the account.
/// Errors "noGroup" / "disabled" go back to the sign-in page as such.
pub fn sign_in_account(a: &auth::Auth, id: &Identity, role: Option<Role>) -> Result<User, String> {
    a.edit(|list| {
        let found = list.iter().position(|u| u.sso == id.key).or_else(|| {
            id.address.as_deref().and_then(|addr| list.iter().position(|u| u.sso.is_empty() && u.username.contains('@') && u.username.eq_ignore_ascii_case(addr)))
        });
        match found {
            Some(i) => {
                if list[i].disabled {
                    return Err("disabled".into());
                }
                list[i].sso = id.key.clone();
                if !list[i].role_fixed {
                    let Some(r) = role else { return Err("noGroup".into()) };
                    let old = list[i].role;
                    list[i].role = r;
                    if !auth::has_active_admin(list) {
                        list[i].role = old; // the directory never removes the last admin
                    }
                }
                Ok(list[i].clone())
            }
            None => {
                let Some(r) = role else { return Err("noGroup".into()) };
                let u = User::without_password(free_username(&id.login, list), id.name.clone(), r, id.key.clone());
                list.push(u.clone());
                Ok(u)
            }
        }
    })
}

// ---------------------------------------------------------------- test

#[derive(Deserialize)]
struct TestBody {
    #[serde(default)]
    issuer: String,
}

async fn test(State(_s): State<Shared>, CurrentUser(me): CurrentUser, Json(b): Json<TestBody>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    let issuer = b.issuer.trim().trim_end_matches('/').to_string();
    if issuer.is_empty() {
        return Err(crate::api::bad("the issuer is empty"));
    }
    let r = crate::api::blocking(move || -> Result<Value, String> {
        let d = discover(&issuer, true)?;
        let keys = jwks(&d.jwks_uri, true)?;
        Ok(json!({"issuer": d.issuer, "authorizationEndpoint": d.authorization_endpoint, "tokenEndpoint": d.token_endpoint, "keys": keys.len()}))
    })
    .await?;
    r.map(Json).map_err(|e| (StatusCode::BAD_GATEWAY, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gr(group: &str, role: Role) -> GroupRole {
        GroupRole { group: group.into(), role }
    }

    #[test]
    fn role_mapping() {
        let table = vec![gr("Viewers", Role::Viewer), gr("/Cutters", Role::Operator), gr("it-admins", Role::Admin)];
        let g = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(map_role(&table, &g(&["viewers"])), Some(Role::Viewer));
        assert_eq!(map_role(&table, &g(&["cutters"])), Some(Role::Operator), "leading / ignored");
        assert_eq!(map_role(&table, &g(&["/VIEWERS", "Cutters"])), Some(Role::Operator), "highest");
        assert_eq!(map_role(&table, &g(&["viewers", "IT-Admins", "cutters"])), Some(Role::Admin));
        assert_eq!(map_role(&table, &g(&["other"])), None);
        let mut cfg = SsoConfig { groups: table, ..Default::default() };
        let who = |address: Option<&str>| Identity { key: "k".into(), login: "x".into(), address: address.map(String::from), name: String::new(), groups: Vec::new() };
        assert!(cfg.domain_allowed(&who(Some("a@anything.example"))), "no list, not Google: any");
        let google = SsoConfig { provider: "google".into(), ..Default::default() };
        assert!(!google.domain_allowed(&who(Some("a@gmail.com"))), "Google needs its domains");
        let ours = SsoConfig { provider: "google".into(), domains: vec!["@Example.org".into()], ..Default::default() };
        assert!(ours.domain_allowed(&who(Some("a@example.ORG"))));
        assert!(!ours.domain_allowed(&who(Some("a@example.org.evil.com"))));
        assert!(!ours.domain_allowed(&who(None)), "no verified address");
        assert_eq!(cfg.role_for(&g(&["other"])), None, "refused");
        cfg.default_role = Some(Role::Viewer);
        assert_eq!(cfg.role_for(&g(&["other"])), Some(Role::Viewer));
    }

    #[test]
    fn config() {
        let mut c: SsoConfig = serde_json::from_value(json!({"enabled": true, "provider": "Okta", "issuer": " https://id.example.com/ ", "clientId": "abc", "groups": [{"group": " ", "role": "admin"}]})).unwrap();
        c.normalize();
        assert_eq!(c.issuer, "https://id.example.com");
        assert!(c.groups.is_empty());
        assert!(c.active());
        assert_eq!(c.button_name(), "Okta");
        assert_eq!(c.local_accounts, LocalAccounts::AdminOnly);
        assert!(c.local_allowed(Role::Admin) && !c.local_allowed(Role::Viewer));
        assert_eq!(serde_json::to_value(LocalAccounts::Off).unwrap(), json!("none"));
        assert!(SsoConfig::default().local_allowed(Role::Viewer), "off: everyone");
    }

    #[test]
    fn claims_and_identity() {
        let c = json!({"iss": "https://id.example.com/", "aud": ["x", "app"], "exp": 1000, "nonce": "n1", "sub": "42",
            "email": "Ann@Example.com", "email_verified": true, "name": "Ann", "groups": ["/Cutters"]});
        assert!(check_claims(&c, "https://id.example.com", "app", "n1", 1100).is_ok());
        assert!(check_claims(&c, "https://id.example.com", "app", "n1", 1121).is_err(), "expired");
        assert!(check_claims(&c, "https://other", "app", "n1", 900).is_err());
        assert!(check_claims(&c, "https://id.example.com", "other", "n1", 900).is_err());
        assert!(check_claims(&c, "https://id.example.com", "app", "n2", 900).is_err());
        let id = identity(&c, "groups").unwrap();
        assert_eq!(id.key, "https://id.example.com/|42");
        assert_eq!(id.login, "Ann@Example.com");
        assert_eq!(id.address.as_deref(), Some("Ann@Example.com"));
        assert_eq!(id.groups, vec!["/Cutters".to_string()]);
        let entra = json!({"iss": "i", "sub": "s", "email": "a@b.c", "upn": "a@corp.c", "preferred_username": "a@corp.c"});
        let id = identity(&entra, "groups").unwrap();
        assert_eq!(id.login, "a@b.c");
        assert_eq!(id.address.as_deref(), Some("a@corp.c"), "email not verified: upn");
        let unverified = json!({"iss": "i", "sub": "s", "email": "a@b.c", "email_verified": false, "preferred_username": "ann"});
        assert_eq!(identity(&unverified, "groups").unwrap().login, "ann");
    }

    #[test]
    fn next_and_names() {
        assert_eq!(safe_next(Some("/system/users")), "/system/users");
        assert_eq!(safe_next(Some("//evil.com")), "/");
        assert_eq!(safe_next(Some("/\\evil.com")), "/");
        assert_eq!(safe_next(Some("https://evil.com")), "/");
        assert_eq!(safe_next(Some("/\t/evil.com")), "/");
        assert_eq!(safe_next(Some("/ /evil.com")), "/");
        assert_eq!(safe_next(None), "/");
        assert_eq!(enc("a b/é"), "a%20b%2F%C3%A9");
        let list = vec![User::without_password("ann".into(), String::new(), Role::Viewer, String::new()), User::without_password("ann-2".into(), String::new(), Role::Viewer, String::new())];
        assert_eq!(free_username("Ann", &list), "Ann-3");
        assert_eq!(free_username("bob smith", &list), "bobsmith");
    }
}
