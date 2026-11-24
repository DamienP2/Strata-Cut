//! Real time: `GET /ws` (spec § 2.6).
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::HeaderMap,
    response::Response,
    routing::get,
    Router,
};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use crate::{
    auth::{token_from, CurrentUser, User},
    state::{Audience, Shared},
};

pub fn routes() -> Router<Shared> {
    Router::new().route("/ws", get(upgrade))
}

async fn upgrade(State(s): State<Shared>, CurrentUser(me): CurrentUser, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let token = token_from(&headers).unwrap_or_default();
    ws.on_upgrade(move |socket| run(s, me, token, socket))
}

/// Does this user receive an event sent to `a`?
pub fn reaches(a: &Audience, u: &User) -> bool {
    match a {
        Audience::All => true,
        Audience::Admins => u.is_admin(),
        Audience::NotAdmins => !u.is_admin(),
        Audience::Jobs => true,
    }
}

fn hello(s: &Shared, me: &User) -> Value {
    let cfg = s.config.load();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let mut v = json!({"type": "hello", "user": me.public(), "serverName": cfg.name, "serverTime": now, "timezone": cfg.timezone});
    if let Value::Object(m) = &mut v {
        m.extend(s.app_hello(me));
    }
    v
}

async fn run(s: Shared, me: User, token: String, mut socket: WebSocket) {
    // Subscribe before the hello: nothing is missed in between.
    let mut events = s.events.subscribe();
    let mut ended = s.auth.ended.subscribe();
    let mut changed = s.auth.changed.subscribe();
    let mut closing = s.closing.subscribe();
    if *closing.borrow() || socket.send(Message::Text(hello(&s, &me).to_string().into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Ok(ev) => {
                    let text = if ev.audience == Audience::Jobs { json!({"type": "jobs", "jobs": s.clips.jobs_json(&me)}).to_string() } else { ev.json.to_string() };
                    if reaches(&ev.audience, &me) && socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                // Too slow: close, the client reconnects and gets everything anew.
                Err(RecvError::Lagged(_)) | Err(RecvError::Closed) => break,
            },
            t = ended.recv() => match t {
                Ok(t) if t == token => {
                    let _ = socket.send(Message::Text(json!({"type": "sessionEnded"}).to_string().into())).await;
                    break;
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    if s.auth.user_for_token(&token).is_none() {
                        let _ = socket.send(Message::Text(json!({"type": "sessionEnded"}).to_string().into())).await;
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            id = changed.recv() => match id {
                Ok(id) if id == me.id => break,
                Ok(_) => {}
                Err(_) => break,
            },
            _ = closing.changed() => break,
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audiences() {
        let mk = |role| User::without_password("u".into(), String::new(), role, String::new());
        let admin = mk(crate::auth::Role::Admin);
        let viewer = mk(crate::auth::Role::Viewer);
        assert!(reaches(&Audience::All, &viewer));
        assert!(reaches(&Audience::Admins, &admin) && !reaches(&Audience::Admins, &viewer));
        assert!(reaches(&Audience::NotAdmins, &viewer) && !reaches(&Audience::NotAdmins, &admin));
    }
}
