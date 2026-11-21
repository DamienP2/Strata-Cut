//! Request guards: CSRF, "this machine", sign-in throttling, headers on every answer (spec § 2.5, 5.1).
use axum::{
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::state::Shared;

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

/// `scheme://host[:port]` → `host[:port]`, lowercase.
fn origin_host(origin: &str) -> Option<String> {
    let rest = origin.split_once("://")?.1;
    let host = rest.split('/').next().unwrap_or("");
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Is this a request sent by another site?
/// `Sec-Fetch-Site: cross-site`, or an `Origin` that is neither `Host` nor `X-Forwarded-Host`
/// (port included; `Origin: null` = another site), unless the origin is trusted.
pub fn other_site(headers: &HeaderMap, trusted: &[String]) -> bool {
    if header(headers, "sec-fetch-site").is_some_and(|v| v.eq_ignore_ascii_case("cross-site")) {
        let origin = header(headers, "origin").unwrap_or("");
        return !is_trusted(origin, trusted);
    }
    let Some(origin) = header(headers, "origin") else { return false };
    if is_trusted(origin, trusted) {
        return false;
    }
    let Some(from) = origin_host(origin) else { return true }; // "null" or garbage
    let hosts = [header(headers, "host"), header(headers, "x-forwarded-host").map(|v| v.split(',').next().unwrap_or("").trim())];
    !hosts.into_iter().flatten().any(|h| h.eq_ignore_ascii_case(&from))
}

fn is_trusted(origin: &str, trusted: &[String]) -> bool {
    let o = origin.trim_end_matches('/');
    !o.is_empty() && trusted.iter().any(|t| t.trim_end_matches('/').eq_ignore_ascii_case(o))
}

/// Does this request need the CSRF check? Changes (not GET/HEAD/OPTIONS) and WebSocket
/// upgrades carried by the cookie alone (no `Authorization`).
pub fn needs_check(method: &Method, headers: &HeaderMap) -> bool {
    let upgrade = header(headers, "upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let changes = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    (changes || upgrade) && !headers.contains_key("authorization")
}

/// Refuses state-changing requests sent by another site with only the cookie.
pub async fn csrf(State(s): State<Shared>, req: Request, next: Next) -> Response {
    if needs_check(req.method(), req.headers()) && other_site(req.headers(), &s.config.load().trusted_origins) {
        return (StatusCode::FORBIDDEN, "request sent by another site refused").into_response();
    }
    next.run(req).await
}

fn local_name(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    // strip the port: "[::1]:8080", "localhost:8080", "127.0.0.1"
    let name = if let Some(rest) = h.strip_prefix('[') { rest.split(']').next().unwrap_or("").to_string() } else if h.matches(':').count() == 1 { h.split(':').next().unwrap_or("").to_string() } else { h };
    matches!(name.as_str(), "localhost" | "127.0.0.1" | "::1")
}

/// An IPv6 address that wraps an IPv4 one (`::ffff:127.0.0.1`) as that one.
pub fn plain_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        ip => ip,
    }
}

/// A request from the server itself: loopback peer, not another site, local `Host`/`Origin`.
pub fn this_machine(peer: SocketAddr, headers: &HeaderMap) -> bool {
    if !plain_ip(peer.ip()).is_loopback() || headers.contains_key("x-forwarded-for") || headers.contains_key("forwarded") || headers.contains_key("x-forwarded-host") {
        return false;
    }
    if header(headers, "sec-fetch-site").is_some_and(|v| v.eq_ignore_ascii_case("cross-site")) {
        return false;
    }
    if !header(headers, "host").is_some_and(local_name) {
        return false;
    }
    match header(headers, "origin") {
        None => true,
        Some(o) => origin_host(o).is_some_and(|h| local_name(&h)),
    }
}

/// The address of whoever sent the request. Behind a reverse proxy on this machine (a loopback
/// peer), the one the proxy names last in `X-Forwarded-For` (the one it saw itself): else every
/// client would share the proxy's address, and its sign-in throttling.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    let ip = plain_ip(peer.ip());
    if !ip.is_loopback() {
        return ip;
    }
    let forwarded = header(headers, "x-forwarded-for").and_then(|v| v.rsplit(',').next()).or_else(|| header(headers, "x-real-ip"));
    forwarded.and_then(|v| v.trim().parse().ok()).unwrap_or(ip)
}

/// Headers on every answer.
pub async fn every_answer(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("x-content-type-options", axum::http::HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", axum::http::HeaderValue::from_static("same-origin"));
    res
}

/// Free tries per (address + name), and per address.
pub const FREE_PER_NAME: u32 = 5;
pub const FREE_PER_ADDRESS: u32 = 20;
const MAX_WAIT: Duration = Duration::from_secs(300);
const FORGET_AFTER: Duration = Duration::from_secs(15 * 60);

/// How long to wait before the next try after `failures` failed tries, the last one `since` ago:
/// none while tries are free, then 2^(tries over) s (5 min at most).
pub fn wait_for(failures: u32, free: u32, since: Duration) -> Option<Duration> {
    if failures < free || since >= FORGET_AFTER {
        return None;
    }
    let over = (failures - free + 1).min(16);
    let wait = Duration::from_secs(1u64 << over).min(MAX_WAIT);
    (since < wait).then(|| wait - since)
}

#[derive(Clone, Copy)]
struct Tries {
    failures: u32,
    last: Instant,
}

/// Sign-in throttling (lives across hot restarts).
#[derive(Default)]
pub struct Logins {
    tries: Mutex<HashMap<String, Tries>>,
}

impl Logins {
    fn keys(ip: IpAddr, name: &str) -> [(String, u32); 2] {
        [(format!("{ip}|{}", name.to_lowercase()), FREE_PER_NAME), (ip.to_string(), FREE_PER_ADDRESS)]
    }

    /// Ok, or the number of seconds to wait.
    pub fn check(&self, ip: IpAddr, name: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut map = self.tries.lock().unwrap();
        map.retain(|_, t| now.duration_since(t.last) < FORGET_AFTER);
        let wait = Self::keys(ip, name).iter().filter_map(|(k, free)| map.get(k).and_then(|t| wait_for(t.failures, *free, now.duration_since(t.last)))).max();
        match wait {
            Some(w) => Err(w.as_secs_f64().ceil().max(1.0) as u64),
            None => Ok(()),
        }
    }

    pub fn failed(&self, ip: IpAddr, name: &str) {
        let now = Instant::now();
        let mut map = self.tries.lock().unwrap();
        for (k, _) in Self::keys(ip, name) {
            let t = map.entry(k).or_insert(Tries { failures: 0, last: now });
            if now.duration_since(t.last) >= FORGET_AFTER {
                t.failures = 0;
            }
            t.failures += 1;
            t.last = now;
        }
    }

    /// A success resets the (address + name) count.
    pub fn succeeded(&self, ip: IpAddr, name: &str) {
        let [(k, _), _] = Self::keys(ip, name);
        self.tries.lock().unwrap().remove(&k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn client_behind_a_proxy() {
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let far: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        let fwd = h(&[("x-forwarded-for", "198.51.100.1, 198.51.100.7")]);
        assert_eq!(client_ip(local, &fwd).to_string(), "198.51.100.7");
        // Only a proxy on this machine is believed.
        assert_eq!(client_ip(far, &fwd).to_string(), "203.0.113.9");
        assert_eq!(client_ip(local, &h(&[("x-forwarded-for", "garbage")])).to_string(), "127.0.0.1");
        assert_eq!(client_ip(local, &HeaderMap::new()).to_string(), "127.0.0.1");
    }

    fn h(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, HeaderValue::from_static(v));
        }
        m
    }

    #[test]
    fn csrf_decision() {
        let none: Vec<String> = vec![];
        assert!(!other_site(&h(&[("host", "srv:8080")]), &none), "no origin: curl and the like");
        assert!(!other_site(&h(&[("host", "srv:8080"), ("origin", "http://srv:8080")]), &none));
        assert!(!other_site(&h(&[("host", "SRV:8080"), ("origin", "http://srv:8080/")]), &none));
        assert!(other_site(&h(&[("host", "srv:8080"), ("origin", "http://srv:9090")]), &none), "port counts");
        assert!(other_site(&h(&[("host", "srv:8080"), ("origin", "http://evil.com")]), &none));
        assert!(other_site(&h(&[("host", "srv:8080"), ("origin", "null")]), &none));
        assert!(other_site(&h(&[("host", "srv:8080"), ("sec-fetch-site", "cross-site")]), &none));
        assert!(!other_site(&h(&[("host", "127.0.0.1:8080"), ("x-forwarded-host", "cut.example.com"), ("origin", "https://cut.example.com")]), &none));
        let trusted = vec!["https://panel.example.com".to_string()];
        assert!(!other_site(&h(&[("host", "srv:8080"), ("origin", "https://panel.example.com")]), &trusted));
        assert!(!other_site(&h(&[("host", "srv:8080"), ("origin", "https://panel.example.com"), ("sec-fetch-site", "cross-site")]), &trusted));
        // only changes and upgrades without Authorization are checked
        assert!(!needs_check(&Method::GET, &h(&[])));
        assert!(needs_check(&Method::GET, &h(&[("upgrade", "websocket")])));
        assert!(needs_check(&Method::POST, &h(&[])));
        assert!(!needs_check(&Method::POST, &h(&[("authorization", "Bearer x")])));
    }

    #[test]
    fn machine() {
        let lo: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let lo6: SocketAddr = "[::1]:5000".parse().unwrap();
        let far: SocketAddr = "192.168.1.9:5000".parse().unwrap();
        assert!(this_machine(lo, &h(&[("host", "localhost:8080")])));
        assert!(this_machine(lo6, &h(&[("host", "[::1]:8080"), ("origin", "http://[::1]:8080")])));
        assert!(this_machine(lo, &h(&[("host", "127.0.0.1"), ("origin", "http://localhost:8080")])));
        assert!(!this_machine(far, &h(&[("host", "localhost:8080")])));
        assert!(!this_machine(lo, &h(&[("host", "srv.example.com")])));
        assert!(!this_machine(lo, &h(&[("host", "localhost:8080"), ("x-forwarded-for", "1.2.3.4")])));
        assert!(!this_machine(lo, &h(&[("host", "localhost:8080"), ("origin", "http://evil.com")])));
    }

    #[test]
    fn throttling_math() {
        let s = Duration::from_secs;
        assert_eq!(wait_for(4, 5, s(0)), None, "free tries");
        assert_eq!(wait_for(5, 5, s(0)), Some(s(2)), "6th try: 2^1 s");
        assert_eq!(wait_for(5, 5, s(1)), Some(s(1)));
        assert_eq!(wait_for(5, 5, s(2)), None);
        assert_eq!(wait_for(6, 5, s(0)), Some(s(4)));
        assert_eq!(wait_for(12, 5, s(0)), Some(s(256)));
        assert_eq!(wait_for(13, 5, s(0)), Some(s(300)), "capped at 5 min");
        assert_eq!(wait_for(40, 5, s(0)), Some(s(300)));
        assert_eq!(wait_for(40, 5, s(15 * 60)), None, "forgotten after 15 min");
        assert_eq!(wait_for(20, 20, s(0)), Some(s(2)), "per address");

        let l = Logins::default();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..5 {
            assert!(l.check(ip, "bob").is_ok());
            l.failed(ip, "bob");
        }
        assert_eq!(l.check(ip, "Bob"), Err(2));
        assert!(l.check(ip, "alice").is_ok(), "other name, same address");
        l.succeeded(ip, "bob");
        assert!(l.check(ip, "bob").is_ok());
    }
}
