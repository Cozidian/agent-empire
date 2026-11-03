//! Optional flight recorder for the galaxy's frame costs.

use crate::game::Mode;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs::{self, File, OpenOptions}, io::{self, Write}, path::Path, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use std::os::unix::fs::OpenOptionsExt;

const INTERVAL: Duration = Duration::from_secs(10);
const LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Default)]
struct Frames { n: u64, total: [f64; 3], max: [f64; 3], slow: u64 }

impl Frames {
    fn add(&mut self, ms: [f64; 3]) {
        self.n += 1;
        for (i, v) in ms.into_iter().enumerate() { self.total[i] += v; self.max[i] = self.max[i].max(v); }
        if ms.iter().sum::<f64>() > 1000.0 / 30.0 { self.slow += 1; }
    }

    fn report(&self, view: &str, cols: usize, rows: usize) -> Value {
        json!({ "view": view, "cols": cols, "rows": rows, "frames": self.n, "over_budget": self.slow,
            "mean_ms": self.total.map(|v| v / self.n.max(1) as f64), "max_ms": self.max })
    }
}

fn stat_ticks(s: &str) -> Option<u64> {
    let a: Vec<_> = s.get(s.rfind(')')? + 1..)?.split_whitespace().collect();
    Some(a.get(11)?.parse::<u64>().ok()? + a.get(12)?.parse::<u64>().ok()?)
}

fn cpu_seconds() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut t); }
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9
}

fn threads() -> BTreeMap<u32, (String, u64)> {
    let mut out = BTreeMap::new();
    if let Ok(entries) = fs::read_dir("/proc/self/task") {
        for e in entries.flatten() {
            let Some(id) = e.file_name().to_str().and_then(|s| s.parse().ok()) else { continue };
            let Ok(s) = fs::read_to_string(e.path().join("stat")) else { continue };
            if let Some(ticks) = stat_ticks(&s) {
                let name = s.find('(').zip(s.rfind(')')).map(|(a, b)| s[a + 1..b].to_owned()).unwrap_or_default();
                out.insert(id, (name, ticks));
            }
        }
    }
    out
}

fn memory() -> Value {
    let Ok(s) = fs::read_to_string("/proc/self/status") else { return Value::Null };
    let kb = |key: &str| s.lines().find_map(|l| l.strip_prefix(key)?.split_whitespace().next()?.parse::<u64>().ok());
    json!({ "rss_kib": kb("VmRSS:"), "peak_rss_kib": kb("VmHWM:") })
}

fn view(mode: &Mode) -> &'static str {
    match mode {
        Mode::Research => "research",
        Mode::Galaxy => "galaxy", Mode::Workspace { .. } => "workspace", Mode::Planet(_) => "planet", Mode::Comms(_) => "comms",
        Mode::Root(_) => "root", Mode::Folders { .. } => "folders", Mode::Commander => "commander", Mode::Anomalies { .. } => "anomalies", Mode::Trade { .. } => "trade",
        Mode::Colonize(_) => "colonize", Mode::Deploy { .. } => "deploy", Mode::Confirm { .. } => "confirm", Mode::Ship { .. } => "ship",
        Mode::Actions { .. } => "actions", Mode::Build { .. } => "build", Mode::Log { .. } => "log", Mode::Brief { .. } => "brief",
        Mode::Situation { .. } => "situation", Mode::Help => "help",
    }
}

/// A bounded, local flight log; it never records terminal contents or project paths.
pub struct Trace {
    file: File,
    bytes: u64,
    started: Instant,
    at: Instant,
    cpu: f64,
    threads: BTreeMap<u32, (String, u64)>,
    frames: BTreeMap<(&'static str, usize, usize), Frames>,
}

impl Trace {
    pub fn from_env() -> io::Result<Option<Self>> {
        let Some(dir) = std::env::var_os("EMPIRE_PERF_DIR").filter(|s| !s.is_empty()) else { return Ok(None) };
        Self::open(Path::new(&dir)).map(Some)
    }

    fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let path = dir.join(format!("ui-{}-{stamp}.jsonl", std::process::id()));
        let file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
        let now = Instant::now();
        let mut t = Self { file, bytes: 0, started: now, at: now, cpu: cpu_seconds(), threads: threads(), frames: BTreeMap::new() };
        t.write(&json!({ "event": "start", "schema": 1, "pid": std::process::id(), "version": env!("CARGO_PKG_VERSION"),
            "build": crate::daemon::build_id(), "logical_cpus": std::thread::available_parallelism().map_or(1, |n| n.get()),
            "interval_seconds": INTERVAL.as_secs(), "limit_bytes": LIMIT, "stages": ["update", "draw", "present"],
            "cpu_unit": "percent of one core", "time_unix_ms": stamp / 1_000_000 }))?;
        Ok(t)
    }

    fn write(&mut self, value: &Value) -> io::Result<()> {
        let mut line = serde_json::to_vec(value)?; line.push(b'\n');
        if self.bytes + line.len() as u64 > LIMIT { return Err(io::Error::other("performance log reached its 8 MiB limit")); }
        self.file.write_all(&line)?;
        self.bytes += line.len() as u64;
        Ok(())
    }

    pub fn frame(&mut self, mode: &Mode, size: (usize, usize), ms: [f64; 3], context: impl FnOnce() -> Value) -> io::Result<()> {
        self.frames.entry((view(mode), size.0, size.1)).or_default().add(ms);
        if self.at.elapsed() >= INTERVAL { self.flush(context())?; }
        Ok(())
    }

    fn flush(&mut self, context: Value) -> io::Result<()> {
        if self.frames.is_empty() { return Ok(()) }
        let now = Instant::now();
        let elapsed = (now - self.at).as_secs_f64();
        let cpu = cpu_seconds();
        let current = threads();
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
        let mut busy: Vec<_> = current.iter().filter_map(|(id, (name, ticks))| {
            let (_, old) = self.threads.get(id)?;
            let pct = ticks.saturating_sub(*old) as f64 / hz / elapsed * 100.0;
            Some((*id, name, pct))
        }).collect();
        busy.sort_by(|a, b| b.2.total_cmp(&a.2));
        let count: u64 = self.frames.values().map(|f| f.n).sum();
        let report = json!({ "event": "sample", "time_unix_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "uptime_seconds": self.started.elapsed().as_secs_f64(), "window_seconds": elapsed,
            "cpu_percent": (cpu - self.cpu).max(0.0) / elapsed * 100.0, "memory": memory(), "thread_count": current.len(),
            "top_threads": busy.into_iter().take(8).map(|(id, name, pct)| json!({ "tid": id, "name": name, "cpu_percent": pct })).collect::<Vec<_>>(),
            "fps": count as f64 / elapsed, "views": self.frames.iter().map(|((v, c, r), f)| f.report(v, *c, *r)).collect::<Vec<_>>(),
            "context_last_frame": context });
        self.write(&report)?;
        self.frames.clear(); self.at = now; self.cpu = cpu; self.threads = current;
        Ok(())
    }
}

impl Drop for Trace {
    fn drop(&mut self) { let _ = self.flush(Value::Null); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_costs_keep_totals_peaks_and_missed_budgets() {
        let mut f = Frames::default();
        f.add([1.0, 20.0, 2.0]); f.add([3.0, 40.0, 4.0]);
        assert_eq!(f.n, 2);
        assert_eq!(f.total, [4.0, 60.0, 6.0]);
        assert_eq!(f.max, [3.0, 40.0, 4.0]);
        assert_eq!(f.slow, 1);
    }

    #[test]
    fn thread_cpu_ignores_spaces_and_parentheses_in_names() {
        let s = "42 (a worker (pool)) S 1 2 3 4 5 6 7 8 9 10 120 30 0 0";
        assert_eq!(stat_ticks(s), Some(150));
        assert_eq!(stat_ticks("gone"), None);
    }

    #[test]
    fn log_keeps_views_separate_and_never_overwrites_an_existing_run() {
        let dir = std::env::temp_dir().join(format!("empire-perf-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let mut t = Trace::open(&dir).unwrap();
        t.frame(&Mode::Galaxy, (80, 24), [1.0, 20.0, 2.0], || Value::Null).unwrap();
        t.frame(&Mode::Comms(1), (120, 40), [1.0, 2.0, 3.0], || Value::Null).unwrap();
        t.flush(Value::Null).unwrap();
        let path = fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        let rows: Vec<Value> = fs::read_to_string(&path).unwrap().lines().map(|s| serde_json::from_str(s).unwrap()).collect();
        assert_eq!(rows[0]["event"], "start");
        assert_eq!(rows[1]["views"].as_array().unwrap().len(), 2);
        assert_eq!(rows[1]["views"][0]["view"], "comms");
        assert_eq!(rows[1]["views"][0]["mean_ms"], json!([1.0, 2.0, 3.0]));
        assert_eq!(rows[1]["views"][1]["mean_ms"], json!([1.0, 20.0, 2.0]));
        let before = fs::read(&path).unwrap();
        t.bytes = LIMIT;
        assert!(t.write(&json!({ "test": true })).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        drop(Trace::open(&dir).unwrap());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        drop(t);
        fs::remove_dir_all(dir).unwrap();
    }
}
