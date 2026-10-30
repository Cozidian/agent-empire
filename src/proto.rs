//! Wire protocol between the game and the daemon: one JSON object per line.

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Req {
    Ping,
    List,
    /// The agent list, for onlookers (status bars) that shouldn't count as a galaxy being open.
    Peek,
    Spawn { kind: String, cwd: String },
    Kill { id: u32 },
    Input { id: u32, data: String },
    Resize { id: u32, cols: u16, rows: u16 },
    /// `scroll`: how many lines back into the scrollback to look (0 = live).
    Screen {
        id: u32,
        #[serde(default)]
        scroll: usize,
    },
    /// The player looked at this agent: a finished turn is no longer news.
    Seen { id: u32 },
    /// Reported by agent hooks (e.g. Claude Code's) from inside the agent.
    Hook { id: u32, event: String, message: Option<String> },
    /// Pick an option of the question an agent is asking (`key` as listed in its `Ask`).
    Answer { id: u32, key: String },
    /// What an agent tells us about itself (Claude Code, through its statusline).
    Report { id: u32, report: Report },
    Shutdown,
    /// A new mission for the world at `cwd`: launch a ship with it right away, or queue it
    /// for the next agent there that's free.
    Mission { cwd: String, text: String, kind: String, launch: bool },
    /// One order to many agents at once, followed as one fleet order.
    Broadcast { ids: Vec<u32>, text: String },
    /// Found a moon: a git worktree of the world at `cwd` on `branch`, with an agent on it.
    Moon { cwd: String, branch: String, kind: String },
    /// Merge a moon's branch back into its world and retire it.
    Dock { cwd: String },
    /// Strike a mission from the log.
    Scrub { id: u32 },
    /// Exec the binary on disk in place of the running daemon, keeping every agent.
    Upgrade,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentView {
    pub id: u32,
    pub kind: String,
    pub cwd: String,
    pub title: String,
    /// booting | working | blocked | done | idle | exited
    pub status: String,
    /// Seconds since the status last changed.
    pub age: f32,
    /// Turns finished since launch.
    #[serde(default)]
    pub turns: u32,
    #[serde(default)]
    pub report: Report,
    /// The question it's waiting on, when blocked.
    #[serde(default)]
    pub ask: Option<Ask>,
}

/// A task written on a world, carried out by one of its agents.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Mission {
    pub id: u32,
    pub cwd: String,
    pub text: String,
    /// queued | sent | active | done
    pub state: String,
    pub agent: Option<u32>,
    /// Unix time it was written, and finished.
    pub at: u64,
    pub done_at: Option<u64>,
    /// Missions given together as one fleet order share this.
    #[serde(default)]
    pub fleet: Option<u32>,
}

/// A question read off an agent's screen, answerable from the galaxy.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Ask {
    pub question: String,
    /// What it's about (the command, the file, …), a few lines above the question.
    pub context: Vec<String>,
    pub options: Vec<Choice>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Choice {
    /// What to type to pick it.
    pub key: String,
    pub label: String,
}

/// Self-reported agent vitals; anything unknown stays `None`.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub model: Option<String>,
    /// Context window used, percent.
    pub ctx: Option<f32>,
    /// Spend this session, USD.
    pub cost: Option<f32>,
    pub added: Option<u32>,
    pub removed: Option<u32>,
    #[serde(default)]
    pub session: Option<String>,
}

/// A run of terminal cells sharing colours. `t` has one char per cell; wide
/// glyphs are followed by a `\0` for the cell they cover.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Run {
    pub t: String,
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
    #[serde(default)]
    pub b: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ScreenView {
    pub cols: u16,
    pub rows: u16,
    pub cursor: (u16, u16),
    pub hide_cursor: bool,
    pub lines: Vec<Vec<Run>>,
    /// Lines scrolled back (clamped), and how far back the history goes.
    #[serde(default)]
    pub scroll: usize,
    #[serde(default)]
    pub history: usize,
    /// The agent wants mouse events (it scrolls itself).
    #[serde(default)]
    pub mouse: bool,
    /// SGR mouse encoding (`ESC[<b;x;yM`) rather than the legacy one.
    #[serde(default)]
    pub sgr: bool,
    /// Full-screen app on the alternate screen (no scrollback of its own).
    #[serde(default)]
    pub alt: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Resp {
    Pong { version: String },
    Agents {
        agents: Vec<AgentView>,
        #[serde(default)]
        missions: Vec<Mission>,
    },
    Spawned { id: u32 },
    Screen { screen: ScreenView },
    Ok,
    Err { msg: String },
}

pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("EMPIRE_SOCK") {
        return p.into();
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join(format!("empire-{}.sock", unsafe { libc::getuid() }))
}

/// A persistent connection to the daemon.
pub struct Conn {
    rd: BufReader<UnixStream>,
    wr: UnixStream,
}

impl Conn {
    pub fn open() -> io::Result<Self> {
        let s = UnixStream::connect(socket_path())?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self { rd: BufReader::new(s.try_clone()?), wr: s })
    }

    pub fn call(&mut self, req: &Req) -> io::Result<Resp> {
        let mut line = serde_json::to_string(req).map_err(io::Error::other)?;
        line.push('\n');
        self.wr.write_all(line.as_bytes())?;
        let mut buf = String::new();
        if self.rd.read_line(&mut buf)? == 0 {
            return Err(io::Error::other("daemon hung up"));
        }
        match serde_json::from_str(&buf).map_err(io::Error::other)? {
            Resp::Err { msg } => Err(io::Error::other(msg)),
            r => Ok(r),
        }
    }
}

/// One-shot request on a fresh connection.
pub fn call(req: &Req) -> io::Result<Resp> {
    Conn::open()?.call(req)
}
