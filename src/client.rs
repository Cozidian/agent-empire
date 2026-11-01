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
    MissionResult(Result<(), String>),
    ChatSent(u32, Result<(), String>),
    Agents(Vec<AgentView>, Vec<Mission>),
    Down(String),
    Screen(u32, ScreenView),
    Spawned { id: u32, kind: String, cwd: String },
    Error(String),
    /// A docking attempt: `Err` holds why it failed.
    Docked { moon: String, result: Result<(), String> },
    /// Jev's suggestion for the mission being written; `key` says which text it was for.
    Jev { key: String, result: Result<crate::jev::Pick, String> },
}

pub enum Cmd {
    Spawn { kind: String, cwd: String },
    Kill(u32),
    Input(u32, String),
    /// A chat message: pasted, then submitted on its own so it isn't taken as a newline.
    Say(u32, String),
    Resize(u32, u16, u16),
    Seen(u32),
    Answer(u32, String),
    Mission { cwd: String, text: String, kind: String, launch: bool, attachments: Vec<crate::screenshots::Attachment>, draft: bool, role: String, to: Option<u32>, campaign: Option<crate::proto::CampaignRef>, spec: crate::proto::ShipSpec },
    Scrub(u32),
    Broadcast(Vec<u32>, String),
    Moon { cwd: String, branch: String, kind: String },
    Dock(String),
    Priority(u32),
}

pub struct Client {
    pub rx: Receiver<Msg>,
    events: Sender<Msg>,
    tx: Sender<Cmd>,
    watch: Arc<Mutex<Option<u32>>>,
    scroll: Arc<AtomicUsize>,
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
        (Self { rx, tx, events, watch: Arc::default(), scroll: Arc::default() }, commands)
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
        let events = mtx.clone();
        thread::spawn(move || {
            let mut conn: Option<Conn> = None;
            for cmd in crx {
                let draft = matches!(&cmd, Cmd::Mission { draft: true, .. });
                if conn.is_none() {
                    conn = Conn::open().ok();
                }
                let Some(c) = conn.as_mut() else {
                    let message = if let Cmd::Say(id, _) = &cmd { Msg::ChatSent(*id, Err("daemon offline".into())) }
                        else if draft { Msg::MissionResult(Err("daemon offline".into())) } else { Msg::Error("daemon offline".into()) };
                    let _ = mtx.send(message);
                    continue;
                };
                if let Cmd::Say(id, text) = cmd {
                    let result = send_chat(&mut |request| c.call(request), id, &text);
                    if result.is_err() { conn = None; }
                    let _ = mtx.send(Msg::ChatSent(id, result.map_err(|e| e.to_string())));
                    continue;
                }
                let dock = match &cmd {
                    Cmd::Dock(m) => Some(m.clone()),
                    _ => None,
                };
                let (req, spawn) = match cmd {
                    Cmd::Spawn { kind, cwd } => (Req::Spawn { kind: kind.clone(), cwd: cwd.clone() }, Some((kind, cwd))),
                    Cmd::Kill(id) => (Req::Kill { id }, None),
                    Cmd::Input(id, data) => (Req::Input { id, data }, None),
                    Cmd::Say(..) => unreachable!(),
                    Cmd::Resize(id, cols, rows) => (Req::Resize { id, cols, rows }, None),
                    Cmd::Seen(id) => (Req::Seen { id }, None),
                    Cmd::Answer(id, key) => (Req::Answer { id, key }, None),
                    Cmd::Mission { cwd, text, kind, launch, attachments, role, to, campaign, spec, .. } => (Req::Mission { cwd, text, kind, launch, attachments, role, to, campaign, spec }, None),
                    Cmd::Scrub(id) => (Req::Scrub { id }, None),
                    Cmd::Broadcast(ids, text) => (Req::Broadcast { ids, text }, None),
                    Cmd::Moon { cwd, branch, kind } => (Req::Moon { cwd, branch: branch.clone(), kind: kind.clone() }, Some((kind, format!("moon {branch}")))),
                    Cmd::Dock(cwd) => (Req::Dock { cwd }, None),
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
                    Ok(Resp::Spawned { id }) => spawn.map(|(kind, cwd)| Msg::Spawned { id, kind, cwd }),
                    Ok(Resp::Err { msg }) => Some(match dock {
                        Some(moon) => Msg::Docked { moon, result: Err(msg) },
                        None => Msg::Error(msg),
                    }),
                    Ok(_) => dock.map(|moon| Msg::Docked { moon, result: Ok(()) }),
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

        Self { rx, tx, events, watch, scroll }
    }

    /// Ask Jev which ship should take a mission, in the background.
    pub fn ask_jev(&self, key: String, mission: String, world: String, ships: Vec<crate::jev::Ship>) {
        let events = self.events.clone();
        thread::spawn(move || {
            let result = crate::jev::ask(&mission, &world, &ships);
            let _ = events.send(Msg::Jev { key, result });
        });
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
    }

    /// Look this many lines back into the watched agent's history.
    pub fn scroll(&self, lines: usize) {
        self.scroll.store(lines, Relaxed);
    }
}

fn send_chat(call: &mut impl FnMut(&Req) -> std::io::Result<Resp>, id: u32, text: &str) -> std::io::Result<()> {
    let expect_ok = |reply| match reply {
        Resp::Ok => Ok(()),
        Resp::Err { msg } => Err(std::io::Error::other(msg)),
        _ => Err(std::io::Error::other("unexpected chat reply")),
    };
    expect_ok(call(&Req::Input { id, data: format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")) })?)?;
    thread::sleep(Duration::from_millis(150));
    expect_ok(call(&Req::Input { id, data: "\r".into() })?)
}

#[cfg(test)]
mod chat_delivery_tests {
    use super::*;

    #[test]
    fn multiline_reply_is_one_bracketed_paste_then_enter() {
        let mut inputs = Vec::new();
        send_chat(&mut |r| {
            if let Req::Input { id, data } = r { inputs.push((*id, data.clone())); }
            Ok(Resp::Ok)
        }, 7, "first\nsecond").unwrap();
        assert_eq!(inputs, [(7, "\x1b[200~first\nsecond\x1b[201~".into()), (7, "\r".into())]);
    }

    #[test]
    fn failed_paste_is_reported_and_never_followed_by_enter() {
        let mut count = 0;
        let result = send_chat(&mut |_| { count += 1; Err(std::io::Error::other("offline")) }, 7, "reply");
        assert!(result.is_err());
        assert_eq!(count, 1);
    }
}
