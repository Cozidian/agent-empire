//! The empire daemon: owns every agent's pseudo-terminal and screen state, works
//! out who is busy, and serves the game over a unix socket. Agents live as long
//! as the daemon does, so the game UI can come and go.

use crate::proto::*;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::num::NonZeroU16;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
        unsafe { libc::kill(self.pid, libc::SIGHUP) };
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
    /// The last question the player answered, and how often it was on screen then.
    answered: Option<(String, usize)>,
    status: Status,
    since: Instant,
    exited: Option<Instant>,
    turns: u32,
    report: Report,
    /// The status we last told the desktop about.
    told: Status,
    /// When the current turn started.
    worked_at: Option<Instant>,
}

impl Agent {
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

    /// The question line near the bottom of the screen, if the agent seems to be
    /// asking the player something, plus how many times that line is on screen.
    fn question(&self) -> Option<(String, usize)> {
        const ASK: [&str; 10] = [
            "do you want", "would you like", "(y/n)", "[y/n]", "proceed?", "allow execution",
            "apply this change", "yes, and don't ask", "enter to confirm", "trust this folder",
        ];
        let text = self.parser.screen().contents().to_lowercase();
        let hits: Vec<&str> = text.lines().map(unbox).rev().filter(|l| !l.is_empty()).take(8).filter(|l| ASK.iter().any(|k| l.contains(k))).collect();
        // the question itself, not one of its numbered answers (unless that's all there is)
        let line = hits.iter().find(|l| option(l).is_none()).or(hits.first())?;
        let line = line.to_string();
        let count = text.lines().filter(|l| unbox(l) == line).count();
        Some((line, count))
    }

    /// The question being asked, with its context and choices, for answering from orbit.
    fn ask(&self) -> Option<Ask> {
        let (q, _) = self.question()?;
        let lines: Vec<String> = self.parser.screen().contents().lines().map(|l| unbox(l).to_string()).collect();
        let qi = lines.iter().rposition(|l| l.to_lowercase() == q)?;
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

    /// TUI agents print an interrupt hint only while they're busy.
    fn busy_marker(&self) -> bool {
        let text = self.parser.screen().contents().to_lowercase();
        let tail: Vec<&str> = text.lines().rev().filter(|l| !l.trim().is_empty()).take(10).collect();
        tail.iter().any(|l| l.contains("esc to interrupt") || l.contains("ctrl+c to interrupt"))
    }

    /// Agents whose idle screen animates, so output activity says nothing.
    fn animated(&self) -> bool {
        matches!(self.kind.as_str(), "codex")
    }

    /// Reports working/done through hooks, so its screen output (statusline
    /// repaints and all) says nothing about whether it's busy.
    fn hooked(&self) -> bool {
        self.kind == "claude"
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
            status: self.status.name().into(),
            age: (now - self.since).as_secs_f32(),
            turns: self.turns,
            report: self.report.clone(),
            ask: if self.status == Status::Blocked { self.ask() } else { None },
        }
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
        "grok" => {
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
        answered: None,
        turns: 0,
        report: Report::default(),
        told: Status::Booting,
        worked_at: None,
        status: Status::Booting,
        since: now,
        exited: None,
    }
}

/// Launch an agent in `cwd`, picking up its old session if we have one.
fn spawn(hub: &Shared, kind: &str, cwd: &str, resume: Option<&str>, prompt: Option<&str>) -> anyhow::Result<u32> {
    let id = next_id(hub);
    let exe = exe_path().to_string_lossy().into_owned();
    let pty = native_pty_system().openpty(PtySize { rows: ROWS, cols: COLS, pixel_width: 0, pixel_height: 0 })?;
    let (prog, mut args) = command(kind, &exe, resume);
    if let Some(p) = prompt {
        args.push(p.into()); // every species takes an opening prompt as its last argument
    }
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
    watch(hub, agent)?;
    Ok(id)
}

/// Start pumping an agent's output and waiting for it to exit.
fn watch(hub: &Shared, agent: Agent) -> io::Result<()> {
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
                    let replies = std::mem::take(&mut g.parser.callbacks_mut().replies);
                    if !replies.is_empty() {
                        let _ = g.writer.write_all(&replies);
                    }
                }
            }
        }
        a.lock().unwrap().exited = Some(Instant::now());
    });
    thread::spawn(move || {
        let mut st = 0;
        unsafe { libc::waitpid(pid, &mut st, 0) };
        agent.lock().unwrap().exited.get_or_insert(Instant::now());
    });
    Ok(())
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
    status: String,
    age: f32,
    turns: u32,
    report: Report,
    session: Option<String>,
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
            status: g.status.name().into(),
            age: (now - g.since).as_secs_f32(),
            turns: g.turns,
            report: g.report.clone(),
            session: g.session.clone(),
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
        a.report = h.report;
        a.session = h.session;
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
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn missions_file() -> PathBuf {
    dirs_state().join("missions.json")
}

/// Hand queued missions to free agents and follow the ones under way.
fn dispatch(hub: &Shared, logged: &mut Vec<Mission>) {
    let agents: Vec<Arc<Mutex<Agent>>> = hub.lock().unwrap().agents.values().cloned().collect();
    // (id, cwd, status, exited)
    let roster: Vec<(u32, String, Status, bool)> = agents
        .iter()
        .map(|a| {
            let g = a.lock().unwrap();
            (g.id, g.cwd.clone(), g.status, g.exited.is_some())
        })
        .collect();
    let mut h = hub.lock().unwrap();
    let busy: HashSet<u32> = h.missions.iter().filter(|m| matches!(m.state.as_str(), "sent" | "active")).filter_map(|m| m.agent).collect();
    let mut taken = HashSet::new();
    for m in h.missions.iter_mut() {
        let me = m.agent.and_then(|id| roster.iter().find(|r| r.0 == id));
        match m.state.as_str() {
            "queued" => {
                let free = roster.iter().find(|(id, cwd, st, dead)| {
                    *cwd == m.cwd && !dead && matches!(st, Status::Idle | Status::Done) && !busy.contains(id) && !taken.contains(id)
                });
                if let Some(&(id, ..)) = free
                    && let Some(a) = agents.iter().find(|a| a.lock().unwrap().id == id)
                {
                    taken.insert(id);
                    m.state = "sent".into();
                    m.agent = Some(id);
                    order(a.clone(), m.text.clone());
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
    if h.missions != *logged {
        let _ = std::fs::create_dir_all(dirs_state());
        let _ = std::fs::write(missions_file(), serde_json::to_string(&h.missions).unwrap_or_default());
        *logged = h.missions.clone();
    }
}

/// Give an agent an order: typed, then submitted separately, so TUIs don't take
/// it as a pasted newline.
fn order(a: Arc<Mutex<Agent>>, text: String) {
    thread::spawn(move || {
        let _ = a.lock().unwrap().writer.write_all(text.as_bytes());
        thread::sleep(Duration::from_millis(150));
        let mut g = a.lock().unwrap();
        g.last_in = Instant::now();
        if g.status == Status::Done {
            g.set(Status::Idle);
        }
        let _ = g.writer.write_all(b"\r");
    });
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
        return spawn(hub, kind, &p, None, None).map_err(|e| e.to_string());
    }
    let exists = git(&top, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok();
    if exists {
        git(&top, &["worktree", "add", &p, branch])?;
    } else {
        git(&top, &["worktree", "add", "-b", branch, &p])?;
    }
    spawn(hub, kind, &p, None, None).map_err(|e| e.to_string())
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
            g.exited.is_none().then(|| (g.id, Saved { kind: g.kind.clone(), cwd: g.cwd.clone(), session: g.session.clone() }))
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
            if let Err(e) = spawn(hub, &a.kind, &a.cwd, session.as_deref(), None) {
                eprintln!("could not bring back {} in {}: {e}", a.kind, a.cwd);
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

fn grok_session(cwd: &str, born: SystemTime, claimed: &HashSet<String>) -> Option<String> {
    let enc: String = cwd
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    let dir = crate::game::home().join(".grok/sessions").join(enc);
    recent_entries(&dir, born)
        .into_iter()
        .filter(|(_, p)| p.is_dir())
        .filter_map(|(_, p)| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .find(|n| !claimed.contains(n))
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
        Req::Mission { cwd, text, kind, launch } => {
            let id = hub.lock().unwrap().missions.iter().map(|m| m.id).max().unwrap_or(0) + 1;
            let mut m = Mission { id, cwd: cwd.clone(), text: text.clone(), state: "queued".into(), agent: None, at: unix_now(), done_at: None, fleet: None };
            if launch {
                match spawn(hub, &kind, &cwd, None, Some(&text)) {
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
                    let g = a.lock().unwrap();
                    // a blocked agent would take the order as an answer to its question
                    if g.exited.is_some() || g.status == Status::Blocked {
                        continue;
                    }
                    g.cwd.clone()
                };
                h.missions.push(Mission { id: next, cwd, text: text.clone(), state: "sent".into(), agent: Some(id), at: unix_now(), done_at: None, fleet: Some(fleet) });
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
        Req::Scrub { id } => {
            hub.lock().unwrap().missions.retain(|m| m.id != id);
            Resp::Ok
        }
        Req::Spawn { kind, cwd } => match spawn(hub, &kind, &cwd, None, None) {
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
                }
                match g.writer.write_all(data.as_bytes()) {
                    Ok(()) => Resp::Ok,
                    Err(e) => Resp::Err { msg: e.to_string() },
                }
            }
            None => missing(id),
        },
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
                Resp::Ok
            }
            None => missing(id),
        },
        Req::Screen { id, scroll } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
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
        Req::Hook { id, event, message } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
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
                discover_sessions(&h);
                save_registry(&h, &mut saved);
            }
            dispatch(&h, &mut logged);
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
    let message = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from));
    let _ = call(&Req::Hook { id, event: event.into(), message });
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
