//! Registered stations and evidence of a ship working in one.
use std::path::{Path, PathBuf};
use std::io::{BufRead, Seek, SeekFrom};
use std::process::Command;
use std::time::{Duration, Instant};

pub fn list(path: &str) -> Option<Vec<String>> {
    let out = Command::new("git").arg("-C").arg(path).args(["worktree", "list", "--porcelain", "-z"]).output().ok()?;
    if !out.status.success() { return None; }
    let mut paths: Vec<_> = out.stdout.split(|b| *b == 0).filter_map(|f| std::str::from_utf8(f).ok()?.strip_prefix("worktree "))
        .filter_map(|p| Path::new(p).canonicalize().ok()).filter(|p| p.join(".git").exists())
        .map(|p| p.to_string_lossy().into_owned()).collect();
    paths.sort(); paths.dedup(); Some(paths)
}

pub fn destination(from: &str, cwd: &str) -> Option<String> {
    let cwd = Path::new(cwd).canonicalize().ok()?;
    // use path components, so /repo-fix2 never matches /repo-fix; nested repos don't move ships
    let out = Command::new("git").arg("-C").arg(&cwd).args(["rev-parse", "--show-toplevel"]).output().ok()?;
    if !out.status.success() { return None; }
    let top = Path::new(String::from_utf8_lossy(&out.stdout).trim()).canonicalize().ok()?.to_string_lossy().into_owned();
    let from = Path::new(from).canonicalize().ok()?.to_string_lossy().into_owned();
    (top == from || list(&from)?.contains(&top)).then_some(top)
}

#[derive(Default)]
pub struct Trail {
    offset: u64,
    session: String,
    path: Option<PathBuf>,
    searched: Option<Instant>,
}
impl Trail {
    pub fn poll(&mut self, root: &Path, session: &str) -> Vec<String> {
        if self.session != session { *self = Self { session: session.into(), ..Default::default() }; }
        if self.path.is_none() && self.searched.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
            self.path = session_file(root, session, 3); self.searched = Some(Instant::now());
        }
        self.path.clone().map_or_else(Vec::new, |p| self.read(&p))
    }

    pub fn read(&mut self, path: &Path) -> Vec<String> {
        let Ok(file) = std::fs::File::open(path) else { self.path = None; return Vec::new() };
        let len = file.metadata().map_or(0, |m| m.len());
        if len < self.offset { self.offset = 0; }
        let mut reader = std::io::BufReader::new(file);
        // after an upgrade, start near the live end rather than replaying days of old moves
        if self.offset == 0 && len > 4 * 1024 * 1024 {
            self.offset = len - 4 * 1024 * 1024;
            if reader.seek(SeekFrom::Start(self.offset)).is_err() { return Vec::new(); }
            let mut fragment = Vec::new();
            self.offset += reader.read_until(b'\n', &mut fragment).unwrap_or(0) as u64;
        }
        if reader.seek(SeekFrom::Start(self.offset)).is_err() { return Vec::new(); }
        let start = self.offset;
        let mut paths = Vec::new(); let mut line = Vec::new();
        // bound each round, and leave a partially written record for the next round
        while self.offset - start < 4 * 1024 * 1024 {
            line.clear();
            let Ok(n) = reader.read_until(b'\n', &mut line) else { break };
            if n == 0 || line.last() != Some(&b'\n') { break; }
            self.offset += n as u64;
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) else { continue };
            let p = &v["payload"]; let item = &p["item"];
            if v["type"] != "event_msg" || p["type"] != "item_completed" || item["type"] != "CommandExecution" { continue; }
            if !matches!(item["status"].as_str(), Some("completed" | "failed")) { continue; }
            if let Some(cwd) = item["cwd"].as_str().and_then(local_path) && paths.last() != Some(&cwd) { paths.push(cwd); }
        }
        paths
    }
}

fn session_file(root: &Path, session: &str, depth: u8) -> Option<PathBuf> {
    let suffix = format!("-{session}.jsonl");
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_file() && e.file_name().to_string_lossy().ends_with(&suffix) { return Some(e.path()); }
        if t.is_dir() && depth > 0 && let Some(p) = session_file(&e.path(), session, depth - 1) { return Some(p); }
    }
    None
}

fn local_path(cwd: &str) -> Option<String> {
    if cwd.starts_with('/') { return Some(cwd.into()); }
    let uri = cwd.strip_prefix("file://")?;
    if !uri.starts_with('/') { return None; }
    let mut bytes = Vec::new(); let mut it = uri.bytes();
    while let Some(b) = it.next() {
        bytes.push(if b == b'%' {
            let a = (it.next()? as char).to_digit(16)?; let b = (it.next()? as char).to_digit(16)?;
            (a * 16 + b) as u8
        } else { b });
    }
    String::from_utf8(bytes).ok().filter(|p| !p.contains('\0'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    struct Repo { dir: PathBuf, main: String, station: String }
    impl Repo {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("empire-stations-{name}-{}", std::process::id()));
            let main = dir.join("root/project").to_string_lossy().into_owned();
            let station = dir.join("outside/work tree").to_string_lossy().into_owned();
            std::fs::create_dir_all(&main).unwrap();
            let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&main).args(args).output().unwrap().status.success());
            run(&["init", "-b", "main"]);
            run(&["-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "--allow-empty", "-m", "initial"]);
            run(&["worktree", "add", "-b", "feature", &station]);
            Self { dir, main, station }
        }
    }
    impl Drop for Repo { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); } }

    #[test]
    fn registered_stations_outside_discovery_roots_are_found_and_missing_ones_are_skipped() {
        let r = Repo::new("discover");
        let paths = list(&r.main).unwrap_or_default();
        assert!(paths.contains(&r.main));
        assert!(paths.contains(&r.station));
        assert_eq!(list(&r.station), Some(paths));
        std::fs::remove_dir_all(&r.station).unwrap();
        assert_eq!(list(&r.main), Some(vec![r.main.clone()]));
    }

    #[test]
    fn a_ship_only_moves_to_a_registered_checkout_of_its_own_repository() {
        let r = Repo::new("move");
        let sub = Path::new(&r.station).join("src"); std::fs::create_dir(&sub).unwrap();
        assert_eq!(destination(&r.main, sub.to_str().unwrap()), Some(r.station.clone()));
        assert_eq!(destination(&r.station, &r.main), Some(r.main.clone()));
        assert_eq!(destination(&r.main, &r.main), Some(r.main.clone()));
        assert_eq!(destination(&r.main, "/tmp"), None);
        let unrelated = Repo::new("unrelated");
        assert_eq!(destination(&r.main, &unrelated.station), None);
    }

    fn execution(cwd: &str) -> String {
        format!("{}\n", serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
            "type":"CommandExecution","cwd":cwd,"status":"completed","aggregated_output":"ignore this output"
        }}}))
    }

    #[test]
    fn execution_trail_follows_actual_commands_once_and_waits_for_complete_lines() {
        let dir = std::env::temp_dir().join(format!("empire-trail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap(); let path = dir.join("session.jsonl");
        let first = execution("file:///outside/work%20tree/src");
        let second = execution("/outside/another");
        let irrelevant = format!("{}\n", serde_json::json!({"type":"response_item","payload":{"type":"message","cwd":"/untrusted/text"}}));
        std::fs::write(&path, format!("{irrelevant}{first}{}", &second[..second.len()-1])).unwrap();
        let mut t = Trail::default();
        assert_eq!(t.read(&path), vec!["/outside/work tree/src"]);
        assert!(t.read(&path).is_empty());
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"\n").unwrap();
        assert_eq!(t.read(&path), vec!["/outside/another"]);
        assert!(t.read(&path).is_empty());
        std::fs::write(&path, execution("/reset")).unwrap();
        assert_eq!(t.read(&path), vec!["/reset"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
