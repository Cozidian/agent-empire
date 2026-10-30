//! The empire daemon: owns every agent's pseudo-terminal and screen state, works
//! out who is busy, and serves the game over a unix socket. Agents live as long
//! as the daemon does, so the game UI can come and go.

use crate::proto::*;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::num::NonZeroU16;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const ROWS: u16 = 40;
const COLS: u16 = 120;
/// Ignore status heuristics while an agent paints its startup screen.
const BOOT: f32 = 4.0;
/// Output this soon after the player typed is just echo, not work.
const ECHO: f32 = 0.4;
/// Lines of history kept per agent for scrolling back in comms.
const HISTORY: usize = 5000;

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

struct Agent {
    id: u32,
    kind: String,
    cwd: String,
    parser: vt100::Parser<Cb>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
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
}

impl Agent {
    fn set(&mut self, s: Status) {
        if self.status != s {
            if s == Status::Done {
                self.turns += 1;
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
        let line = text.lines().rev().filter(|l| !l.trim().is_empty()).take(8).find(|l| ASK.iter().any(|k| l.contains(k)))?;
        let line = line.trim().to_string();
        let count = text.lines().filter(|l| l.trim() == line).count();
        Some((line, count))
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

fn command(kind: &str, exe: &str) -> (String, Vec<String>) {
    match kind {
        "claude" => ("claude".into(), vec!["--settings".into(), claude_hooks(exe)]),
        // inline mode keeps its output in the scrollback, so comms can scroll it
        "codex" => ("codex".into(), vec!["--no-alt-screen".into()]),
        "cursor" => ("cursor-agent".into(), vec![]),
        "shell" => (std::env::var("SHELL").unwrap_or_else(|_| "bash".into()), vec![]),
        k => (k.into(), vec![]),
    }
}

fn spawn(hub: &Shared, kind: &str, cwd: &str) -> anyhow::Result<u32> {
    let id = {
        let mut h = hub.lock().unwrap();
        h.next += 1;
        h.next
    };
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let pty = native_pty_system().openpty(PtySize { rows: ROWS, cols: COLS, pixel_width: 0, pixel_height: 0 })?;
    let (prog, args) = command(kind, &exe);
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
    let mut child = pty.slave.spawn_command(cmd)?;
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader()?;
    let now = Instant::now();
    let agent = Arc::new(Mutex::new(Agent {
        id,
        kind: kind.into(),
        cwd: cwd.into(),
        parser: vt100::Parser::new_with_callbacks(NonZeroU16::new(ROWS).unwrap(), NonZeroU16::new(COLS).unwrap(), HISTORY, Cb::default()),
        writer: pty.master.take_writer()?,
        killer: child.clone_killer(),
        master: pty.master,
        started: now,
        last_out: now,
        last_in: now,
        hook: None,
        answered: None,
        turns: 0,
        report: Report::default(),
        status: Status::Booting,
        since: now,
        exited: None,
    }));
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
        let _ = child.wait();
        agent.lock().unwrap().exited.get_or_insert(Instant::now());
    });
    Ok(id)
}

fn handle(hub: &Shared, req: Req) -> Resp {
    let get = |id: u32| hub.lock().unwrap().agents.get(&id).cloned();
    let missing = |id: u32| Resp::Err { msg: format!("no agent #{id}") };
    match req {
        Req::Ping => Resp::Pong { version: env!("CARGO_PKG_VERSION").into() },
        Req::List => {
            let now = Instant::now();
            let list: Vec<Arc<Mutex<Agent>>> = hub.lock().unwrap().agents.values().cloned().collect();
            let mut agents: Vec<AgentView> = list.iter().map(|a| a.lock().unwrap().view(now)).collect();
            agents.sort_by_key(|a| a.id);
            Resp::Agents { agents }
        }
        Req::Spawn { kind, cwd } => match spawn(hub, &kind, &cwd) {
            Ok(id) => Resp::Spawned { id },
            Err(e) => Resp::Err { msg: format!("could not launch {kind}: {e}") },
        },
        Req::Kill { id } => match get(id) {
            Some(a) => {
                let _ = a.lock().unwrap().killer.kill();
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
        Req::Resize { id, cols, rows } => match get(id) {
            Some(a) => {
                let mut g = a.lock().unwrap();
                let (cols, rows) = (cols.clamp(20, 400), rows.clamp(5, 200));
                let _ = g.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
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
                a.lock().unwrap().report = report;
                Resp::Ok
            }
            None => missing(id),
        },
        Req::Shutdown => {
            for a in hub.lock().unwrap().agents.values() {
                let _ = a.lock().unwrap().killer.kill();
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

    // status bookkeeping, and reaping agents a while after they exit
    let h = hub.clone();
    thread::spawn(move || {
        loop {
            let now = Instant::now();
            let list: Vec<(u32, Arc<Mutex<Agent>>)> = h.lock().unwrap().agents.iter().map(|(k, v)| (*k, v.clone())).collect();
            let mut dead = Vec::new();
            for (id, a) in list {
                let mut g = a.lock().unwrap();
                g.update(now);
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
