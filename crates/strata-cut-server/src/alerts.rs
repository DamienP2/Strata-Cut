//! Alerts (Système › Alertes): what goes wrong told outside the app, to a webhook (Slack, Teams,
//! Discord, Mattermost, or any service reading JSON) and by e-mail, and told again when it is over.
//!
//! Every 5 s the server's state is looked at: a source that should be received and is not, a
//! picture black or frozen, silence, a live lost on its way or that could not start, a disk
//! almost full, a loop cut short for room, the server started again after stopping unexpectedly.
//! A problem is told once it has lasted `delay_secs` (one that happens once, at once).
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpStream,
    sync::{Arc, OnceLock},
    time::Duration,
};

use arc_swap::ArcSwap;
use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    api::{bad, blocking, require, ApiResult},
    auth::CurrentUser,
    recorder::{now_ms, Manager, State as RecState},
    state::Shared,
    streams::Streams,
};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Alerts {
    pub enabled: bool,
    /// The language of the messages: "fr" or "en".
    pub language: String,
    /// POSTed a JSON message (`text` and `content`: what the chat services show).
    pub webhook: String,
    /// E-mail recipients, separated by commas.
    pub email_to: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// "starttls", "tls" (from the first byte) or "none".
    pub smtp_security: String,
    pub smtp_user: String,
    pub smtp_password: String,
    pub smtp_from: String,
    /// A problem is told once it has lasted this long (s).
    pub delay_secs: u32,
    /// What is told.
    pub signal: bool,
    pub content: bool,
    pub streams: bool,
    pub disk: bool,
    pub restart: bool,
}

impl Default for Alerts {
    fn default() -> Self {
        Alerts {
            enabled: false,
            language: "fr".into(),
            webhook: String::new(),
            email_to: String::new(),
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_security: "starttls".into(),
            smtp_user: String::new(),
            smtp_password: String::new(),
            smtp_from: String::new(),
            delay_secs: 30,
            signal: true,
            content: true,
            streams: true,
            disk: true,
            restart: true,
        }
    }
}

impl Alerts {
    pub fn normalize(&mut self) {
        for s in [&mut self.webhook, &mut self.email_to, &mut self.smtp_host, &mut self.smtp_user, &mut self.smtp_from] {
            *s = s.trim().to_string();
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.webhook.is_empty() && !(self.webhook.starts_with("https://") || self.webhook.starts_with("http://")) {
            return Err("the webhook address starts with https://".into());
        }
        if !["starttls", "tls", "none"].contains(&self.smtp_security.as_str()) {
            return Err("the mail server's security is starttls, tls or none".into());
        }
        if !self.email_to.is_empty() {
            if self.smtp_host.is_empty() || self.smtp_from.is_empty() {
                return Err("give the mail server and the sender's address".into());
            }
            if self.smtp_port == 0 {
                return Err("give the mail server's port".into());
            }
            if recipients(&self.email_to).iter().chain(std::iter::once(&self.smtp_from)).any(|a| !plausible(a)) {
                return Err("an e-mail address is not valid".into());
            }
        }
        if !(5..=3600).contains(&self.delay_secs) {
            return Err("the delay before an alert is 5 to 3600 s".into());
        }
        Ok(())
    }

    fn fr(&self) -> bool {
        self.language != "en"
    }
}

fn recipients(list: &str) -> Vec<String> {
    list.split([',', ';', ' ', '\n']).map(str::trim).filter(|a| !a.is_empty()).map(String::from).collect()
}

/// Looks like an address, and nothing that could break an SMTP command.
fn plausible(a: &str) -> bool {
    let ok_chars = a.chars().all(|c| !c.is_whitespace() && !c.is_control() && c != '<' && c != '>');
    ok_chars && a.split_once('@').is_some_and(|(u, d)| !u.is_empty() && d.contains('.'))
}

/// The settings in use (set at each start of the web server, when the settings change).
fn current() -> &'static ArcSwap<Alerts> {
    static A: OnceLock<ArcSwap<Alerts>> = OnceLock::new();
    A.get_or_init(|| ArcSwap::from_pointee(Alerts::default()))
}

pub fn configure(a: &Alerts, server: &str) {
    current().store(Arc::new(a.clone()));
    *SERVER.lock().unwrap() = server.to_string();
}

static SERVER: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// One message: the problem, or that it is over.
#[derive(Clone, Debug)]
struct Message {
    /// "signal", "content", "stream", "disk", "restart", "test".
    event: &'static str,
    /// "problem", "ok" or "info".
    level: &'static str,
    source: String,
    text: String,
}

/// A problem seen now: its key (one per thing), since when, and how to say it, then that it is over.
struct Seen {
    key: String,
    since: i64,
    /// Told at once (it happened once: a live that could not start, a restart).
    once: bool,
    event: &'static str,
    source: String,
    text: String,
    over: String,
}

/// Watches the server's state; started once, outside the web server's restarts.
pub fn start(data_dir: std::path::PathBuf, recorders: Arc<Manager>, streams: Arc<Streams>) {
    tokio::spawn(async move {
        let mut told: HashMap<String, (i64, bool, String, &'static str, String)> = HashMap::new();
        let mut restart_told = false;
        let mut failed_seen: HashMap<String, i64> = HashMap::new();
        let mut first = true;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let a = current().load_full();
            // Off: nothing is kept to be told later (once on, what is already wrong is told
            // after the delay; the lives that failed before are not).
            if !a.enabled {
                told.clear();
                first = true;
                continue;
            }
            let now = now_ms();
            let mut seen: Vec<Seen> = Vec::new();
            let fr = a.fr();
            let say = |f: &str, e: &str| if fr { f.to_string() } else { e.to_string() };

            // The sources that should be received.
            for r in recorders.recs() {
                let c = r.config();
                let st = r.status.lock().unwrap().clone();
                let wanted = c.enabled || st.slot.is_some() || r.streams.load(std::sync::atomic::Ordering::Relaxed) > 0;
                if a.signal && wanted && matches!(st.state, RecState::Nosignal | RecState::Error) {
                    let why = if st.state == RecState::Error && !st.message.is_empty() { format!(" ({})", st.message) } else { String::new() };
                    seen.push(Seen {
                        key: format!("signal:{}", c.id),
                        since: st.since,
                        once: false,
                        event: "signal",
                        source: c.name.clone(),
                        text: say(&format!("{} : la source n’est plus reçue{why}", c.name), &format!("{}: the source is no longer received{why}", c.name)),
                        over: say(&format!("{} : la source est de nouveau reçue", c.name), &format!("{}: the source is received again", c.name)),
                    });
                }
                if a.content && st.state == RecState::Recording {
                    for (k, since, fr_w, en_w) in [("black", st.black_since, "image noire", "black picture"), ("frozen", st.frozen_since, "image figée", "frozen picture"), ("silent", st.silent_since, "silence", "silence")] {
                        if let Some(since) = since {
                            seen.push(Seen {
                                key: format!("{k}:{}", c.id),
                                since,
                                once: false,
                                event: "content",
                                source: c.name.clone(),
                                text: say(&format!("{} : {fr_w}", c.name), &format!("{}: {en_w}", c.name)),
                                over: say(&format!("{} : fin de « {fr_w} »", c.name), &format!("{}: {en_w} over", c.name)),
                            });
                        }
                    }
                }
                if a.disk && st.space_limited {
                    seen.push(Seen {
                        key: format!("space:{}", c.id),
                        since: now,
                        once: false,
                        event: "disk",
                        source: c.name.clone(),
                        text: say(&format!("{} : sa boucle est raccourcie faute de place sur le disque", c.name), &format!("{}: its loop is cut short for lack of disk space", c.name)),
                        over: say(&format!("{} : sa boucle a de nouveau toute sa durée", c.name), &format!("{}: its loop has its full length again", c.name)),
                    });
                }
            }

            // The lives.
            if a.streams {
                let list = streams.json();
                for s in list.as_array().into_iter().flatten() {
                    let dest = s["destination"].as_str().unwrap_or_default().to_string();
                    let label = format!("{} → {}", name_of(&recorders, s["source"].as_str().unwrap_or_default()), streams.name_of(&dest));
                    let message = s["message"].as_str().unwrap_or_default();
                    match s["state"].as_str().unwrap_or_default() {
                        "retrying" => seen.push(Seen {
                            key: format!("stream:{dest}"),
                            since: now,
                            once: false,
                            event: "stream",
                            source: label.clone(),
                            text: say(&format!("Diffusion {label} coupée : {message}"), &format!("Live {label} lost: {message}")),
                            over: say(&format!("Diffusion {label} rétablie"), &format!("Live {label} back")),
                        }),
                        "failed" => {
                            let at = s["started"].as_i64().unwrap_or(now);
                            // Each failure told once.
                            if failed_seen.get(&dest) != Some(&at) {
                                failed_seen.insert(dest.clone(), at);
                                if !first {
                                    seen.push(Seen {
                                        key: format!("failed:{dest}:{at}"),
                                        since: now,
                                        once: true,
                                        event: "stream",
                                        source: label.clone(),
                                        text: say(&format!("Diffusion {label} : n’a pas pu démarrer ({message})"), &format!("Live {label}: could not start ({message})")),
                                        over: String::new(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            // The disks of the loops and the clips.
            if a.disk {
                let dirs = [recorders.recordings_dir(), data_dir.join("clips")];
                let full = dirs
                    .iter()
                    .map(|d| crate::health::disk(d))
                    .filter(|&(t, _)| t > 0)
                    .map(|(t, f)| (t - f.min(t)) as f64 / t as f64 * 100.0)
                    .fold(0.0_f64, f64::max);
                if full >= 95.0 {
                    seen.push(Seen {
                        key: "disk".into(),
                        since: now,
                        once: false,
                        event: "disk",
                        source: String::new(),
                        text: say(&format!("Disque presque plein : {full:.0} %"), &format!("Disk almost full: {full:.0} %")),
                        over: say("Le disque a de nouveau de la place", "The disk has room again"),
                    });
                }
            }

            // Started again after stopping unexpectedly (told once, at the start).
            if a.restart && !restart_told && crate::health::crashed_before() {
                restart_told = true;
                seen.push(Seen {
                    key: "restart".into(),
                    since: now,
                    once: true,
                    event: "restart",
                    source: String::new(),
                    text: say("Strata Cut a redémarré après un arrêt inattendu (voir Système › Santé)", "Strata Cut started again after an unexpected stop (see Système › Santé)"),
                    over: String::new(),
                });
            }
            first = false;

            let delay = a.delay_secs as i64 * 1000;
            let mut out: Vec<Message> = Vec::new();
            for s in &seen {
                let entry = told.entry(s.key.clone()).or_insert((s.since, false, s.over.clone(), s.event, s.source.clone()));
                if !entry.1 && (s.once || now - entry.0 >= delay) {
                    entry.1 = true;
                    out.push(Message { event: s.event, level: "problem", source: s.source.clone(), text: s.text.clone() });
                }
            }
            // Over: said when it had been told.
            let keys: Vec<String> = told.keys().cloned().collect();
            for k in keys {
                if seen.iter().any(|s| s.key == k) {
                    continue;
                }
                let (_, was_told, over, event, source) = told.remove(&k).unwrap();
                // A picture or a sound that "ends" because the source is no longer received: its
                // loss is what is told, not that.
                let gone = event == "content" && k.split_once(':').and_then(|(_, id)| recorders.get(id)).is_none_or(|r| r.state() != RecState::Recording);
                if was_told && !over.is_empty() && !gone {
                    out.push(Message { event, level: "ok", source, text: over });
                }
            }
            if !out.is_empty() {
                let a = a.clone();
                tokio::task::spawn_blocking(move || {
                    for m in out {
                        for (channel, r) in send(&a, &m) {
                            if let Err(e) = r {
                                tracing::warn!("alert by {channel}: {e}");
                            }
                        }
                    }
                });
            }
        }
    });
}

fn name_of(m: &Manager, id: &str) -> String {
    m.get(id).map(|r| r.config().name).unwrap_or_else(|| id.to_string())
}

/// Sends a message on every channel set: (channel, result).
fn send(a: &Alerts, m: &Message) -> Vec<(&'static str, Result<(), String>)> {
    let mut out = Vec::new();
    let server = SERVER.lock().unwrap().clone();
    if !a.webhook.is_empty() {
        out.push(("webhook", webhook(a, &server, m)));
    }
    if !a.email_to.is_empty() {
        out.push(("e-mail", email(a, &server, m)));
    }
    out
}

fn webhook(a: &Alerts, server: &str, m: &Message) -> Result<(), String> {
    let mark = match m.level {
        "problem" => "⚠️",
        "ok" => "✅",
        _ => "ℹ️",
    };
    let text = format!("{mark} [{server}] {}", m.text);
    let body = json!({
        "text": text, "content": text,
        "event": m.event, "level": m.level, "source": m.source, "server": server,
        "message": m.text, "at": jiff::Timestamp::now().to_string(),
    });
    let agent = crate::update::http_agent(3, Duration::from_secs(10));
    agent.post(&a.webhook).header("Content-Type", "application/json").send(body.to_string()).map(drop).map_err(|e| e.to_string())
}

// ---- E-mail: a small SMTP client (STARTTLS or TLS, AUTH PLAIN), on the TLS the server already has.

trait Wire: Read + Write {}
impl<T: Read + Write> Wire for T {}

struct Smtp {
    io: Box<dyn Wire>,
}

impl Smtp {
    /// One reply (its last line's code), several lines possibly ("250-…").
    fn reply(&mut self) -> Result<(u16, String), String> {
        let mut all = String::new();
        loop {
            let mut line = Vec::new();
            let mut b = [0u8; 1];
            loop {
                let n = self.io.read(&mut b).map_err(|e| e.to_string())?;
                if n == 0 {
                    return Err("the mail server closed the connection".into());
                }
                line.push(b[0]);
                if b[0] == b'\n' || line.len() > 4096 {
                    break;
                }
            }
            let line = String::from_utf8_lossy(&line).trim_end().to_string();
            all.push_str(&line);
            all.push('\n');
            if line.len() < 4 || line.as_bytes()[3] != b'-' {
                let code = line.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| format!("unexpected answer: {line}"))?;
                return Ok((code, all));
            }
        }
    }

    fn cmd(&mut self, line: &str, want: &[u16]) -> Result<String, String> {
        self.io.write_all(format!("{line}\r\n").as_bytes()).map_err(|e| e.to_string())?;
        self.expect(want, line.split(' ').next().unwrap_or(line))
    }

    fn expect(&mut self, want: &[u16], what: &str) -> Result<String, String> {
        let (code, text) = self.reply()?;
        if want.contains(&code) { Ok(text) } else { Err(format!("{what}: {}", text.trim())) }
    }
}

fn tls(host: &str, tcp: TcpStream) -> Result<Box<dyn Wire>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = rustls_platform_verifier::Verifier::new(provider.clone()).map_err(|e| e.to_string())?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
    let conn = rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| e.to_string())?;
    Ok(Box::new(rustls::StreamOwned::new(conn, tcp)))
}

/// "=?UTF-8?B?…?=": a subject with accents.
fn encoded(s: &str) -> String {
    use base64::Engine;
    if s.is_ascii() { s.to_string() } else { format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(s)) }
}

fn email(a: &Alerts, server: &str, m: &Message) -> Result<(), String> {
    use base64::Engine;
    let to = recipients(&a.email_to);
    let addr = format!("{}:{}", a.smtp_host, a.smtp_port);
    let sock = std::net::ToSocketAddrs::to_socket_addrs(&addr).map_err(|e| format!("{addr}: {e}"))?.next().ok_or_else(|| format!("{addr}: no address"))?;
    let tcp = TcpStream::connect_timeout(&sock, Duration::from_secs(10)).map_err(|e| format!("{addr}: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(20))).ok();
    tcp.set_write_timeout(Some(Duration::from_secs(20))).ok();
    let me = gethostname::gethostname().to_string_lossy().into_owned();
    let me = if me.is_empty() { "strata-cut".to_string() } else { me };
    let mut s = if a.smtp_security == "tls" { Smtp { io: tls(&a.smtp_host, tcp.try_clone().map_err(|e| e.to_string())?)? } } else { Smtp { io: Box::new(tcp.try_clone().map_err(|e| e.to_string())?) } };
    s.expect(&[220], "connection")?;
    let ehlo = s.cmd(&format!("EHLO {me}"), &[250])?;
    if a.smtp_security == "starttls" {
        if !ehlo.to_ascii_uppercase().contains("STARTTLS") {
            return Err("the mail server does not offer STARTTLS".into());
        }
        s.cmd("STARTTLS", &[220])?;
        s = Smtp { io: tls(&a.smtp_host, tcp)? };
        s.cmd(&format!("EHLO {me}"), &[250])?;
    }
    if !a.smtp_user.is_empty() {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("\0{}\0{}", a.smtp_user, a.smtp_password));
        s.cmd(&format!("AUTH PLAIN {token}"), &[235]).map_err(|_| "the mail server refused the user or the password".to_string())?;
    }
    s.cmd(&format!("MAIL FROM:<{}>", a.smtp_from), &[250])?;
    for r in &to {
        s.cmd(&format!("RCPT TO:<{r}>"), &[250, 251])?;
    }
    s.cmd("DATA", &[354])?;
    let subject = format!("[{server}] {}", m.text);
    let date = jiff::fmt::rfc2822::to_string(&jiff::Zoned::now()).unwrap_or_default();
    let id = format!("<{}.{}@{me}>", now_ms(), std::process::id());
    let body = format!("{}\r\n\r\n{}: {server}\r\n{}", m.text, if a.fr() { "Serveur" } else { "Server" }, jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S %Z"));
    // Lines starting with a dot are doubled (the end of DATA is a lone dot).
    let body: String = body.split("\r\n").map(|l| if l.starts_with('.') { format!(".{l}") } else { l.to_string() }).collect::<Vec<_>>().join("\r\n");
    let msg = format!(
        "From: Strata Cut <{}>\r\nTo: {}\r\nSubject: {}\r\nDate: {date}\r\nMessage-ID: {id}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}\r\n.\r\n",
        a.smtp_from,
        to.join(", "),
        encoded(&subject)
    );
    s.io.write_all(msg.as_bytes()).map_err(|e| e.to_string())?;
    s.expect(&[250], "message")?;
    let _ = s.cmd("QUIT", &[221]);
    Ok(())
}

// ---- Route: a test message, with the settings shown (saved or not).

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/system/alerts/test", post(test))
}

async fn test(State(s): State<Shared>, CurrentUser(me): CurrentUser, Json(mut a): Json<Alerts>) -> ApiResult<Json<Value>> {
    require(me.is_admin())?;
    if a.smtp_password == crate::sso::MASK {
        a.smtp_password = s.config.load().alerts.smtp_password.clone();
    }
    a.normalize();
    a.validate().map_err(bad)?;
    if a.webhook.is_empty() && a.email_to.is_empty() {
        return Err(bad("give a webhook or an e-mail address"));
    }
    let fr = a.fr();
    let m = Message {
        event: "test",
        level: "info",
        source: String::new(),
        text: if fr { "Message d’essai des alertes de Strata Cut".into() } else { "Strata Cut alerts: a test message".into() },
    };
    let results = blocking(move || send(&a, &m)).await?;
    let mut out = serde_json::Map::new();
    for (channel, r) in results {
        out.insert(if channel == "webhook" { "webhook".into() } else { "email".into() }, match r {
            Ok(()) => json!({"ok": true}),
            Err(e) => json!({"ok": false, "error": e}),
        });
    }
    Ok(Json(Value::Object(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks() {
        let mut a = Alerts::default();
        assert!(a.validate().is_ok());
        a.email_to = "ops@example.com, regie@example.com".into();
        assert!(a.validate().is_err()); // no mail server
        a.smtp_host = "smtp.example.com".into();
        a.smtp_from = "strata@example.com".into();
        assert!(a.validate().is_ok());
        assert_eq!(recipients(&a.email_to), ["ops@example.com", "regie@example.com"]);
        a.email_to = "x@y.z>\r\nRCPT TO:<evil@example.com".into();
        assert!(a.validate().is_err());
        a.email_to.clear();
        a.webhook = "ftp://x".into();
        assert!(a.validate().is_err());
    }

    #[test]
    fn subject_with_accents() {
        assert_eq!(encoded("plain"), "plain");
        assert!(encoded("Diffusion coupée").starts_with("=?UTF-8?B?"));
    }
}
