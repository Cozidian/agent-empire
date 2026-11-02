//! Planetary survey: a background thread that keeps an eye on every colonized
//! folder (git state, size, main language) and on how much of each agent's
//! usage allowance is left.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What we know about a folder.
#[derive(Clone, Default, Debug)]
pub struct World {
    pub git: bool,
    pub checked: Option<i64>,
    pub conflicts: Vec<String>,
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    /// Uncommitted files.
    pub dirty: u32,
    /// Unix time of the last commit.
    pub last_commit: Option<i64>,
    pub commits: u32,
    pub files: u32,
    pub lang: String,
    /// For a linked worktree (a moon): the main worktree it belongs to.
    pub host: Option<String>,
    /// Has a GitHub remote, so it trades with the station.
    pub github: bool,
    /// Your open pull requests there.
    pub prs: Vec<Pr>,
    /// Everyone else's open pull requests there.
    pub others: Vec<Pr>,
    pub prs_checked: Option<i64>,
    pub prs_error: Option<String>,
    /// PRs seen merged since the galaxy opened (number, title), oldest first.
    pub merged: Vec<(u32, String)>,
    /// AI resources built here: AGENTS.md, skills, MCP servers, …
    pub structures: Vec<crate::structure::Built>,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Pr {
    pub number: u32,
    pub title: String,
    /// pass | fail | pending | none
    pub ci: &'static str,
    /// Login of who opened it.
    pub author: String,
    /// Your review is requested (directly or through a team).
    pub asks_you: bool,
    pub draft: bool,
}

/// One usage window of one provider, e.g. claude's 5-hour window.
#[derive(Clone, Debug)]
pub struct Limit {
    pub who: &'static str,
    /// "5h", "wk", …
    pub window: String,
    pub used: f32,
    /// Unix time the window resets.
    pub resets: Option<i64>,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub worlds: HashMap<String, World>,
    pub limits: Vec<Limit>,
}

pub struct Survey {
    paths: Arc<Mutex<Vec<String>>>,
    data: Arc<Mutex<Snapshot>>,
}

/// git status every few seconds; the slow stuff (file count, history) less often.
const QUICK: Duration = Duration::from_secs(8);
const SLOW: Duration = Duration::from_secs(120);
const USAGE: Duration = Duration::from_secs(15);
/// GitHub is asked about pull requests this often per world.
const TRADE: Duration = Duration::from_secs(150);

impl Survey {
    pub fn start() -> Self {
        let paths: Arc<Mutex<Vec<String>>> = Arc::default();
        let data: Arc<Mutex<Snapshot>> = Arc::default();
        let (p, d) = (paths.clone(), data.clone());
        thread::spawn(move || {
            let mut quick: HashMap<String, Instant> = HashMap::new();
            let mut slow: HashMap<String, Instant> = HashMap::new();
            let mut trade: HashMap<String, Instant> = HashMap::new();
            let mut usage: Option<Instant> = None;
            loop {
                let want = p.lock().unwrap().clone();
                for path in &want {
                    let due = |m: &HashMap<String, Instant>, every| m.get(path).is_none_or(|t| t.elapsed() > every);
                    let (q, s) = (due(&quick, QUICK), due(&slow, SLOW));
                    if !q && !s && !due(&trade, TRADE) {
                        continue;
                    }
                    let mut w = d.lock().unwrap().worlds.get(path).cloned().unwrap_or_default();
                    if q {
                        status(path, &mut w);
                        w.structures = crate::structure::survey(path);
                        quick.insert(path.clone(), Instant::now());
                    }
                    if s {
                        size(path, &mut w);
                        slow.insert(path.clone(), Instant::now());
                    }
                    // moons share their world's pull requests; ask once per repo
                    if w.github && w.host.is_none() && due(&trade, TRADE) {
                        match prs(path) {
                            Ok(now) => {
                                // A PR that left the list was merged or closed: find out which.
                                for gone in w.prs.iter().filter(|o| !now.iter().any(|n| n.number == o.number)) {
                                    if pr_state(path, gone.number).as_deref() == Some("MERGED") {
                                        w.merged.push((gone.number, gone.title.clone()));
                                    }
                                }
                                w.prs = now;
                                w.prs_checked = Some(self::now());
                                w.prs_error = None;
                            }
                            Err(e) => w.prs_error = Some(e),
                        }
                        match others_prs(path) {
                            Ok(o) => w.others = o,
                            Err(e) => w.prs_error = Some(e),
                        }
                        trade.insert(path.clone(), Instant::now());
                    }
                    d.lock().unwrap().worlds.insert(path.clone(), w);
                }
                if usage.is_none_or(|t| t.elapsed() > USAGE) {
                    let mut l = claude_limits();
                    l.extend(codex_limits());
                    d.lock().unwrap().limits = l;
                    usage = Some(Instant::now());
                }
                thread::sleep(Duration::from_millis(500));
            }
        });
        Self { paths, data }
    }

    /// The folders worth surveying.
    pub fn watch(&self, paths: Vec<String>) {
        let mut p = self.paths.lock().unwrap();
        if *p != paths {
            *p = paths;
        }
    }

    /// Look at a world's structures again now (you've just written one), not on the next round.
    pub fn rescan(&self, path: &str) {
        let built = crate::structure::survey(path);
        self.data.lock().unwrap().worlds.entry(path.to_string()).or_default().structures = built;
    }

    pub fn snapshot(&self) -> Snapshot {
        self.data.lock().unwrap().clone()
    }
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn git(path: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(path).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn status(path: &str, w: &mut World) {
    let Some(s) = git(path, &["status", "--porcelain=v2", "--branch"]) else {
        *w = World::default();
        return;
    };
    w.git = true;
    w.checked = Some(now());
    w.conflicts.clear();
    w.dirty = 0;
    for line in s.lines() {
        if let Some(b) = line.strip_prefix("# branch.head ") {
            w.branch = if b == "(detached)" { "detached".into() } else { b.into() };
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            let mut it = ab.split(' ').map(|n| n.trim_start_matches(['+', '-']).parse().unwrap_or(0));
            (w.ahead, w.behind) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
        } else if !line.starts_with('#') {
            w.dirty += 1;
            if line.starts_with("u ") {
                if let Some(file) = line.splitn(11, ' ').nth(10) { w.conflicts.push(file.to_string()); }
            }
        }
    }
    w.last_commit = git(path, &["log", "-1", "--format=%ct"]).and_then(|s| s.trim().parse().ok());
    w.host = world_host(path);
}

pub fn world_host(path: &str) -> Option<String> {
    git(path, &["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"]).and_then(|s| {
        let mut l = s.lines();
        let (own, common) = (l.next()?.to_string(), l.next()?.to_string());
        (own != common).then(|| common.strip_suffix("/.git").map(String::from)).flatten()
    })
}

fn size(path: &str, w: &mut World) {
    if !w.git {
        return;
    }
    w.github = git(path, &["remote", "get-url", "origin"]).is_some_and(|u| u.contains("github.com"));
    w.commits = git(path, &["rev-list", "--count", "HEAD"]).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let Some(files) = git(path, &["ls-files"]) else { return };
    let mut by: HashMap<&str, u32> = HashMap::new();
    w.files = 0;
    for f in files.lines() {
        w.files += 1;
        if let Some(l) = Path::new(f).extension().and_then(|e| e.to_str()).and_then(lang) {
            *by.entry(l).or_default() += 1;
        }
    }
    w.lang = by.into_iter().max_by_key(|&(l, n)| (n, l)).map(|(l, _)| l.to_string()).unwrap_or_default();
}

/// Your open PRs in the repo at `path`, with their CI verdict.
fn prs(path: &str) -> Result<Vec<Pr>, String> {
    pr_list(path, &["--author", "@me"])
}

/// Everyone else's open PRs in the repo at `path`, marking those that want your review.
fn others_prs(path: &str) -> Result<Vec<Pr>, String> {
    let mut v = pr_list(path, &["--search", "-author:@me"])?;
    let asks = pr_list(path, &["--search", "review-requested:@me"])?;
    for p in &mut v {
        p.asks_you = asks.iter().any(|a| a.number == p.number);
    }
    Ok(v)
}

fn pr_list(path: &str, filter: &[&str]) -> Result<Vec<Pr>, String> {
    let Ok(out) = Command::new("gh")
        .args(["pr", "list", "--limit", "100", "--json", "number,title,statusCheckRollup,author,isDraft"])
        .args(filter)
        .current_dir(path)
        .output()
    else {
        return Err("GitHub unavailable: install gh and sign in".into());
    };
    if !out.status.success() { return Err("GitHub refresh failed; check gh authentication and connection".into()); }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| "Invalid GitHub response")?;
    Ok(v.as_array().ok_or("Invalid GitHub response")?
        .iter()
        .map(|p| {
            let checks: Vec<&serde_json::Value> = p["statusCheckRollup"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
            let word = |c: &serde_json::Value| c["conclusion"].as_str().or(c["state"].as_str()).unwrap_or("").to_uppercase();
            let ci = if checks.is_empty() {
                "none"
            } else if checks.iter().any(|c| matches!(word(c).as_str(), "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED")) {
                "fail"
            } else if checks.iter().any(|c| matches!(word(c).as_str(), "" | "PENDING" | "EXPECTED") || c["status"].as_str().is_some_and(|s| s != "COMPLETED")) {
                "pending"
            } else {
                "pass"
            };
            Pr {
                number: p["number"].as_u64().unwrap_or(0) as u32,
                title: p["title"].as_str().unwrap_or("").into(),
                ci,
                author: p["author"]["login"].as_str().unwrap_or("").into(),
                asks_you: false,
                draft: p["isDraft"].as_bool().unwrap_or(false),
            }
        })
        .collect())
}

fn pr_state(path: &str, n: u32) -> Option<String> {
    let out = Command::new("gh").args(["pr", "view", &n.to_string(), "--json", "state", "-q", ".state"]).current_dir(path).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn lang(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "Rust",
        "ts" | "tsx" | "mts" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "rb" => "Ruby",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" => "C++",
        "cs" => "C#",
        "php" => "PHP",
        "lua" => "Lua",
        "sh" | "bash" | "zsh" | "fish" => "Shell",
        "nix" => "Nix",
        "zig" => "Zig",
        "ex" | "exs" => "Elixir",
        "hs" => "Haskell",
        "scala" => "Scala",
        "dart" => "Dart",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "el" => "Emacs Lisp",
        "clj" | "cljs" => "Clojure",
        "ml" => "OCaml",
        "tf" => "Terraform",
        "bicep" => "Bicep",
        _ => return None,
    })
}

// ─────────────────────────────── usage ─────────────────────────────────

/// Where `empire statusline` leaves Claude's latest rate limits.
pub fn claude_limits_file() -> PathBuf {
    crate::daemon::dirs_state().join("claude-limits.json")
}

/// Epoch seconds from a number (s or ms) or an RFC 3339 timestamp.
fn epoch(v: &serde_json::Value) -> Option<i64> {
    if let Some(n) = v.as_f64() {
        return Some(if n > 1e12 { (n / 1000.0) as i64 } else { n as i64 });
    }
    let s = v.as_str()?;
    let (date, time) = s.split_once('T')?;
    let d: Vec<i64> = date.split('-').filter_map(|x| x.parse().ok()).collect();
    let t: Vec<i64> = time.get(..8)?.split(':').filter_map(|x| x.parse().ok()).collect();
    if d.len() != 3 || t.len() != 3 {
        return None;
    }
    // days from civil (Howard Hinnant), UTC only
    let (y, m) = if d[1] <= 2 { (d[0] - 1, d[1] + 9) } else { (d[0], d[1] - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d[2] - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + t[0] * 3600 + t[1] * 60 + t[2])
}

/// A window that has already reset is back to 0% used.
fn limit(who: &'static str, window: String, used: f64, resets: Option<i64>) -> Limit {
    let used = if resets.is_some_and(|r| r <= now()) { 0.0 } else { used as f32 };
    Limit { who, window, used, resets }
}

fn claude_limits() -> Vec<Limit> {
    let Ok(s) = std::fs::read_to_string(claude_limits_file()) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else { return Vec::new() };
    [("five_hour", "5h"), ("seven_day", "wk")]
        .iter()
        .filter_map(|(k, label)| {
            let w = v.get(k)?;
            let used = w.get("used_percentage").and_then(|u| u.as_f64())?;
            Some(limit("claude", label.to_string(), used, w.get("resets_at").and_then(epoch)))
        })
        .collect()
}

fn window_name(mins: i64) -> String {
    match mins {
        m if m >= 10_000 => "wk".into(),
        m if m >= 1440 => format!("{}d", m / 1440),
        m => format!("{}h", (m + 30) / 60),
    }
}

/// Codex writes its rate limits into every session log; take the newest.
fn codex_limits() -> Vec<Limit> {
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| crate::game::home().join(".codex"));
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    // sessions/YYYY/MM/DD/rollout-*.jsonl: only the newest couple of days matter
    let newest = |p: &Path, n: usize| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        v.sort();
        v.into_iter().rev().take(n).collect()
    };
    for y in newest(&home.join("sessions"), 1) {
        for m in newest(&y, 2) {
            for d in newest(&m, 2) {
                for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                        files.push((t, e.path()));
                    }
                }
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, f) in files.iter().take(6) {
        if let Some(l) = codex_file(f) {
            return l;
        }
    }
    Vec::new()
}

fn codex_file(path: &Path) -> Option<Vec<Limit>> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(512 * 1024))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().rev().find(|l| l.contains("\"rate_limits\":{"))?;
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let rl = find(&v, "rate_limits")?;
    let out: Vec<Limit> = ["primary", "secondary"]
        .iter()
        .filter_map(|k| {
            let w = rl.get(k)?;
            let used = w.get("used_percent")?.as_f64()?;
            let mins = w.get("window_minutes").and_then(|m| m.as_i64()).unwrap_or(0);
            Some(limit("codex", window_name(mins), used, w.get("resets_at").and_then(epoch)))
        })
        .collect();
    (!out.is_empty()).then_some(out)
}

fn find<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m.get(key).filter(|x| !x.is_null()).or_else(|| m.values().find_map(|x| find(x, key))),
        serde_json::Value::Array(a) => a.iter().find_map(|x| find(x, key)),
        _ => None,
    }
}

#[cfg(test)]
mod anomaly_tests {
    use super::*;
    #[test]
    fn a_real_merge_conflict_is_surveyed_and_recovery_clears_it() {
        let dir = std::env::temp_dir().join(format!("empire-conflict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(&dir).args(["-c", "user.name=Empire Test", "-c", "user.email=empire@example.invalid", "-c", "commit.gpgsign=false"])
                .args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-b", "main"]);
        std::fs::write(dir.join("file with spaces.txt"), "base\n").unwrap();
        run(&["add", "."]); run(&["commit", "-m", "base"]);
        run(&["checkout", "-b", "moon"]);
        std::fs::write(dir.join("file with spaces.txt"), "moon\n").unwrap();
        run(&["commit", "-am", "moon"]);
        run(&["checkout", "main"]);
        std::fs::write(dir.join("file with spaces.txt"), "world\n").unwrap();
        run(&["commit", "-am", "world"]);
        let merge = Command::new("git").arg("-C").arg(&dir).args(["merge", "moon"]).output().unwrap();
        assert!(!merge.status.success());
        let path = dir.to_str().unwrap();
        let mut world = World::default();
        status(path, &mut world);
        assert_eq!(world.conflicts, vec!["file with spaces.txt"]);
        assert!(world.checked.is_some());
        run(&["merge", "--abort"]);
        status(path, &mut world);
        assert!(world.conflicts.is_empty());
        assert!(world.checked.is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
