//! What each source, live, clip… costs the server: the processes LiveCut starts (and theirs:
//! the browser of a web page), and its own threads that work for one of them, sampled every
//! 2 seconds (processor, memory, disk writes), with the last 10 minutes of each.
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Mutex,
    time::Instant,
};

/// Whom a process or a thread works for: "rec:<id>" (a source), "streamer:<id>" (the Streamer of
/// a source and its lives), "clip:<id>" (a clip being cut), "sends"; and what it does.
#[derive(Clone, Debug)]
pub struct Owner {
    pub key: String,
    pub role: &'static str,
}

/// The work the server does itself (interface, measures, short tasks).
pub const SERVER: &str = "livecut";
/// 10 minutes of samples.
const HISTORY: usize = 300;

static PIDS: Mutex<Option<HashMap<u32, Owner>>> = Mutex::new(None);
static TIDS: Mutex<Option<HashMap<u32, Owner>>> = Mutex::new(None);
static STATE: Mutex<Option<State>> = Mutex::new(None);

/// A process started for someone (its children count for it too).
pub fn tag(pid: Option<u32>, key: impl Into<String>, role: &'static str) {
    if let Some(pid) = pid {
        PIDS.lock().unwrap().get_or_insert_with(HashMap::new).insert(pid, Owner { key: key.into(), role });
    }
}

/// The calling thread works for someone (until it ends).
pub fn tag_thread(key: impl Into<String>, role: &'static str) {
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u32;
    TIDS.lock().unwrap().get_or_insert_with(HashMap::new).insert(tid, Owner { key: key.into(), role });
}

/// What one owner costs now.
#[derive(Clone, Debug, Default)]
pub struct Use {
    pub key: String,
    /// What runs for it (in the order first seen).
    pub roles: Vec<&'static str>,
    /// Processor, in cores (1.0: one core all the time).
    pub cores: f64,
    /// Memory of its processes (its threads share the server's), bytes.
    pub memory: u64,
    /// Written to disk, bytes a second.
    pub writes: f64,
    /// The last 10 minutes, one value (cores) every 2 seconds, oldest first.
    pub history: Vec<f32>,
}

/// The whole machine's processor, from /proc/stat, %: in all, its busiest core, what the
/// network's interrupts take, what the hypervisor took from this machine (a VM: "steal").
#[derive(Clone, Copy, Debug, Default)]
pub struct Cpu {
    pub total: f32,
    pub busiest_core: f32,
    pub softirq: f32,
    pub steal: f32,
}

#[derive(Default)]
struct State {
    at: Option<Instant>,
    /// /proc/stat's lines (all, then each core) at the last sample.
    cpu_ticks: Vec<Vec<u64>>,
    cpu: Cpu,
    /// The machine's processor (%) over the last 10 minutes.
    cpu_history: VecDeque<f32>,
    /// Last ticks and written bytes of each process (and of each thread, apart).
    ticks: HashMap<u32, u64>,
    thread_ticks: HashMap<u32, u64>,
    writes: HashMap<u32, u64>,
    history: HashMap<String, VecDeque<f32>>,
    last: Vec<Use>,
}

struct Stat {
    ppid: u32,
    ticks: u64,
    rss_pages: u64,
}

/// /proc/<pid>/stat: its parent, processor ticks (user + system) and resident pages.
fn stat(path: &str) -> Option<Stat> {
    let text = std::fs::read_to_string(path).ok()?;
    let rest = &text[text.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some(Stat { ppid: f.get(1)?.parse().ok()?, ticks: f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?, rss_pages: f.get(21)?.parse().ok()? })
}

fn written(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    text.lines().find_map(|l| l.strip_prefix("write_bytes:")).and_then(|v| v.trim().parse().ok())
}

/// /proc/stat: the line of all the cores, then one a core (user nice system idle iowait irq
/// softirq steal …).
fn cpu_lines() -> Vec<Vec<u64>> {
    std::fs::read_to_string("/proc/stat")
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("cpu"))
        .map(|l| l.split_whitespace().skip(1).take(8).filter_map(|v| v.parse().ok()).collect())
        .collect()
}

/// The share (%) of each field between two readings of a line: (busy, softirq, steal).
fn shares(a: &[u64], b: &[u64]) -> (f32, f32, f32) {
    let d: Vec<u64> = b.iter().zip(a).map(|(x, y)| x.saturating_sub(*y)).collect();
    let total: u64 = d.iter().sum();
    if total == 0 || d.len() < 8 {
        return (0.0, 0.0, 0.0);
    }
    let pct = |v: u64| v as f32 * 100.0 / total as f32;
    let idle = d[3] + d[4];
    (pct(total - idle), pct(d[6]), pct(d[7]))
}

/// The machine's processor now, and its last 10 minutes (%).
pub fn cpu() -> (Cpu, Vec<f32>) {
    let st = STATE.lock().unwrap();
    st.as_ref().map(|s| (s.cpu, s.cpu_history.iter().copied().collect())).unwrap_or_default()
}

/// Stops what was started for `key` (its processes and theirs). How many were stopped.
pub fn stop(key: &str) -> usize {
    let pids: Vec<u32> = PIDS.lock().unwrap().get_or_insert_with(HashMap::new).iter().filter(|(_, o)| o.key == key).map(|(p, _)| *p).collect();
    for p in &pids {
        // Its process group too (a browser and its own processes).
        unsafe {
            let g = libc::getpgid(*p as i32);
            if g > 1 && g == *p as i32 {
                libc::kill(-g, libc::SIGTERM);
            }
            libc::kill(*p as i32, libc::SIGTERM);
        }
    }
    pids.len()
}

/// Samples every process LiveCut started and its own threads; what each owner costs.
pub fn sample() -> Vec<Use> {
    let tck = (unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).max(1) as f64;
    let page = (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).max(1) as u64;
    let me = std::process::id();
    // Every process, and its parent: ours are those under the server.
    let mut all: HashMap<u32, Stat> = HashMap::new();
    if let Ok(dir) = std::fs::read_dir("/proc") {
        for e in dir.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if let Some(s) = stat(&format!("/proc/{pid}/stat")) {
                all.insert(pid, s);
            }
        }
    }
    let mut pids_guard = PIDS.lock().unwrap();
    let pids = pids_guard.get_or_insert_with(HashMap::new);
    pids.retain(|pid, _| all.contains_key(pid));
    let owner_of = |mut pid: u32| -> Owner {
        for _ in 0..64 {
            if pid == me || pid <= 1 {
                break;
            }
            if let Some(o) = pids.get(&pid) {
                return o.clone();
            }
            match all.get(&pid) {
                Some(s) => pid = s.ppid,
                None => break,
            }
        }
        Owner { key: SERVER.into(), role: "Short tasks" }
    };
    let ours: Vec<u32> = all.keys().copied().filter(|&p| p != me && is_under(p, me, &all)).collect();
    let mut st_guard = STATE.lock().unwrap();
    let st = st_guard.get_or_insert_with(State::default);
    let now = Instant::now();
    let dt = st.at.map(|a| now.duration_since(a).as_secs_f64()).filter(|d| *d > 0.2);
    st.at = Some(now);
    // The machine: in all, its busiest core, interrupts, steal.
    let lines = cpu_lines();
    if lines.len() == st.cpu_ticks.len() && !lines.is_empty() {
        let (total, softirq, steal) = shares(&st.cpu_ticks[0], &lines[0]);
        let busiest = lines.iter().zip(&st.cpu_ticks).skip(1).map(|(b, a)| shares(a, b).0).fold(0f32, f32::max);
        st.cpu = Cpu { total, busiest_core: busiest, softirq, steal };
        st.cpu_history.push_back(total);
        while st.cpu_history.len() > HISTORY {
            st.cpu_history.pop_front();
        }
    }
    st.cpu_ticks = lines;
    let mut uses: HashMap<String, Use> = HashMap::new();
    let mut add = |o: &Owner, cores: f64, memory: u64, writes: f64| {
        let u = uses.entry(o.key.clone()).or_insert_with(|| Use { key: o.key.clone(), ..Default::default() });
        u.cores += cores;
        u.memory += memory;
        u.writes += writes;
        if !u.roles.contains(&o.role) {
            u.roles.push(o.role);
        }
    };
    let mut seen: HashSet<u32> = HashSet::new();
    for pid in ours {
        let s = &all[&pid];
        seen.insert(pid);
        let before = st.ticks.insert(pid, s.ticks);
        let w = written(pid);
        let wbefore = w.and_then(|w| st.writes.insert(pid, w));
        let cores = match (dt, before) {
            (Some(dt), Some(b)) => s.ticks.saturating_sub(b) as f64 / tck / dt,
            _ => 0.0,
        };
        let writes = match (dt, w, wbefore) {
            (Some(dt), Some(w), Some(b)) => w.saturating_sub(b) as f64 / dt,
            _ => 0.0,
        };
        add(&owner_of(pid), cores, s.rss_pages * page, writes);
    }
    drop(pids_guard);
    // The server's own threads: those working for someone count for it, the rest for the server.
    let mut tids_guard = TIDS.lock().unwrap();
    let tids = tids_guard.get_or_insert_with(HashMap::new);
    let mut alive: HashSet<u32> = HashSet::new();
    let mut tagged_ticks = 0f64;
    if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
        for e in dir.flatten() {
            let Some(tid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            alive.insert(tid);
            let Some(o) = tids.get(&tid).cloned() else { continue };
            let Some(s) = stat(&format!("/proc/self/task/{tid}/stat")) else { continue };
            let before = st.thread_ticks.insert(tid, s.ticks);
            if let (Some(dt), Some(b)) = (dt, before) {
                let c = s.ticks.saturating_sub(b) as f64 / tck / dt;
                tagged_ticks += c;
                add(&o, c, 0, 0.0);
            } else {
                add(&o, 0.0, 0, 0.0);
            }
        }
    }
    tids.retain(|t, _| alive.contains(t));
    drop(tids_guard);
    st.thread_ticks.retain(|t, _| alive.contains(t));
    if let Some(s) = all.get(&me) {
        let before = st.ticks.insert(me, s.ticks);
        let w = written(me);
        let wbefore = w.and_then(|w| st.writes.insert(me, w));
        let cores = match (dt, before) {
            (Some(dt), Some(b)) => (s.ticks.saturating_sub(b) as f64 / tck / dt - tagged_ticks).max(0.0),
            _ => 0.0,
        };
        let writes = match (dt, w, wbefore) {
            (Some(dt), Some(w), Some(b)) => w.saturating_sub(b) as f64 / dt,
            _ => 0.0,
        };
        add(&Owner { key: SERVER.into(), role: "Interface and measures" }, cores, s.rss_pages * page, writes);
        seen.insert(me);
    }
    st.ticks.retain(|p, _| seen.contains(p));
    st.writes.retain(|p, _| seen.contains(p));
    // The last 10 minutes of each (one that stopped keeps its history a while, at 0).
    let keys: HashSet<String> = uses.keys().cloned().chain(st.history.keys().cloned()).collect();
    for k in keys {
        let v = uses.get(&k).map_or(0.0, |u| u.cores as f32);
        let h = st.history.entry(k).or_default();
        h.push_back(v);
        while h.len() > HISTORY {
            h.pop_front();
        }
    }
    st.history.retain(|k, h| uses.contains_key(k) || h.iter().any(|v| *v > 0.0));
    let mut out: Vec<Use> = uses.into_values().collect();
    for u in &mut out {
        u.history = st.history.get(&u.key).map(|h| h.iter().copied().collect()).unwrap_or_default();
    }
    out.sort_by(|a, b| b.cores.total_cmp(&a.cores));
    st.last = out.clone();
    out
}

fn is_under(mut pid: u32, root: u32, all: &HashMap<u32, Stat>) -> bool {
    for _ in 0..64 {
        match all.get(&pid) {
            Some(s) if s.ppid == root => return true,
            Some(s) if s.ppid > 1 => pid = s.ppid,
            _ => return false,
        }
    }
    false
}

/// The last sample.
pub fn last() -> Vec<Use> {
    STATE.lock().unwrap().as_ref().map(|s| s.last.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_stat_line() {
        let dir = std::env::temp_dir().join(format!("sc-procs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("stat");
        // A name with spaces and parentheses, as the kernel writes it.
        std::fs::write(&f, "1234 (ffmpeg (x) y) S 77 1234 1234 0 -1 4194304 100 0 0 0 250 50 0 0 20 0 3 0 100 1000000 2048 18446744073709551615").unwrap();
        let s = stat(f.to_str().unwrap()).unwrap();
        assert_eq!((s.ppid, s.ticks, s.rss_pages), (77, 300, 2048));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shares_of_the_processor() {
        // user nice system idle iowait irq softirq steal
        let a = [100, 0, 100, 700, 0, 0, 0, 0];
        let b = [200, 0, 150, 750, 0, 0, 50, 50];
        let (busy, soft, steal) = shares(&a, &b);
        assert!((busy - 83.3).abs() < 0.1 && (soft - 16.7).abs() < 0.1 && (steal - 16.7).abs() < 0.1, "{busy} {soft} {steal}");
    }

    #[test]
    fn our_own_threads_count() {
        tag_thread("rec:test", "Receiving");
        let a = sample();
        assert!(a.iter().any(|u| u.key == SERVER));
        assert!(a.iter().any(|u| u.key == "rec:test" && u.roles == vec!["Receiving"]));
    }
}
