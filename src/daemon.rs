//! The empire daemon: owns every agent's pseudo-terminal and screen state, works
//! out who is busy, and serves the game over a unix socket. Agents live as long
//! as the daemon does, so the game UI can come and go.

use crate::role::Role;
use crate::proto::*;
use crate::jev;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::num::NonZeroU16;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const ROWS: u16 = 40;
const COLS: u16 = 120;
/// Ignore status heuristics while an agent paints its startup screen.
const BOOT: f32 = 4.0;
/// Output this soon after the player typed is just echo, not work.
const ECHO: f32 = 0.4;
/// Lines of history kept per agent for scrolling back in comms.
const HISTORY: usize = 5000;
/// How long an edit counts toward a border conflict.
const WAR_MEMORY: u64 = 600;
/// Turns shorter than this finishing aren't worth a desktop notification.
const DESKTOP_TURN: u64 = 15;
/// The build this daemon was started from (see `build_id`).
static BUILD: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[derive(Clone, Copy, PartialEq, Debug)]
enum Status {
    Booting,
    Working,
    Blocked,
    Done,
    Idle,
    Exited,
}

impl Status {
    fn parse(s: &str) -> Self {
        match s {
            "working" => Status::Working,
            "blocked" => Status::Blocked,
            "done" => Status::Done,
            "idle" => Status::Idle,
            "exited" => Status::Exited,
            _ => Status::Booting,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Status::Booting => "booting",
            Status::Working => "working",
            Status::Blocked => "blocked",
            Status::Done => "done",
            Status::Idle => "idle",
            Status::Exited => "exited",
        }
    }
}

/// vt100 callbacks: remember the window title and answer terminal queries
/// (cursor position, device attributes, colours) that TUIs block on at startup.
#[derive(Default)]
struct Cb {
    replies: Vec<u8>,
    title: String,
}

impl vt100::Callbacks for Cb {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = String::from_utf8_lossy(title).trim().to_string();
    }

    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _: Option<u8>, params: &[&[u16]], c: char) {
        let p0 = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let reply = match (i1, c, p0) {
            (None, 'n', 6) => {
                let (r, col) = screen.cursor_position();
                format!("\x1b[{};{}R", r + 1, col + 1)
            }
            (None, 'n', 5) => "\x1b[0n".into(),
            (None, 'c', _) => "\x1b[?62;22c".into(),
            (Some(b'>'), 'c', _) => "\x1b[>1;10;0c".into(),
            _ => return,
        };
        self.replies.extend_from_slice(reply.as_bytes());
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        if params.len() >= 2 && params[1] == b"?" {
            let reply = match params[0] {
                b"10" => "\x1b]10;rgb:d8d8/dede/e9e9\x1b\\",
                b"11" => "\x1b]11;rgb:0b0b/0f0f/1a1a\x1b\\",
                _ => return,
            };
            self.replies.extend_from_slice(reply.as_bytes());
        }
    }
}

/// The daemon's end of an agent's terminal: the pty master and the child's pid.
/// Plain fds and pids survive the daemon exec'ing a newer version of itself.
struct Pty {
    master: File,
    pid: i32,
}

impl Pty {
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        self.master.write_all(b)
    }

    fn resize(&self, rows: u16, cols: u16) {
        let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    }

    fn kill(&self) {
        // pid 0 would hang up our whole process group
        if self.pid > 0 {
            unsafe { libc::kill(self.pid, libc::SIGHUP) };
        }
    }
}

struct Agent {
    id: u32,
    kind: String,
    cwd: String,
    parser: vt100::Parser<Cb>,
    writer: Pty,
    /// The agent's own session id, for resuming it after a reboot.
    session: Option<String>,
    born: SystemTime,
    started: Instant,
    last_out: Instant,
    last_in: Instant,
    /// Status reported by the agent's own hooks, when it has them.
    hook: Option<Status>,
    /// A hook has reported in, so those reports outrank the screen.
    hooks: bool,
    /// The last question the player answered, and how often it was on screen then.
    answered: Option<(String, usize)>,
    status: Status,
    since: Instant,
    exited: Option<Instant>,
    turns: u32,
    report: Report,
    /// Files it edited lately (absolute path → when), for spotting border conflicts.
    touched: HashMap<String, Instant>,
    /// Screen lines already read for edits.
    edits_seen: HashSet<String>,
    /// The status we last told the desktop about.
    told: Status,
    /// When the current turn started.
    worked_at: Option<Instant>,
    /// The last thing it was asked to do, and what's being typed to it now.
    task: String,
    typed: String,
    role: Role,
    /// Its own orders and skills, from the design it was launched from.
    spec: ShipSpec,
    /// A refit ordered for it; `role` and `spec` stay what it's actually running with.
    refit: Option<RefitState>,
    /// The command a shell session runs instead of the shell (an editor on a file).
    run: Vec<String>,
    /// Screen generation, bumped on every change, and the signal waiting viewers sleep on.
    version: u64,
    changed: Arc<Condvar>,
}

impl Agent {
    /// Remember a prompt as what it's working on: its first line, unless it's a slash
    /// command or too short to be one (a menu pick, a y/n).
    fn note_task(&mut self, text: &str) {
        let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        if line.chars().count() >= 3 && !line.starts_with('/') {
            self.task = line.to_string();
        }
    }

    /// Follow keystrokes from comms, so a prompt typed there becomes the task once it's sent.
    fn keys(&mut self, data: &str) {
        let mut cs = data.chars().peekable();
        while let Some(c) = cs.next() {
            match c {
                // escape sequences: arrows and the like are skipped, a paste's brackets kept out
                '\x1b' => {
                    if cs.peek() == Some(&'[') {
                        cs.next();
                        while cs.next().is_some_and(|c| !('@'..='~').contains(&c)) {}
                    } else if cs.next() == Some('O') {
                        cs.next(); // SS3: arrows and F1-F4 on some terminals
                    }
                }
                '\r' => {
                    let typed = std::mem::take(&mut self.typed);
                    self.note_task(&typed);
                }
                '\x7f' | '\x08' => {
                    self.typed.pop();
                }
                '\x15' | '\x03' => self.typed.clear(), // ^U, ^C
                '\n' => self.typed.push(' '),
                c if !c.is_control() => self.typed.push(c),
                _ => {}
            }
        }
    }

    fn set(&mut self, s: Status) {
        if self.status != s {
            if s == Status::Done {
                self.turns += 1;
            }
            if s == Status::Working && self.status != Status::Blocked {
                self.worked_at = Some(Instant::now());
            }
            self.status = s;
            self.since = Instant::now();
        }
    }

    /// The question near the bottom of the screen, if the agent seems to be asking
    /// the player something, plus how many times its first line is on screen.
    /// The text includes what the question is about (the command, the edit), so a
    /// later prompt is not the same question as the one just answered. A confirm
    /// hint under the choices is not the question.
    fn question(&self) -> Option<(String, usize)> {
        const ASK: [&str; 14] = [
            "do you want", "would you like", "(y/n)", "[y/n]", "proceed?", "yes, proceed", "allow execution",
            "allow once", "yes, allow", "apply this change", "yes, and don't ask", "enter to confirm",
            "trust this folder", "do you trust",
        ];
        let text = self.parser.screen().contents().to_lowercase();
        let lines: Vec<&str> = text.lines().map(unbox).filter(|l| !l.is_empty()).collect();
        let window = &lines[lines.len().saturating_sub(20)..];
        let hit = |l: &str| ASK.iter().any(|k| l.contains(k));
        let hint = |l: &str| l.contains("enter to confirm") && !l.contains('?');
        let real = window.iter().enumerate().rev().find(|(_, l)| hit(l) && option(l).is_none() && !hint(l)).map(|(i, _)| i);
        let i = real.or_else(|| window.iter().enumerate().rev().find(|(_, l)| hit(l) && option(l).is_none()).map(|(i, _)| i))?;
        let mut parts = vec![window[i].to_string()];
        for l in &window[i + 1..] {
            if option(l).is_some() || hint(l) { break; }
            if l.starts_with("environment:") || l.starts_with("permission rule:") || l.starts_with("input:") { continue; }
            parts.push((*l).to_string());
        }
        let header = window[i];
        let count = lines.iter().filter(|l| **l == header).count();
        Some((parts.join("\n"), count))
    }

    /// The question being asked, with its context and choices, for answering from orbit.
    fn ask(&self) -> Option<Ask> {
        let (q, _) = self.question()?;
        let header = q.lines().next().unwrap_or(q.as_str());
        let lines: Vec<String> = self.parser.screen().contents().lines().map(|l| unbox(l).to_string()).collect();
        let qi = lines.iter().rposition(|l| l.to_lowercase() == header)?;
        let mut context = Vec::new();
        let mut gaps = 0;
        for l in lines[..qi].iter().rev() {
            if l.is_empty() {
                // one blank line inside the dialog is fine, two means we've left it
                if !context.is_empty() {
                    gaps += 1;
                    if gaps > 1 {
                        break;
                    }
                }
                continue;
            }
            if l.chars().all(|c| "─━═╭╮╰╯┌┐└┘".contains(c) || c == ' ') || context.len() == 5 {
                break;
            }
            context.push(l.clone());
        }
        context.reverse();
        let mut options = Vec::new();
        for l in &lines[qi + 1..] {
            if let Some((n, label)) = option(l) {
                options.push(Choice { key: n.into(), label: label.into() });
            }
        }
        if options.is_empty() && (q.contains("(y/n)") || q.contains("[y/n]")) {
            options = vec![Choice { key: "y\r".into(), label: "Yes".into() }, Choice { key: "n\r".into(), label: "No".into() }];
        }
        let question = lines[qi].clone();
        Some(Ask { question, context, options })
    }

    /// A path as the agent named it, made absolute against its world.
    fn resolve(&self, f: &str) -> String {
        let p = std::path::Path::new(f);
        let p = if p.is_absolute() { p.to_path_buf() } else { std::path::Path::new(&self.cwd).join(p) };
        p.to_string_lossy().into_owned()
    }

    /// Agents without edit hooks: pick edits off the screen ("• Edited src/a.rs (+3 -1)",
    /// "⏺ Update(src/a.rs)", "Added …", "Wrote …", Grok's "Edit `src/a.rs`").
    fn scan_edits(&mut self) {
        let text = self.parser.screen().contents();
        let now = Instant::now();
        for line in text.lines() {
            let Some(path) = edit_path(line) else { continue };
            if !self.edits_seen.insert(line.trim().to_string()) {
                continue;
            }
            let f = self.resolve(&path);
            self.touched.insert(f, now);
        }
    }

    /// TUI agents print an interrupt hint only while they're busy.
    fn busy_marker(&self) -> bool {
        let text = self.parser.screen().contents().to_lowercase();
        let tail: Vec<&str> = text.lines().rev().filter(|l| !l.trim().is_empty()).take(10).collect();
        tail.iter().any(|l| l.contains("esc to interrupt") || l.contains("ctrl+c to interrupt") || l.contains("send a message to interrupt"))
    }

    /// Agents whose idle screen animates, so output activity says nothing.
    fn animated(&self) -> bool {
        matches!(self.kind.as_str(), "codex")
    }

    /// Reports working/done through hooks, so its screen output (statusline
    /// repaints and all) says nothing about whether it's busy. Grok counts once
    /// a hook has actually reported: until the folder is trusted, its screen does.
    fn hooked(&self) -> bool {
        self.kind == "claude" || self.hooks
    }

    /// Asking, and not a question the player already answered.
    fn asking(&self) -> bool {
        match (self.question(), &self.answered) {
            (Some((q, n)), Some((aq, an))) => !(q == *aq && n <= *an),
            (q, _) => q.is_some(),
        }
    }

    fn update(&mut self, now: Instant) {
        if self.exited.is_some() {
            self.set(Status::Exited);
            return;
        }
        // the prompt has left the screen: the next one, even the same command, is a new question
        if self.question().is_none() {
            self.answered = None;
        }
        if self.kind == "shell" { self.set(Status::Idle); return; }
        let up = (now - self.started).as_secs_f32();
        let quiet = (now - self.last_out).as_secs_f32();
        let fresh = self.last_out > self.last_in + Duration::from_secs_f32(ECHO);
        if up < BOOT {
            self.set(Status::Booting);
            return;
        }
        if self.status == Status::Booting {
            self.set(Status::Idle);
        }
        // hooks are authoritative, but fall back to heuristics if one goes stale
        if let Some(h) = self.hook {
            if !(h == Status::Working && quiet > 30.0) {
                if h == Status::Done && self.status == Status::Idle {
                    return; // already seen
                }
                self.set(h);
                return;
            }
            // went quiet without a Stop hook: the turn was interrupted
            self.hook = None;
            if self.hooked() {
                self.set(Status::Done);
                return;
            }
        }
        let animated = self.animated();
        let settled = |secs: f32| animated || quiet > secs;
        if self.hooked() {
            // only questions the hooks don't cover, like the trust dialog
            if settled(0.8) && self.asking() {
                self.set(Status::Blocked);
            } else if self.status == Status::Blocked && !self.asking() {
                self.set(Status::Idle);
            }
            return;
        }
        // a busy agent's spinner/timer keeps ticking; a stale hint on screen doesn't
        if (self.busy_marker() && quiet < 2.5) || (!animated && quiet < 1.0 && fresh) {
            self.set(Status::Working);
        } else if settled(0.8) && self.asking() {
            self.set(Status::Blocked);
        } else if settled(1.5) && matches!(self.status, Status::Working | Status::Blocked) {
            self.set(Status::Done);
        }
    }

    fn view(&self, now: Instant) -> AgentView {
        AgentView {
            id: self.id,
            kind: self.kind.clone(),
            cwd: self.cwd.clone(),
            title: self.parser.callbacks().title.clone(),
            output: if self.kind != "shell" && matches!(self.status, Status::Done | Status::Blocked | Status::Idle) {
                let text = self.parser.screen().contents();
                let lines: Vec<_> = text.lines().filter(|s| !s.trim().is_empty()).collect();
                lines.iter().skip(lines.len().saturating_sub(18)).copied().collect::<Vec<_>>().join("\n").chars().take(4000).collect()
            } else { String::new() },
            task: self.task.clone(),
            role: self.role.name().into(),
            design: self.spec.design.clone(),
            status: self.status.name().into(),
            age: (now - self.since).as_secs_f32(),
            turns: self.turns,
            report: self.report.clone(),
            ask: if self.status == Status::Blocked { self.ask() } else { None },
            war: Vec::new(),
            overlap: Vec::new(),
            spec: self.spec.clone(),
            refit: self.refit.clone(),
            run: self.run.clone(),
        }
    }

    fn refit_is(&self, state: &str) -> bool { self.refit.as_ref().is_some_and(|r| r.state == state) }

    /// The screen changed: wake anyone waiting to see it.
    fn touch(&mut self) {
        self.version += 1;
        self.changed.notify_all();
    }

    fn screen(&self) -> ScreenView {
        let s = self.parser.screen();
        let (rows, cols) = s.size();
        let (rows, cols) = (rows.get(), cols.get());
        let lines = (0..rows)
            .map(|r| {
                let mut runs: Vec<Run> = Vec::new();
                for c in 0..cols {
                    let Some(cell) = s.cell(r, c) else { continue };
                    let ch = if cell.is_wide_continuation() {
                        "\0"
                    } else if cell.contents().is_empty() {
                        " "
                    } else {
                        cell.contents()
                    };
                    let (mut fg, mut bg) = (rgb(cell.fgcolor(), cell.bold()), rgb(cell.bgcolor(), false));
                    if cell.inverse() {
                        (fg, bg) = (Some(bg.unwrap_or(DEFAULT_BG)), Some(fg.unwrap_or(DEFAULT_FG)));
                    }
                    let b = cell.bold();
                    match runs.last_mut() {
                        Some(run) if run.fg == fg && run.bg == bg && run.b == b => run.t.push_str(ch),
                        _ => runs.push(Run { t: ch.to_string(), fg, bg, b }),
                    }
                }
                runs
            })
            .collect();
        let cursor = s.cursor_position();
        ScreenView {
            cols,
            rows,
            cursor,
            hide_cursor: s.hide_cursor() || s.scrollback() > 0,
            lines,
            scroll: s.scrollback(),
            history: s.scrollback_len(),
            mouse: s.mouse_protocol_mode() != vt100::MouseProtocolMode::None,
            sgr: s.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr,
            alt: s.alternate_screen(),
            version: self.version,
            exited: self.exited.is_some(),
        }
    }
}

/// `❯ 2. Yes, and don't ask again` → ("2", "Yes, and don't ask again")
fn option(l: &str) -> Option<(&str, &str)> {
    let t = l.trim_start_matches(['❯', '›', '>', '▶', '→', ' ']);
    let (n, label) = t.split_once(". ")?;
    (!n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit())).then(|| (n, label.trim()))
}

/// A screen line without the box drawing around it.
fn unbox(l: &str) -> &str {
    l.trim().trim_matches(['│', '┃', '║', '|']).trim()
}

const DEFAULT_FG: [u8; 3] = [0xd8, 0xde, 0xe9];
const DEFAULT_BG: [u8; 3] = [0x0b, 0x0f, 0x1a];

/// A tasteful xterm-256 palette.
fn rgb(c: vt100::Color, bold: bool) -> Option<[u8; 3]> {
    const BASE: [[u8; 3]; 16] = [
        [0x1d, 0x20, 0x2b], [0xe0, 0x6c, 0x75], [0x98, 0xc3, 0x79], [0xe5, 0xc0, 0x7b],
        [0x61, 0xaf, 0xef], [0xc6, 0x78, 0xdd], [0x56, 0xb6, 0xc2], [0xab, 0xb2, 0xbf],
        [0x5c, 0x63, 0x70], [0xff, 0x7b, 0x86], [0xb5, 0xe8, 0x90], [0xff, 0xd8, 0x8a],
        [0x82, 0xc8, 0xff], [0xe0, 0x99, 0xff], [0x7a, 0xd8, 0xe4], [0xf0, 0xf2, 0xf6],
    ];
    match c {
        vt100::Color::Default => None,
        vt100::Color::Rgb(r, g, b) => Some([r, g, b]),
        vt100::Color::Idx(i) if i < 16 => Some(BASE[if bold && i < 8 { i + 8 } else { i } as usize]),
        vt100::Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let lv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Some([lv(i / 36), lv((i / 6) % 6), lv(i % 6)])
        }
        vt100::Color::Idx(i) => {
            let g = 8 + (i - 232) * 10;
            Some([g, g, g])
        }
    }
}

#[derive(Default)]
struct Hub {
    agents: HashMap<u32, Arc<Mutex<Agent>>>,
    next: u32,
    /// When a galaxy last asked for the agent list: while one is open, it does the alerting.
    watched: Option<Instant>,
    missions: Vec<Mission>,
    /// What Jev (or the word-overlap fallback) said about pairs of ships on one world.
    overlap: OverlapBook,
}

type Shared = Arc<Mutex<Hub>>;

/// Claude Code reports its own state through hooks that call back into us.
fn claude_hooks(exe: &str) -> String {
    let hook = |ev: &str| serde_json::json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": format!("'{exe}' hook {ev}") }] }]);
    serde_json::json!({ "hooks": {
        "UserPromptSubmit": hook("working"),
        "PreToolUse": hook("working"),
        "PostToolUse": hook("working"),
        "Notification": hook("notify"),
        "Stop": hook("done"),
    }, "statusLine": { "type": "command", "command": format!("'{exe}' statusline"), "padding": 0 }})
    .to_string()
}

fn command(kind: &str, exe: &str, resume: Option<&str>) -> (String, Vec<String>) {
    let mut args: Vec<String> = Vec::new();
    let prog = match kind {
        "claude" => {
            if let Some(id) = resume {
                args.extend(["--resume".into(), id.into()]);
            }
            args.extend(["--settings".into(), claude_hooks(exe)]);
            "claude".into()
        }
        // inline mode keeps its output in the scrollback, so comms can scroll it
        "codex" => {
            args.push("--no-alt-screen".into());
            if let Some(id) = resume {
                args.extend(["resume".into(), id.into()]);
            }
            "codex".into()
        }
        // inline mode keeps its output in the scrollback, so comms can scroll it
        "grok" => {
            args.push("--no-alt-screen".into());
            if let Some(id) = resume {
                args.extend(["--resume".into(), id.into()]);
            }
            "grok".into()
        }
        "cursor" => "cursor-agent".into(),
        "shell" => std::env::var("SHELL").unwrap_or_else(|_| "bash".into()),
        k => k.into(),
    };
    (prog, args)
}

fn next_id(hub: &Shared) -> u32 {
    let mut h = hub.lock().unwrap();
    h.next += 1;
    h.next
}

fn new_agent(id: u32, kind: &str, cwd: &str, writer: Pty, rows: u16, cols: u16) -> Agent {
    let now = Instant::now();
    Agent {
        id,
        kind: kind.into(),
        cwd: cwd.into(),
        parser: vt100::Parser::new_with_callbacks(NonZeroU16::new(rows).unwrap(), NonZeroU16::new(cols).unwrap(), HISTORY, Cb::default()),
        writer,
        session: None,
        born: SystemTime::now(),
        started: now,
        last_out: now,
        last_in: now,
        hook: None,
        hooks: false,
        answered: None,
        turns: 0,
        report: Report::default(),
        touched: HashMap::new(),
        edits_seen: HashSet::new(),
        told: Status::Booting,
        worked_at: None,
        task: String::new(),
        typed: String::new(),
        role: Role::None,
        spec: ShipSpec::default(),
        refit: None,
        run: Vec::new(),
        version: 1,
        changed: Arc::default(),
        status: Status::Booting,
        since: now,
        exited: None,
    }
}

/// The program and arguments a ship is launched with: its role's and outfit's flags, the
/// species' own (resuming `resume`), then the opening prompt.
fn launch_line(kind: &str, exe: &str, resume: Option<&str>, prompt: Option<&str>, role: Role, spec: &ShipSpec) -> (String, Vec<String>) {
    let (prog, args) = command(kind, exe, resume);
    let mut args = [role.launch_args(kind, spec), args].concat();
    if let Some(p) = prompt {
        args.push(role.opening(kind, spec, p)); // every species takes an opening prompt as its last argument
    }
    (prog, args)
}

/// Launch an agent in `cwd`, picking up its old session if we have one.
#[allow(clippy::too_many_arguments)]
fn spawn(hub: &Shared, kind: &str, cwd: &str, resume: Option<&str>, prompt: Option<&str>, role: Role, spec: &ShipSpec, run: &[String]) -> anyhow::Result<u32> {
    let id = next_id(hub);
    spawn_as(hub, id, kind, cwd, resume, prompt, role, spec, run).map(|_| id)
}

#[cfg(test)]
thread_local! {
    /// A stand-in program for the species, so tests never launch a real agent.
    static STAND_IN: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Launch an agent as ship `id` (a new one, or one being relaunched in place).
#[allow(clippy::too_many_arguments)]
fn spawn_as(hub: &Shared, id: u32, kind: &str, cwd: &str, resume: Option<&str>, prompt: Option<&str>, role: Role, spec: &ShipSpec, run: &[String]) -> anyhow::Result<Arc<Mutex<Agent>>> {
    let exe = exe_path().to_string_lossy().into_owned();
    if kind == "grok" {
        install_grok_hooks(&exe);
    }
    let pty = native_pty_system().openpty(PtySize { rows: ROWS, cols: COLS, pixel_width: 0, pixel_height: 0 })?;
    // a shell session can run one command instead of the shell; agents always run their species
    let (prog, args) = match run.split_first() {
        Some((prog, args)) if kind == "shell" => (prog.clone(), args.to_vec()),
        _ => launch_line(kind, &exe, resume, prompt, role, spec),
    };
    #[cfg(test)]
    let prog = STAND_IN.with(|s| s.borrow().clone()).unwrap_or(prog);
    let mut cmd = CommandBuilder::new(&prog);
    cmd.args(&args);
    cmd.cwd(cwd);
    // markers of whatever Claude session started the daemon; agents are their own sessions
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k == "CLAUDECODE" || k == "CLAUDE_PID" || k == "CLAUDE_EFFORT" || (k.starts_with("CLAUDE_CODE_") && !k.starts_with("CLAUDE_CODE_USE_")) {
            cmd.env_remove(&*k);
        }
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("EMPIRE_AGENT_ID", id.to_string());
    cmd.env("EMPIRE_SOCK", socket_path());
    let child = pty.slave.spawn_command(cmd)?;
    drop(pty.slave);
    let pid = child.process_id().ok_or_else(|| anyhow::anyhow!("no pid"))? as i32;
    drop(child); // we reap it ourselves (waitpid), which also works after an exec
    let fd = pty.master.as_raw_fd().ok_or_else(|| anyhow::anyhow!("no pty fd"))?;
    let master = unsafe { File::from_raw_fd(libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3)) };
    drop(pty.master);
    let mut agent = new_agent(id, kind, cwd, Pty { master, pid }, ROWS, COLS);
    agent.session = resume.map(String::from);
    agent.role = role;
    agent.spec = spec.clone();
    agent.note_task(prompt.unwrap_or(""));
    if kind == "shell" {
        agent.run = run.to_vec();
    }
    Ok(watch(hub, agent)?)
}

/// Start pumping an agent's output and waiting for it to exit.
fn watch(hub: &Shared, agent: Agent) -> io::Result<Arc<Mutex<Agent>>> {
    let mut reader = agent.writer.master.try_clone()?;
    let pid = agent.writer.pid;
    let agent = Arc::new(Mutex::new(agent));
    let id = agent.lock().unwrap().id;
    hub.lock().unwrap().agents.insert(id, agent.clone());

    // pump output into the terminal emulator, answering queries as we go
    let a = agent.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 16384];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut g = a.lock().unwrap();
                    g.parser.process(&buf[..n]);
                    g.last_out = Instant::now();
                    g.touch();
                    let replies = std::mem::take(&mut g.parser.callbacks_mut().replies);
                    if !replies.is_empty() {
                        let _ = g.writer.write_all(&replies);
                    }
                }
            }
        }
        let mut g = a.lock().unwrap();
        g.exited = Some(Instant::now());
        g.touch();
    });
    let a = agent.clone();
    thread::spawn(move || {
        let mut st = 0;
        unsafe { libc::waitpid(pid, &mut st, 0) };
        a.lock().unwrap().exited.get_or_insert(Instant::now());
    });
    Ok(agent)
}

// ───────────────────────── surviving restarts ──────────────────────────

/// The binary we're running (cargo may have replaced it since: "/path (deleted)").
pub fn exe_path() -> PathBuf {
    let p = std::env::current_exe().unwrap_or_else(|_| "empire".into());
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_suffix(" (deleted)").unwrap_or(&s).to_string())
}

/// A fingerprint of the binary on disk, so a running daemon can tell it's out of date.
pub fn build_id() -> String {
    std::fs::metadata(exe_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or_else(|| env!("CARGO_PKG_VERSION").into(), |d| format!("{}-{}", env!("CARGO_PKG_VERSION"), d.as_millis()))
}

/// Everything an agent needs to carry over into the new daemon.
#[derive(Serialize, Deserialize)]
struct Handoff {
    id: u32,
    kind: String,
    cwd: String,
    fd: i32,
    pid: i32,
    rows: u16,
    cols: u16,
    screen: String,
    title: String,
    #[serde(default)]
    task: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    spec: ShipSpec,
    status: String,
    age: f32,
    turns: u32,
    report: Report,
    session: Option<String>,
    #[serde(default)]
    refit: Option<RefitState>,
    #[serde(default)]
    run: Vec<String>,
}

/// Replace this daemon with the binary on disk, keeping every agent: same pid,
/// so the agents stay our children, and their pty fds survive the exec.
fn upgrade(hub: &Shared) -> io::Result<()> {
    let h = hub.lock().unwrap();
    let now = Instant::now();
    let mut out = Vec::new();
    for a in h.agents.values() {
        let g = a.lock().unwrap();
        if g.exited.is_some() {
            continue;
        }
        let fd = unsafe { libc::dup(g.writer.master.as_raw_fd()) }; // no CLOEXEC: this one crosses over
        let s = g.parser.screen();
        let (rows, cols) = s.size();
        let screen = format!("{}{}", s.input_mode_formatted(), s.contents_formatted());
        out.push(Handoff {
            id: g.id,
            kind: g.kind.clone(),
            cwd: g.cwd.clone(),
            fd,
            pid: g.writer.pid,
            rows: rows.get(),
            cols: cols.get(),
            screen,
            title: g.parser.callbacks().title.clone(),
            task: g.task.clone(),
            role: g.role.name().into(),
            spec: g.spec.clone(),
            status: g.status.name().into(),
            age: (now - g.since).as_secs_f32(),
            turns: g.turns,
            report: g.report.clone(),
            session: g.session.clone(),
            refit: g.refit.clone(),
            run: g.run.clone(),
        });
    }
    let file = dirs_state().join("handoff.json");
    std::fs::write(&file, serde_json::to_string(&(h.next, out))?)?;
    let _ = std::fs::remove_file(socket_path());
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(exe_path()).arg("daemon").env("EMPIRE_HANDOFF", &file).exec();
    Err(err)
}

/// The other half of `upgrade`: pick the agents back up.
fn adopt(hub: &Shared, file: &str) -> anyhow::Result<()> {
    let (next, list): (u32, Vec<Handoff>) = serde_json::from_str(&std::fs::read_to_string(file)?)?;
    let _ = std::fs::remove_file(file);
    hub.lock().unwrap().next = next;
    let now = Instant::now();
    for h in list {
        let master = unsafe { File::from_raw_fd(h.fd) };
        unsafe { libc::fcntl(h.fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        let mut a = new_agent(h.id, &h.kind, &h.cwd, Pty { master, pid: h.pid }, h.rows.max(2), h.cols.max(2));
        a.parser.process(h.screen.as_bytes());
        a.parser.callbacks_mut().title = h.title;
        a.parser.callbacks_mut().replies.clear();
        a.status = Status::parse(&h.status);
        a.told = a.status;
        a.since = now.checked_sub(Duration::from_secs_f32(h.age)).unwrap_or(now);
        a.started = now.checked_sub(Duration::from_secs(60)).unwrap_or(now); // long past booting
        a.turns = h.turns;
        a.task = h.task;
        a.role = Role::parse(&h.role);
        a.spec = h.spec;
        a.report = h.report;
        a.session = h.session;
        a.refit = resumed(h.refit);
        a.run = h.run;
        watch(hub, a)?;
    }
    Ok(())
}

/// Who should come back after a reboot or a crash: written as agents come and go,
/// cleared by `empire stop`.
#[derive(Serialize, Deserialize, PartialEq, Clone)]
struct Saved {
    kind: String,
    cwd: String,
    session: Option<String>,
    #[serde(default)]
    role: String,
    #[serde(default)]
    spec: ShipSpec,
    #[serde(default)]
    refit: Option<RefitState>,
}

/// A refit cut short by a restart goes back to waiting: the ship comes back as it was.
fn resumed(refit: Option<RefitState>) -> Option<RefitState> {
    refit.map(|r| if r.state == "applying" { RefitState { state: "pending".into(), ..r } } else { r })
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn missions_file() -> PathBuf {
    dirs_state().join("missions.json")
}

/// Hand queued missions to free agents and follow the ones under way.
fn dispatch(hub: &Shared, logged: &mut Vec<Mission>) {
    let agents: Vec<Arc<Mutex<Agent>>> = hub.lock().unwrap().agents.values().filter(|a| a.lock().unwrap().kind != "shell").cloned().collect();
    // (id, cwd, status, exited); a ship standing down for a refit isn't gone
    let roster: Vec<(u32, String, Status, bool)> = agents
        .iter()
        .map(|a| {
            let g = a.lock().unwrap();
            (g.id, g.cwd.clone(), g.status, g.exited.is_some() && !g.refit_is("applying"))
        })
        .collect();
    // ships waiting for or under refit take no new missions until it's done
    let refitting: HashSet<u32> = agents.iter().filter_map(|a| {
        let g = a.lock().unwrap();
        (g.refit_is("pending") || g.refit_is("applying")).then_some(g.id)
    }).collect();
    let mut h = hub.lock().unwrap();
    let busy: HashSet<u32> = h.missions.iter().filter(|m| matches!(m.state.as_str(), "sent" | "active")).filter_map(|m| m.agent).collect();
    let mut taken = HashSet::new();
    for m in h.missions.iter_mut() {
        if !m.campaign_job.is_empty() { continue; } // campaign receipts, not terminal status, control these missions
        let me = m.agent.and_then(|id| roster.iter().find(|r| r.0 == id));
        match m.state.as_str() {
            "queued" => {
                // a mission given to one ship waits for it; if it's gone, anyone here may take it
                if m.agent.is_some() && me.is_none_or(|r| r.3) {
                    m.agent = None;
                }
                let free = roster.iter().find(|(id, cwd, st, dead)| {
                    *cwd == m.cwd && !dead && matches!(st, Status::Idle | Status::Done) && !busy.contains(id) && !taken.contains(id) && !refitting.contains(id) && m.agent.is_none_or(|a| a == *id)
                });
                if let Some(&(id, ..)) = free
                    && let Some(a) = agents.iter().find(|a| a.lock().unwrap().id == id)
                {
                    taken.insert(id);
                    m.state = "sent".into();
                    m.agent = Some(id);
                    a.lock().unwrap().note_task(&m.text);
                    order(a.clone(), crate::screenshots::prompt(&m.text, &m.attachments));
                }
            }
            "sent" | "active" => match me {
                None | Some((_, _, _, true)) => {
                    // its agent is gone: back in the queue
                    m.state = "queued".into();
                    m.agent = None;
                }
                Some((_, _, Status::Working | Status::Blocked, _)) => m.state = "active".into(),
                Some((_, _, Status::Done | Status::Idle, _)) if m.state == "active" => {
                    m.state = "done".into();
                    m.done_at = Some(unix_now());
                }
                _ => {}
            },
            _ => {}
        }
    }
    // a pending refit goes ahead only between turns, with no mission of its own under way
    for a in &agents {
        let mut g = a.lock().unwrap();
        if !g.refit_is("pending") || g.exited.is_some() || !matches!(g.status, Status::Idle | Status::Done) || busy.contains(&g.id) || taken.contains(&g.id) {
            continue;
        }
        let id = g.id;
        let Some(r) = g.refit.as_mut() else { continue };
        r.state = "applying".into();
        let (role, spec) = (Role::parse(&r.role), r.spec.clone());
        let hub = hub.clone();
        thread::spawn(move || refit(&hub, id, role, &spec));
    }
    if h.missions != *logged {
        if crate::state::edit(&missions_file(), |saved: &mut Vec<Mission>| *saved = h.missions.clone()).is_ok() { *logged = h.missions.clone(); }
    }
}

// ───────────────────────── overlapping work ─────────────────────────

/// Set to 1 to flag two Working ships on one world whose tasks look like the same job or like
/// edits to the same files: earlier than the border-conflict detector, which waits for an edit.
/// Jev answers one yes/no question per pair (needs `TYPESAFE_API_KEY`); with no key, or if Jev
/// fails, word and file-path overlap in the task text stands in. It only ever adds a dim badge.
const OVERLAP_FLAG: &str = "EMPIRE_JEV_OVERLAP";
/// Jev is asked about a world at most this often.
const OVERLAP_DEBOUNCE: Duration = Duration::from_secs(30);
/// Ships considered per world; the rest are left out.
const OVERLAP_SHIPS: usize = 6;
const OVERLAP_TASK: usize = 300;
const OVERLAP_BOOK: usize = 1024;

#[derive(Clone, Copy, PartialEq, Debug)]
enum OverlapMode {
    Off,
    /// Flag on, no key: the word-overlap fallback only.
    Words,
    Jev,
}

fn overlap_mode_of(flag: bool, key: bool) -> OverlapMode {
    match (flag, key) {
        (false, _) => OverlapMode::Off,
        (true, false) => OverlapMode::Words,
        (true, true) => OverlapMode::Jev,
    }
}

fn overlap_mode() -> OverlapMode {
    let flag = std::env::var(OVERLAP_FLAG).ok();
    overlap_mode_of(jev::enabled_with(Some("x"), flag.as_deref()), jev::enabled(OVERLAP_FLAG))
}

/// A Working ship's id, species, world and task.
type OverlapShip = (u32, String, String, String);

/// A ship that can overlap: Working, not a terminal, with a task.
fn overlap_ship(a: &Agent) -> Option<OverlapShip> {
    (a.kind != "shell" && a.exited.is_none() && a.status == Status::Working && !a.task.is_empty()).then(|| (a.id, a.kind.clone(), a.cwd.clone(), a.task.clone()))
}

#[derive(Default)]
struct OverlapBook {
    /// P(same work) of a pair and the tasks they have now, by `pair_key`.
    pairs: HashMap<u64, f32>,
    /// Per world: the hash of the sorted (id, task) pairs last settled, and when Jev was last asked.
    seen: HashMap<String, (u64, Option<Instant>)>,
    /// A request is in flight (one at a time).
    busy: bool,
}

/// Ships by world, two or more, fewest ids first and at most `OVERLAP_SHIPS` each.
fn overlap_groups(ships: &[OverlapShip]) -> Vec<(String, Vec<OverlapShip>)> {
    let mut by: std::collections::BTreeMap<&str, Vec<OverlapShip>> = Default::default();
    for s in ships {
        by.entry(s.2.as_str()).or_default().push(s.clone());
    }
    by.into_iter().filter(|(_, v)| v.len() >= 2).map(|(k, mut v)| { v.sort_by_key(|s| s.0); v.truncate(OVERLAP_SHIPS); (k.to_string(), v) }).collect()
}

/// A world's group as the cache sees it: the sorted (id, task) pairs.
fn group_hash(ships: &[OverlapShip]) -> u64 {
    let mut v: Vec<(u32, &str)> = ships.iter().map(|s| (s.0, s.3.as_str())).collect();
    v.sort();
    crate::jev::digest(&serde_json::json!(v))
}

/// A pair and their tasks. `a` has the lower id.
fn pair_key(a: &OverlapShip, b: &OverlapShip) -> u64 {
    crate::jev::digest(&serde_json::json!([a.0, a.3, b.0, b.3]))
}

fn pair_name(a: &OverlapShip, b: &OverlapShip) -> String {
    format!("pair_{}_{}", a.0, b.0)
}

fn overlap_req(world: &str, ships: &[OverlapShip], pairs: &[(usize, usize)]) -> serde_json::Value {
    let fleet: serde_json::Map<String, serde_json::Value> = ships.iter().map(|s| (format!("ship_{}", s.0), serde_json::json!({ "species": s.1, "task": s.3.chars().take(OVERLAP_TASK).collect::<String>() }))).collect();
    let questions = pairs.iter().map(|&(i, j)| {
        let (a, b) = (&ships[i], &ships[j]);
        (pair_name(a, b), jev::noul_q(&format!("Do the tasks of `ships.ship_{}` and `ships.ship_{}` ask for the same work, or for work likely to edit the same files?", a.0, b.0)))
    }).collect::<Vec<_>>();
    jev::request(serde_json::json!({ "world": crate::game::base_name(world), "ships": fleet }), questions.iter().map(|(k, v)| (k.as_str(), v.clone())).collect())
}

fn path_like(t: &str) -> bool {
    if t.contains('/') {
        return t.chars().any(char::is_alphanumeric);
    }
    t.rsplit_once('.').is_some_and(|(stem, ext)| stem.chars().count() >= 2 && (2..=5).contains(&ext.chars().count()) && ext.chars().all(char::is_alphanumeric) && ext.chars().any(char::is_alphabetic))
}

fn task_paths(t: &str) -> HashSet<String> {
    t.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~'))).trim_start_matches("./").trim_end_matches('.'))
        .filter(|w| path_like(w))
        .map(str::to_lowercase)
        .collect()
}

fn task_words(t: &str) -> HashSet<String> {
    const SKIP: [&str; 36] = [
        "this", "that", "with", "from", "into", "then", "there", "their", "them", "they", "about", "also", "just", "only", "some", "more",
        "when", "what", "which", "your", "each", "every", "other", "please", "make", "sure", "need", "want", "should", "would", "could",
        "have", "will", "code", "file", "files",
    ];
    t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 4 && !SKIP.contains(w)).map(String::from).collect()
}

/// The stand-in for Jev: the same file named in both tasks is a likely yes; so is most of the
/// (few) meaningful words being shared. Anything less is a no.
fn overlap_fallback(a: &str, b: &str) -> f32 {
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let (pa, pb) = (task_paths(a), task_paths(b));
    let file = pa.iter().any(|p| pb.iter().any(|q| p == q || (base(p) == base(q) && base(p).contains('.'))));
    if file {
        return 0.8;
    }
    let (wa, wb) = (task_words(a), task_words(b));
    let shared = wa.intersection(&wb).count();
    if shared >= 3 && shared * 10 >= wa.len().min(wb.len()) * 6 { 0.7 } else { 0.0 }
}

/// Work out which pairs of Working ships on each world overlap, when the set of tasks changes.
/// Jev is asked about a world's new pairs on a spawned thread, at most once in 30 s, one request
/// at a time; the fallback answers at once.
fn overlap_tick(hub: &Shared, now: Instant, mode: OverlapMode) {
    if mode == OverlapMode::Off {
        return;
    }
    let ships: Vec<OverlapShip> = {
        let agents: Vec<Arc<Mutex<Agent>>> = hub.lock().unwrap().agents.values().cloned().collect();
        agents.iter().filter_map(|a| overlap_ship(&a.lock().unwrap())).collect()
    };
    let mut h = hub.lock().unwrap();
    if h.overlap.pairs.len() > OVERLAP_BOOK {
        h.overlap.pairs.clear();
        h.overlap.seen.clear();
    }
    for (world, group) in overlap_groups(&ships) {
        let sig = group_hash(&group);
        if h.overlap.seen.get(&world).is_some_and(|s| s.0 == sig) {
            continue;
        }
        let todo: Vec<(usize, usize)> = (0..group.len()).flat_map(|i| (i + 1..group.len()).map(move |j| (i, j))).filter(|&(i, j)| !h.overlap.pairs.contains_key(&pair_key(&group[i], &group[j]))).collect();
        let last = h.overlap.seen.get(&world).and_then(|s| s.1);
        if mode == OverlapMode::Words || todo.is_empty() {
            for &(i, j) in &todo {
                h.overlap.pairs.insert(pair_key(&group[i], &group[j]), overlap_fallback(&group[i].3, &group[j].3));
            }
            h.overlap.seen.insert(world, (sig, last));
            continue;
        }
        if h.overlap.busy || last.is_some_and(|t| now - t < OVERLAP_DEBOUNCE) {
            continue;
        }
        h.overlap.busy = true;
        h.overlap.seen.insert(world.clone(), (sig, Some(now)));
        let (hub, req) = (hub.clone(), overlap_req(&world, &group, &todo));
        thread::spawn(move || {
            let answer = jev::post(&req).ok();
            let mut h = hub.lock().unwrap();
            for &(i, j) in &todo {
                let p = answer.as_ref().and_then(|v| jev::noul(v, &pair_name(&group[i], &group[j]))).unwrap_or_else(|| overlap_fallback(&group[i].3, &group[j].3));
                h.overlap.pairs.insert(pair_key(&group[i], &group[j]), p);
            }
            h.overlap.busy = false;
        });
        break;
    }
}

/// Each ship's overlaps now: the pairs on one world that were answered for the tasks they
/// have now and came out at `Overlap::SHOW` or more. A task that changed, or a ship that
/// stopped Working, drops out by itself.
fn overlaps(ships: &[OverlapShip], pairs: &HashMap<u64, f32>) -> HashMap<u32, Vec<Overlap>> {
    let mut out: HashMap<u32, Vec<Overlap>> = HashMap::new();
    for (_, group) in overlap_groups(ships) {
        for i in 0..group.len() {
            for j in i + 1..group.len() {
                let Some(&p) = pairs.get(&pair_key(&group[i], &group[j])) else { continue };
                if p >= Overlap::SHOW {
                    out.entry(group[i].0).or_default().push(Overlap { rival: group[j].0, p });
                    out.entry(group[j].0).or_default().push(Overlap { rival: group[i].0, p });
                }
            }
        }
    }
    out
}

/// Found a dedicated campaign worktree; never adopt an unrelated directory or branch.

fn order(a: Arc<Mutex<Agent>>, text: String) {
    thread::spawn(move || {
        // Multiline mission prompts and image references must be a single paste.
        let pasted = format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""));
        let _ = a.lock().unwrap().writer.write_all(pasted.as_bytes());
        thread::sleep(Duration::from_millis(150));
        let mut g = a.lock().unwrap();
        g.last_in = Instant::now();
        if g.status == Status::Done {
            g.set(Status::Idle);
        }
        let _ = g.writer.write_all(b"\r");
    });
}

// ──────────────────────────────── refits ───────────────────────────────

/// How long a relaunched ship must stay up to count as refitted.
const REFIT_GRACE: Duration = Duration::from_millis(if cfg!(test) { 600 } else { 3000 });

/// The session a ship would be refitted on: only a species that resumes a session by id can be
/// relaunched without losing its work, and only once it has one.
fn refit_session(kind: &str, session: Option<&str>) -> Result<String, String> {
    // the species first: one that can't resume will never get a session worth waiting for
    if !command(kind, "", Some("\0probe")).1.iter().any(|a| a == "\0probe") {
        return Err(format!("{kind} can't resume a session, so it can't be refitted in place"));
    }
    Ok(session.filter(|s| !s.is_empty()).ok_or("no session to resume yet: let it finish a turn first")?.into())
}

/// Carry out a ship's refit and record how it went.
fn refit(hub: &Shared, id: u32, role: Role, spec: &ShipSpec) {
    let (state, note) = match relaunch(hub, id, role, spec) {
        Ok(note) => ("active", note),
        Err(e) => e,
    };
    if let Some(a) = hub.lock().unwrap().agents.get(&id)
        && let Some(r) = a.lock().unwrap().refit.as_mut()
    {
        r.state = state.into();
        r.note = note;
    }
}

/// Stand a ship down and launch it again as the same ship, on its own session, with a new role
/// and outfit. The harness does the resuming and keeps its own permissions. If the new launch
/// fails or dies at once, it's launched again as it was. Err is (failed | unsupported, why).
fn relaunch(hub: &Shared, id: u32, role: Role, spec: &ShipSpec) -> Result<String, (&'static str, String)> {
    let old = hub.lock().unwrap().agents.get(&id).cloned().ok_or(("failed", format!("no agent #{id}")))?;
    let (kind, cwd, session, was, was_spec, task, turns, report, refit) = {
        let g = old.lock().unwrap();
        if g.exited.is_some() && !g.refit_is("applying") {
            return Err(("failed", "it has left".into()));
        }
        (g.kind.clone(), g.cwd.clone(), g.session.clone(), g.role, g.spec.clone(), g.task.clone(), g.turns, g.report.clone(), g.refit.clone())
    };
    let session = refit_session(&kind, session.as_deref()).map_err(|e| ("unsupported", e))?;
    old.lock().unwrap().writer.kill();
    if !(0..50).any(|_| {
        thread::sleep(Duration::from_millis(100));
        old.lock().unwrap().exited.is_some()
    }) {
        return Err(("failed", "it didn't stand down; still running its old outfit".into()));
    }
    // the same ship, picking up where it was
    let launch = |role: Role, spec: &ShipSpec| -> Result<Arc<Mutex<Agent>>, String> {
        let a = spawn_as(hub, id, &kind, &cwd, Some(&session), None, role, spec, &[]).map_err(|e| e.to_string())?;
        {
            let mut g = a.lock().unwrap();
            g.task = task.clone();
            g.turns = turns;
            g.report = report.clone();
            g.refit = refit.clone();
        }
        thread::sleep(REFIT_GRACE);
        if a.lock().unwrap().exited.is_some() { Err("it shut down right after relaunch".into()) } else { Ok(a) }
    };
    match launch(role, spec) {
        Ok(_) => Ok(format!("resumed session {session}")),
        Err(e) => match launch(was, &was_spec) {
            Ok(_) => Err(("failed", format!("{e}; relaunched with its old outfit"))),
            Err(e2) => Err(("failed", format!("{e}; its old outfit didn't come back either: {e2}"))),
        },
    }
}

// ──────────────────────────────── moons ────────────────────────────────

fn git(dir: &str, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("git failed").to_string())
    }
}

fn moons_dir() -> PathBuf {
    let home = crate::game::home();
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")).join("empire/moons")
}

/// A worktree of the repo at `cwd` on `branch` (new or existing), with an agent on it.
fn launch_moon(hub: &Shared, cwd: &str, branch: &str, kind: &str) -> Result<u32, String> {
    let top = git(cwd, &["rev-parse", "--show-toplevel"]).map_err(|_| "only a git world can have moons".to_string())?;
    let slug: String = branch.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '-' }).collect();
    let h = top.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    let path = moons_dir().join(format!("{}-{:08x}", crate::game::base_name(&top), h as u32)).join(&slug);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let p = path.to_string_lossy().into_owned();
    // the moon is still there (its crew left): just send a ship back
    if path.is_dir() && git(&p, &["rev-parse", "--git-dir"]).is_ok() {
        return spawn(hub, kind, &p, None, None, Role::None, &ShipSpec::default(), &[]).map_err(|e| e.to_string());
    }
    let exists = git(&top, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok();
    if exists {
        git(&top, &["worktree", "add", &p, branch])?;
    } else {
        git(&top, &["worktree", "add", "-b", branch, &p])?;
    }
    spawn(hub, kind, &p, None, None, Role::None, &ShipSpec::default(), &[]).map_err(|e| e.to_string())
}

/// Merge a moon's branch into its world, then recall its agents and remove it.
/// A merge conflict is a collision: the merge is aborted and nothing changes.
fn dock(hub: &Shared, moon: &str) -> Result<String, String> {
    let common = git(moon, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let own = git(moon, &["rev-parse", "--path-format=absolute", "--git-dir"])?;
    let host = common.strip_suffix("/.git").filter(|_| common != own).ok_or("that world isn't a moon")?.to_string();
    let branch = git(moon, &["branch", "--show-current"])?;
    if branch.is_empty() {
        return Err("the moon isn't on a branch".into());
    }
    if !git(moon, &["status", "--porcelain"])?.is_empty() {
        return Err("the moon has uncommitted work: have its agent commit first".into());
    }
    if !git(&host, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err(format!("{} has uncommitted changes: commit or stash them before docking", crate::game::base_name(&host)));
    }
    if git(&host, &["merge", "--no-ff", "-m", &format!("Dock moon {branch}"), &branch]).is_err() {
        let files = git(&host, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
        let _ = git(&host, &["merge", "--abort"]);
        let files: Vec<&str> = files.lines().collect();
        return Err(format!("collision! {} conflict{} ({})", files.len(), if files.len() == 1 { "" } else { "s" }, files.join(", ")));
    }
    // recall the moon's crew before taking the ground out from under them
    for a in hub.lock().unwrap().agents.values() {
        let g = a.lock().unwrap();
        if g.cwd == moon {
            g.writer.kill();
        }
    }
    thread::sleep(Duration::from_millis(400));
    git(&host, &["worktree", "remove", "--force", moon])?;
    let _ = git(&host, &["branch", "-d", &branch]);
    Ok(host)
}

fn registry_file() -> PathBuf {
    dirs_state().join("agents.json")
}

fn save_registry(hub: &Shared, last: &mut Vec<Saved>) {
    let mut now: Vec<(u32, Saved)> = hub
        .lock()
        .unwrap()
        .agents
        .values()
        .filter_map(|a| {
            let g = a.lock().unwrap();
            // a ship standing down for a refit is still on the roster
            (g.exited.is_none() || g.refit_is("applying")).then(|| (g.id, Saved { kind: g.kind.clone(), cwd: g.cwd.clone(), session: g.session.clone(), role: g.role.name().into(), spec: g.spec.clone(), refit: g.refit.clone() }))
        })
        .collect();
    now.sort_by_key(|(id, _)| *id);
    let now: Vec<Saved> = now.into_iter().map(|(_, s)| s).collect();
    if now != *last {
        let _ = std::fs::create_dir_all(dirs_state());
        let _ = std::fs::write(registry_file(), serde_json::to_string(&now).unwrap_or_default());
        *last = now;
    }
}

/// Relaunch whoever was running when the daemon last went down unexpectedly.
fn resurrect(hub: &Shared) {
    let Ok(s) = std::fs::read_to_string(registry_file()) else { return };
    let list: Vec<Saved> = serde_json::from_str(&s).unwrap_or_default();
    for a in list {
        if std::path::Path::new(&a.cwd).is_dir() {
            // a claude that never got a prompt has nothing to resume
            let session = a.session.filter(|s| a.kind != "claude" || claude_transcript(s));
            match spawn(hub, &a.kind, &a.cwd, session.as_deref(), None, Role::parse(&a.role), &a.spec, &[]) {
                Ok(id) => if let Some(g) = hub.lock().unwrap().agents.get(&id) { g.lock().unwrap().refit = resumed(a.refit) },
                Err(e) => eprintln!("could not bring back {} in {}: {e}", a.kind, a.cwd),
            }
        }
    }
}

fn claude_transcript(session: &str) -> bool {
    let home = crate::game::home();
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
    std::fs::read_dir(dir.join("projects")).into_iter().flatten().flatten().any(|p| p.path().join(format!("{session}.jsonl")).is_file())
}

/// Codex and grok don't tell us their session id; find the session they just
/// started in this folder.
fn discover_sessions(hub: &Shared) {
    let agents: Vec<Arc<Mutex<Agent>>> = hub.lock().unwrap().agents.values().cloned().collect();
    let mut claimed: HashSet<String> = agents.iter().filter_map(|a| a.lock().unwrap().session.clone()).collect();
    for a in agents {
        let (kind, cwd, born, has) = {
            let g = a.lock().unwrap();
            (g.kind.clone(), g.cwd.clone(), g.born, g.session.is_some())
        };
        if has || !matches!(kind.as_str(), "codex" | "grok") {
            continue;
        }
        let found = if kind == "codex" { codex_session(&cwd, born, &claimed) } else { grok_session(&cwd, born, &claimed) };
        if let Some(s) = found {
            claimed.insert(s.clone());
            a.lock().unwrap().session = Some(s);
        }
    }
    // Grok's context and spend live in the session directory, not a statusline.
    let groks: Vec<(Arc<Mutex<Agent>>, String, String)> = hub
        .lock()
        .unwrap()
        .agents
        .values()
        .cloned()
        .filter_map(|a| {
            let g = a.lock().unwrap();
            let session = (g.kind == "grok").then(|| g.session.clone()).flatten()?;
            Some((a.clone(), g.cwd.clone(), session))
        })
        .collect();
    for (a, cwd, session) in groks {
        let Some(mut report) = grok_vitals(&grok_sessions_dir(&cwd).join(&session)) else { continue };
        report.session = Some(session);
        a.lock().unwrap().report = report;
    }
}

/// Context, spend and lines changed, from a Grok session directory.
/// `costUsdTicks` is 10^10 ticks per USD.
fn grok_vitals(dir: &std::path::Path) -> Option<Report> {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let signals = read("signals.json");
    let usage = read("usage.json");
    if signals.is_none() && usage.is_none() {
        return None;
    }
    let num = |v: &serde_json::Value, k: &str| v.get(k).and_then(|n| n.as_f64());
    let signals = signals.unwrap_or_default();
    let usage = usage.unwrap_or_default();
    Some(Report {
        model: signals.get("primaryModelId").and_then(|m| m.as_str()).map(str::to_string),
        ctx: num(&signals, "contextWindowUsage").map(|n| n as f32),
        cost: usage.get("session").and_then(|s| num(s, "costUsdTicks")).map(|n| (n / 1e10) as f32),
        added: num(&signals, "agentLinesAdded").map(|n| n as u32),
        removed: num(&signals, "agentLinesRemoved").map(|n| n as u32),
        session: None,
    })
}

fn recent_entries(dir: &std::path::Path, since: SystemTime) -> Vec<(SystemTime, PathBuf)> {
    let mut v: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let t = m.created().or_else(|_| m.modified()).ok()?;
            (t + Duration::from_secs(2) >= since).then(|| (t, e.path()))
        })
        .collect();
    v.sort();
    v
}

fn codex_session(cwd: &str, born: SystemTime, claimed: &HashSet<String>) -> Option<String> {
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| crate::game::home().join(".codex"));
    let day = |t: SystemTime| {
        let secs = t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs() as i64;
        let tm = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&secs, &mut tm);
            tm
        };
        Some(home.join(format!("sessions/{:04}/{:02}/{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)))
    };
    let dirs: Vec<PathBuf> = [day(born), day(SystemTime::now())].into_iter().flatten().collect();
    for dir in dirs {
        for (_, f) in recent_entries(&dir, born) {
            let Ok(file) = File::open(&f) else { continue };
            let mut first = String::new();
            let _ = BufReader::new(file).read_line(&mut first);
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&first) else { continue };
            let p = &v["payload"];
            if p["cwd"].as_str() == Some(cwd)
                && let Some(id) = p["id"].as_str()
                && !claimed.contains(id)
            {
                return Some(id.into());
            }
        }
    }
    None
}

fn grok_home() -> PathBuf {
    std::env::var_os("GROK_HOME").map(PathBuf::from).unwrap_or_else(|| crate::game::home().join(".grok"))
}

/// Grok groups sessions by a URL-encoded working directory.
fn grok_sessions_dir(cwd: &str) -> PathBuf {
    let enc: String = cwd
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    grok_home().join("sessions").join(enc)
}

fn grok_session(cwd: &str, born: SystemTime, claimed: &HashSet<String>) -> Option<String> {
    let dir = grok_sessions_dir(cwd);
    recent_entries(&dir, born)
        .into_iter()
        .filter(|(_, p)| p.is_dir())
        .filter_map(|(_, p)| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .find(|n| !claimed.contains(n))
}

/// A file path named on an agent's screen, from Claude/Codex edit lines and Grok's
/// ``Edit `path` `` / ``Write `path` `` titles.
fn edit_path(line: &str) -> Option<String> {
    let l = line.trim().trim_start_matches(['•', '●', '⏺', '·', '-', '*', ' ']);
    // longer prefixes first: "Edited `" must not be read as "Edit `"
    let path = if let Some(rest) = ["Edited `", "Wrote `", "Edit `", "Write `"].iter().find_map(|p| l.strip_prefix(p)) {
        rest.split('`').next().unwrap_or("").trim().to_string()
    } else {
        let rest = ["Edited ", "Added ", "Updated ", "Created ", "Wrote ", "Deleted ", "Update(", "Write(", "Edit("].iter().find_map(|p| l.strip_prefix(p))?;
        rest.chars().take_while(|c| !c.is_whitespace() && !"()".contains(*c)).collect()
    };
    (!path.is_empty() && (path.contains('/') || path.contains('.'))).then_some(path)
}

/// What a hook said. A permission prompt is named by its type: the display text changes.
fn hook_message(v: &serde_json::Value) -> Option<String> {
    let text = |k: &str| v.get(k).and_then(|m| m.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
    let kind = text("notification_type").or_else(|| text("notificationType"));
    if kind.as_deref() == Some("permission_prompt") {
        return kind;
    }
    text("message").or(kind)
}

/// The file an edit tool is about to change. Claude names tools in PascalCase;
/// Grok's are `write` and `search_replace`, with either `tool_input` or `toolInput`.
fn edited_file(v: &serde_json::Value) -> Option<String> {
    let name = v.get("tool_name").or_else(|| v.get("toolName")).and_then(|n| n.as_str())?;
    if !matches!(name.to_ascii_lowercase().as_str(), "edit" | "write" | "multiedit" | "notebookedit" | "search_replace") {
        return None;
    }
    let input = v.get("tool_input").or_else(|| v.get("toolInput"))?;
    ["file_path", "path", "notebook_path"].iter().find_map(|k| input.get(*k).and_then(|p| p.as_str()).filter(|s| !s.is_empty()).map(str::to_string))
}

/// Hooks Grok loads from `~/.grok/hooks/`. `args` runs `command` with no shell.
/// `empire hook` returns at once when `EMPIRE_AGENT_ID` is unset, so this file
/// is a no-op for a Grok the empire did not launch.
fn grok_hooks_json(exe: &str) -> serde_json::Value {
    let group = |event: &str| {
        serde_json::json!([{ "hooks": [{ "type": "command", "command": exe, "args": ["hook", event], "timeout": 5 }] }])
    };
    serde_json::json!({
        "hooks": {
            "UserPromptSubmit": group("working"),
            "PreToolUse": group("working"),
            "PostToolUse": group("working"),
            "Notification": [{
                "matcher": "permission_prompt",
                "hooks": [{ "type": "command", "command": exe, "args": ["hook", "notify"], "timeout": 5 }],
            }],
            "Stop": group("done"),
            "StopCancelled": group("done"),
            "SessionStart": group("session"),
        }
    })
}

/// Write `empire.json` beside any other Grok hooks, and only when it changed.
/// Never touches the player's other hook files. A failure still lets the ship launch.
fn install_grok_hooks(exe: &str) {
    let path = grok_home().join("hooks").join("empire.json");
    let Ok(body) = serde_json::to_string_pretty(&grok_hooks_json(exe)) else { return };
    let body = format!("{body}\n");
    if std::fs::read_to_string(&path).ok().as_deref() == Some(body.as_str()) {
        return;
    }
    if std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new("."))).is_ok() {
        let _ = std::fs::write(path, body);
    }
}

fn handle(hub: &Shared, req: Req) -> Resp {
    let get = |id: u32| hub.lock().unwrap().agents.get(&id).cloned();
    let missing = |id: u32| Resp::Err { msg: format!("no agent #{id}") };
    match req {
        Req::Ping => Resp::Pong { version: BUILD.get().cloned().unwrap_or_default() },
        Req::List | Req::Peek => {
            let now = Instant::now();
            if matches!(req, Req::List) {
                hub.lock().unwrap().watched = Some(now);
            }
            let list: Vec<Arc<Mutex<Agent>>> = hub.lock().unwrap().agents.values().cloned().collect();
            let mut agents: Vec<AgentView> = list.iter().map(|a| a.lock().unwrap().view(now)).collect();
            agents.sort_by_key(|a| a.id);
            // border conflicts: agents on one world that edited the same files lately
            let touched: Vec<(u32, String, HashSet<String>)> = list
                .iter()
                .map(|a| {
                    let g = a.lock().unwrap();
                    (g.id, g.cwd.clone(), g.touched.keys().cloned().collect())
                })
                .collect();
            for v in agents.iter_mut() {
                let Some((_, cwd, mine)) = touched.iter().find(|t| t.0 == v.id) else { continue };
                for (rid, rcwd, theirs) in &touched {
                    if *rid == v.id || rcwd != cwd {
                        continue;
                    }
                    let mut files: Vec<String> = mine.intersection(theirs).map(|f| f.strip_prefix(&format!("{cwd}/")).unwrap_or(f).to_string()).collect();
                    if !files.is_empty() {
                        files.sort();
                        v.war.push(War { rival: *rid, files });
                    }
                }
            }
            // overlapping work: ships on one world whose tasks look like the same job (not yet a file conflict)
            let ships: Vec<OverlapShip> = list.iter().filter_map(|a| overlap_ship(&a.lock().unwrap())).collect();
            let found = overlaps(&ships, &hub.lock().unwrap().overlap.pairs);
            for v in agents.iter_mut() {
                let war: Vec<u32> = v.war.iter().map(|w| w.rival).collect();
                v.overlap = found.get(&v.id).map(|o| o.iter().filter(|o| !war.contains(&o.rival)).cloned().collect()).unwrap_or_default();
            }
            let missions = hub.lock().unwrap().missions.clone();
            Resp::Agents { agents, missions }
        }
        Req::Upgrade => {
            let h = hub.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(150));
                if let Err(e) = upgrade(&h) {
                    eprintln!("upgrade failed: {e}");
                }
            });
            Resp::Ok
        }
        Req::Mission { cwd, text, kind, launch, attachments, role, to, campaign, spec } => {
            if let Err(msg) = crate::screenshots::validate(&attachments) { return Resp::Err { msg }; }
            let id = hub.lock().unwrap().missions.iter().map(|m| m.id).max().unwrap_or(0) + 1;
            let prompt = crate::screenshots::prompt(&text, &attachments);
            let mut m = Mission { id, cwd: cwd.clone(), text: text.clone(), attachments, state: "queued".into(), agent: to.filter(|_| !launch), at: unix_now(), done_at: None, fleet: None, campaign, campaign_job: String::new() };
            if launch {
                match spawn(hub, &kind, &cwd, None, Some(&prompt), Role::parse(&role), &spec, &[]) {
                    Ok(aid) => {
                        m.state = "sent".into();
                        m.agent = Some(aid);
                    }
                    Err(e) => return Resp::Err { msg: e.to_string() },
                }
            }
            hub.lock().unwrap().missions.push(m);
            Resp::Ok
        }
        Req::Broadcast { ids, text } => {
            let mut h = hub.lock().unwrap();
            let fleet = h.missions.iter().filter_map(|m| m.fleet).max().unwrap_or(0) + 1;
            let mut next = h.missions.iter().map(|m| m.id).max().unwrap_or(0) + 1;
            let mut sent = 0;
            for id in ids {
                let Some(a) = h.agents.get(&id).cloned() else { continue };
                let cwd = {
                    let mut g = a.lock().unwrap();
                    // a blocked agent would take the order as an answer to its question
                    if g.exited.is_some() || g.status == Status::Blocked {
                        continue;
                    }
                    g.note_task(&text);
                    g.cwd.clone()
                };
                h.missions.push(Mission { id: next, cwd, text: text.clone(), attachments: Vec::new(), state: "sent".into(), agent: Some(id), at: unix_now(), done_at: None, fleet: Some(fleet), campaign: None, campaign_job: String::new() });
                next += 1;
                sent += 1;
                order(a, text.clone());
            }
            if sent == 0 { Resp::Err { msg: "no agent free to take the order".into() } } else { Resp::Ok }
        }
        Req::Moon { cwd, branch, kind } => match launch_moon(hub, &cwd, &branch, &kind) {
            Ok(id) => Resp::Spawned { id },
            Err(msg) => Resp::Err { msg },
        },
        Req::Dock { cwd } => match dock(hub, &cwd) {
            Ok(host) => {
                eprintln!("docked {cwd} into {host}");
                Resp::Ok
            }
            Err(msg) => Resp::Err { msg },
        },
        Req::Refit { id, role, spec, confirm } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
                let role = Role::parse(&role);
                if g.exited.is_some() {
                    return Resp::Err { msg: format!("agent #{id} has left") };
                }
                if g.refit_is("applying") {
                    return Resp::Err { msg: "it's being refitted right now".into() };
                }
                if role == g.role && spec == g.spec {
                    return Resp::Err { msg: "it already carries that outfit".into() };
                }
                // a claude that never got a prompt has nothing to resume
                let can = refit_session(&g.kind, g.session.as_deref()).and_then(|s| {
                    if g.kind == "claude" && !claude_transcript(&s) { Err("no conversation to resume yet: let it finish a turn first".into()) } else { Ok(s) }
                });
                match (can, confirm) {
                    (Ok(_), false) => Resp::Ok,
                    (Err(msg), false) => Resp::Err { msg },
                    (Ok(_), true) => {
                        g.refit = Some(RefitState { role: role.name().into(), spec, state: "pending".into(), note: String::new() });
                        Resp::Ok
                    }
                    (Err(msg), true) => {
                        g.refit = Some(RefitState { role: role.name().into(), spec, state: "unsupported".into(), note: msg.clone() });
                        Resp::Err { msg }
                    }
                }
            }
            None => missing(id),
        },
        Req::CancelRefit { id } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
                if g.refit_is("applying") {
                    return Resp::Err { msg: "the refit is already under way".into() };
                }
                g.refit = None;
                Resp::Ok
            }
            None => missing(id),
        },
        Req::Scrub { id } => {
            hub.lock().unwrap().missions.retain(|m| m.id != id);
            Resp::Ok
        }
        Req::Spawn { kind, cwd, run } => match spawn(hub, &kind, &cwd, None, None, Role::None, &ShipSpec::default(), &run) {
            Ok(id) => Resp::Spawned { id },
            Err(e) => Resp::Err { msg: format!("could not launch {kind}: {e}") },
        },
        Req::Kill { id } => match get(id) {
            Some(a) => {
                a.lock().unwrap().writer.kill();
                Resp::Ok
            }
            None => missing(id),
        },
        Req::Input { id, data } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
                g.last_in = Instant::now();
                if g.status == Status::Done {
                    g.set(Status::Idle);
                }
                if g.status == Status::Blocked {
                    g.answered = g.question();
                    if g.hook == Some(Status::Blocked) {
                        g.hook = None; // answered: let the next hook/heuristic decide
                    }
                    g.typed.clear(); // an answer to its question, not a new task
                } else {
                    g.keys(&data);
                }
                match g.writer.write_all(data.as_bytes()) {
                    Ok(()) => Resp::Ok,
                    Err(e) => Resp::Err { msg: e.to_string() },
                }
            }
            None => missing(id),
        },
        Req::Priority { id } => {
            let Some(me) = get(id) else { return missing(id) };
            let (cwd, mine) = {
                let g = me.lock().unwrap();
                (g.cwd.clone(), g.touched.keys().cloned().collect::<HashSet<String>>())
            };
            let rivals: Vec<Arc<Mutex<Agent>>> = hub.lock().unwrap().agents.values().filter(|a| !Arc::ptr_eq(a, &me)).cloned().collect();
            let mut told = 0;
            for r in rivals {
                let files: Vec<String> = {
                    let mut g = r.lock().unwrap();
                    // (one waiting on a question would take the message as its answer)
                    if g.cwd != cwd || g.status == Status::Blocked {
                        continue;
                    }
                    let f: Vec<String> = g.touched.keys().filter(|k| mine.contains(*k)).cloned().collect();
                    for k in &f {
                        g.touched.remove(k);
                    }
                    f
                };
                if files.is_empty() {
                    continue;
                }
                let names: Vec<&str> = files.iter().map(|f| f.strip_prefix(&format!("{cwd}/")).unwrap_or(f)).collect();
                order(r, format!("Heads up: another agent in this folder is working on {} and has priority there. Please leave those files alone; work around them or wait until they're done.", names.join(", ")));
                told += 1;
            }
            if told == 0 { Resp::Err { msg: "no conflict to settle".into() } } else { Resp::Ok }
        }
        Req::Answer { id, key } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
                let before = g.question();
                g.answered = before.clone();
                if g.hook == Some(Status::Blocked) {
                    g.hook = None;
                }
                g.last_in = Instant::now();
                if let Err(e) = g.writer.write_all(key.as_bytes()) {
                    return Resp::Err { msg: e.to_string() };
                }
                // some menus only move the cursor on a digit; confirm if it's still asking
                if key.len() <= 2 && key.chars().all(|c| c.is_ascii_digit()) {
                    let a = a.clone();
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(600));
                        let mut g = a.lock().unwrap();
                        if g.question().is_some() && g.question().map(|q| q.0) == before.map(|q| q.0) {
                            let _ = g.writer.write_all(b"\r");
                        }
                    });
                }
                Resp::Ok
            }
            None => missing(id),
        },
        Req::Resize { id, cols, rows } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
                let (cols, rows) = (cols.clamp(20, 400), rows.clamp(5, 200));
                g.writer.resize(rows, cols);
                g.parser.set_size(NonZeroU16::new(rows).unwrap(), NonZeroU16::new(cols).unwrap());
                g.touch();
                Resp::Ok
            }
            None => missing(id),
        },
        Req::Screen { id, scroll, since, wait } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
                if wait > 0 && g.version == since {
                    let changed = g.changed.clone();
                    g = changed.wait_timeout_while(g, Duration::from_millis(wait.min(1000)), |g| g.version == since).unwrap().0;
                }
                g.parser.screen_mut().set_scrollback(scroll);
                let screen = g.screen();
                g.parser.screen_mut().set_scrollback(0);
                Resp::Screen { screen }
            }
            None => missing(id),
        },
        Req::Seen { id } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
                if g.status == Status::Done {
                    g.set(Status::Idle);
                }
                Resp::Ok
            }
            None => missing(id),
        },
        Req::Hook { id, event, message, file, session } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
                g.hooks = true;
                if let Some(s) = session.filter(|s| !s.is_empty()) {
                    g.session = Some(s);
                }
                if let Some(f) = file {
                    let f = g.resolve(&f);
                    g.touched.insert(f, Instant::now());
                }
                let msg = message.unwrap_or_default().to_lowercase();
                g.hook = match event.as_str() {
                    "working" => Some(Status::Working),
                    "done" => Some(Status::Done),
                    "notify" if msg.contains("permission") || msg.contains("approv") || msg.contains("needs your") => Some(Status::Blocked),
                    _ => g.hook,
                };
                Resp::Ok
            }
            None => missing(id),
        },
        Req::Report { id, report } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
                if report.session.is_some() {
                    g.session = report.session.clone();
                }
                g.report = report;
                Resp::Ok
            }
            None => missing(id),
        },
        Req::CampaignReport { .. } => Resp::Err { msg: "Campaigns have been retired. Continue with the project's agent terminal.".into() },
        Req::Shutdown => {
            // a deliberate stop: nobody comes back next time
            let _ = std::fs::remove_file(registry_file());
            for a in hub.lock().unwrap().agents.values() {
                a.lock().unwrap().writer.kill();
            }
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(200));
                let _ = std::fs::remove_file(socket_path());
                std::process::exit(0);
            });
            Resp::Ok
        }
    }
}

fn serve(hub: Shared, s: UnixStream) -> io::Result<()> {
    let mut w = s.try_clone()?;
    for line in BufReader::new(s).lines() {
        let line = line?;
        let resp = match serde_json::from_str::<Req>(&line) {
            Ok(req) => handle(&hub, req),
            Err(e) => Resp::Err { msg: format!("bad request: {e}") },
        };
        let mut out = serde_json::to_string(&resp).map_err(io::Error::other)?;
        out.push('\n');
        w.write_all(out.as_bytes())?;
    }
    Ok(())
}

pub fn run() -> io::Result<()> {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        eprintln!("empire daemon already running at {}", path.display());
        return Ok(());
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    let hub: Shared = Arc::default();
    let _ = BUILD.set(build_id());
    if let Ok(s) = std::fs::read_to_string(missions_file()) {
        let mut ms: Vec<Mission> = serde_json::from_str(&s).unwrap_or_default();
        // agent ids don't survive a restart; anything under way goes back in the queue
        for m in ms.iter_mut().filter(|m| m.state != "done") {
            if std::env::var_os("EMPIRE_HANDOFF").is_none() {
                if !m.campaign_job.is_empty() { m.agent = None; continue; }
                m.state = "queued".into();
                m.agent = None;
            }
        }
        hub.lock().unwrap().missions = ms;
    }
    match std::env::var("EMPIRE_HANDOFF") {
        Ok(f) => {
            unsafe { std::env::remove_var("EMPIRE_HANDOFF") };
            match adopt(&hub, &f) {
                Ok(()) => eprintln!("upgraded in place, {} agents carried over", hub.lock().unwrap().agents.len()),
                Err(e) => eprintln!("handoff failed: {e}"),
            }
        }
        Err(_) => resurrect(&hub),
    }

    // status bookkeeping, and reaping agents a while after they exit
    let h = hub.clone();
    thread::spawn(move || {
        let mut saved = Vec::new();
        let mut logged = Vec::new();
        let mut tick = 0u32;
        loop {
            tick += 1;
            if tick % 10 == 0 {
                for a in h.lock().unwrap().agents.values() {
                    let mut g = a.lock().unwrap();
                    if g.kind != "shell" && !g.hooked() {
                        g.scan_edits();
                    }
                    g.touched.retain(|_, t| t.elapsed() < Duration::from_secs(WAR_MEMORY));
                }
                discover_sessions(&h);
                save_registry(&h, &mut saved);
            }
            dispatch(&h, &mut logged);
            overlap_tick(&h, Instant::now(), overlap_mode());
            // legacy campaign records remain on disk; the workspace no longer dispatches them.
            let now = Instant::now();
            let list: Vec<(u32, Arc<Mutex<Agent>>)> = h.lock().unwrap().agents.iter().map(|(k, v)| (*k, v.clone())).collect();
            let mut dead = Vec::new();
            let watched = h.lock().unwrap().watched.is_some_and(|w| now - w < Duration::from_secs(3));
            for (id, a) in list {
                let mut g = a.lock().unwrap();
                g.update(now);
                if g.status != g.told {
                    g.told = g.status;
                    let long = g.worked_at.is_some_and(|w| now - w > Duration::from_secs(DESKTOP_TURN));
                    if !watched && (g.status == Status::Blocked || (g.status == Status::Done && long)) {
                        alert(&h, &g);
                    }
                }
                if g.exited.is_some_and(|e| now - e > Duration::from_secs(6)) {
                    dead.push(id);
                }
            }
            let mut g = h.lock().unwrap();
            for id in dead {
                g.agents.remove(&id);
            }
            drop(g);
            thread::sleep(Duration::from_millis(200));
        }
    });

    for s in listener.incoming().flatten() {
        let h = hub.clone();
        thread::spawn(move || serve(h, s));
    }
    Ok(())
}

/// `empire hook <event>`: called by Claude Code from inside an agent. Must be
/// quick, quiet and never fail loudly.
pub fn hook(event: &str) {
    let mut input = String::new();
    let _ = io::stdin().read_to_string(&mut input);
    let Some(id) = std::env::var("EMPIRE_AGENT_ID").ok().and_then(|s| s.parse().ok()) else { return };
    let v = serde_json::from_str::<serde_json::Value>(&input).unwrap_or_default();
    let message = hook_message(&v);
    let file = edited_file(&v);
    let session = v.get("session_id").or_else(|| v.get("sessionId")).and_then(|s| s.as_str()).map(String::from);
    let _ = call(&Req::Hook { id, event: event.into(), message, file, session });
}

/// `empire statusline`: Claude Code's statusline command inside the empire. Reports
/// the agent's vitals to the daemon, keeps the latest rate limits on disk for the
/// galaxy, and still shows the player's own statusline if they have one.
pub fn statusline() {
    let mut input = String::new();
    let _ = io::stdin().read_to_string(&mut input);
    let v: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
    let f = |path: &[&str]| path.iter().try_fold(&v, |v, k| v.get(k));
    if let Some(rl) = f(&["rate_limits"]) {
        let file = crate::survey::claude_limits_file();
        let _ = std::fs::create_dir_all(dirs_state());
        let tmp = file.with_extension("tmp");
        if std::fs::write(&tmp, rl.to_string()).is_ok() {
            let _ = std::fs::rename(tmp, file);
        }
    }
    if let Some(id) = std::env::var("EMPIRE_AGENT_ID").ok().and_then(|s| s.parse().ok()) {
        let num = |path: &[&str]| f(path).and_then(|v| v.as_f64());
        let report = Report {
            model: f(&["model", "display_name"]).and_then(|m| m.as_str()).map(String::from),
            ctx: num(&["context_window", "used_percentage"]).map(|x| x as f32),
            cost: num(&["cost", "total_cost_usd"]).map(|x| x as f32),
            added: num(&["cost", "total_lines_added"]).map(|x| x as u32),
            removed: num(&["cost", "total_lines_removed"]).map(|x| x as u32),
            session: f(&["session_id"]).and_then(|s| s.as_str()).map(String::from),
        };
        let _ = call(&Req::Report { id, report });
    }
    // the player's own statusline, if any
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| home.join(".claude"));
    let own = std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|s| s.get("statusLine")?.get("command")?.as_str().map(String::from));
    if let Some(cmd) = own {
        use std::process::{Command, Stdio};
        if let Ok(mut child) = Command::new("sh").arg("-c").arg(cmd).stdin(Stdio::piped()).spawn() {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(input.as_bytes());
            }
            let _ = child.wait();
        }
    }
}

/// A desktop notification for an agent that needs you. A question's choices
/// become buttons, so it can be answered right from the notification.
fn alert(hub: &Shared, a: &Agent) {
    let place = crate::game::base_name(&a.cwd);
    let (title, body, urgency, ask) = if a.status == Status::Blocked {
        let ask = a.ask();
        let body = ask.as_ref().map(|q| q.context.iter().chain([&q.question]).cloned().collect::<Vec<_>>().join("\n")).unwrap_or_default();
        (format!("▲ {} on {place} awaits orders", a.kind), body, "critical", ask)
    } else {
        let title = a.parser.callbacks().title.clone();
        (format!("✔ {} finished on {place}", a.kind), title, "normal", None)
    };
    let (id, hub) = (a.id, hub.clone());
    thread::spawn(move || {
        let mut cmd = std::process::Command::new("notify-send");
        cmd.args(["-a", "Agent Empire", "-u", urgency, &title, &body]);
        let options = ask.map(|q| q.options).unwrap_or_default();
        for (i, o) in options.iter().take(3).enumerate() {
            cmd.arg(format!("--action={i}={}", o.label));
        }
        let Ok(out) = cmd.stderr(std::process::Stdio::null()).output() else { return };
        // a button was pressed: answer the agent, if it's still asking
        let Ok(i) = String::from_utf8_lossy(&out.stdout).trim().parse::<usize>() else { return };
        let (Some(o), Some(a)) = (options.get(i), hub.lock().unwrap().agents.get(&id).cloned()) else { return };
        let mut g = a.lock().unwrap();
        if g.status == Status::Blocked {
            g.answered = g.question();
            g.hook = None;
            let _ = g.writer.write_all(o.key.as_bytes());
        }
    });
}

/// `empire status`: one line for a status bar (`--json`: waybar-style JSON).
pub fn status(json: bool) {
    let agents = match call(&Req::Peek) {
        Ok(Resp::Agents { agents, .. }) => agents,
        _ => {
            if json {
                println!(r#"{{"text":"","tooltip":"empire daemon offline","class":"offline"}}"#);
            }
            return;
        }
    };
    let n = |s: &str| agents.iter().filter(|a| a.status == s).count();
    let parts: Vec<String> = [("blocked", "▲"), ("done", "✔"), ("working", "◆"), ("idle", "○")]
        .iter()
        .filter(|(s, _)| n(s) > 0)
        .map(|(s, g)| format!("{g}{}", n(s)))
        .collect();
    let text = parts.join(" ");
    if !json {
        println!("{text}");
        return;
    }
    let tip: Vec<String> = agents
        .iter()
        .map(|a| {
            let what = a.ask.as_ref().map(|q| q.question.clone()).unwrap_or_else(|| a.title.clone());
            format!("{} {} @ {} — {what}", a.status, a.kind, crate::game::base_name(&a.cwd))
        })
        .collect();
    let class = ["blocked", "done", "working"].into_iter().find(|s| n(s) > 0).unwrap_or("idle");
    println!("{}", serde_json::json!({ "text": text, "tooltip": tip.join("\n"), "class": class, "alt": class }));
}

/// Make sure the running daemon is this build, upgrading it in place (agents and
/// all) if not. Returns a complaint if the daemon is too old to upgrade itself.
pub fn ensure_current() -> Option<String> {
    let want = build_id();
    match call(&Req::Ping) {
        Ok(Resp::Pong { version }) if version == want => return None,
        Ok(_) => {}
        Err(_) => return None,
    }
    match call(&Req::Upgrade) {
        Ok(Resp::Ok) => {}
        _ => return Some("the daemon is too old to upgrade itself: `empire stop` once to restart it".into()),
    }
    for _ in 0..60 {
        thread::sleep(Duration::from_millis(100));
        if let Ok(Resp::Pong { version }) = call(&Req::Ping)
            && version == want
        {
            return None;
        }
    }
    Some("the daemon didn't come back from its upgrade; see ~/.local/state/empire/daemon.log".into())
}

/// Start the daemon in the background (own session, detached from this terminal).
pub fn ensure_running() -> io::Result<()> {
    if call(&Req::Ping).is_ok() {
        return Ok(());
    }
    use std::os::unix::process::CommandExt;
    let log_dir = dirs_state();
    let _ = std::fs::create_dir_all(&log_dir);
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("daemon.log"))?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("daemon").stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(60));
        if call(&Req::Ping).is_ok() {
            return Ok(());
        }
    }
    Err(io::Error::other("daemon did not come up"))
}

pub fn dirs_state() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    std::env::var_os("XDG_STATE_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| home.join(".local/state")).join("empire")
}

#[cfg(test)]
mod screenshot_tests {
    use super::*;

    #[test]
    fn queued_mission_preserves_attachments_and_rejects_missing_files() {
        let dir = std::env::temp_dir().join(format!("empire-daemon-images-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("screenshot.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let attachments = vec![crate::screenshots::Attachment { name: "screenshot.png".into(), path: path.to_str().unwrap().into() }];
        let hub = Shared::default();
        let request = Req::Mission { cwd: "/world".into(), text: "Fix this".into(), kind: "codex".into(), launch: false, attachments: attachments.clone(), role: String::new(), to: None, campaign: None, spec: ShipSpec::default() };
        assert!(matches!(handle(&hub, request.clone()), Resp::Ok));
        assert_eq!(hub.lock().unwrap().missions[0].attachments, attachments);
        std::fs::remove_file(path).unwrap();
        assert!(matches!(handle(&hub, request), Resp::Err { .. }));
        assert_eq!(hub.lock().unwrap().missions.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn multiline_image_prompt_is_one_paste_followed_by_enter() {
        let (tx, mut rx) = UnixStream::pair().unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let writer = Pty { master: File::from(std::os::fd::OwnedFd::from(tx)), pid: 0 };
        let agent = Arc::new(Mutex::new(new_agent(1, "codex", "/world", writer, 40, 120)));
        let prompt = crate::screenshots::prompt("Fix this\nKeep the colours", &[
            crate::screenshots::Attachment { name: "shot.png".into(), path: "/saved/shot.png".into() },
        ]);
        let expected = format!("\x1b[200~{prompt}\x1b[201~\r");
        order(agent, prompt);
        let mut bytes = vec![0; expected.len()];
        rx.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, expected.as_bytes());
    }

    #[test]
    fn the_last_prompt_typed_in_comms_becomes_the_task() {
        let (tx, _rx) = UnixStream::pair().unwrap();
        let writer = Pty { master: File::from(std::os::fd::OwnedFd::from(tx)), pid: 0 };
        let mut a = new_agent(1, "claude", "/world", writer, 40, 120);
        a.note_task("Build the trade routes\nwith details");
        assert_eq!(a.task, "Build the trade routes");
        // typed with a typo, an arrow key and a paste, then sent
        a.keys("Fix the logx");
        a.keys("\x7f\x7fgs\x1b[D\x1b[C \x1b[200~sorting~\x1b[201~");
        assert_eq!(a.task, "Build the trade routes");
        a.keys("\r");
        assert_eq!(a.task, "Fix the logs sorting~");
        // slash commands and menu picks aren't tasks
        a.keys("/clear\r1\r");
        assert_eq!(a.task, "Fix the logs sorting~");
    }

    #[test]
    fn grok_launches_inline_and_resumes() {
        let (prog, args) = command("grok", "/usr/bin/empire", Some("abc"));
        assert_eq!(prog, "grok");
        assert_eq!(args, ["--no-alt-screen", "--resume", "abc"]);
        let (prog, args) = command("grok", "/usr/bin/empire", None);
        assert_eq!(prog, "grok");
        assert_eq!(args, ["--no-alt-screen"]);
    }

    #[test]
    fn grok_hooks_call_empire_without_a_shell() {
        let v = grok_hooks_json("/usr/bin/empire");
        let hooks = &v["hooks"];
        for (event, arg) in [
            ("UserPromptSubmit", "working"),
            ("PreToolUse", "working"),
            ("PostToolUse", "working"),
            ("Stop", "done"),
            ("StopCancelled", "done"),
            ("SessionStart", "session"),
        ] {
            let h = &hooks[event][0]["hooks"][0];
            assert_eq!(h["type"], "command");
            assert_eq!(h["command"], "/usr/bin/empire");
            assert_eq!(h["args"], serde_json::json!(["hook", arg]));
            assert_eq!(h["timeout"], 5);
            assert!(hooks[event][0].get("matcher").is_none());
        }
        let n = &hooks["Notification"][0];
        assert_eq!(n["matcher"], "permission_prompt");
        assert_eq!(n["hooks"][0]["args"], serde_json::json!(["hook", "notify"]));
    }

    #[test]
    fn hook_payloads_name_edits_and_permission_prompts() {
        let write = serde_json::json!({ "toolName": "write", "toolInput": { "file_path": "src/a.rs" } });
        assert_eq!(edited_file(&write).as_deref(), Some("src/a.rs"));
        let replace = serde_json::json!({ "tool_name": "search_replace", "tool_input": { "path": "src/b.rs" } });
        assert_eq!(edited_file(&replace).as_deref(), Some("src/b.rs"));
        let claude = serde_json::json!({ "tool_name": "Edit", "tool_input": { "file_path": "src/c.rs" } });
        assert_eq!(edited_file(&claude).as_deref(), Some("src/c.rs"));
        let notebook = serde_json::json!({ "tool_name": "NotebookEdit", "tool_input": { "notebook_path": "n.ipynb" } });
        assert_eq!(edited_file(&notebook).as_deref(), Some("n.ipynb"));
        let shell = serde_json::json!({ "tool_name": "run_terminal_command", "tool_input": { "command": "ls" } });
        assert_eq!(edited_file(&shell), None);
        let prompt = serde_json::json!({ "notificationType": "permission_prompt", "message": "Allow this command?" });
        assert_eq!(hook_message(&prompt).as_deref(), Some("permission_prompt"));
        let claude_note = serde_json::json!({ "message": "Claude needs your permission" });
        assert_eq!(hook_message(&claude_note).as_deref(), Some("Claude needs your permission"));
    }

    #[test]
    fn screen_edit_lines_include_grok_titles() {
        assert_eq!(edit_path("Edit `src/a.rs`").as_deref(), Some("src/a.rs"));
        assert_eq!(edit_path("  • Edited src/a.rs (+3 -1)").as_deref(), Some("src/a.rs"));
        assert_eq!(edit_path("⏺ Update(src/a.rs)").as_deref(), Some("src/a.rs"));
        assert_eq!(edit_path("Wrote README").as_deref(), None);
        let (tx, _rx) = UnixStream::pair().unwrap();
        let writer = Pty { master: File::from(std::os::fd::OwnedFd::from(tx)), pid: 0 };
        let mut a = new_agent(1, "grok", "/world", writer, 40, 120);
        a.parser.process(b"Edit `src/a.rs`\r\n");
        a.scan_edits();
        assert!(a.touched.keys().any(|k| k.ends_with("src/a.rs")));
    }

    #[test]
    fn grok_vitals_read_session_files() {
        let dir = std::env::temp_dir().join(format!("empire-grok-vitals-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("signals.json"), r#"{"primaryModelId":"grok-4","contextWindowUsage":13,"agentLinesAdded":4,"agentLinesRemoved":1}"#).unwrap();
        std::fs::write(dir.join("usage.json"), r#"{"session":{"costUsdTicks":1423104000}}"#).unwrap();
        let report = grok_vitals(&dir).unwrap();
        assert_eq!(report.model.as_deref(), Some("grok-4"));
        assert_eq!(report.ctx, Some(13.0));
        assert_eq!(report.added, Some(4));
        assert_eq!(report.removed, Some(1));
        assert!((report.cost.unwrap() - 0.1423104).abs() < 1e-5);
        assert!(report.session.is_none());
        std::fs::remove_dir_all(dir).unwrap();
        assert!(grok_vitals(std::path::Path::new("/no/such/empire-grok-session")).is_none());
    }

    /// A Codex approval sits above its status line. The footer is the same for every
    /// command, so a later prompt must still count as waiting.
    #[test]
    fn a_later_codex_command_approval_still_needs_you() {
        let (tx, _rx) = UnixStream::pair().unwrap();
        let writer = Pty { master: File::from(std::os::fd::OwnedFd::from(tx)), pid: 0 };
        let mut a = new_agent(1, "codex", "/world", writer, 40, 120);
        let screen = "\
Would you like to run the following command?\r\n\
\r\n\
Environment: local\r\n\
\r\n\
Reason: Allow the focused workspace tests to access pnpm's package store outside the sandbox?\r\n\
\r\n\
$ pnpm test --runInBand components/__tests__/text-help-workspace.test.tsx\r\n\
\r\n\
> 1. Yes, proceed (y)\r\n\
2. Yes, and don't ask again for commands that start with `pnpm test` (p)\r\n\
3. No, and tell Codex what to do differently (esc)\r\n\
\r\n\
Press enter to confirm or esc to cancel\r\n\
\r\n\
  GPT-6-Astra high · ~/code/app · ← for agents\r\n";
        a.parser.process(screen.as_bytes());
        // an earlier approval already answered that same footer
        a.answered = Some(("press enter to confirm or esc to cancel".into(), 1));
        a.started = Instant::now() - Duration::from_secs(30);
        a.status = Status::Done;
        a.update(Instant::now());
        assert!(a.asking(), "the command prompt is a new question");
        assert_eq!(a.status, Status::Blocked);
        let (q, _) = a.question().unwrap();
        assert!(q.contains("pnpm test"), "{q}");
        let ask = a.ask().unwrap();
        assert!(ask.options.len() >= 3, "{:?}", ask.options);
        assert!(ask.options.iter().any(|o| o.label.to_lowercase().contains("proceed")));
    }
}

#[cfg(test)]
mod outfit_tests {
    use super::*;

    #[test]
    fn a_ship_keeps_its_outfit_across_upgrades_and_restarts() {
        // records written before designs still load, bare
        let old: Saved = serde_json::from_str(r#"{"kind":"codex","cwd":"/w","session":null,"role":"scout"}"#).unwrap();
        assert_eq!(old.spec, ShipSpec::default());
        let spec = ShipSpec { orders: "Keep diffs small.".into(), skills: vec!["release".into()], design: "Frigate".into() };
        let saved = Saved { kind: "claude".into(), cwd: "/w".into(), session: Some("s".into()), role: "builder".into(), spec: spec.clone(), refit: None };
        assert!(serde_json::from_str::<Saved>(&serde_json::to_string(&saved).unwrap()).unwrap() == saved);
        let hand = r#"{"id":1,"kind":"claude","cwd":"/w","fd":3,"pid":9,"rows":24,"cols":80,"screen":"","title":"","status":"idle","age":1.0,"turns":0,"report":{},"session":null}"#;
        let h: Handoff = serde_json::from_str(hand).unwrap();
        assert_eq!(h.spec, ShipSpec::default());
        let mut v: serde_json::Value = serde_json::from_str(hand).unwrap();
        v["spec"] = serde_json::to_value(&spec).unwrap();
        assert_eq!(serde_json::from_value::<Handoff>(v).unwrap().spec, spec);
    }
}


#[cfg(test)]
mod refit_tests {
    use super::*;

    /// A ship on the hub with no real process behind it.
    fn ship(hub: &Shared, id: u32, kind: &str, session: Option<&str>, status: Status) -> Arc<Mutex<Agent>> {
        let (tx, _rx) = UnixStream::pair().unwrap();
        let mut a = new_agent(id, kind, "/world", Pty { master: File::from(std::os::fd::OwnedFd::from(tx)), pid: 0 }, 24, 80);
        a.session = session.map(String::from);
        a.status = status;
        a.role = Role::Builder;
        let a = Arc::new(Mutex::new(a));
        hub.lock().unwrap().agents.insert(id, a.clone());
        a
    }

    #[test]
    fn ordinary_terminals_never_take_planet_tasks_or_request_agent_attention() {
        let hub = Shared::default();
        let a = ship(&hub, 1, "shell", None, Status::Idle);
        a.lock().unwrap().parser.process(b"Do you want to proceed? esc to interrupt");
        a.lock().unwrap().started = Instant::now() - Duration::from_secs(30);
        a.lock().unwrap().update(Instant::now());
        assert_eq!(a.lock().unwrap().status, Status::Idle);
        hub.lock().unwrap().missions.push(Mission { id: 1, cwd: "/world".into(), text: "Review this".into(), state: "queued".into(), ..Mission::default() });
        dispatch(&hub, &mut vec![]);
        assert_eq!(hub.lock().unwrap().missions[0].state, "queued");
        assert_eq!(hub.lock().unwrap().missions[0].agent, None);
    }

    fn outfit() -> ShipSpec { ShipSpec { orders: "Review before you build.".into(), skills: vec!["audit".into()], design: "Warden".into() } }

    fn state(a: &Arc<Mutex<Agent>>) -> Option<String> { a.lock().unwrap().refit.as_ref().map(|r| r.state.clone()) }

    fn refit_req(id: u32, confirm: bool) -> Req { Req::Refit { id, role: "inspector".into(), spec: outfit(), confirm } }

    #[test]
    fn records_without_a_refit_still_load_and_an_interrupted_one_waits_again() {
        let old: Saved = serde_json::from_str(r#"{"kind":"codex","cwd":"/w","session":"s","role":"builder"}"#).unwrap();
        assert!(old.refit.is_none());
        let hand = r#"{"id":1,"kind":"claude","cwd":"/w","fd":3,"pid":9,"rows":24,"cols":80,"screen":"","title":"","status":"idle","age":1.0,"turns":0,"report":{},"session":"s"}"#;
        assert!(serde_json::from_str::<Handoff>(hand).unwrap().refit.is_none());
        let r = RefitState { role: "inspector".into(), spec: outfit(), state: "applying".into(), note: String::new() };
        let saved = Saved { refit: Some(r.clone()), ..old };
        let back: Saved = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert!(back == saved);
        let mut v: serde_json::Value = serde_json::from_str(hand).unwrap();
        v["refit"] = serde_json::to_value(&r).unwrap();
        assert_eq!(serde_json::from_value::<Handoff>(v).unwrap().refit, Some(r.clone()));
        // a restart mid-refit brings the ship back as it was, the refit still waiting
        assert_eq!(resumed(Some(r.clone())).unwrap().state, "pending");
        let active = RefitState { state: "active".into(), ..r };
        assert_eq!(resumed(Some(active.clone())), Some(active));
    }

    #[test]
    fn a_refit_is_checked_then_queued_only_when_confirmed() {
        let hub = Shared::default();
        let a = ship(&hub, 1, "codex", Some("s"), Status::Idle);
        assert!(matches!(handle(&hub, refit_req(1, false)), Resp::Ok));
        assert_eq!(state(&a), None, "a check changes nothing");
        assert!(matches!(handle(&hub, refit_req(1, true)), Resp::Ok));
        assert_eq!(state(&a), Some("pending".into()));
        // still what it was launched with until the refit is applied
        assert_eq!(a.lock().unwrap().role, Role::Builder);
        assert_eq!(a.lock().unwrap().view(Instant::now()).refit.unwrap().spec, outfit());
        // the same outfit again is no refit
        let same = Req::Refit { id: 1, role: "builder".into(), spec: ShipSpec::default(), confirm: true };
        assert!(matches!(handle(&hub, same), Resp::Err { .. }));
        assert!(matches!(handle(&hub, Req::CancelRefit { id: 1 }), Resp::Ok));
        assert_eq!(state(&a), None);
        a.lock().unwrap().refit = Some(RefitState { state: "applying".into(), ..Default::default() });
        assert!(matches!(handle(&hub, Req::CancelRefit { id: 1 }), Resp::Err { .. }));
        assert!(matches!(handle(&hub, refit_req(1, true)), Resp::Err { .. }));
    }

    #[test]
    fn a_ship_that_cant_resume_is_unsupported() {
        let hub = Shared::default();
        let fresh = ship(&hub, 1, "codex", None, Status::Idle);
        let cursor = ship(&hub, 2, "cursor", Some("s"), Status::Idle);
        assert!(matches!(handle(&hub, refit_req(1, false)), Resp::Err { .. }));
        assert_eq!(state(&fresh), None);
        assert!(matches!(handle(&hub, refit_req(1, true)), Resp::Err { .. }));
        assert_eq!(state(&fresh), Some("unsupported".into()));
        assert!(matches!(handle(&hub, refit_req(2, true)), Resp::Err { msg } if msg.contains("can't resume")));
        assert_eq!(state(&cursor), Some("unsupported".into()));
        assert!(refit_session("shell", Some("s")).is_err());
        // a species that can't resume says so, rather than asking to wait for a session it'll never get
        assert!(refit_session("shell", None).is_err_and(|e| e.contains("can't resume")));
        for kind in ["claude", "codex", "grok"] {
            assert_eq!(refit_session(kind, Some("s")).as_deref(), Ok("s"));
        }
    }

    #[test]
    fn a_busy_ship_stays_pending_and_takes_no_new_missions() {
        let hub = Shared::default();
        let pending = || Some(RefitState { role: "inspector".into(), spec: outfit(), state: "pending".into(), note: String::new() });
        let working = ship(&hub, 1, "codex", Some("s1"), Status::Working);
        let on_mission = ship(&hub, 2, "codex", Some("s2"), Status::Idle);
        working.lock().unwrap().refit = pending();
        on_mission.lock().unwrap().refit = pending();
        let sent = Mission { id: 1, cwd: "/world".into(), text: "keep going".into(), state: "sent".into(), agent: Some(2), ..Mission::default() };
        let queued = Mission { id: 2, cwd: "/world".into(), text: "next".into(), state: "queued".into(), ..Mission::default() };
        hub.lock().unwrap().missions = vec![sent, queued];
        // already logged, so nothing is written to the real state dir
        let mut logged = hub.lock().unwrap().missions.clone();
        for _ in 0..3 {
            dispatch(&hub, &mut logged);
        }
        assert_eq!(state(&working), Some("pending".into()));
        assert_eq!(state(&on_mission), Some("pending".into()));
        assert_eq!(hub.lock().unwrap().missions[1].state, "queued");
        assert_eq!(working.lock().unwrap().role, Role::Builder);
    }

    #[test]
    fn a_free_ship_starts_its_refit_and_one_that_lost_its_session_is_unsupported() {
        let hub = Shared::default();
        // no session: the relaunch stops before touching any process
        let a = ship(&hub, 1, "codex", None, Status::Done);
        a.lock().unwrap().refit = Some(RefitState { role: "inspector".into(), spec: outfit(), state: "pending".into(), note: String::new() });
        let queued = Mission { id: 1, cwd: "/world".into(), text: "next".into(), state: "queued".into(), agent: Some(1), ..Mission::default() };
        hub.lock().unwrap().missions = vec![queued];
        let mut logged = hub.lock().unwrap().missions.clone();
        dispatch(&hub, &mut logged);
        assert_ne!(state(&a), Some("pending".into()));
        assert_eq!(hub.lock().unwrap().missions[0].state, "queued", "the refit goes first");
        let t = Instant::now();
        while state(&a).as_deref() != Some("unsupported") && t.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(state(&a), Some("unsupported".into()));
        assert_eq!(a.lock().unwrap().role, Role::Builder);
    }

    #[test]
    fn relaunch_lines_resume_the_same_session_with_the_new_outfit() {
        let spec = outfit();
        let (prog, args) = launch_line("claude", "/bin/empire", Some("sess-1"), None, Role::Scout, &spec);
        assert_eq!(prog, "claude");
        let at = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].clone());
        assert_eq!(at("--resume").as_deref(), Some("sess-1"));
        assert_eq!(at("--permission-mode").as_deref(), Some("plan"));
        assert!(at("--append-system-prompt").unwrap().contains("Review before you build.") && at("--append-system-prompt").unwrap().contains("audit"));
        let (_, codex) = launch_line("codex", "/bin/empire", Some("sess-2"), None, Role::Builder, &spec);
        assert!(codex.windows(2).any(|w| w == ["resume", "sess-2"]));
        assert!(codex.iter().any(|a| a.starts_with("developer_instructions=") && a.contains("Review before you build.")));
        let (_, grok) = launch_line("grok", "/bin/empire", Some("sess-3"), None, Role::Inspector, &spec);
        assert!(grok.windows(2).any(|w| w == ["--resume", "sess-3"]));
        assert!(grok.windows(2).any(|w| w[0] == "--rules" && w[1].contains("You are an inspector")));
        for a in [args, codex, grok].concat() {
            assert!(!["--trust", "--yolo", "--dangerously-skip-permissions", "--dangerously-bypass-approvals-and-sandbox"].contains(&a.as_str()));
        }
    }

    /// A disposable stand-in for claude: logs its arguments, refuses to start as a scout.
    fn stand_in(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("launches.log");
        let script = dir.join("fake-claude");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\necho ---- >> '{}'\nfor a in \"$@\"; do [ \"$a\" = --permission-mode ] && exit 3; done\nexec sleep 30\n", log.display(), log.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        STAND_IN.with(|s| *s.borrow_mut() = Some(script.to_string_lossy().into_owned()));
        log
    }

    #[test]
    fn a_terminal_can_run_one_command_and_says_when_it_has_ended() {
        let dir = std::env::temp_dir().join(format!("empire-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hub = Shared::default();
        let run: Vec<String> = ["sh", "-c", "printf 'writing AGENTS.md'"].map(String::from).to_vec();
        let a = spawn_as(&hub, 3, "shell", &dir.to_string_lossy(), None, None, Role::None, &ShipSpec::default(), &run).unwrap();
        for _ in 0..200 {
            let g = a.lock().unwrap();
            if g.exited.is_some() && g.parser.screen().contents().contains("writing AGENTS.md") {
                break;
            }
            drop(g);
            thread::sleep(Duration::from_millis(25));
        }
        let g = a.lock().unwrap();
        assert!(g.parser.screen().contents().contains("writing AGENTS.md"), "it ran the command, not a shell");
        assert!(g.screen().exited, "the screen says the program has ended");
        assert_eq!(g.view(Instant::now()).run, run);
        drop(g);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refit_relaunches_the_same_ship_on_its_session_and_restores_it_on_failure() {
        let dir = std::env::temp_dir().join(format!("empire-refit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = stand_in(&dir);
        let hub = Shared::default();
        let cwd = dir.to_string_lossy().into_owned();
        let first = spawn_as(&hub, 7, "claude", &cwd, Some("sess-1"), None, Role::Builder, &ShipSpec::default(), &[]).unwrap();
        let old_pid = first.lock().unwrap().writer.pid;
        // let the first launch get going before it's stood down
        let t = Instant::now();
        while !std::fs::read_to_string(&log).unwrap_or_default().contains("----") && t.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(20));
        }
        {
            let mut g = first.lock().unwrap();
            g.task = "Chart the hyperlanes".into();
            g.turns = 4;
            g.refit = Some(RefitState { role: "inspector".into(), spec: outfit(), state: "applying".into(), note: String::new() });
        }
        refit(&hub, 7, Role::Inspector, &outfit());
        let now = hub.lock().unwrap().agents.get(&7).cloned().unwrap();
        {
            let g = now.lock().unwrap();
            assert_eq!(g.id, 7);
            assert_ne!(g.writer.pid, old_pid);
            assert!(g.exited.is_none());
            assert_eq!(g.role, Role::Inspector);
            assert_eq!(g.spec, outfit());
            assert_eq!(g.session.as_deref(), Some("sess-1"));
            assert_eq!((g.task.as_str(), g.turns), ("Chart the hyperlanes", 4));
            assert_eq!(g.refit.as_ref().map(|r| r.state.as_str()), Some("active"));
        }
        assert!(first.lock().unwrap().exited.is_some(), "the old harness stood down");
        let launches = std::fs::read_to_string(&log).unwrap();
        let second: Vec<&str> = launches.split("----\n").nth(1).unwrap().lines().collect();
        assert!(second.windows(2).any(|w| w == ["--resume", "sess-1"]));
        assert!(second.iter().any(|a| a.contains("You are an inspector")));
        assert!(launches.contains("Review before you build."));

        // the stand-in won't start as a scout: the ship comes back as it was
        now.lock().unwrap().refit = Some(RefitState { role: "scout".into(), spec: ShipSpec::default(), state: "applying".into(), note: String::new() });
        refit(&hub, 7, Role::Scout, &ShipSpec::default());
        let back = hub.lock().unwrap().agents.get(&7).cloned().unwrap();
        {
            let g = back.lock().unwrap();
            assert!(g.exited.is_none());
            assert_eq!(g.role, Role::Inspector);
            assert_eq!(g.spec, outfit());
            let r = g.refit.as_ref().unwrap();
            assert_eq!(r.state, "failed");
            assert!(r.note.contains("old outfit"), "{}", r.note);
        }
        let all = std::fs::read_to_string(&log).unwrap();
        assert_eq!(all.matches("----").count(), 4);
        assert!(all.split("----\n").all(|l| l.trim().is_empty() || l.contains("sess-1")));
        back.lock().unwrap().writer.kill();
        STAND_IN.with(|s| *s.borrow_mut() = None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod overlap_tests {
    use super::*;
    use serde_json::json;

    fn ship(id: u32, cwd: &str, task: &str) -> OverlapShip { (id, "claude".into(), cwd.into(), task.into()) }

    fn agent(hub: &Shared, id: u32, kind: &str, cwd: &str, task: &str, status: Status) -> Arc<Mutex<Agent>> {
        let (tx, _rx) = UnixStream::pair().unwrap();
        let mut a = new_agent(id, kind, cwd, Pty { master: File::from(std::os::fd::OwnedFd::from(tx)), pid: 0 }, 24, 80);
        a.status = status;
        a.task = task.into();
        let a = Arc::new(Mutex::new(a));
        hub.lock().unwrap().agents.insert(id, a.clone());
        a
    }

    fn views(hub: &Shared) -> Vec<AgentView> {
        match handle(hub, Req::Peek) {
            Resp::Agents { agents, .. } => agents,
            _ => panic!("agents"),
        }
    }

    const A: &str = "Refactor the outliner rendering in src/draw.rs to use the new badge colours";
    const B: &str = "Update the outliner rendering so badges use the new colours, mostly src/draw.rs";

    #[test]
    fn it_runs_only_with_its_flag_and_works_without_a_key() {
        assert_eq!(overlap_mode_of(false, true), OverlapMode::Off);
        assert_eq!(overlap_mode_of(true, false), OverlapMode::Words);
        assert_eq!(overlap_mode_of(true, true), OverlapMode::Jev);
        let hub = Shared::default();
        agent(&hub, 1, "claude", "/w", A, Status::Working);
        agent(&hub, 2, "codex", "/w", B, Status::Working);
        overlap_tick(&hub, Instant::now(), OverlapMode::Off);
        assert!(hub.lock().unwrap().overlap.pairs.is_empty());
    }

    #[test]
    fn only_working_ships_with_a_task_on_a_shared_world_are_compared_and_six_at_most() {
        let hub = Shared::default();
        let a = agent(&hub, 1, "claude", "/w", A, Status::Working);
        let ships = |hub: &Shared| -> Vec<OverlapShip> { hub.lock().unwrap().agents.values().filter_map(|a| overlap_ship(&a.lock().unwrap())).collect() };
        assert_eq!(overlap_groups(&ships(&hub)).len(), 0, "one ship has nobody to overlap with");
        agent(&hub, 2, "claude", "/w", B, Status::Working);
        agent(&hub, 3, "claude", "/w", B, Status::Blocked);
        agent(&hub, 4, "claude", "/w", B, Status::Done);
        agent(&hub, 5, "shell", "/w", B, Status::Working);
        agent(&hub, 6, "claude", "/w", "", Status::Working);
        agent(&hub, 7, "claude", "/elsewhere", B, Status::Working);
        let g = overlap_groups(&ships(&hub));
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].0.as_str(), g[0].1.iter().map(|s| s.0).collect::<Vec<_>>()), ("/w", vec![1, 2]));
        a.lock().unwrap().exited = Some(Instant::now());
        assert!(overlap_groups(&ships(&hub)).is_empty());
        let many: Vec<_> = (1..=9).map(|i| ship(10 - i, "/w", "x")).collect();
        assert_eq!(overlap_groups(&many)[0].1.iter().map(|s| s.0).collect::<Vec<_>>(), [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn the_cache_key_is_the_sorted_id_and_task_pairs() {
        let (a, b) = (ship(1, "/w", "one"), ship(2, "/w", "two"));
        assert_eq!(group_hash(&[a.clone(), b.clone()]), group_hash(&[b.clone(), a.clone()]));
        assert_ne!(group_hash(&[a.clone(), b.clone()]), group_hash(&[a.clone(), ship(2, "/w", "two, differently")]));
        assert_ne!(pair_key(&a, &b), pair_key(&a, &ship(2, "/w", "other")));
        assert_eq!(pair_key(&a, &b), pair_key(&a, &ship(2, "/elsewhere", "two")), "the world is not part of a pair");
    }

    #[test]
    fn the_request_asks_one_noul_per_pair_and_sends_no_terminal_text() {
        let long = "y".repeat(500);
        let ships = [ship(3, "/home/me/code/web", "first"), ship(5, "/home/me/code/web", &long), ship(8, "/home/me/code/web", "third")];
        let pairs = [(0, 1), (0, 2), (1, 2)];
        let r = overlap_req("/home/me/code/web", &ships, &pairs);
        assert_eq!(r["model"], jev::model());
        assert_eq!(r["state"]["world"], "web");
        let fleet = r["state"]["ships"].as_object().unwrap();
        assert_eq!(fleet.keys().collect::<Vec<_>>(), ["ship_3", "ship_5", "ship_8"]);
        assert_eq!(fleet["ship_3"], json!({ "species": "claude", "task": "first" }));
        assert_eq!(fleet["ship_5"]["task"].as_str().unwrap().chars().count(), 300);
        assert_eq!(r["state"].as_object().unwrap().len(), 2, "world and ships, nothing else");
        let q = r["questions"].as_object().unwrap();
        assert_eq!(q.keys().collect::<Vec<_>>(), ["pair_3_5", "pair_3_8", "pair_5_8"]);
        assert!(q.values().all(|q| q["type"] == "noul" && q["instructions"].as_str().unwrap().contains("same work")));
        assert!(q["pair_3_8"]["instructions"].as_str().unwrap().contains("ship_3") && q["pair_3_8"]["instructions"].as_str().unwrap().contains("ship_8"));
    }

    #[test]
    fn the_fallback_reads_shared_files_and_shared_words() {
        // the same file named in both
        assert_eq!(overlap_fallback("fix the crash in src/daemon.rs", "add logging to src/daemon.rs"), 0.8);
        assert_eq!(overlap_fallback("fix the crash in src/daemon.rs", "add logging to daemon.rs please"), 0.8, "a bare file name matches its path");
        assert_eq!(overlap_fallback("edit ./Cargo.toml", "bump versions in cargo.toml."), 0.8);
        assert_eq!(overlap_fallback(A, B), 0.8, "both name src/draw.rs");
        // mostly the same words
        assert_eq!(overlap_fallback("Refactor the outliner rendering to use the new badge colours", "Update the outliner rendering so badges use the new colours"), 0.7);
        // different jobs, different files
        assert_eq!(overlap_fallback("fix the crash in src/daemon.rs", "restyle the title in src/draw.rs"), 0.0);
        assert_eq!(overlap_fallback("write the release notes", "tune the music score"), 0.0);
        // too little to go on, version numbers and "e.g." are not files
        assert_eq!(overlap_fallback("fix tests", "fix tests"), 0.0);
        assert_eq!(overlap_fallback("use v1.2 e.g. here", "bump to v1.2 e.g. there"), 0.0);
        assert_eq!(overlap_fallback("", ""), 0.0);
    }

    #[test]
    fn answered_pairs_show_from_0_7_and_vanish_when_a_task_changes() {
        let (a, b, c) = (ship(1, "/w", "one"), ship(2, "/w", "two"), ship(3, "/w", "three"));
        let mut pairs = HashMap::new();
        pairs.insert(pair_key(&a, &b), 0.7);
        pairs.insert(pair_key(&a, &c), 0.69);
        let found = overlaps(&[a.clone(), b.clone(), c.clone()], &pairs);
        assert_eq!(found[&1], vec![Overlap { rival: 2, p: 0.7 }]);
        assert_eq!(found[&2], vec![Overlap { rival: 1, p: 0.7 }]);
        assert!(!found.contains_key(&3), "unsure is shown as nothing");
        assert!(overlaps(&[a.clone(), ship(2, "/w", "two, now different"), c.clone()], &pairs).is_empty());
        assert!(overlaps(&[a.clone(), ship(2, "/other", "two")], &pairs).is_empty(), "not on the same world");
        assert!(overlaps(&[a], &pairs).is_empty());
    }

    #[test]
    fn without_a_key_the_fallback_flags_the_pair_and_the_views_carry_it_until_one_stops_working() {
        let hub = Shared::default();
        let a = agent(&hub, 1, "claude", "/w", A, Status::Working);
        let b = agent(&hub, 2, "codex", "/w", B, Status::Working);
        agent(&hub, 3, "claude", "/w", "restyle something unrelated in the music", Status::Working);
        overlap_tick(&hub, Instant::now(), OverlapMode::Words);
        let v = views(&hub);
        assert_eq!(v[0].overlap, vec![Overlap { rival: 2, p: 0.8 }]);
        assert_eq!(v[1].overlap, vec![Overlap { rival: 1, p: 0.8 }]);
        assert!(v[2].overlap.is_empty());
        // a task changes: that pair is read again, and this one is different work
        b.lock().unwrap().task = "write the release notes for the changelog".into();
        assert!(views(&hub)[0].overlap.is_empty());
        overlap_tick(&hub, Instant::now(), OverlapMode::Words);
        assert!(views(&hub)[0].overlap.is_empty());
        b.lock().unwrap().task = B.into();
        assert_eq!(views(&hub)[0].overlap.len(), 1, "an answered pair is remembered, not asked again");
        // it stops working: the badge goes
        a.lock().unwrap().status = Status::Done;
        assert!(views(&hub).iter().all(|v| v.overlap.is_empty()));
    }

    #[test]
    fn a_file_conflict_is_left_to_the_red_border_marker() {
        let hub = Shared::default();
        let a = agent(&hub, 1, "claude", "/w", A, Status::Working);
        let b = agent(&hub, 2, "codex", "/w", B, Status::Working);
        overlap_tick(&hub, Instant::now(), OverlapMode::Words);
        for x in [&a, &b] { x.lock().unwrap().touched.insert("/w/src/draw.rs".into(), Instant::now()); }
        let v = views(&hub);
        assert_eq!(v[0].war.len(), 1);
        assert!(v.iter().all(|v| v.overlap.is_empty()));
    }

    #[test]
    fn jev_is_asked_once_in_the_background_debounced_and_a_failure_falls_back() {
        if jev::enabled("EMPIRE_JEV_OVERLAP_TEST_NEVER") || std::env::var_os("TYPESAFE_API_KEY").is_some() {
            return; // a real key in the environment would make this a live call
        }
        let hub = Shared::default();
        let b = agent(&hub, 2, "codex", "/w", B, Status::Working);
        agent(&hub, 1, "claude", "/w", A, Status::Working);
        let now = Instant::now();
        overlap_tick(&hub, now, OverlapMode::Jev);
        assert!(hub.lock().unwrap().overlap.seen.contains_key("/w"));
        for _ in 0..200 {
            if !hub.lock().unwrap().overlap.busy { break; }
            thread::sleep(Duration::from_millis(10));
        }
        // no key: the request failed, so the words answered
        assert_eq!(hub.lock().unwrap().overlap.pairs.len(), 1);
        assert_eq!(views(&hub)[0].overlap.len(), 1);
        // a new task inside the debounce window waits
        b.lock().unwrap().task = "something else entirely about audio".into();
        overlap_tick(&hub, now + Duration::from_secs(5), OverlapMode::Jev);
        assert!(!hub.lock().unwrap().overlap.busy);
        assert_eq!(hub.lock().unwrap().overlap.pairs.len(), 1);
        overlap_tick(&hub, now + Duration::from_secs(31), OverlapMode::Jev);
        for _ in 0..200 {
            if !hub.lock().unwrap().overlap.busy { break; }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(hub.lock().unwrap().overlap.pairs.len(), 2);
    }

    #[test]
    fn views_from_an_older_daemon_have_no_overlap() {
        let v: AgentView = serde_json::from_str(r#"{"id":1,"kind":"claude","cwd":"/w","title":"","status":"working","age":0.5}"#).unwrap();
        assert!(v.overlap.is_empty());
        let mut v = v;
        v.overlap = vec![Overlap { rival: 7, p: 0.8 }];
        let back: AgentView = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(back.overlap, v.overlap);
    }
}
