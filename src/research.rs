//! A galaxy-wide laboratory. Experiments own snapshots; adoption is a separate action.
use crate::{game::Line, gfx::{Fb, Out, V3, v3}};
use crossterm::{event::{self, Event, KeyCode, KeyEventKind, KeyModifiers}, cursor, execute, terminal, style};
use serde::{Deserialize, Serialize};
use std::{fs, io, path::{Path, PathBuf}, process::{Command, Stdio, Child}, time::{Instant, Duration, SystemTime, UNIX_EPOCH}, os::unix::{process::CommandExt, fs::PermissionsExt}};

const RUNNER: &str = include_str!("../research/runner.py");
const REVISION: &str = "6868817518876de8fd170c6a8354daca273b6191";
const INK: V3 = v3(0.84, 0.89, 0.95);
const DIM: V3 = v3(0.47, 0.56, 0.67);
const ACCENT: V3 = v3(0.43, 0.88, 0.79);
const BG: V3 = v3(0.018, 0.027, 0.048);
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
fn data() -> PathBuf { std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| crate::game::home().join(".local/share")).join("empire/research") }
pub fn root() -> PathBuf { std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| crate::game::home().join(".local/state")).join("empire/research") }
fn python() -> PathBuf { std::env::var_os("EMPIRE_RESEARCH_PYTHON").map(PathBuf::from).unwrap_or_else(|| data().join("venv/bin/python")) }
fn ready() -> bool { python().is_file() }
fn json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let tmp = path.with_extension("tmp"); fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?; fs::rename(tmp, path)
}
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Request { pub problem: String, pub project: String, pub revision: String, pub tasks: Vec<String>, pub minutes: u32 }
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Record {
    pub id: String, pub problem: String, pub project: String, pub status: String, pub stage: String, pub note: String,
    pub calls: u32, pub max_calls: u32, pub input_tokens: u64, pub output_tokens: u64, pub cached_input_tokens: u64,
    pub updated: u64, pub pid: u32, pub commit: String, pub changed_files: Vec<String>, pub events: Vec<Step>,
}
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Step { pub label: String, pub stage: String, pub status: String, pub note: String }
impl Record {
    pub fn active(&self) -> bool { matches!(self.status.as_str(), "queued" | "running") }
    fn read(dir: &Path) -> io::Result<Self> {
        let mut r: Self = serde_json::from_slice(&fs::read(dir.join("state.json"))?)?;
        if r.active() && now().saturating_sub(r.updated) > 30 && (r.pid == 0 || unsafe { libc::kill(r.pid as i32, 0) } != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)) {
            r.status = "interrupted".into(); r.note = "Worker is no longer running. Evidence retained; start a new experiment.".into();
        }
        Ok(r)
    }
}
fn validate(mut q: Request) -> io::Result<Request> {
    if q.problem.trim().is_empty() || q.tasks.is_empty() || q.tasks.len() > 3 || q.tasks.iter().any(|t| t.trim().is_empty()) { return Err(io::Error::other("Describe a question and 1–3 representative tasks.")); }
    if !(1..=120).contains(&q.minutes) { return Err(io::Error::other("Time budget must be 1–120 minutes.")); }
    q.project = fs::canonicalize(crate::game::expand(&q.project))?.to_string_lossy().into_owned();
    let p = Command::new("git").args(["-C", &q.project, "rev-parse", "--verify", "--end-of-options", &format!("{}^{{commit}}", q.revision)]).output()?;
    if !p.status.success() { return Err(io::Error::other("Choose a Git repository and an existing commit or ref. Commit your representative tasks first.")); }
    q.revision = String::from_utf8_lossy(&p.stdout).trim().into();
    Ok(q)
}
fn start(q: &Request, base: &Path) -> io::Result<String> {
    if !ready() { return Err(io::Error::other("Install the research runtime first: empire research setup")); }
    fs::create_dir_all(base)?; fs::set_permissions(base, fs::Permissions::from_mode(0o700))?;
    for e in fs::read_dir(base)?.flatten() { if Record::read(&e.path()).is_ok_and(|r| r.active()) { return Err(io::Error::other("An experiment is already running. Wait or cancel it first.")); } }
    let id = format!("{}-{}", now(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos());
    let dir = base.join(&id); fs::create_dir(&dir)?;
    json(&dir.join("request.json"), q)?;
    let r = Record { id: id.clone(), problem: q.problem.clone(), project: q.project.clone(), status: "queued".into(), stage: "snapshot".into(),
        note: "Starting isolated research worker".into(), max_calls: q.tasks.len() as u32 * 10, updated: now(), commit: q.revision.clone(), ..Default::default() };
    json(&dir.join("state.json"), &r)?;
    let log = fs::File::create(dir.join("worker.log"))?;
    let mut command = Command::new(python());
    command.args(["-c", RUNNER, "run"]).arg(&dir).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    unsafe { command.pre_exec(|| { if libc::setsid() < 0 { return Err(io::Error::last_os_error()); } libc::umask(0o077); Ok(()) }); }
    match command.spawn() {
        Ok(mut child) => { std::thread::spawn(move || { let _ = child.wait(); }); }
        Err(e) => { let mut r = r; r.status = "failed".into(); r.note = e.to_string(); json(&dir.join("state.json"), &r)?; return Err(e); }
    }
    Ok(id)
}
fn apply(dir: &Path, target: &str) -> io::Result<String> {
    let p = Command::new(python()).args(["-c", RUNNER, "apply"]).arg(dir).arg("--to").arg(target).output()?;
    if !p.status.success() { return Err(io::Error::other(String::from_utf8_lossy(&p.stderr).lines().last().unwrap_or("Resource application failed").to_string())); }
    Ok(String::from_utf8_lossy(&p.stdout).trim().into())
}
fn checked(cmd: &mut Command) -> io::Result<()> { if cmd.status()?.success() { Ok(()) } else { Err(io::Error::other("Setup command failed; see its output.")) } }
fn setup() -> io::Result<()> {
    let dir = data(); fs::create_dir_all(&dir)?;
    use std::os::fd::AsRawFd;
    let lock = fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("setup.lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 { return Err(io::Error::other("Research setup is already running.")); }
    let src = dir.join("harness2");
    if !src.exists() { checked(Command::new("git").args(["clone", "https://github.com/google-research/harness2"]).arg(&src))?; }
    checked(Command::new("git").arg("-C").arg(&src).args(["checkout", "--detach", REVISION]))?;
    let env = dir.join("venv");
    if !env.exists() { checked(Command::new("uv").args(["venv", "--python", "3.12"]).arg(&env))?; }
    checked(Command::new("uv").args(["pip", "install", "--python"]).arg(env.join("bin/python")).arg(&src))?;
    checked(Command::new(env.join("bin/python")).args(["-c", "from harness2.core.schedule import run_domain; print('Harness2 runtime ready')"]))?;
    println!("Research ready. Uses your native Codex login; run `codex login` if needed."); Ok(())
}

pub struct Draft { pub fields: Vec<Line>, pub field: usize, pub review: Option<Request> }
impl Draft {
    fn new(project: &str) -> Self { Self { fields: ["", project, "HEAD", "", "30"].into_iter().map(|s| { let mut l = Line::default(); l.set(s.into()); l }).collect(), field: 0, review: None } }
    fn request(&self) -> io::Result<Request> {
        validate(Request { problem: self.fields[0].s.trim().into(), project: self.fields[1].s.trim().into(), revision: self.fields[2].s.trim().into(),
            tasks: self.fields[3].s.lines().map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(), minutes: self.fields[4].s.parse().unwrap_or(0) })
    }
}
pub struct View {
    pub records: Vec<Record>, pub projects: Vec<String>, pub sel: usize, pub detail: bool, pub page: String, pub scroll: usize,
    pub draft: Option<Draft>, pub confirm: Option<String>, pub message: String,
    base: Option<PathBuf>, polled: Instant, setup: Option<Child>, project: String,
}
impl Default for View {
    fn default() -> Self { Self { records: vec![], projects: vec![], sel: 0, detail: false, page: "overview".into(), scroll: 0, draft: None, confirm: None, message: String::new(),
        base: if cfg!(test) { None } else { Some(root()) }, polled: Instant::now() - Duration::from_secs(2), setup: None, project: String::new() } }
}
impl View {
    pub fn open(&mut self, project: Option<String>) { if let Some(p) = project { self.project = p; } self.poll(true); }
    pub fn poll(&mut self, force: bool) {
        if !force && self.polled.elapsed() < Duration::from_secs(1) { return; } self.polled = Instant::now();
        if let Some(c) = &mut self.setup && let Ok(Some(status)) = c.try_wait() {
            self.message = if status.success() { "Runtime ready. Press n to begin a study.".into() } else { format!("Setup failed. Read {}", data().join("setup.log").display()) }; self.setup = None;
        }
        let Some(base) = &self.base else { return; };
        let id = self.records.get(self.sel).map(|r| r.id.clone());
        self.records = fs::read_dir(base).into_iter().flatten().flatten().filter_map(|e| Record::read(&e.path()).ok()).collect();
        self.records.sort_by(|a, b| b.id.cmp(&a.id));
        self.sel = id.and_then(|id| self.records.iter().position(|r| r.id == id)).unwrap_or(self.sel).min(self.records.len().saturating_sub(1));
    }
    fn result(&mut self, r: io::Result<String>) { self.message = match r { Ok(s) => s, Err(e) => e.to_string() }; }
    pub fn input(&mut self, ev: &Event) -> bool {
        if let Event::Paste(s) = ev { if let Some(d) = &mut self.draft && d.review.is_none() { d.fields[d.field].paste_lines(s); } return false; }
        let Event::Key(k) = ev else {
            if let Event::Mouse(m) = ev { match m.kind { event::MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3), event::MouseEventKind::ScrollDown => self.scroll += 3, _ => {} } }
            return false;
        };
        if k.kind == KeyEventKind::Release { return false; }
        if let Some(action) = self.confirm.clone() {
            match k.code {
                KeyCode::Down | KeyCode::PageDown => self.scroll += 1,
                KeyCode::Up | KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(1),
                _ => {}
            }
            if k.code == KeyCode::Char('y') {
                if let (Some(base), Some(r)) = (&self.base, self.records.get(self.sel)) {
                    let dir = base.join(&r.id);
                    let result = if action == "apply" { apply(&dir, &r.project) } else { fs::write(dir.join("cancel"), "cancel\n").map(|_| "Cancellation requested; active model process will stop.".into()) };
                    self.result(result); self.poll(true);
                }
                self.confirm = None; self.scroll = 0;
            } else if matches!(k.code, KeyCode::Esc | KeyCode::Char('n')) { self.confirm = None; self.scroll = 0; }
            return false;
        }
        if let Some(d) = &mut self.draft {
            if let Some(q) = &d.review {
                match k.code {
                    KeyCode::Esc => { d.review = None; self.scroll = 0; },
                    KeyCode::PageDown | KeyCode::Down => self.scroll += 1,
                    KeyCode::PageUp | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                    KeyCode::Enter => {
                        let result = self.base.as_ref().ok_or_else(|| io::Error::other("Preview cannot launch research")).and_then(|p| start(q, p));
                        match result { Ok(id) => { self.draft = None; self.poll(true); self.sel = self.records.iter().position(|r| r.id == id).unwrap_or(0); self.detail = true; self.page = "overview".into(); self.scroll = 0; self.message.clear(); }, Err(e) => self.message = e.to_string() }
                    }
                    _ => {}
                }
            } else { match k.code {
                KeyCode::Esc => self.draft = None,
                KeyCode::Up | KeyCode::Down if d.field == 1 && !self.projects.is_empty() => {
                    let current = self.projects.iter().position(|p| p == &d.fields[1].s);
                    let next = if k.code == KeyCode::Down { current.map_or(0, |i| (i + 1) % self.projects.len()) } else { current.map_or(self.projects.len() - 1, |i| (i + self.projects.len() - 1) % self.projects.len()) };
                    d.fields[1].set(self.projects[next].clone());
                }
                KeyCode::Tab => d.field = (d.field + 1) % 5,
                KeyCode::BackTab => d.field = (d.field + 4) % 5,
                KeyCode::Enter if k.modifiers.contains(KeyModifiers::ALT) && d.field == 3 => d.fields[3].paste_lines("\n"),
                KeyCode::Enter if k.modifiers.contains(KeyModifiers::CONTROL) || d.field == 4 => match d.request() { Ok(q) => { d.review = Some(q); self.scroll = 0; self.message.clear(); }, Err(e) => self.message = e.to_string() },
                KeyCode::Enter => d.field = (d.field + 1) % 5,
                _ => { d.fields[d.field].key(k); }
            } }
            return false;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => { if self.detail { self.detail = false; self.scroll = 0; } else { return true; } }
            KeyCode::Char('n') => { self.draft = Some(Draft::new(&self.project)); self.message.clear(); self.scroll = 0; }
            KeyCode::Char('i') if self.setup.is_none() => {
                let result = (|| { fs::create_dir_all(data())?; let f = fs::File::create(data().join("setup.log"))?;
                    Command::new(std::env::current_exe()?).args(["research", "setup"]).stdin(Stdio::null()).stdout(f.try_clone()?).stderr(f).spawn() })();
                match result { Ok(c) => { self.setup = Some(c); self.message = "Installing pinned Harness2 runtime. You can leave this screen.".into(); }, Err(e) => self.message = e.to_string() }
            }
            KeyCode::Enter if !self.records.is_empty() => { self.detail = true; self.scroll = 0; self.message.clear(); }
            KeyCode::Up | KeyCode::Char('k') => { if self.detail { self.scroll = self.scroll.saturating_sub(1); } else { self.sel = self.sel.saturating_sub(1); } }
            KeyCode::Down | KeyCode::Char('j') => { if self.detail { self.scroll += 1; } else { self.sel = (self.sel + 1).min(self.records.len().saturating_sub(1)); } }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(12),
            KeyCode::PageDown => self.scroll += 12,
            KeyCode::Home => self.scroll = 0,
            KeyCode::Char('p' | 'v' | 'o') if self.detail => { self.page = match k.code { KeyCode::Char('p') => "proposal.patch", KeyCode::Char('v') => "REPORT.md", _ => "overview" }.into(); self.scroll = 0; }
            KeyCode::Char('a') if self.detail && self.page == "proposal.patch" => {
                if self.records.get(self.sel).is_some_and(|r| r.status == "review") { self.confirm = Some("apply".into()); self.scroll = 0; }
            }
            KeyCode::Char('x') if self.detail => { if self.records.get(self.sel).is_some_and(Record::active) { self.confirm = Some("cancel".into()); self.scroll = 0; } }
            _ => {}
        }
        false
    }
    fn document(&self, width: usize) -> Document {
        let mut d = Document::new(width);
        if let Some(action) = &self.confirm {
            d.heading(if action == "apply" { "Adopt reviewed resources?" } else { "Cancel this study?" });
            if let Some(r) = self.records.get(self.sel) {
                d.line(&r.problem, INK); d.space();
                if action == "apply" {
                    d.label("TARGET PROJECT", &r.project);
                    d.heading("Resources to apply");
                    for file in &r.changed_files { d.line(&format!("+ {file}"), ACCENT); }
                    d.space(); d.line("Only the reviewed resource patch is applied. Conflicting originals block adoption. No commit or push is made.", INK);
                } else { d.line("The active model process will stop. Evidence collected so far is retained.", WARN); }
            }
            return d;
        }
        if !self.message.is_empty() { d.band(&self.message, WARN, PANEL); d.space(); }
        if let Some(draft) = &self.draft {
            if let Some(q) = &draft.review {
                d.heading("Ready to research"); d.line(&q.problem, INK); d.space();
                d.label("PROJECT", &q.project); d.label("FROZEN COMMIT", &q.revision);
                d.heading("Representative tasks");
                for (i, task) in q.tasks.iter().enumerate() { d.line(&format!("{}. {task}", i + 1), INK); d.space(); }
                d.heading("Budget & scope");
                d.band(&format!("{} minutes  /  {} model calls maximum", q.minutes, q.tasks.len() * 10), ACCENT, PANEL);
                d.line("Native Codex account. Token usage is recorded; billed cost is unavailable.", DIM);
                d.space(); d.line("Trials use isolated copies of this commit. Uncommitted files are excluded. Resources are proposed for review; adoption is a separate action.", INK);
            } else {
                d.heading("New study"); d.line("Ask one question; try it on 1–3 tasks.", DIM); d.space();
                let names = ["Research question", "Project folder", "Commit or ref", "Representative tasks", "Time limit · minutes"];
                let placeholders = ["What should your agents do better?", "/path/to/project", "HEAD", "One task per line", "30"];
                for i in 0..5 {
                    let active = i == draft.field; let start = d.rows.len();
                    d.line(&format!("{}  {}", i + 1, names[i]), if active { ACCENT } else { DIM });
                    let field = &draft.fields[i]; let mut s = field.s.clone();
                    if active { let pos = s.char_indices().nth(field.cur).map_or(s.len(), |(p, _)| p); s.insert(pos, '▏'); }
                    if field.s.is_empty() { s = format!("{}{}", if active { "▏" } else { "" }, placeholders[i]); }
                    let from = d.rows.len();
                    d.band(&s, if field.s.is_empty() && !active { DIM } else { INK }, if active { FOCUS } else { PANEL });
                    if active {
                        let cursor = d.rows[from..].iter().position(|r| r.text.contains('▏')).unwrap_or(0) + from;
                        d.focus = Some((start, cursor + 1));
                        if i == 1 { d.line("↑↓ choose project · or type a path", DIM); }
                        if i == 3 { d.line("Alt+Enter adds a task · paste works too", DIM); }
                    }
                    d.space();
                }
            }
            return d;
        }
        if !self.detail {
            d.heading(if self.records.is_empty() { "Better ways to work" } else { "Your studies" });
            if self.records.is_empty() {
                d.line("Investigate how your agents work, then review the evidence before adopting a change.", INK);
                d.space(); d.band("n  Start your first study", ACCENT, FOCUS); d.space();
                d.label("01  ASK", "Choose a committed project, a question and a representative task.");
                d.label("02  EXPERIMENT", "Compare a baseline, alternative instructions and a combined candidate.");
                d.label("03  REVIEW", "Inspect execution evidence and the proposed resource patch.");
                d.line("Start small: one project and one task.", DIM);
            } else {
                d.line("Open a study to follow progress or review its evidence.", DIM); d.space();
                for (i, r) in self.records.iter().enumerate() {
                    let start = d.rows.len(); let selected = i == self.sel;
                    let bg = if selected { FOCUS } else { PANEL };
                    d.band(&format!("{} {}  ·  {}", if selected { "›" } else { " " }, r.status.to_uppercase(), project_name(&r.project)), status_color(&r.status), bg);
                    d.band(&r.problem, INK, bg);
                    d.band(&format!("{}/{} calls  ·  {}", r.calls, r.max_calls, r.stage), DIM, bg);
                    if selected { d.focus = Some((start, d.rows.len())); }
                    d.space();
                }
            }
            if !ready() { d.heading("Runtime setup"); d.line("i  Install the research runtime", ACCENT); d.line("Requires git, uv and Python 3.12. Uses your native Codex login.", DIM); }
            return d;
        }
        let Some(r) = self.records.get(self.sel) else { return d; };
        if self.page != "overview" {
            let text = self.base.as_ref().and_then(|base| {
                let dir = base.join(&r.id);
                fs::read_to_string(dir.join(&self.page)).ok().or_else(|| if self.page == "REPORT.md" { fs::read_to_string(dir.join("error.log")).ok() } else { None })
            });
            if let Some(text) = text { d.artifact(&text, self.page == "proposal.patch"); }
            else { d.heading("Not available yet"); d.line("This artifact will appear as the study progresses. Press o for live progress.", DIM); }
            return d;
        }
        d.heading(&r.problem);
        d.band(&format!("{}  ·  {}", r.status.to_uppercase(), r.note), status_color(&r.status), PANEL); d.space();
        if r.status == "review" {
            d.line("Compare baseline and candidate evidence before adoption. Improvement is not independently established; try fresh tasks next.", WARN); d.space();
        }
        d.line(&format!("{} / {} model calls", r.calls, r.max_calls), ACCENT);
        d.heading("Experiment trail");
        if r.events.is_empty() { d.line("Waiting for the first session…", DIM); }
        for e in &r.events {
            let mark = match e.status.as_str() { "done" => "✓", "failed" => "!", _ => "›" };
            d.line(&format!("{mark} {} / {} · {}", e.stage, e.label, e.status), status_color(&e.status));
            if !e.note.is_empty() { d.line(&format!("  {}", e.note), INK); } d.space();
        }
        if !r.changed_files.is_empty() { d.heading("Proposed resources"); for file in &r.changed_files { d.line(&format!("+ {file}"), ACCENT); } }
        d.heading("Project & usage");
        d.label("PROJECT", &r.project); d.label("SNAPSHOT", &r.commit);
        d.line(&format!("Tokens: {} in · {} out · {} cached input", r.input_tokens, r.output_tokens, r.cached_input_tokens), INK);
        d.line("Billed cost unavailable · native Codex account", DIM);
        if let Some(base) = &self.base { d.space(); d.label("EVIDENCE FOLDER", &base.join(&r.id).display().to_string()); }
        d
    }
}

const PANEL: V3 = v3(0.045, 0.065, 0.095);
const FOCUS: V3 = v3(0.055, 0.15, 0.17);
const WARN: V3 = v3(0.94, 0.74, 0.44);
const RED: V3 = v3(0.98, 0.57, 0.57);
const BLUE: V3 = v3(0.58, 0.73, 0.98);
const RULE: V3 = v3(0.20, 0.29, 0.36);
fn status_color(s: &str) -> V3 { match s { "failed" | "interrupted" => RED, "cancelled" => WARN, "review" | "done" | "applied" => ACCENT, "running" | "queued" => BLUE, _ => DIM } }
fn project_name(s: &str) -> &str { Path::new(s).file_name().and_then(|s| s.to_str()).unwrap_or(s) }

struct Row { text: String, fg: V3, bg: V3 }
struct Document { rows: Vec<Row>, width: usize, focus: Option<(usize, usize)> }
impl Document {
    fn new(width: usize) -> Self { Self { rows: vec![], width, focus: None } }
    fn space(&mut self) { self.line("", INK); }
    fn line(&mut self, s: &str, fg: V3) { self.band(s, fg, BG); }
    fn band(&mut self, s: &str, fg: V3, bg: V3) {
        for text in wrap(s, self.width.saturating_sub(4)) { self.rows.push(Row { text, fg, bg }); }
    }
    fn heading(&mut self, s: &str) {
        if !self.rows.is_empty() && self.rows.last().is_some_and(|r| !r.text.is_empty()) { self.space(); }
        self.band(s, ACCENT, PANEL); self.space();
    }
    fn label(&mut self, label: &str, value: &str) { self.line(label, DIM); self.line(value, INK); self.space(); }
    fn artifact(&mut self, text: &str, patch: bool) {
        let mut code = false; let mut diff = patch;
        for line in text.lines() {
            if !patch && line.starts_with("```") {
                code = !code; diff = code && line.trim_start_matches('`').trim() == "diff"; self.space(); continue;
            }
            if diff {
                let (fg, bg) = if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff ") { (BLUE, PANEL) }
                    else if line.starts_with('+') { (ACCENT, v3(0.035, 0.12, 0.095)) }
                    else if line.starts_with('-') { (RED, v3(0.15, 0.055, 0.065)) }
                    else if line.starts_with("@@") { (BLUE, PANEL) } else { (INK, PANEL) };
                self.band(line, fg, bg);
            } else if code { self.band(line, INK, PANEL); }
            else if line.starts_with('#') && line.trim_start_matches('#').starts_with(' ') {
                let level = line.chars().take_while(|c| *c == '#').count();
                let title = line.trim_start_matches('#').trim();
                if level <= 2 { self.heading(title); }
                else { self.space(); self.band(title, if level == 3 { BLUE } else { WARN }, PANEL); }
            } else if line.starts_with("Evidence:") || line.starts_with("Project:") || line.starts_with("Snapshot:") { self.line(line, DIM); }
            else { self.line(line, INK); }
        }
    }
}

/// Word wrapping retains explicit newlines and splits long paths without losing characters.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1); let mut out = vec![];
    for line in s.split('\n') {
        let chars: Vec<_> = line.chars().filter(|c| !c.is_control()).collect();
        if chars.is_empty() { out.push(String::new()); continue; }
        let mut from = 0;
        while from < chars.len() {
            let end = (from + width).min(chars.len());
            let split = if end < chars.len() && chars[end] != ' ' {
                (from + 1..end).rev().find(|&i| chars[i] == ' ' && chars[from..i].iter().any(|c| c.is_alphanumeric())).unwrap_or(end)
            } else { end };
            out.push(chars[from..split].iter().collect()); from = split;
            if from < chars.len() && chars[from] == ' ' { from += 1; }
        }
    }
    out
}
fn clipped(s: &str, width: usize) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() <= width { clean } else { clean.chars().take(width.saturating_sub(1)).chain(std::iter::once('…')).collect() }
}
fn paint(fb: &mut Fb, x: usize, y: usize, s: &str, fg: V3, bg: V3) {
    for (i, ch) in s.chars().enumerate() { fb.put_bg((x + i) as i32, y as i32, ch, fg, Some(bg)); }
}
fn rule(fb: &mut Fb, x: usize, y: usize, width: usize) { paint(fb, x, y, &"─".repeat(width), RULE, BG); }
fn actions(v: &View) -> Vec<&'static str> {
    if v.confirm.is_some() { return vec!["y confirm", "Esc back", "n back", "↑↓ scroll"]; }
    if let Some(d) = &v.draft {
        return if d.review.is_some() { vec!["Enter start", "Esc edit", "↑↓ scroll"] }
            else { vec!["Tab fields", "Esc back", "Enter next", "Ctrl+Enter review"] };
    }
    if !v.detail { return vec!["n new study", "Esc back", "Enter open", "↑↓ select", "i install"]; }
    let mut out = vec!["o progress", "v evidence", "p patch", "Esc back"];
    if let Some(r) = v.records.get(v.sel) {
        if r.status == "review" && v.page == "proposal.patch" { out.push("a adopt"); }
        if r.active() { out.push("x cancel"); }
    }
    out
}
pub fn draw(fb: &mut Fb, v: &mut View, _t: f32) {
    fb.clear_text(); fb.px.fill(BG);
    let width = fb.cols.saturating_sub(4).min(104); let x = (fb.cols - width) / 2;
    paint(fb, x, 1, "RESEARCH STATION", ACCENT, BG);
    let context = if v.confirm.is_some() { "Review action" } else if v.draft.is_some() { "Study brief" } else if v.detail { "Study / evidence & resources" } else { "Observe · experiment · learn" };
    paint(fb, x, 2, &clipped(context, width), DIM, BG); rule(fb, x, 3, width);
    let top = 7;
    if v.detail && v.draft.is_none() && v.confirm.is_none() {
        if let Some(r) = v.records.get(v.sel) {
            let stages = ["Snapshot", "Baseline", "Propose", "Trials", "Compose", "Review"];
            let stage = stages.iter().position(|s| s.eq_ignore_ascii_case(&r.stage));
            if width >= 76 {
                let step = width / 6;
                for (i, s) in stages.iter().enumerate() {
                    let current = stage == Some(i);
                    let label = format!("{} {s}", if current { "›" } else if stage.is_some_and(|n| i < n) { "·" } else { " " });
                    if current { fb.panel((x + i * step) as i32, 4, step as i32, 1, FOCUS); }
                    paint(fb, x + i * step, 4, &label, if current { ACCENT } else { DIM }, if current { FOCUS } else { BG });
                }
            } else { paint(fb, x, 4, &clipped(&format!("{} · {}", r.status.to_uppercase(), r.stage), width), status_color(&r.status), BG); }
        }
        let tabs = if width < 34 { [("overview", "o Live"), ("REPORT.md", "v Read"), ("proposal.patch", "p Diff")] }
            else { [("overview", "o Progress"), ("REPORT.md", "v Evidence"), ("proposal.patch", "p Patch")] };
        let mut col = x;
        for (page, label) in tabs { let active = v.page == page; paint(fb, col, 5, label, if active { ACCENT } else { DIM }, if active { FOCUS } else { BG }); col += label.chars().count() + 2; }
    } else { paint(fb, x, 5, if v.confirm.is_some() { "Confirm your choice below" } else if v.draft.as_ref().is_some_and(|d| d.review.is_some()) { "02 / Review & launch" } else if v.draft.is_some() { "01 / Question & tasks" } else { "RESEARCH LABORATORY" }, DIM, BG); }
    let bottom = fb.rows.saturating_sub(5).max(top + 1); let page = bottom.saturating_sub(top).max(1);
    let doc = v.document(width);
    if let Some((start, end)) = doc.focus {
        if start < v.scroll { v.scroll = start; }
        if end > v.scroll + page { v.scroll = if end - start <= page { start } else { end.saturating_sub(page) }; }
    }
    v.scroll = v.scroll.min(doc.rows.len().saturating_sub(page));
    for (i, row) in doc.rows.iter().skip(v.scroll).take(page).enumerate() {
        fb.panel(x as i32, (top + i) as i32, width as i32, 1, row.bg);
        paint(fb, x + 2, top + i, &row.text, row.fg, row.bg);
    }
    rule(fb, x, bottom, width);
    let position = if doc.rows.len() > page {
        let navigation = if v.draft.as_ref().is_some_and(|d| d.review.is_none()) { "Tab fields" } else if !v.detail && v.draft.is_none() && v.confirm.is_none() { "↑↓ select" } else { "↑↓ PgUp PgDn" };
        format!("{}–{} / {} · {navigation}", v.scroll + 1, (v.scroll + page).min(doc.rows.len()), doc.rows.len())
    }
        else if v.detail && v.page == "proposal.patch" { "Resource patch · review before adoption".into() }
        else { "HARNESS² · isolated experiments".into() };
    paint(fb, x, bottom + 1, &clipped(&position, width), DIM, BG);
    let mut row = bottom + 2; let mut col = 0;
    for hint in actions(v) {
        let n = hint.chars().count(); let gap = if col == 0 { 0 } else { 3 };
        if col + gap + n > width { row += 1; col = 0; }
        if row >= fb.rows || n > width { break; }
        if col != 0 { paint(fb, x + col, row, " · ", RULE, BG); col += 3; }
        let key = hint.split(' ').next().unwrap_or("").chars().count();
        for (i, ch) in hint.chars().enumerate() { fb.put_bg((x + col + i) as i32, row as i32, ch, if i < key { ACCENT } else { INK }, Some(BG)); }
        col += n;
    }
}

pub fn cli(args: &[String]) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("setup") => return setup(),
        Some("start") => { let file = args.get(1).ok_or_else(|| io::Error::other("empire research start REQUEST.json"))?;
            let q = validate(serde_json::from_slice(&fs::read(file)?)?)?; println!("{}", start(&q, &root())?); return Ok(()); }
        Some("--snapshot") => { let mut fb = Fb::new(140, 48); let mut view = View { base: None, ..Default::default() }; draw(&mut fb, &mut view, 0.0);
            Out::preview(fb.cols * fb.rows, true).present(&fb, &mut io::stdout())?; return execute!(io::stdout(), style::ResetColor, cursor::MoveTo(0, fb.rows as u16)); }
        Some(_) => return Err(io::Error::other("empire research [setup | start REQUEST.json | --snapshot]")),
        None => {}
    }
    // Can also be opened without the galaxy, its daemon, or any running ships.
    terminal::enable_raw_mode()?;
    struct Restore;
    impl Drop for Restore { fn drop(&mut self) { let _ = execute!(io::stdout(), style::ResetColor, cursor::Show, event::DisableBracketedPaste, terminal::LeaveAlternateScreen); let _ = terminal::disable_raw_mode(); } }
    let _restore = Restore;
    execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide, event::EnableBracketedPaste)?;
    let mut view = View::default();
    let here = std::env::current_dir().ok().filter(|p| p.join(".git").exists()).map(|p| p.to_string_lossy().into_owned());
    view.projects = here.clone().into_iter().collect(); view.open(here); let start = Instant::now();
    let (cols, rows) = terminal::size()?; let mut fb = Fb::new(cols as usize, rows as usize); let mut out = Out::preview(fb.cols * fb.rows, true);
    loop {
        view.poll(false); draw(&mut fb, &mut view, start.elapsed().as_secs_f32()); out.present(&fb, &mut io::stdout())?;
        if event::poll(Duration::from_millis(100))? { let ev = event::read()?;
            if let Event::Resize(c, r) = ev { fb = Fb::new(c as usize, r as usize); out = Out::preview(fb.cols * fb.rows, true); }
            else if view.input(&ev) { break; }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(c: KeyCode) -> Event { Event::Key(event::KeyEvent::new(c, KeyModifiers::NONE)) }
    fn screen(fb: &Fb) -> String { fb.txt.chunks(fb.cols).map(|row| row.iter().map(|c| c.as_ref().map_or(' ', |c| c.ch)).collect::<String>()).collect::<Vec<_>>().join("\n") }
    #[test] fn evidence_has_heading_hierarchy_and_diff_semantics() {
        let dir = std::env::temp_dir().join(format!("empire-research-design-{}", std::process::id()));
        fs::create_dir_all(dir.join("study")).unwrap();
        fs::write(dir.join("study/REPORT.md"), "# Research: Layout\n\n## Result\nCandidate ready for review.\n\n## Evidence\n### task-1\n#### base1\nEvidence: executions/task-1/base1/answer.md\nChecks passed.\n\n## Proposed resource patch\n```diff\n--- a/AGENTS.md\n+++ b/AGENTS.md\n@@ -1 +1 @@\n-old instruction\n+new instruction\n```\n").unwrap();
        let mut v = View { base: Some(dir.clone()), detail: true, page: "REPORT.md".into(), records: vec![Record { id: "study".into(), problem: "Layout".into(), status: "review".into(), stage: "review".into(), ..Default::default() }], ..Default::default() };
        let mut fb = Fb::new(140, 60); draw(&mut fb, &mut v, 0.0);
        let s = screen(&fb); assert!(s.contains("Result")); assert!(!s.contains("## Result"), "Report headings must read as headings, not raw Markdown");
        let cell = |needle: &str| { let row = s.lines().position(|r| r.contains(needle)).unwrap(); let col = s.lines().nth(row).unwrap().chars().collect::<String>().find(needle).unwrap(); fb.txt[row * fb.cols + col].unwrap() };
        assert_ne!(cell("Result").fg, cell("Checks passed.").fg);
        assert_ne!(cell("-old instruction").fg, cell("+new instruction").fg);
        assert_ne!(cell("-old instruction").bg, cell("+new instruction").bg);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test] fn launch_and_confirmation_keep_complete_actions_at_all_widths() {
        for width in [40, 80, 140] {
            let mut fb = Fb::new(width, 32);
            let mut d = Draft::new("/projects/example"); d.review = Some(Request { problem: "Check layout".into(), project: "/projects/example".into(), tasks: vec!["Check footer".into()], minutes: 30, ..Default::default() });
            let mut v = View { draft: Some(d), ..Default::default() }; draw(&mut fb, &mut v, 0.0);
            let s = screen(&fb); assert!(s.contains("Enter start")); assert!(s.contains("Esc edit"), "{width}: {s}");
            v.draft = None; v.detail = true; v.confirm = Some("apply".into());
            v.records.push(Record { project: "/projects/a-long-project-folder/target".into(), changed_files: vec!["AGENTS.md".into()], ..Default::default() });
            draw(&mut fb, &mut v, 0.0); let s = screen(&fb);
            assert!(s.contains("target"), "The confirmation target must wrap, not disappear: {width}: {s}");
            assert!(s.contains("y confirm")); assert!(s.contains("Esc back"));
        }
    }
    #[test] fn drafting_and_cancel_confirmation_do_not_launch_or_apply() {
        let mut v = View::default(); v.input(&key(KeyCode::Char('n'))); assert!(v.draft.is_some());
        v.input(&Event::Paste("Improve layout checks".into())); assert_eq!(v.draft.as_ref().unwrap().fields[0].s, "Improve layout checks");
        v.projects = vec!["/alpha".into(), "/beta".into()];
        v.input(&key(KeyCode::Tab)); v.input(&key(KeyCode::Down)); assert_eq!(v.draft.as_ref().unwrap().fields[1].s, "/alpha");
        v.input(&key(KeyCode::Down)); assert_eq!(v.draft.as_ref().unwrap().fields[1].s, "/beta");
        v.input(&key(KeyCode::Esc)); assert!(v.draft.is_none());
        v.records.push(Record { status: "running".into(), ..Default::default() }); v.detail = true;
        v.input(&key(KeyCode::Char('x'))); assert_eq!(v.confirm.as_deref(), Some("cancel"));
        v.input(&key(KeyCode::Esc)); assert!(v.confirm.is_none()); assert_eq!(v.records[0].status, "running");
        v.records[0].status = "review".into(); v.input(&key(KeyCode::Char('a'))); assert!(v.confirm.is_none());
        v.input(&key(KeyCode::Char('p'))); v.input(&key(KeyCode::Char('a'))); assert_eq!(v.confirm.as_deref(), Some("apply"));
    }
    #[test] fn station_previews_keep_primary_controls_visible() {
        let dir = std::env::temp_dir().join(format!("empire-research-scenes-{}", std::process::id()));
        fs::create_dir_all(dir.join("study")).unwrap();
        let patch = "diff --git a/AGENTS.md b/AGENTS.md\n--- a/AGENTS.md\n+++ b/AGENTS.md\n@@ -1,2 +1,3 @@\n Verify terminal changes.\n-Check that the footer fits.\n+Inspect rendered cells at 40, 80 and 140 columns.\n+Keep the selected action, help and leave visible.\n";
        fs::write(dir.join("study/proposal.patch"), patch).unwrap();
        fs::write(dir.join("study/REPORT.md"), format!("# Research: Better layout verification\n\n## Result\nCandidate resources ready for review; improvement is not independently established.\n\nThis is model-selected execution evidence. Check these resources on fresh tasks before broader adoption.\n\nModel calls: 8/10\nInput tokens: 4254199; output tokens: 32071.\nBilled cost: unavailable from native Codex CLI.\n\n## Evidence\n### task-1\nMake the galaxy footer easier to use on small terminals while retaining the existing keyboard shortcuts. Keep the implementation focused on footer rendering.\n\n#### base1\nEvidence: executions/research/task-1/base1/answer.md\nChanged the footer to keep help and leave visible, prioritize the selected action, and show only complete hints that fit.\n\nChecks run:\n- Footer tests: all 3 pass; clipping failures confirmed before the change.\n- cargo build --offline: passes.\n\n#### agg1\nEvidence: executions/research/task-1/agg1/answer.md\nVerified six selections at 40, 80 and 140 columns. No interactive terminal or audio playback check.\n\n## Proposed resource patch\n```diff\n{patch}```\n")).unwrap();
        for (width, height) in [(40, 24), (80, 32), (140, 48)] {
            for page in ["empty", "list", "draft", "tasks", "launch", "progress", "review", "evidence", "patch", "adopt", "cancel", "failed", "applied", "missing", "setup"] {
                let mut fb = Fb::new(width, height);
                let record = Record { id: "study".into(), problem: "Investigate better verification of terminal UI changes".into(), status: "running".into(), stage: "trials".into(), calls: 4, max_calls: 10, project: "/projects/agent-empire".into(), commit: "274cb2c2fa4e1bc73c5b558530db4598f60b1ba9".into(), note: "Running alternative instructions".into(), changed_files: vec!["AGENTS.md".into(), ".agents/skills/terminal-footer-contract/SKILL.md".into()], events: vec![Step { label: "base1".into(), stage: "baseline".into(), status: "done".into(), note: "Execution evidence saved".into() }, Step { label: "agg1".into(), stage: "trials".into(), status: "running".into(), note: "Checking the candidate instructions".into() }], ..Default::default() };
                let mut v = View { base: Some(dir.clone()), detail: true, records: vec![record], ..Default::default() };
                match page {
                    "empty" | "setup" => { v.records.clear(); v.detail = false; if page == "setup" { v.message = "Installing the research runtime. You can leave this screen.".into(); } }
                    "list" => { v.detail = false; let mut old = v.records[0].clone(); old.status = "review".into(); old.problem = "Improve the release verification workflow".into(); old.stage = "review".into(); v.records.push(old); }
                    "draft" | "tasks" | "launch" => {
                        let mut d = Draft::new("/projects/agent-empire"); d.fields[0].set("Investigate better layout verification".into());
                        d.fields[3].set("Improve the galaxy footer on small terminals.\nCheck essential actions at 40, 80 and 140 columns.".into());
                        if page == "tasks" { d.field = 3; }
                        if page == "launch" { d.review = Some(Request { problem: d.fields[0].s.clone(), project: d.fields[1].s.clone(), revision: v.records[0].commit.clone(), tasks: d.fields[3].s.lines().map(String::from).collect(), minutes: 30 }); }
                        v.draft = Some(d);
                    }
                    "review" | "evidence" | "patch" | "adopt" | "applied" => {
                        v.records[0].status = if page == "applied" { "applied" } else { "review" }.into(); v.records[0].stage = "review".into();
                        v.records[0].note = if page == "applied" { "Reviewed resources applied; not committed" } else { "Candidate resources ready for review" }.into();
                        if page == "evidence" { v.page = "REPORT.md".into(); }
                        if page == "patch" || page == "adopt" { v.page = "proposal.patch".into(); }
                        if page == "adopt" { v.confirm = Some("apply".into()); }
                    }
                    "cancel" => v.confirm = Some("cancel".into()),
                    "failed" => { v.records[0].status = "failed".into(); v.records[0].note = "Codex did not complete. Execution evidence is retained.".into(); }
                    "missing" => { v.page = "REPORT.md".into(); v.records[0].id = "missing".into(); }
                    _ => {}
                }
                draw(&mut fb, &mut v, 4.0);
                let text = screen(&fb); let footer = text.lines().skip(height - 3).collect::<Vec<_>>().join("\n");
                let expected: &[&str] = match page {
                    "empty" | "setup" | "list" => &["n new study", "Esc back", "Enter open", "↑↓ select", "i install"],
                    "draft" | "tasks" => &["Tab fields", "Esc back", "Enter next", "Ctrl+Enter review"],
                    "launch" => &["Enter start", "Esc edit", "↑↓ scroll"],
                    "adopt" | "cancel" => &["y confirm", "Esc back", "n back", "↑↓ scroll"],
                    "patch" => &["o progress", "v evidence", "p patch", "Esc back", "a adopt"],
                    "progress" | "missing" => &["o progress", "v evidence", "p patch", "Esc back", "x cancel"],
                    _ => &["o progress", "v evidence", "p patch", "Esc back"],
                };
                for hint in expected { assert!(footer.contains(hint), "{page} {width} missing {hint}: |{footer}|"); }
                for line in footer.lines() {
                    for hint in line.trim().split(" · ").filter(|s| !s.is_empty()) { assert!(expected.contains(&hint), "{page} {width}: incomplete hint |{hint}|"); }
                }
                assert!(text.contains("RESEARCH STATION"));
                assert!(!fb.txt.iter().flatten().any(|c| c.ch.is_control()));
                if let Some(dir) = std::env::var_os("EMPIRE_RESEARCH_PREVIEWS") {
                    fs::create_dir_all(&dir).unwrap(); let mut ansi = vec![];
                    Out::preview(fb.cols * fb.rows, true).present(&fb, &mut ansi).unwrap();
                    fs::write(PathBuf::from(&dir).join(format!("{page}-{width}.ansi")), ansi).unwrap();
                    fs::write(PathBuf::from(&dir).join(format!("{page}-{width}.txt")), text).unwrap();
                }
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test] fn wrapping_keeps_selection_and_edit_cursor_visible() {
        for width in [40, 80, 140] {
            let mut fb = Fb::new(width, 24);
            let mut v = View { records: (0..12).map(|i| Record { problem: format!("Study {i}: {}", "a long question about checking layout changes ".repeat(3)), status: "review".into(), ..Default::default() }).collect(), sel: 11, ..Default::default() };
            draw(&mut fb, &mut v, 0.0); assert!(screen(&fb).contains("Study 11:"));
            v.sel = 0; draw(&mut fb, &mut v, 0.0); assert!(screen(&fb).contains("Study 0:"));
            let mut d = Draft::new("/example"); d.field = 3; d.fields[3].set("Check narrow layouts.\n".repeat(25)); d.fields[3].cur = 13;
            v.draft = Some(d); draw(&mut fb, &mut v, 0.0); assert!(screen(&fb).contains('▏'));
            v.draft.as_mut().unwrap().fields[3].cur = 400;
            draw(&mut fb, &mut v, 0.0); assert!(screen(&fb).contains('▏'));
        }
        let path = "/projects/a-very-long-project/and-a-resource-name.md";
        assert_eq!(wrap(path, 12).concat(), path);
        assert!(wrap("a question with several words", 12).iter().all(|s| s.chars().count() <= 12));
    }
    #[test] fn pages_render_at_narrow_sizes_and_untrusted_text_is_plain() {
        for width in [40, 80, 140] { let mut fb = Fb::new(width, 32); let mut v = View::default();
            draw(&mut fb, &mut v, 0.0); v.input(&key(KeyCode::Char('n'))); draw(&mut fb, &mut v, 0.0);
            v.draft = None; v.detail = true; v.records.push(Record { problem: "Layout\x1b[31m study".into(), status: "review".into(), ..Default::default() }); draw(&mut fb, &mut v, 0.0);
        }
    }
}
