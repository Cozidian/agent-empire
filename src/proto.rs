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
    CampaignReport { token: String, report: serde_json::Value },
    List,
    /// The agent list, for onlookers (status bars) that shouldn't count as a galaxy being open.
    Peek,
    /// `run`: for a shell session, a command to run in place of the shell (an editor on a file).
    Spawn {
        kind: String,
        cwd: String,
        #[serde(default)]
        run: Vec<String>,
    },
    Kill { id: u32 },
    Input { id: u32, data: String },
    Resize { id: u32, cols: u16, rows: u16 },
    /// `scroll`: how many lines back into the scrollback to look (0 = live).
    Screen {
        id: u32,
        #[serde(default)]
        scroll: usize,
        /// Hold the reply until the screen has moved past this generation, for at most
        /// `wait` milliseconds (0 = reply at once): changes stream without polling.
        #[serde(default)]
        since: u64,
        #[serde(default)]
        wait: u64,
    },
    /// The player looked at this agent: a finished turn is no longer news.
    Seen { id: u32 },
    /// Reported by agent hooks (Claude Code, Grok) from inside the agent.
    Hook {
        id: u32,
        event: String,
        message: Option<String>,
        /// The file an edit tool is about to change.
        #[serde(default)]
        file: Option<String>,
        /// The agent's session id, when the hook payload carries one.
        #[serde(default)]
        session: Option<String>,
        /// Native working directory, when the harness reports one.
        #[serde(default)]
        cwd: Option<String>,
    },
    /// Settle a border conflict: `id` keeps the contested files, its rivals are told to leave them.
    Priority { id: u32 },
    /// Pick an option of the question an agent is asking (`key` as listed in its `Ask`).
    Answer { id: u32, key: String },
    /// What an agent tells us about itself (Claude through its statusline, Grok from its session files).
    Report { id: u32, report: Report },
    Shutdown,
    /// A new mission for the world at `cwd`: launch a ship with it right away, or queue it
    /// for the next agent there that's free.
    Mission {
        cwd: String, text: String, kind: String, launch: bool,
        #[serde(default)]
        attachments: Vec<crate::screenshots::Attachment>,
        /// A newly launched ship's role (see `role::Role`), "" for none.
        #[serde(default)]
        role: String,
        /// Queue it for this ship only (it takes it when it's next free).
        #[serde(default)]
        to: Option<u32>,
        #[serde(default)]
        campaign: Option<CampaignRef>,
        /// A newly launched ship's own orders and skills, beyond its role.
        #[serde(default)]
        spec: ShipSpec,
    },
    /// One order to many agents at once, followed as one fleet order.
    Broadcast { ids: Vec<u32>, text: String },
    /// Found a station: a git worktree of the world at `cwd` on `branch`, with an agent on it.
    Moon { cwd: String, branch: String, kind: String },
    /// Merge a station's branch back into its world and retire it.
    Dock { cwd: String },
    /// Strike a mission from the log.
    Scrub { id: u32 },
    /// Exec the binary on disk in place of the running daemon, keeping every agent.
    Upgrade,
    /// Refit a deployed ship with a new role and outfit. Without `confirm` it's only a check
    /// (can this ship be refitted?); with it the refit is queued and applied when the ship is
    /// next free, by resuming its own session under the new launch flags.
    Refit {
        id: u32,
        #[serde(default)]
        role: String,
        #[serde(default)]
        spec: ShipSpec,
        #[serde(default)]
        confirm: bool,
    },
    /// Call off a pending refit, or dismiss a finished one's notice.
    CancelRefit { id: u32 },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentView {
    pub id: u32,
    pub kind: String,
    pub cwd: String,
    pub title: String,
    /// Recent terminal output for a handoff; older daemons omit it.
    #[serde(default)]
    pub output: String,
    /// The last prompt it was given (typed in comms, a mission, a fleet order).
    #[serde(default)]
    pub task: String,
    /// What it's for (see `role::Role`), "" for none.
    #[serde(default)]
    pub role: String,
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
    /// Border conflicts: other agents on this world editing the same files.
    #[serde(default)]
    pub war: Vec<War>,
    /// The design it was launched from, by name ("" for none).
    #[serde(default)]
    pub design: String,
    /// Its own orders and skills as it's running now (beyond its role).
    #[serde(default)]
    pub spec: ShipSpec,
    /// A refit ordered for it, and how it went; `role`/`design`/`spec` stay what's actually aboard.
    #[serde(default)]
    pub refit: Option<RefitState>,
    /// The command a shell session runs instead of the shell (an editor on a file); empty for none.
    #[serde(default)]
    pub run: Vec<String>,
}

/// A refit ordered for a deployed ship: the outfit it's to get, and where that stands.
/// `state` is pending (waiting for the ship to be free) | applying (relaunching on its session)
/// | active (aboard) | failed (old outfit restored) | unsupported (its species can't resume).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct RefitState {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub spec: ShipSpec,
    pub state: String,
    /// Why it failed or isn't supported, or what happened.
    #[serde(default)]
    pub note: String,
}

/// A ship's outfit beyond its role: standing orders and skills to lean on, and the name of
/// the design it came from. The ship keeps this copy for life, whatever becomes of the design.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ShipSpec {
    #[serde(default)]
    pub orders: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub design: String,
}

impl ShipSpec {
    /// Nothing of its own to tell the ship.
    pub fn is_bare(&self) -> bool { self.orders.trim().is_empty() && self.skills.is_empty() }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct War {
    pub rival: u32,
    /// Contested files, relative to the world.
    pub files: Vec<String>,
}

/// A mission's place in a campaign; the harness still owns its execution.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CampaignRef { pub campaign: u64, pub stage: u64 }

/// A task written on a world, carried out by one of its agents.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Mission {
    pub id: u32,
    pub cwd: String,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<crate::screenshots::Attachment>,
    /// queued | sent | active | done
    pub state: String,
    pub agent: Option<u32>,
    /// Unix time it was written, and finished.
    pub at: u64,
    pub done_at: Option<u64>,
    /// Missions given together as one fleet order share this.
    #[serde(default)]
    pub fleet: Option<u32>,
    #[serde(default)]
    pub campaign: Option<CampaignRef>,
    #[serde(default)] pub campaign_job: String,
}

/// A question read off an agent's screen, answerable from the galaxy.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Ask {
    pub question: String,
    /// What it's about (the command, the file, …), a few lines above the question.
    pub context: Vec<String>,
    pub options: Vec<Choice>,
    /// What's being asked for, between the question and its choices (Codex's `$ command`, a reason).
    #[serde(default)]
    pub detail: Vec<String>,
    /// Advisor's risk read on this approval. Advice for the player to see; nothing acts on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskView>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum RiskLevel {
    Calm,
    Care,
    NeedsYou,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RiskView {
    /// Advisor's risk label (`edit_in_project`, …), or `needs_you` when the denylist forced it.
    pub label: String,
    pub probability: f32,
    pub level: RiskLevel,
    /// Advisor's probability that it touches something outside the project or can't be undone.
    #[serde(default)]
    pub outside: Option<f32>,
    /// Why, when the denylist decided.
    #[serde(default)]
    pub reason: String,
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
    /// Bumped whenever the screen changes; 0 from a daemon that can't wait for changes.
    #[serde(default)]
    pub version: u64,
    /// Its program has ended (an editor that's been quit).
    #[serde(default)]
    pub exited: bool,
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

#[cfg(test)]
mod attachment_tests {
    use super::*;

    #[test]
    fn old_requests_and_saved_missions_remain_readable() {
        let request: Req = serde_json::from_str(r#"{"op":"mission","cwd":"/world","text":"fix it","kind":"codex","launch":false}"#).unwrap();
        assert!(matches!(request, Req::Mission { attachments, campaign: None, spec, .. } if attachments.is_empty() && spec == ShipSpec::default()));
        let mission: Mission = serde_json::from_str(r#"{"id":1,"cwd":"/world","text":"fix it","state":"queued","agent":null,"at":0,"done_at":null}"#).unwrap();
        assert!(mission.attachments.is_empty());
        assert!(mission.campaign.is_none());
    }

    #[test]
    fn attachment_references_survive_mission_persistence() {
        let mission = Mission {
            text: "Fix the screenshot".into(),
            campaign: Some(CampaignRef { campaign: 4, stage: 2 }),
            attachments: vec![crate::screenshots::Attachment { name: "screenshot.png".into(), path: "/saved/123.png".into() }],
            ..Mission::default()
        };
        let restored: Mission = serde_json::from_str(&serde_json::to_string(&mission).unwrap()).unwrap();
        assert_eq!(restored, mission);
        assert!(crate::screenshots::prompt(&restored.text, &restored.attachments).contains("/saved/123.png"));
    }
}

#[cfg(test)]
mod refit_tests {
    use super::*;

    #[test]
    fn agent_views_without_a_refit_still_parse() {
        let old: AgentView = serde_json::from_str(r#"{"id":1,"kind":"claude","cwd":"/w","title":"","status":"idle","age":0.5}"#).unwrap();
        assert!(old.refit.is_none());
        let mut v = old.clone();
        v.refit = Some(RefitState { role: "scout".into(), spec: ShipSpec { orders: "Look only.".into(), skills: vec!["audit".into()], design: "Probe".into() }, state: "pending".into(), note: String::new() });
        let back: AgentView = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(back.refit, v.refit);
    }

    #[test]
    fn refit_requests_default_to_a_dry_run() {
        let r: Req = serde_json::from_str(r#"{"op":"refit","id":3,"role":"builder"}"#).unwrap();
        assert!(matches!(r, Req::Refit { id: 3, confirm: false, ref role, ref spec } if role == "builder" && *spec == ShipSpec::default()));
        let r: Req = serde_json::from_str(r#"{"op":"cancel_refit","id":3}"#).unwrap();
        assert!(matches!(r, Req::CancelRefit { id: 3 }));
    }
}

#[cfg(test)]
mod screen_tests {
    use super::*;

    #[test]
    fn screen_streaming_talks_to_older_builds_both_ways() {
        // an older game asks without a version, and must get an answer at once
        let req: Req = serde_json::from_str(r#"{"op":"screen","id":3,"scroll":2}"#).unwrap();
        assert!(matches!(req, Req::Screen { id: 3, scroll: 2, since: 0, wait: 0 }));
        // an older daemon's screen has no version, which tells the game to poll it as before
        let old = r#"{"type":"screen","screen":{"cols":2,"rows":1,"cursor":[0,0],"hide_cursor":false,"lines":[]}}"#;
        assert!(matches!(serde_json::from_str::<Resp>(old).unwrap(), Resp::Screen { screen } if screen.version == 0));
    }
}
