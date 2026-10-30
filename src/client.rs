//! The game's side of the socket: background threads that keep the agent list
//! fresh, stream the screen of the agent you're talking to, and deliver your
//! commands in order.

use crate::daemon;
use crate::proto::{AgentView, Conn, Mission, Req, Resp, ScreenView};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub enum Msg {
    Agents(Vec<AgentView>, Vec<Mission>),
    Down(String),
    Screen(u32, ScreenView),
    Spawned { id: u32, kind: String, cwd: String },
    Error(String),
}

pub enum Cmd {
    Spawn { kind: String, cwd: String },
    Kill(u32),
    Input(u32, String),
    Resize(u32, u16, u16),
    Seen(u32),
    Answer(u32, String),
    Mission { cwd: String, text: String, kind: String, launch: bool },
    Scrub(u32),
    Broadcast(Vec<u32>, String),
}

pub struct Client {
    pub rx: Receiver<Msg>,
    tx: Sender<Cmd>,
    watch: Arc<Mutex<Option<u32>>>,
    scroll: Arc<AtomicUsize>,
}

impl Client {
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

        // live screen of the agent in the comms window
        let (m, w, sc) = (mtx.clone(), watch.clone(), scroll.clone());
        thread::spawn(move || {
            let mut conn: Option<Conn> = None;
            loop {
                let Some(id) = *w.lock().unwrap() else {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                };
                if conn.is_none() {
                    conn = Conn::open().ok();
                }
                if let Some(c) = conn.as_mut() {
                    match c.call(&Req::Screen { id, scroll: sc.load(Relaxed) }) {
                        Ok(Resp::Screen { screen }) => {
                            if m.send(Msg::Screen(id, screen)).is_err() {
                                return;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => conn = None,
                    }
                }
                thread::sleep(Duration::from_millis(33));
            }
        });

        // commands, strictly in order (keystrokes must not overtake each other)
        thread::spawn(move || {
            let mut conn: Option<Conn> = None;
            for cmd in crx {
                if conn.is_none() {
                    conn = Conn::open().ok();
                }
                let Some(c) = conn.as_mut() else {
                    let _ = mtx.send(Msg::Error("daemon offline".into()));
                    continue;
                };
                let (req, spawn) = match cmd {
                    Cmd::Spawn { kind, cwd } => (Req::Spawn { kind: kind.clone(), cwd: cwd.clone() }, Some((kind, cwd))),
                    Cmd::Kill(id) => (Req::Kill { id }, None),
                    Cmd::Input(id, data) => (Req::Input { id, data }, None),
                    Cmd::Resize(id, cols, rows) => (Req::Resize { id, cols, rows }, None),
                    Cmd::Seen(id) => (Req::Seen { id }, None),
                    Cmd::Answer(id, key) => (Req::Answer { id, key }, None),
                    Cmd::Mission { cwd, text, kind, launch } => (Req::Mission { cwd, text, kind, launch }, None),
                    Cmd::Scrub(id) => (Req::Scrub { id }, None),
                    Cmd::Broadcast(ids, text) => (Req::Broadcast { ids, text }, None),
                };
                let msg = match c.call(&req) {
                    Ok(Resp::Spawned { id }) => spawn.map(|(kind, cwd)| Msg::Spawned { id, kind, cwd }),
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

        Self { rx, tx, watch, scroll }
    }

    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    /// Start (or stop, with `None`) streaming an agent's screen.
    pub fn watch(&self, id: Option<u32>) {
        *self.watch.lock().unwrap() = id;
        self.scroll.store(0, Relaxed);
    }

    /// Look this many lines back into the watched agent's history.
    pub fn scroll(&self, lines: usize) {
        self.scroll.store(lines, Relaxed);
    }
}
