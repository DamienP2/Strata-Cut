//! The resources' guard (Système › Général): a new recording or a new stream started by hand
//! is refused when it would take the server past its limits (processor, memory, the bandwidth
//! going out); the interface says why in a window. What already runs, and the programmed slots,
//! are never stopped by it.
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    perf,
    recorder::{Protocol, RecorderConfig},
    state::AppState,
};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Limits {
    pub enabled: bool,
    /// The processor (all cores, %) and the memory (%) not to go past.
    pub max_cpu: u32,
    pub max_memory: u32,
    /// The bandwidth the streams may send out, Mb/s (0: no limit).
    pub upload_mbps: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { enabled: true, max_cpu: 85, max_memory: 90, upload_mbps: 0.0 }
    }
}

impl Limits {
    pub fn validate(&self) -> Result<(), String> {
        if !(30..=100).contains(&self.max_cpu) || !(30..=100).contains(&self.max_memory) {
            return Err("the limits of the processor and the memory are 30 to 100 %".into());
        }
        if !self.upload_mbps.is_finite() || !(0.0..=100_000.0).contains(&self.upload_mbps) {
            return Err("the bandwidth limit is 0 to 100000 mb/s".into());
        }
        Ok(())
    }
}

/// What something new would take.
#[derive(Debug, Clone, Default)]
pub struct Cost {
    /// "recording" or "stream".
    pub what: &'static str,
    /// The source (and the destination) it is about.
    pub label: String,
    /// Processor cores, and kb/s sent out.
    pub cores: f64,
    pub kbps: f64,
    /// Why it costs this much ("encoded again 1080p"…).
    pub why: &'static str,
}

/// Cores to encode a picture of this height in H.264 (FFmpeg's veryfast preset, 25 to 50 frames).
fn encode_cores(height: u32, fast: bool) -> f64 {
    let c = match height {
        0..=576 => 0.5,
        577..=720 => 1.0,
        721..=1080 => 2.0,
        _ => 6.0,
    };
    if fast { c * 1.5 } else { c }
}

fn height_of(mode: &str) -> (u32, bool) {
    let at = mode.find(['i', 'p']).unwrap_or(mode.len());
    let h = mode[..at].parse().unwrap_or(1080);
    let rate: u32 = mode.get(at + 1..).and_then(|r| r.parse().ok()).unwrap_or(25);
    (h, mode.contains('p') && (rate >= 50 && rate < 100 || rate >= 5000))
}

/// A source that starts recording: its receiver and its preview (a light H.264), and, for a
/// card or a test pattern, the encoding of its pictures.
pub fn recording(c: &RecorderConfig) -> Cost {
    let (h, fast) = height_of(&c.video_mode);
    let (extra, why) = match c.protocol {
        Protocol::Decklink => (encode_cores(if c.video_mode == "auto" { 1080 } else { h }, fast), "the card's pictures encoded in H.264"),
        Protocol::Pattern => (encode_cores(h, fast) * 0.8, "the test pattern made and encoded in H.264"),
        _ => (0.0, "its preview encoded for the browser"),
    };
    Cost { what: "recording", label: c.name.clone(), cores: 0.45 + extra, kbps: 0.0, why }
}

/// Is there room for it? Err: 409, the reason as JSON for the interface's window.
pub fn check(s: &AppState, cost: &Cost) -> Result<(), (StatusCode, String)> {
    let limits = s.config.load().limits.clone();
    if !limits.enabled {
        return Ok(());
    }
    let m = perf::machine();
    let cores = m.cores.max(1) as f64;
    let cpu_now = m.cpu as f64;
    let cpu_adds = cost.cores / cores * 100.0;
    let mem_now = if m.mem_total > 0 { m.mem_used as f64 / m.mem_total as f64 * 100.0 } else { 0.0 };
    let refuse = |reason: &str, now: f64, adds: f64, limit: f64| {
        let body = json!({"guard": {
            "reason": reason, "what": cost.what, "label": cost.label, "why": cost.why,
            "now": (now * 10.0).round() / 10.0, "adds": (adds * 10.0).round() / 10.0, "limit": limit,
            "cores": cost.cores, "machineCores": m.cores,
        }});
        tracing::warn!("guard: {} {} refused ({reason}: {now:.0} + {adds:.0} > {limit:.0})", cost.what, cost.label);
        Err((StatusCode::CONFLICT, body.to_string()))
    };
    if cpu_now + cpu_adds > limits.max_cpu as f64 {
        return refuse("cpu", cpu_now, cpu_adds, limits.max_cpu as f64);
    }
    if mem_now > limits.max_memory as f64 {
        return refuse("memory", mem_now, 0.0, limits.max_memory as f64);
    }
    if limits.upload_mbps > 0.0 && cost.kbps > 0.0 {
        let out: f64 = s.streams.out_kbps() / 1000.0;
        let adds = cost.kbps / 1000.0;
        if out + adds > limits.upload_mbps {
            return refuse("upload", out, adds, limits.upload_mbps);
        }
    }
    Ok(())
}

/// A stream: copy (almost nothing), or encoded again (by its height), and what it sends.
pub fn stream(label: String, encode_again: bool, height: u32, source_height: u32, kbps: f64) -> Cost {
    let h = if height > 0 { height } else { source_height.max(1) };
    let (cores, why) = if encode_again { (encode_cores(h, false), "encoded again in H.264") } else { (0.03, "a copy of the stream") };
    Cost { what: "stream", label, cores, kbps, why }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs() {
        assert_eq!(height_of("1080i50"), (1080, false));
        assert_eq!(height_of("1080p50"), (1080, true));
        assert_eq!(height_of("2160p5994"), (2160, true));
        assert_eq!(height_of("auto").0, 1080);
        assert!(stream("x".into(), true, 0, 1080, 6000.0).cores >= 2.0);
        assert!(stream("x".into(), false, 0, 1080, 6000.0).cores < 0.1);
        assert!(Limits { max_cpu: 20, ..Default::default() }.validate().is_err());
    }
}
