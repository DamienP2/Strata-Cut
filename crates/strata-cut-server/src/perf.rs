//! Machine metrics (sysinfo), sampled at most once a second.
use serde::Serialize;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    /// Percent, all cores.
    pub cpu: f32,
    pub cores: usize,
    pub cpu_name: String,
    pub mem_used: u64,
    pub mem_total: u64,
    pub hostname: String,
}

struct Sampler {
    sys: System,
    at: Option<Instant>,
    last: Machine,
}

static SAMPLER: Mutex<Option<Sampler>> = Mutex::new(None);

pub fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().to_string()
}

/// The machine now (blocking: the first call samples twice, 200 ms apart).
pub fn machine() -> Machine {
    let mut guard = SAMPLER.lock().unwrap_or_else(|p| p.into_inner());
    let s = guard.get_or_insert_with(|| {
        let sys = System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()).with_memory(MemoryRefreshKind::everything()));
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(Duration::from_millis(200)));
        Sampler { sys, at: None, last: Machine::default() }
    });
    if s.at.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
        s.sys.refresh_cpu_usage();
        s.sys.refresh_memory();
        s.at = Some(Instant::now());
        s.last = Machine {
            cpu: (s.sys.global_cpu_usage() * 10.0).round() / 10.0,
            cores: s.sys.cpus().len(),
            cpu_name: s.sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
            mem_used: s.sys.used_memory(),
            mem_total: s.sys.total_memory(),
            hostname: hostname(),
        };
    }
    s.last.clone()
}

#[cfg(test)]
mod tests {
    #[test]
    fn sample() {
        let m = super::machine();
        assert!(m.cores > 0 && m.mem_total > 0 && m.mem_used <= m.mem_total);
        assert!((0.0..=100.0).contains(&m.cpu));
    }
}
