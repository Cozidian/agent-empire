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
            if k.code == KeyCode::Char('y') {
                if let (Some(base), Some(r)) = (&self.base, self.records.get(self.sel)) {
                    let dir = base.join(&r.id);
                    let result = if action == "apply" { apply(&dir, &r.project) } else { fs::write(dir.join("cancel"), "cancel\n").map(|_| "Cancellation requested; active model process will stop.".into()) };
                    self.result(result); self.poll(true);
                }
                self.confirm = None;
            } else if matches!(k.code, KeyCode::Esc | KeyCode::Char('n')) { self.confirm = None; }
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
            KeyCode::Char('n') => { self.draft = Some(Draft::new(&self.project)); self.message.clear(); }
            KeyCode::Char('i') if self.setup.is_none() => {
                let result = (|| { fs::create_dir_all(data())?; let f = fs::File::create(data().join("setup.log"))?;
                    Command::new(std::env::current_exe()?).args(["research", "setup"]).stdin(Stdio::null()).stdout(f.try_clone()?).stderr(f).spawn() })();
                match result { Ok(c) => { self.setup = Some(c); self.message = "Installing pinned Harness2 runtime. You can leave this screen.".into(); }, Err(e) => self.message = e.to_string() }
            }
            KeyCode::Enter if !self.records.is_empty() => { self.detail = true; self.scroll = 0; }
            KeyCode::Up | KeyCode::Char('k') => { if self.detail { self.scroll = self.scroll.saturating_sub(1); } else { self.sel = self.sel.saturating_sub(1); } }
            KeyCode::Down | KeyCode::Char('j') => { if self.detail { self.scroll += 1; } else { self.sel = (self.sel + 1).min(self.records.len().saturating_sub(1)); } }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(12),
            KeyCode::PageDown => self.scroll += 12,
            KeyCode::Home => self.scroll = 0,
            KeyCode::Char('p' | 'v' | 'o') if self.detail => { self.page = match k.code { KeyCode::Char('p') => "proposal.patch", KeyCode::Char('v') => "REPORT.md", _ => "overview" }.into(); self.scroll = 0; }
            KeyCode::Char('a') if self.detail && self.page == "proposal.patch" => {
                if self.records.get(self.sel).is_some_and(|r| r.status == "review") { self.confirm = Some("apply".into()); }
            }
            KeyCode::Char('x') if self.detail => { if self.records.get(self.sel).is_some_and(Record::active) { self.confirm = Some("cancel".into()); } }
            _ => {}
        }
        false
    }
    fn lines(&self) -> Vec<String> {
        if let Some(d) = &self.draft {
            if let Some(q) = &d.review {
                let mut out = vec!["READY TO RESEARCH".into(), String::new(), q.problem.clone(), format!("Project: {}", q.project), format!("Snapshot: {}", q.revision), String::new()];
                out.extend(q.tasks.iter().enumerate().map(|(i, s)| format!("Task {}: {s}", i + 1)));
                out.extend([String::new(), format!("Limit: {} minutes / {} model calls", q.minutes, q.tasks.len() * 10), "Native Codex account; billed cost unavailable. Token usage is recorded.".into(),
                    "Trials use isolated copies of this commit. Uncommitted files are excluded.".into(), "Resources are proposed for review. Adoption is a separate action.".into(), String::new(), "Enter start experiment    Esc edit brief".into()]); return out;
            }
            return vec!["NEW STUDY".into(), "Ask a question; add 1–3 tasks.".into()];
        }
        if !self.detail {
            let mut out = vec!["A place to improve how your agents work.".into(), "Freeze a project · run alternatives · inspect evidence · adopt resources".into(), String::new()];
            if self.records.is_empty() { out.extend(["No studies yet. Press n to ask your first research question.".into(), String::new(),
                "Start small: one committed project and one representative task.".into(), "A study runs a baseline, two proposals, and a combined candidate.".into(),
                "The result is a resource patch and an evidence trail for your review.".into()]); }
            else { for (i, r) in self.records.iter().enumerate() { out.push(format!("{} {}  ·  {}", if i == self.sel { "›" } else { " " }, r.status.to_uppercase(), r.problem)); out.push(format!("    {}  ·  {}/{} calls", r.id, r.calls, r.max_calls)); out.push(String::new()); } }
            return out;
        }
        let Some(r) = self.records.get(self.sel) else { return vec![]; };
        if self.page != "overview" {
            let text = self.base.as_ref().and_then(|p| {
                let dir = p.join(&r.id);
                fs::read_to_string(dir.join(&self.page)).ok().or_else(|| if self.page == "REPORT.md" { fs::read_to_string(dir.join("error.log")).ok() } else { None })
            }).unwrap_or_else(|| "This artifact is not available yet. Press o for live progress.".into());
            return std::iter::once(format!("{} · {}", self.page, r.problem)).chain(std::iter::once(String::new())).chain(text.lines().map(String::from)).collect();
        }
        let mut out = vec![r.problem.clone(), format!("{} · {}", r.status.to_uppercase(), r.note), String::new(), format!("Project: {}", r.project), format!("Snapshot: {}", r.commit),
            format!("Calls {}/{} · tokens {} in / {} out ({} cached input)", r.calls, r.max_calls, r.input_tokens, r.output_tokens, r.cached_input_tokens),
            "Billed cost unavailable · native Codex account".into(), String::new(), "EXPERIMENT TRAIL".into()];
        for e in &r.events { out.push(format!("{} {} / {}  {}", match e.status.as_str() { "done" => "✓", "failed" => "!", _ => "◌" }, e.stage, e.label, e.note)); }
        if r.status == "review" { out.extend([String::new(), "Proposal ready. Compare baseline and candidate evidence before adoption.".into(), "This is not an independent proof of improvement. Try fresh tasks next.".into(), format!("Resources: {}", r.changed_files.join(", "))]); }
        if let Some(base) = &self.base { out.extend([String::new(), format!("Evidence folder: {}", base.join(&r.id).display())]); }
        out
    }
}
fn text(fb: &mut Fb, row: i32, value: &str, col: V3) { let s: String = value.chars().filter(|c| !c.is_control()).take(fb.cols.saturating_sub(6)).collect(); fb.text(3, row, &s, col); }
fn wrap(lines: Vec<String>, width: usize) -> Vec<String> { lines.into_iter().flat_map(|s| { let chars: Vec<_> = s.chars().collect(); if chars.is_empty() { vec![String::new()] } else { chars.chunks(width.max(1)).map(|c| c.iter().collect()).collect() } }).collect() }
pub fn draw(fb: &mut Fb, v: &mut View, t: f32) {
    fb.clear_text(); fb.px.fill(BG);
    text(fb, 1, "RESEARCH STATION", ACCENT);
    text(fb, 2, "HARNESS²  /  observe · experiment · learn", DIM);
    let stages = ["Snapshot", "Baseline", "Propose", "Trials", "Compose", "Review"];
    let stage = v.records.get(v.sel).filter(|_| v.detail && v.draft.is_none()).map_or("", |r| r.stage.as_str());
    let active = stages.iter().position(|s| s.eq_ignore_ascii_case(stage));
    if fb.cols >= 70 {
        let step = (fb.cols.saturating_sub(10) / 6).max(1);
        for (i, s) in stages.iter().enumerate() {
            let x = (5 + i * step) as f32 * 2.0; let y = 18.0;
            let color = if active == Some(i) { ACCENT * (0.85 + (t * 2.0).sin() * 0.15) } else { DIM * 0.6 };
            if i < 5 { fb.line(x + 3.0, y, x + step as f32 * 2.0 - 3.0, y, DIM * 0.28, DIM * 0.28); }
            for a in 0..24 { let a = a as f32 * std::f32::consts::TAU / 24.0; fb.splat(x + a.cos() * 3.0, y + a.sin() * 3.0, color); }
            fb.text(x as i32 / 2 - 2, 6, s, color);
        }
    } else { text(fb, 5, &format!("Stage: {}", if stage.is_empty() { "ready for a question" } else { stage }), ACCENT); }
    let bottom = (fb.rows as i32 - 5).max(9);
    let lines = wrap(v.lines(), fb.cols.saturating_sub(6));
    let page = (bottom - 8).max(1) as usize;
    if !v.detail && v.draft.is_none() { v.scroll = if v.sel * 3 + 5 > page { v.sel * 3 + 5 - page } else { 0 }; }
    v.scroll = v.scroll.min(lines.len().saturating_sub(page));
    for (i, s) in lines.iter().skip(v.scroll).take(page).enumerate() { text(fb, 8 + i as i32, s, if i == 0 && v.scroll == 0 { INK } else { DIM }); }
    if let Some(d) = &v.draft && d.review.is_none() {
        let names = ["Research question", "Project folder (↑↓ choose)", "Commit or ref", "Tasks (one per line; Alt+Enter adds)", "Time limit in minutes"];
        // Show the active field on short terminals; all fields when there is room.
        let count = ((bottom - 13) / 3).max(1) as usize; let first = d.field.saturating_sub(count - 1);
        for i in first..(first + count).min(5) {
            let y = 13 + (i - first) as i32 * 3;
            text(fb, y, &format!("{} {}", if i == d.field { "›" } else { " " }, names[i]), if i == d.field { ACCENT } else { DIM });
            let field = &d.fields[i]; let shown = field.s.replace('\n', "  |  "); let room = fb.cols.saturating_sub(9);
            let tail: String = shown.chars().skip(shown.chars().count().saturating_sub(room)).collect();
            text(fb, y + 1, &format!("  {tail}{}", if i == d.field { "▏" } else { "" }), INK);
        }
    }
    if let Some(action) = &v.confirm {
        let note = if action == "apply" { v.records.get(v.sel).map(|r| format!("Apply {} reviewed resources to {}?", r.changed_files.len(), r.project)).unwrap_or_default() } else { "Cancel this experiment and retain its evidence?".into() };
        text(fb, bottom, &note, ACCENT); text(fb, bottom + 1, "y confirm · n / Esc back", INK);
    } else {
        let message = if !v.message.is_empty() { v.message.clone() } else if !ready() { "Runtime not installed · i install (git + uv + Python 3.12)".into() } else { String::new() };
        text(fb, bottom, &message, ACCENT);
        let hints = if fb.cols < 70 && v.draft.is_none() && v.detail { "v evidence · p patch · o progress" } else if fb.cols < 70 && v.draft.is_none() { "n new · Enter open · i install" } else if v.draft.as_ref().is_some_and(|d| d.review.is_some()) { "Enter start experiment · Esc edit brief" } else if v.draft.is_some() { "Tab fields · Enter next / review · Esc back" } else if v.detail { "o progress · v evidence · p patch · a adopt · x cancel" } else { "n new study · Enter open · ↑↓ select · i install runtime" };
        text(fb, bottom + 1, hints, INK);
    }
    if v.records.is_empty() && v.draft.is_none() && fb.cols >= 100 && fb.rows >= 36 {
        let (x, y) = (fb.w as f32 * 0.72, fb.h as f32 * 0.63);
        for n in 0..180 { let a = n as f32 * std::f32::consts::TAU / 180.0;
            fb.splat(x + a.cos() * 30.0, y + a.sin() * 12.0, ACCENT * 0.45);
            fb.splat(x + a.cos() * 12.0, y + a.sin() * 30.0, ACCENT * 0.45);
        }
        for i in 0..3 { let a = t * 0.2 + i as f32 * std::f32::consts::TAU / 3.0;
            let (nx, ny) = (x + a.cos() * 30.0, y + a.sin() * 12.0);
            fb.line(x, y, nx, ny, ACCENT * 0.1, ACCENT * 0.6);
            for dy in -2..=2 { for dx in -2..=2 { fb.splat(nx + dx as f32, ny + dy as f32, ACCENT * (1.0 / (1 + dx*dx + dy*dy) as f32)); } }
        }
        fb.text(x as i32 / 2 - 7, y as i32 / 4, "better workflows", ACCENT);
    }
    text(fb, fb.rows as i32 - 2, if fb.cols < 70 && v.detail && v.draft.is_none() { "a adopt · x cancel · Esc back" } else { "↑↓ / PgUp PgDn scroll · Esc back" }, DIM);
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
        for (width, height) in [(40, 24), (80, 32), (140, 48)] {
            let mut fb = Fb::new(width, height); let mut v = View::default();
            for page in ["empty", "draft", "progress", "review"] {
                match page {
                    "draft" => { let mut d = Draft::new("/projects/example"); d.fields[0].set("Investigate better layout verification".into()); d.fields[3].set("Make a settings panel; check narrow and wide layouts".into()); v.draft = Some(d); }
                    "progress" => { v.draft = None; v.detail = true; v.records.push(Record { problem: "Example study: better layout verification".into(), status: "running".into(), stage: "trials".into(), calls: 4, max_calls: 10, project: "/projects/example".into(), note: "Running alternative instructions".into(), events: vec![Step { label: "baseline".into(), stage: "baseline".into(), status: "done".into(), note: "Evidence saved".into() }], ..Default::default() }); }
                    "review" => { v.records[0].status = "review".into(); v.records[0].stage = "review".into(); v.records[0].note = "Candidate resources ready for review".into(); }
                    _ => {}
                }
                draw(&mut fb, &mut v, 4.0);
                let text: String = fb.txt.iter().map(|c| c.as_ref().map_or(' ', |c| c.ch)).collect();
                assert!(text.contains("RESEARCH STATION"));
                assert!(text.contains(if page == "empty" { "n new" } else if page == "draft" { "Tab fields" } else { "v evidence" }));
                if let Some(dir) = std::env::var_os("EMPIRE_RESEARCH_PREVIEWS") {
                    fs::create_dir_all(&dir).unwrap(); let mut ansi = vec![];
                    Out::preview(fb.cols * fb.rows, true).present(&fb, &mut ansi).unwrap();
                    fs::write(PathBuf::from(dir).join(format!("{page}-{width}.ansi")), ansi).unwrap();
                }
            }
        }
    }
    #[test] fn pages_render_at_narrow_sizes_and_untrusted_text_is_plain() {
        for width in [40, 80, 140] { let mut fb = Fb::new(width, 32); let mut v = View::default();
            draw(&mut fb, &mut v, 0.0); v.input(&key(KeyCode::Char('n'))); draw(&mut fb, &mut v, 0.0);
            v.draft = None; v.detail = true; v.records.push(Record { problem: "Layout\x1b[31m study".into(), status: "review".into(), ..Default::default() }); draw(&mut fb, &mut v, 0.0);
        }
    }
}
