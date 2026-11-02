//! The game's side of the socket: background threads that keep the agent list
//! fresh, stream the screen of the agent you're talking to, and deliver your
//! commands in order.

use crate::daemon;
use crate::proto::{AgentView, Conn, Mission, Req, Resp, ScreenView};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub enum Msg {
    MissionResult(Result<(), String>),
    Agents(Vec<AgentView>, Vec<Mission>),
    Down(String),
    Screen(u32, ScreenView),
    Spawned { id: u32, kind: String, cwd: String },
    /// An editor session is open and ready to show.
    Editing(u32),
    Error(String),
}

pub enum Cmd {
    Spawn { kind: String, cwd: String },
    /// Open a terminal session in `cwd` that runs `run` (an editor on a file).
    Edit { cwd: String, run: Vec<String> },
    Kill(u32),
    Input(u32, String),
    /// A chat message: pasted, then submitted on its own so it isn't taken as a newline.
    Resize(u32, u16, u16),
    Seen(u32),
    Answer(u32, String),
    Mission { cwd: String, text: String, kind: String, launch: bool, attachments: Vec<crate::screenshots::Attachment>, draft: bool, role: String, to: Option<u32>, campaign: Option<crate::proto::CampaignRef>, spec: crate::proto::ShipSpec },
    Scrub(u32),
    Priority(u32),
}

pub struct Client {
    pub rx: Receiver<Msg>,
    events: Sender<Msg>,
    tx: Sender<Cmd>,
    watch: Arc<Mutex<Option<u32>>>,
    scroll: Arc<AtomicUsize>,
    /// Ask for the watched screen right away.
    kick: Sender<()>,
}

impl Client {
    #[cfg(test)]
    pub fn isolated() -> Self {
        Self::recording().0
    }

    #[cfg(test)]
    pub fn recording() -> (Self, Receiver<Cmd>) {
        let (events, rx) = mpsc::channel();
        let (tx, commands) = mpsc::channel();
        (Self { rx, tx, events, watch: Arc::default(), scroll: Arc::default(), kick: mpsc::channel().0 }, commands)
    }

    pub fn start() -> Self {
        let (mtx, rx) = mpsc::channel();
        let (tx, crx) = mpsc::channel::<Cmd>();
        let watch: Arc<Mutex<Option<u32>>> = Arc::default();
        let scroll: Arc<AtomicUsize> = Arc::default();

        // agent list, ~4x a second; restart the daemon if it disappears
        let m = mtx.clone();
        thread::spawn(move || {
            let mut conn: Option<Conn> = None;
            loop {
                if conn.is_none() {
                    conn = Conn::open().ok().or_else(|| daemon::ensure_running().ok().and_then(|_| Conn::open().ok()));
                }
                let msg = match conn.as_mut().map(|c| c.call(&Req::List)) {
                    Some(Ok(Resp::Agents { agents, missions })) => Msg::Agents(agents, missions),
                    Some(Ok(_)) => Msg::Down("unexpected reply".into()),
                    Some(Err(e)) => {
                        conn = None;
                        Msg::Down(e.to_string())
                    }
                    None => Msg::Down("daemon offline".into()),
                };
                let down = matches!(msg, Msg::Down(_));
                if m.send(msg).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(if down { 2000 } else { 250 }));
            }
        });

        // live screen of the agent in the comms window: the daemon holds each request until
        // the screen changes, so output shows the moment it lands rather than on the next poll
        let (m, w, sc) = (mtx.clone(), watch.clone(), scroll.clone());
        thread::spawn(move || {
            let mut conn: Option<Conn> = None;
            // (agent, scroll, version) of the last screen delivered
            let mut seen: Option<(u32, usize, u64)> = None;
            loop {
                let Some(id) = *w.lock().unwrap() else {
                    seen = None;
                    thread::sleep(Duration::from_millis(20));
                    continue;
                };
                let scroll = sc.load(Relaxed);
                let since = match seen {
                    Some((i, s, v)) if i == id && s == scroll => v,
                    _ => 0,
                };
                if conn.is_none() {
                    conn = Conn::open().ok();
                }
                let Some(c) = conn.as_mut() else {
                    thread::sleep(Duration::from_millis(250));
                    continue;
                };
                let asked = Instant::now();
                match c.call(&Req::Screen { id, scroll, since, wait: 250 }) {
                    Ok(Resp::Screen { screen }) => {
                        let version = screen.version;
                        // you moved on while this one waited: a fresher request follows
                        if *w.lock().unwrap() == Some(id) && sc.load(Relaxed) == scroll {
                            seen = Some((id, scroll, version));
                            if m.send(Msg::Screen(id, screen)).is_err() {
                                return;
                            }
                        }
                        // an older daemon answers at once, so poll it as before; otherwise cap at ~120 updates a second
                        let gap = Duration::from_millis(if version == 0 { 33 } else { 8 });
                        if let Some(rest) = gap.checked_sub(asked.elapsed()) {
                            thread::sleep(rest);
                        }
                    }
                    Ok(_) => thread::sleep(Duration::from_millis(33)),
                    Err(_) => conn = None,
                }
            }
        });

        // a screen at once when you open comms or scroll, without waiting out the stream's long poll
        let (m, w, sc) = (mtx.clone(), watch.clone(), scroll.clone());
        let (kick, kicks) = mpsc::channel::<()>();
        thread::spawn(move || {
            let mut conn: Option<Conn> = None;
            while kicks.recv().is_ok() {
                while kicks.try_recv().is_ok() {}
                let Some(id) = *w.lock().unwrap() else { continue };
                let scroll = sc.load(Relaxed);
                if conn.is_none() {
                    conn = Conn::open().ok();
                }
                let Some(c) = conn.as_mut() else { continue };
                match c.call(&Req::Screen { id, scroll, since: 0, wait: 0 }) {
                    Ok(Resp::Screen { screen }) if *w.lock().unwrap() == Some(id) && sc.load(Relaxed) == scroll => {
                        if m.send(Msg::Screen(id, screen)).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => conn = None,
                }
            }
        });

        // commands, strictly in order (keystrokes must not overtake each other)
        let events = mtx.clone();
        thread::spawn(move || {
            let mut conn: Option<Conn> = None;
            for cmd in crx {
                let draft = matches!(&cmd, Cmd::Mission { draft: true, .. });
                if conn.is_none() {
                    conn = Conn::open().ok();
                }
                let Some(c) = conn.as_mut() else {
                    let message = if draft { Msg::MissionResult(Err("daemon offline".into())) } else { Msg::Error("daemon offline".into()) };
                    let _ = mtx.send(message);
                    continue;
                };
                let editing = matches!(cmd, Cmd::Edit { .. });
                let (req, spawn) = match cmd {
                    Cmd::Spawn { kind, cwd } => (Req::Spawn { kind: kind.clone(), cwd: cwd.clone(), run: Vec::new() }, Some((kind, cwd))),
                    Cmd::Edit { cwd, run } => (Req::Spawn { kind: "shell".into(), cwd, run }, None),
                    Cmd::Kill(id) => (Req::Kill { id }, None),
                    Cmd::Input(id, data) => (Req::Input { id, data }, None),
                    Cmd::Resize(id, cols, rows) => (Req::Resize { id, cols, rows }, None),
                    Cmd::Seen(id) => (Req::Seen { id }, None),
                    Cmd::Answer(id, key) => (Req::Answer { id, key }, None),
                    Cmd::Mission { cwd, text, kind, launch, attachments, role, to, campaign, spec, .. } => (Req::Mission { cwd, text, kind, launch, attachments, role, to, campaign, spec }, None),
                    Cmd::Scrub(id) => (Req::Scrub { id }, None),
                    Cmd::Priority(id) => (Req::Priority { id }, None),
                };
                let reply = c.call(&req);
                if draft {
                    let result = match reply {
                        Ok(Resp::Ok) => Ok(()),
                        Ok(Resp::Err { msg }) => Err(msg),
                        Ok(_) => Err("unexpected mission reply".into()),
                        Err(e) => { conn = None; Err(e.to_string()) },
                    };
                    let _ = mtx.send(Msg::MissionResult(result));
                    continue;
                }
                let msg = match reply {
                    Ok(Resp::Spawned { id }) if editing => Some(Msg::Editing(id)),
                    Ok(Resp::Spawned { id }) => spawn.map(|(kind, cwd)| Msg::Spawned { id, kind, cwd }),
                    Ok(Resp::Err { msg }) => Some(Msg::Error(msg)),
                    Ok(_) => None,
                    Err(e) => {
                        conn = None;
                        Some(Msg::Error(e.to_string()))
                    }
                };
                if let Some(m) = msg {
                    let _ = mtx.send(m);
                }
            }
        });

        Self { rx, tx, events, watch, scroll, kick }
    }


    pub fn open_pr(&self, path: String, number: u32) {
        let events = self.events.clone();
        thread::spawn(move || {
            let result = std::process::Command::new("gh").args(["pr", "view", &number.to_string(), "--web"])
                .current_dir(path).output();
            let error = match result {
                Ok(out) if out.status.success() => return,
                Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
                Err(e) => e.to_string(),
            };
            let _ = events.send(Msg::Error(format!("Could not open PR #{number}: {error}")));
        });
    }

    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    /// Start (or stop, with `None`) streaming an agent's screen.
    pub fn watch(&self, id: Option<u32>) {
        *self.watch.lock().unwrap() = id;
        self.scroll.store(0, Relaxed);
        let _ = self.kick.send(());
    }

    /// Look this many lines back into the watched agent's history.
    pub fn scroll(&self, lines: usize) {
        if self.scroll.swap(lines, Relaxed) != lines {
            let _ = self.kick.send(());
        }
    }
}
