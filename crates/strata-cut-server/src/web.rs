//! The web app: embedded in the binary, or served from `--web-dir`.
use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;
use std::io::Write;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct WebApp;

static GZ: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/web_gz.rs"));

fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers.get(header::ACCEPT_ENCODING).and_then(|v| v.to_str().ok()).is_some_and(|v| {
        v.split(',').any(|p| {
            let mut it = p.split(';');
            let name = it.next().unwrap_or("").trim();
            let q0 = it.any(|q| q.trim().replace(' ', "") == "q=0");
            (name == "gzip" || name == "*") && !q0
        })
    })
}

pub async fn embedded(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "no such route").into_response();
    }
    let (path, file) = match WebApp::get(path) {
        Some(f) if !path.is_empty() => (path, f),
        _ => match WebApp::get("index.html") {
            Some(f) => ("index.html", f),
            None => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    "<!doctype html><meta charset=utf-8><title>Strata LiveCut</title><body style=\"font:14px system-ui;background:#0b0c12;color:#e6e8f2;padding:40px\">Web app not built — run <code>cd web &amp;&amp; npm install &amp;&amp; npm run build</code>",
                )
                    .into_response()
            }
        },
    };
    let mime = file.metadata.mimetype().to_string();
    let cache = if path.starts_with("static/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    let gz = GZ.iter().find(|(p, _)| *p == path).map(|(_, d)| *d);
    let mut res = match gz {
        Some(d) if accepts_gzip(&headers) => {
            let mut r = Response::new(Body::from(d));
            r.headers_mut().insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            r
        }
        _ => Response::new(Body::from(file.data.into_owned())),
    };
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    res
}

/// Gzips JSON / text / SVG / XML answers between 1 KB and 64 MB on the fly.
pub async fn compress(req: Request, next: Next) -> Response {
    let gzip = accepts_gzip(req.headers());
    let res = next.run(req).await;
    if !gzip || res.status() != StatusCode::OK || res.headers().contains_key(header::CONTENT_ENCODING) {
        return res;
    }
    let ct = res.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !(ct.starts_with("application/json") || ct.starts_with("text/") || ct.contains("svg") || ct.contains("xml")) {
        return res;
    }
    let len = res.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<usize>().ok());
    if matches!(len, Some(n) if !(1024..=64 << 20).contains(&n)) || len.is_none() {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 64 << 20).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "body too large").into_response();
    };
    let level = if bytes.len() > 256 << 10 { 1 } else { 6 };
    let work = move || {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
        let _ = gz.write_all(&bytes);
        gz.finish().unwrap_or_default()
    };
    let out = if level == 1 { tokio::task::spawn_blocking(work).await.unwrap_or_default() } else { work() };
    parts.headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    parts.headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(out))
}
