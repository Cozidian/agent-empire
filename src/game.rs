//! The game: galaxy model, camera, input and the main loop.
//!
//! Folders are planets, their parent folders are star systems, and agents are
//! ships in orbit around the folder they work in.

use crate::client::{Client, Cmd, Msg};
use crate::draw;
use crate::gfx::*;
use crate::role::{ROLES, Role};
use crate::structure::{Built, KINDS, Kind};
use crate::proto::{AgentView, Ask, Mission, RefitState, Report, ScreenView, ShipSpec, War};
use crate::survey;
use crate::sound::{Cue, Sound};
use crate::screenshots::{self, Attachment};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode,
    KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::{cursor, execute, style, terminal};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::f32::consts::TAU;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

/// Re-ping a blocked agent this often (seconds).
/// Re-announce a still-blocked agent this often.
const NAG: f32 = 120.0;
/// A turn shorter than this finishing is not worth a sound.
const SHORT_TURN: f32 = 6.0;
const FPS: u64 = 30;
const DIM_LOG: V3 = v3(0.5, 0.55, 0.65);
/// How many situation log entries are kept.
const LOG_KEEP: usize = 500;

/// What a situation log entry is about, for filtering.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ev {
    Alert,
    Mission,
    Agent,
    Trade,
    Prestige,
    World,
    Error,
}

impl Ev {
    pub const ALL: [Ev; 7] = [Ev::Alert, Ev::Mission, Ev::Agent, Ev::Trade, Ev::Prestige, Ev::World, Ev::Error];

    pub fn label(self) -> &'static str {
        match self {
            Ev::Alert => "alerts",
            Ev::Mission => "missions",
            Ev::Agent => "agents",
            Ev::Trade => "trade",
            Ev::Prestige => "prestige",
            Ev::World => "worlds",
            Ev::Error => "errors",
        }
    }
}

/// One situation log entry, with what it's about.
pub struct LogEntry {
    pub id: u64,
    pub at: i64,
    pub clock: String,
    pub msg: String,
    pub col: V3,
    pub kind: Ev,
    pub world: Option<String>,
    pub agent: Option<u32>,
}

impl LogEntry {
    fn on(&mut self, world: &str) -> &mut Self {
        self.world = Some(world.to_string());
        self
    }

    fn by(&mut self, agent: impl Into<Option<u32>>) -> &mut Self {
        self.agent = agent.into();
        self
    }
}

/// The situation log view's filter: one kind of entry, one world (and its moons), some text.
#[derive(Default)]
pub struct SitFilter {
    pub kind: Option<Ev>,
    pub world: Option<String>,
    pub query: String,
}
/// Default names for new moons (they become branch names).
const MOONS: [&str; 12] = ["luna", "phobos", "deimos", "io", "europa", "ganymede", "callisto", "titan", "rhea", "triton", "charon", "oberon"];
/// Radius of a system's first planetary orbit, and the gap between orbits.
const ORBIT0: f32 = 70.0;
const ORBIT_STEP: f32 = 42.0;

// ─────────────────────────────── model ─────────────────────────────────

/// Declaration order is attention order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Status {
    Blocked,
    Done,
    Working,
    Booting,
    Idle,
    Exited,
}

impl Status {
    fn parse(s: &str) -> Self {
        match s {
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            "working" => Self::Working,
            "booting" => Self::Booting,
            "exited" => Self::Exited,
            _ => Self::Idle,
        }
    }
    pub fn color(self) -> V3 {
        match self {
            Self::Blocked => v3(1.0, 0.25, 0.15),
            Self::Done => v3(0.3, 1.0, 0.5),
            Self::Working => v3(0.2, 0.85, 1.0),
            Self::Booting => v3(1.0, 0.75, 0.3),
            Self::Idle => v3(0.42, 0.5, 0.68),
            Self::Exited => v3(0.35, 0.35, 0.38),
        }
    }
    pub fn glyph(self, t: f32) -> char {
        match self {
            Self::Blocked => '▲',
            Self::Done => '✔',
            Self::Working => ['◐', '◓', '◑', '◒'][(t * 6.0) as usize % 4],
            Self::Booting => '✦',
            Self::Idle => '○',
            Self::Exited => '✝',
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Blocked => "AWAITING ORDERS",
            Self::Done => "MISSION COMPLETE",
            Self::Working => "WORKING",
            Self::Booting => "LAUNCHING",
            Self::Idle => "STANDING BY",
            Self::Exited => "LOST",
        }
    }
    pub fn needs_you(self) -> bool {
        matches!(self, Self::Blocked | Self::Done)
    }
}

pub fn kind_color(k: &str) -> V3 {
    match k {
        "claude" => v3(1.0, 0.58, 0.32),
        "codex" => v3(0.78, 0.88, 1.0),
        "grok" => v3(0.82, 0.5, 1.0),
        "opencode" => v3(0.45, 0.95, 0.6),
        "gemini" => v3(0.45, 0.65, 1.0),
        "shell" => v3(0.6, 0.62, 0.66),
        _ => hsv(rnd(k.bytes().fold(7u32, |h, b| h.wrapping_mul(31) ^ b as u32)), 0.55, 1.0),
    }
}

pub struct Agent {
    pub id: u32,
    pub kind: String,
    pub cwd: String,
    pub title: String,
    pub status: Status,
    pub since: f32,
    pub born: f32,
    pub dying: Option<f32>,
    pub seed: f32,
    pub last_ping: f32,
    /// When it last started working.
    pub busy_at: f32,
    pub turns: u32,
    pub report: Report,
    pub ask: Option<Ask>,
    pub war: Vec<War>,
    pub role: Role,
    /// The design it was launched from, by name ("" for none).
    pub design: String,
    /// Its own orders and skills as it's running now.
    pub spec: ShipSpec,
    /// A refit ordered for it (pending until the daemon relaunches it), and how it went.
    pub refit: Option<RefitState>,
    pub wpos: (f32, f32),
    /// In front of its planet (drawn after it).
    pub front: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Sel {
    GitHub,
    Sys(String),
    Sector(u64, String),
    Lane(u64),
    Planet(String),
    Agent(u32),
}

pub struct Planet {
    pub key: String,
    pub name: String,
    pub agents: Vec<usize>,
    pub terrain: u8,
    pub seed: f32,
    pub wpos: (f32, f32),
    pub r: f32,
    /// A moon (git worktree) orbits this planet instead of a star.
    pub host: Option<usize>,
    pub moons: Vec<usize>,
    /// Development from prestige: 0 outpost, 1 colony, 2 city world, 3 ecumenopolis.
    pub level: u8,
    /// No agents here now: a world from the empire's history.
    pub dormant: bool,
    /// 0 (lively) … 1 (forgotten, lost in the nebula), from how long it's been left alone.
    pub fog: f32,
    /// Unix time of the last sign of life (agents or commits).
    pub last_seen: u64,
}

pub struct System {
    pub key: String,
    pub name: String,
    pub wpos: (f32, f32),
    pub planets: Vec<usize>,
    pub orbits: Vec<f32>,
    pub star: V3,
}

pub struct World {
    pub systems: Vec<System>,
    pub planets: Vec<Planet>,
    pub lanes: Vec<(usize, usize)>,
    /// The GitHub trade station, when any world has a GitHub remote.
    pub station: Option<(f32, f32)>,
}

impl World {
    pub fn planet(&self, key: &str) -> Option<&Planet> {
        self.planets.iter().find(|p| p.key == key)
    }
    pub fn system_of(&self, planet: usize) -> Option<&System> {
        let p = self.planets[planet].host.unwrap_or(planet);
        self.systems.iter().find(|s| s.planets.contains(&p))
    }
}

pub struct Cam {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub tx: f32,
    pub ty: f32,
    pub tz: f32,
}

/// A one-line text editor.
#[derive(Default, Clone)]
pub struct Line {
    pub s: String,
    pub cur: usize,
}

impl Line {
    /// Paste keeping line breaks, for text that can span lines.
    fn paste_lines(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        let clean: String = text.chars().filter(|&c| c == '\n' || !c.is_control()).collect();
        let byte = self.byte(self.cur);
        self.s.insert_str(byte, &clean);
        self.cur += clean.chars().count();
    }

    /// Move to the line above or below, keeping the column; false if there isn't one.
    fn vertical(&mut self, up: bool) -> bool {
        let chars: Vec<char> = self.s.chars().collect();
        let start = |i: usize| chars[..i].iter().rposition(|&c| c == '\n').map_or(0, |p| p + 1);
        let end = |i: usize| chars[i..].iter().position(|&c| c == '\n').map_or(chars.len(), |p| i + p);
        let here = start(self.cur);
        let col = self.cur - here;
        let to = if up {
            if here == 0 {
                return false;
            }
            start(here - 1)
        } else {
            let e = end(self.cur);
            if e == chars.len() {
                return false;
            }
            e + 1
        };
        self.cur = (to + col).min(end(to));
        true
    }

    fn byte(&self, i: usize) -> usize {
        self.s.char_indices().nth(i).map_or(self.s.len(), |(b, _)| b)
    }
    fn len(&self) -> usize {
        self.s.chars().count()
    }
    fn set(&mut self, s: String) {
        self.cur = s.chars().count();
        self.s = s;
    }
    /// Returns true if the text changed.
    fn key(&mut self, k: &KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('u') if ctrl => {
                self.set(String::new());
                true
            }
            KeyCode::Char('w') | KeyCode::Backspace if ctrl || (k.code == KeyCode::Backspace && k.modifiers.contains(KeyModifiers::ALT)) => {
                let head: String = self.s.chars().take(self.cur).collect();
                let trimmed = head.trim_end_matches('/');
                let cut = trimmed.rfind('/').map_or(0, |i| i + 1);
                let tail: String = self.s.chars().skip(self.cur).collect();
                self.set(head[..cut].to_string());
                let cur = self.cur;
                self.s.push_str(&tail);
                self.cur = cur;
                true
            }
            KeyCode::Char('a') if ctrl => {
                self.cur = 0;
                false
            }
            KeyCode::Char('e') if ctrl => {
                self.cur = self.len();
                false
            }
            KeyCode::Char(c) if !ctrl => {
                let b = self.byte(self.cur);
                self.s.insert(b, c);
                self.cur += 1;
                true
            }
            KeyCode::Backspace if self.cur > 0 => {
                self.cur -= 1;
                let b = self.byte(self.cur);
                self.s.remove(b);
                true
            }
            KeyCode::Delete if self.cur < self.len() => {
                let b = self.byte(self.cur);
                self.s.remove(b);
                true
            }
            KeyCode::Left => {
                self.cur = self.cur.saturating_sub(1);
                false
            }
            KeyCode::Right => {
                self.cur = (self.cur + 1).min(self.len());
                false
            }
            KeyCode::Home => {
                self.cur = 0;
                false
            }
            KeyCode::End => {
                self.cur = self.len();
                false
            }
            _ => false,
        }
    }
}

pub struct Colonize {
    pub path: Line,
    pub kind: usize,
    pub comps: Vec<String>,
    pub comp_i: usize,
    /// The path doesn't exist yet and the player has been asked to confirm.
    pub mkdir: bool,
}

/// A landed view of a world's AI buildings. Files are read only when selected.
#[derive(Clone)]
pub struct PlanetView {
    pub key: String,
    pub built: Vec<Built>,
    pub sel: usize,
    pub file: usize,
    /// Buildings, files, or the file preview.
    pub focus: usize,
    pub scroll: usize,
    pub preview: String,
}

impl PlanetView {
    fn new(key: String) -> Self {
        let built = crate::structure::survey(&key);
        let mut v = Self { key, built, sel: 0, file: 0, focus: 0, scroll: 0, preview: String::new() };
        v.read();
        v
    }

    pub fn building(&self) -> Option<&Built> { self.built.iter().find(|b| b.kind == KINDS[self.sel]) }
    pub fn files(&self) -> &[String] { self.building().map_or(&[], |b| b.files.as_slice()) }
    pub fn path(&self) -> Option<String> {
        self.files().get(self.file).map(|f| if KINDS[self.sel] == Kind::Academy { format!("{f}/SKILL.md") } else { f.clone() })
    }

    fn read(&mut self) {
        use std::io::Read;
        self.scroll = 0;
        self.preview = match self.path() {
            None => "This plot is empty. Press B to draft a construction order.".into(),
            Some(p) => {
                let read = || -> io::Result<String> {
                    let p = Path::new(&self.key).join(p);
                    if !std::fs::metadata(&p)?.is_file() { return Err(io::Error::other("not a regular file")); }
                    let f = std::fs::File::open(p)?;
                    let mut bytes = Vec::new();
                    f.take(65_537).read_to_end(&mut bytes)?;
                    let truncated = bytes.len() > 65_536;
                    bytes.truncate(65_536);
                    let mut text: String = String::from_utf8_lossy(&bytes).chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect();
                    text = text.replace('\t', "    ");
                    if truncated { text.push_str("\n[Preview limited to 64 KiB]"); }
                    if text.is_empty() { text = "[Empty file]".into(); }
                    Ok(text)
                };
                read().unwrap_or_else(|e| format!("Cannot read this file: {e}. Press R to refresh."))
            }
        };
    }

    pub fn lines(&self, width: usize) -> Vec<String> {
        let mut out = Vec::new();
        for line in self.preview.lines() {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() { out.push(String::new()); }
            for part in chars.chunks(width.max(1)) { out.push(part.iter().collect()); }
        }
        out
    }
}

#[derive(Clone, Copy)]
pub enum SectorEdit { New, Rename, Path, Remove }

#[derive(Default)]
pub struct SectorView {
    pub id: u64,
    pub world: String,
    pub worlds: bool,
    pub edit: Option<SectorEdit>,
    pub text: Line,
    pub error: String,
}

#[derive(Default, Debug, PartialEq)]
pub struct SectorStats {
    pub worlds: usize, pub ships: usize, pub working: usize, pub blocked: usize, pub finished: usize,
    pub missions: usize, pub queued: usize, pub anomalies: usize,
}

impl SectorStats {
    pub fn activity(&self) -> String { format!("{} ships · {} working · {} waiting · {} finished", self.ships, self.working, self.blocked, self.finished) }
    pub fn work(&self) -> String { format!("{} missions under way · {} queued · {} anomalies", self.missions, self.queued, self.anomalies) }
}

#[derive(Clone, Copy, PartialEq)]
pub enum LaneEdit { From, To, Label, Remove }

#[derive(Default)]
pub struct LaneView {
    pub id: u64,
    pub edit: Option<LaneEdit>,
    pub draft_id: Option<u64>,
    pub from: String,
    pub to: String,
    pub text: Line,
    pub error: String,
}

#[derive(Clone, Copy, PartialEq)]
pub enum CampaignEdit { Objective, Criteria, World, Task, Handoff, Verify, Link, Complete }

#[derive(Default)]
pub struct CampaignView {
    pub id: u64, pub stage: u64, pub stages: bool, pub edit: Option<CampaignEdit>, pub new: bool, pub stage_edit: bool,
    pub objective: String, pub world: String, pub text: Line, pub mission: usize, pub scroll: usize, pub error: String, pub details: bool,
}

struct CampaignDraft { link: crate::proto::CampaignRef, context: String }

pub enum Mode {
    Galaxy,
    /// On a world's surface, browsing its buildings and their files.
    Planet(PlanetView),
    Commander,
    Sectors,
    Hyperlanes,
    Campaigns,
    Anomalies { id: u64, typing: bool },
    Trade { sel: usize },
    /// Talking to an agent in a popup over the galaxy: its terminal, and a line to write in.
    Chat { id: u32, text: Line, attachments: Vec<Attachment>, sending: bool },
    /// Its terminal, full screen.
    Comms(u32),
    Colonize(Colonize),
    Deploy { planet: String, kind: usize },
    Confirm { id: u32 },
    /// A ship's card: what it's doing, and what you can do with it.
    Ship { id: u32, sel: usize },
    /// Refitting a deployed ship from a shipyard design: what would change, then `confirm`.
    ShipRefit { id: u32, sel: usize, confirm: bool },
    /// What a world needs, as a menu of quick actions.
    Actions { planet: String, sel: usize },
    /// A world's structures (its AI resources): what stands, and what can be built.
    Build { planet: String, sel: usize },
    /// A world's missions (its moons' too), grouped by `App::log_sort`; `open` ones show in full.
    Log { world: String, sel: usize, open: HashSet<u32> },
    /// Founding a moon (git worktree) off a world.
    Moon { planet: String, branch: Line, kind: usize },
    /// Merging a moon back into its world.
    Dock { moon: String },
    /// One order for every agent in a system or on a world.
    Fleet { target: Sel, text: Line },
    /// Writing a mission for a world: hand it to a new ship now, or queue it.
    /// `role` is an index into `role::ROLES`, for a ship launched with it. When not
    /// launching, `to` gives it to one ship here, or (`None`) to the first free one.
    /// `ship` is a new ship's outfit: the design it started from, and its own orders and skills.
    Brief { planet: String, text: Line, kind: usize, launch: bool, role: usize, to: Option<u32>, attachments: Vec<Attachment>, sending: bool, ship: Outfit },
    /// Refitting the composer's new ship: its standing orders and skills, saved as a design if wanted.
    Refit(RefitView),
    /// The saved ship designs: rename or scrap them.
    Shipyard(YardView),
    /// The situation log in full: filter it, read an entry's details. `id` is the selected entry
    /// (it stays put as news arrives); `typing` edits the search.
    Situation { id: u64, typing: bool },
    Help,
}

/// A new ship's outfit in the mission composer: the design it started from (if any), and its
/// standing orders and skills as adjusted for this launch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outfit {
    pub design: Option<u64>,
    pub orders: String,
    pub skills: Vec<String>,
}

/// The refit dialog over the composer, which comes back (as `brief`) when it closes.
pub struct RefitView {
    pub brief: Box<Mode>,
    pub orders: Line,
    /// Skills to choose from (any species' folders, from this world), and those chosen.
    pub skills: Vec<String>,
    pub picked: Vec<String>,
    /// 0 is the orders; then each skill.
    pub sel: usize,
    /// The design's name, while saving it.
    pub naming: Option<Line>,
    pub error: String,
}

#[derive(Default)]
pub struct YardView {
    pub sel: usize,
    pub rename: Option<Line>,
    pub remove: bool,
    pub error: String,
}

/// Asking Advisor which ship should take the mission being written: once the text has rested
/// for a moment, and again only if it changes.
#[derive(Default)]
pub struct AdvisorState {
    /// The world and text last seen, and since when.
    seen: String,
    seen_at: Option<Instant>,
    /// What the pending or last answer was asked about.
    asked: String,
    pub thinking: bool,
    pub pick: Option<crate::advisor::Pick>,
    pub err: Option<String>,
    /// The player chose for themselves; suggestions no longer move the selection.
    touched: bool,
}

/// How sure Advisor must be before its pick is preselected (a starting point, not a measured threshold).
pub const ADVISOR_SURE: f32 = 0.6;

/// The ships on a world a mission can be given to, in the composer's order.
pub fn brief_ships(agents: &[Agent], planet: &str) -> Vec<u32> {
    let mut v: Vec<u32> = agents.iter().filter(|a| a.cwd == planet && a.dying.is_none() && a.status != Status::Exited).map(|a| a.id).collect();
    v.sort();
    v.truncate(5);
    v
}

/// How the mission log is grouped.
#[derive(Clone, Copy, PartialEq)]
pub enum LogSort {
    Status,
    Planet,
    Fleet,
}

pub struct TradeRow {
    pub path: String,
    pub pr: Option<survey::Pr>,
}

/// A ready-to-send mission for a pull request: review someone else's, fix or finish your own.
pub fn pr_brief(pr: &survey::Pr, mine: bool) -> String {
    let (n, title) = (pr.number, &pr.title);
    // agents may be at work in this checkout, so nobody switches its branch
    if !mine {
        format!("Review PR #{n} \"{title}\" by @{}. Read it with `gh pr view {n}` and `gh pr diff {n}` (don't switch this checkout's branch; use a temporary git worktree if you need to run it). Then give me a review: what it does, bugs and risks, and suggestions, most important first. Don't post anything to GitHub until I say so.", pr.author)
    } else if pr.ci == "fail" {
        format!("Fix the failing checks on my PR #{n} \"{title}\". Find out why with `gh pr checks {n}` and `gh run view --log-failed`, fix it in a temporary git worktree on the PR's branch (don't switch this checkout's branch), and push the fix to the PR.")
    } else {
        format!("Address the review feedback on my PR #{n} \"{title}\". Read it with `gh pr view {n} --comments`, make the changes in a temporary git worktree on the PR's branch (don't switch this checkout's branch), and push them to the PR. Tell me what you changed and anything you disagreed with.")
    }
}

/// Which pull requests the GitHub station lists.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TradeView {
    All,
    Review,
    Mine,
}

impl TradeView {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Review,
            Self::Review => Self::Mine,
            Self::Mine => Self::All,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all open PRs",
            Self::Review => "waiting on your review",
            Self::Mine => "your PRs",
        }
    }
}

impl LogSort {
    pub fn next(self) -> Self {
        match self {
            Self::Status => Self::Planet,
            Self::Planet => Self::Fleet,
            Self::Fleet => Self::Status,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Planet => "world",
            Self::Fleet => "fleet",
        }
    }
}

pub enum Fx {
    Ring { x: f32, y: f32, t0: f32, col: V3, speed: f32, dur: f32 },
    Spark { x: f32, y: f32, vx: f32, vy: f32, t0: f32, life: f32, col: V3 },
}

pub struct App {
    pub agents: Vec<Agent>,
    pub online: bool,
    pub err: String,
    pub loaded: bool,
    pub sel: Option<Sel>,
    seq: u64,
    pub cam: Cam,
    pub fitted: bool,
    pub sys_pos: HashMap<String, (f32, f32)>,
    pub fx: Vec<Fx>,
    pub log: VecDeque<LogEntry>,
    /// What the situation log view shows.
    pub sit: SitFilter,
    pub campaigns: crate::campaign::Book,
    pub campaign_view: CampaignView,
    pub campaign_error: String,
    campaign_path: Option<PathBuf>,
    campaign_draft: Option<CampaignDraft>,
    pub hyperlanes: crate::hyperlane::Book,
    pub lane_view: LaneView,
    pub dependencies: bool,
    pub lane_error: String,
    lane_path: Option<PathBuf>,
    /// Saved ship designs (the shipyard), and where they're kept (nowhere in tests).
    pub designs: crate::design::Book,
    pub design_path: Option<PathBuf>,
    /// Where the user's own skill folders are (none in tests).
    pub skill_home: PathBuf,
    pub sectors: crate::sector::Book,
    pub sector_view: SectorView,
    pub sector_focus: Option<u64>,
    pub sector_error: String,
    sector_path: Option<PathBuf>,
    pub anomalies: crate::anomaly::Book,
    pub anomaly_filter: crate::anomaly::Filter,
    pub anomaly_detail: usize,
    pub anomaly_error: String,
    anomaly_path: Option<PathBuf>,
    anomaly_return: Option<u64>,
    pub toast: Option<(String, f32)>,
    pub mode: Mode,
    planet_return: Option<PlanetView>,
    pub commander: String,
    pub commander_detail: usize,
    commander_return: bool,
    campaign_return: bool,
    pub kinds: Vec<String>,
    pub screen: Option<(u32, ScreenView)>,
    pub panel: bool,
    pub bg: Vec<V3>,
    pub bg_key: (usize, usize, i32),
    /// When the comms window was opened.
    pub comms_at: Instant,
    /// Lines scrolled back in the comms window (0 = live).
    pub scroll: usize,
    /// Outliner rows wheeled past, and the selection it was wheeled at (it follows the selection once that moves).
    pub out_off: usize,
    pub out_pin: Option<Sel>,
    /// Situation log entries wheeled past in the side panel (0 = newest at the top).
    pub log_off: usize,
    pub missions: Vec<Mission>,
    pub log_sort: LogSort,
    /// Advisor's suggestion for the mission being written.
    pub advisor: AdvisorState,
    pub trade_view: TradeView,
    pub prestige: Prestige,
    /// Worlds that just levelled up, waiting for their fanfare on the map.
    pub fanfares: Vec<String>,
    /// Every world ever colonized, and when it last had agents.
    pub history: HashMap<String, u64>,
    history_saved: Instant,
    /// Folder and usage stats from the survey thread.
    pub survey: survey::Snapshot,
    /// Text selection in the comms window.
    pub pick: Option<Pick>,
    /// Last click in comms: (cell, when, clicks in a row) for double/triple click.
    click: ((i32, i32), Instant, u8),
}

/// A selection in the comms window, in terminal cells (row, col). `a` is where
/// the drag started; the selection runs between `a` and `b` in reading order.
#[derive(Clone, Copy, Debug)]
pub struct Pick {
    pub a: (i32, i32),
    pub b: (i32, i32),
}

impl Pick {
    /// (start, end) in reading order, both inclusive.
    pub fn span(&self) -> ((i32, i32), (i32, i32)) {
        if self.a <= self.b { (self.a, self.b) } else { (self.b, self.a) }
    }

    pub fn has(&self, r: i32, c: i32) -> bool {
        let (s, e) = self.span();
        (r, c) >= s && (r, c) <= e
    }
}

// ───────────────────────────── paths & names ───────────────────────────

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

pub fn tilde(p: &str) -> String {
    let h = home();
    let h = h.to_string_lossy();
    match p.strip_prefix(h.as_ref()) {
        Some("") => "~".into(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => p.into(),
    }
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix('~') {
        Some(rest) => home().join(rest.trim_start_matches('/')),
        None => PathBuf::from(p),
    }
}

pub fn base_name(p: &str) -> String {
    if Path::new(p) == home() {
        return "~".into();
    }
    Path::new(p).file_name().map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned())
}

fn parent(p: &str) -> String {
    Path::new(p).parent().map_or_else(|| "/".into(), |x| x.to_string_lossy().into_owned())
}

fn hash(s: &str) -> u32 {
    s.bytes().fold(2_166_136_261u32, |h, b| (h ^ b as u32).wrapping_mul(16_777_619))
}

/// Strip spinners and status glyphs agents put in front of their titles.
fn clean_title(t: &str) -> String {
    t.trim_start_matches(|c: char| !c.is_alphanumeric()).trim().to_string()
}

pub fn clock() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// Agent CLIs installed on this machine (plus a plain shell).
fn detect_kinds() -> Vec<String> {
    const SPECIES: [&str; 3] = ["claude", "codex", "grok"];
    let path = std::env::var_os("PATH").unwrap_or_default();
    let has = |bin: &str| std::env::split_paths(&path).any(|d| d.join(bin).is_file());
    let v: Vec<String> = SPECIES.iter().filter(|b| has(b)).map(|k| k.to_string()).collect();
    if v.is_empty() { SPECIES.map(String::from).to_vec() } else { v }
}

/// Directory completions for a typed path.
fn complete(input: &str) -> Vec<String> {
    let (dir, prefix) = match input.rfind('/') {
        Some(i) => (&input[..=i], &input[i + 1..]),
        None => ("", input),
    };
    let base = if dir.is_empty() { PathBuf::from(".") } else { expand(dir) };
    let Ok(rd) = std::fs::read_dir(base) else { return Vec::new() };
    let mut v: Vec<_> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| prefix.starts_with('.') || !n.starts_with('.'))
        .filter_map(|n| fuzzy_score(prefix, &n).map(|score| (score, n.to_lowercase(), n)))
        .collect();
    v.sort();
    v.into_iter().map(|(_, _, n)| format!("{dir}{n}/")).collect()
}

/// Exact names, prefixes, substrings, then ordered letters with the fewest gaps.
fn fuzzy_score(query: &str, name: &str) -> Option<(u8, usize, usize)> {
    let (q, n) = (query.to_lowercase(), name.to_lowercase());
    if q == n { return Some((0, 0, 0)); }
    if n.starts_with(&q) { return Some((1, 0, 0)); }
    if let Some(i) = n.find(&q) { return Some((2, 0, n[..i].chars().count())); }
    let (q, n): (Vec<_>, Vec<_>) = (q.chars().collect(), n.chars().collect());
    let mut best = None;
    for start in 0..n.len() {
        if n[start] != q[0] { continue; }
        let mut at = 1;
        for end in start + 1..n.len() {
            if n[end] == q[at] { at += 1; }
            if at == q.len() {
                let score = (3, end - start + 1 - q.len(), start);
                best = Some(best.map_or(score, |b| std::cmp::min(b, score)));
                break;
            }
        }
    }
    best
}

// ─────────────────────────────── layout ────────────────────────────────

/// Screen layout: the galaxy viewport (pixels) and the outliner panel (cells).
pub struct Lay {
    pub cols: i32,
    pub rows: i32,
    pub pw: i32,
    pub vw: f32,
    pub vh: f32,
    pub vy0: f32,
}

impl Lay {
    pub fn new(fb: &Fb, panel: bool) -> Self {
        let (cols, rows) = (fb.cols as i32, fb.rows as i32);
        let pw = if panel && cols >= 90 { (cols as f32 * 0.32).clamp(36.0, 52.0) as i32 } else { 0 };
        Self { cols, rows, pw, vw: ((cols - pw) * 2) as f32, vh: ((rows - 2) * 4) as f32, vy0: 4.0 }
    }
    pub fn to_screen(&self, c: &Cam, (x, y): (f32, f32)) -> (f32, f32) {
        ((x - c.x) * c.z + self.vw * 0.5, (y - c.y) * c.z + self.vy0 + self.vh * 0.5)
    }
    pub fn to_world(&self, c: &Cam, (sx, sy): (f32, f32)) -> (f32, f32) {
        ((sx - self.vw * 0.5) / c.z + c.x, (sy - self.vy0 - self.vh * 0.5) / c.z + c.y)
    }
}

// ──────────────────────────────── app ──────────────────────────────────

impl App {
    /// Projects currently in this galaxy; worktrees share their host's PRs.
    pub fn trade_rows(&self, w: &World) -> Vec<TradeRow> {
        let mut projects: Vec<_> = w.planets.iter().filter(|p| p.host.is_none())
            .filter_map(|p| self.survey.worlds.get(&p.key).filter(|s| s.github).map(|s| (&p.key, s))).collect();
        projects.sort_by_key(|(key, s)| (!s.prs.iter().any(|p| p.ci == "fail"), !s.others.iter().any(|p| p.asks_you), *key));
        let mut rows = Vec::new();
        for (path, s) in projects {
            rows.push(TradeRow { path: path.clone(), pr: None });
            let mut prs: Vec<survey::Pr> = match self.trade_view {
                TradeView::All => s.prs.iter().chain(&s.others).cloned().collect(),
                TradeView::Review => s.others.iter().filter(|p| p.asks_you).cloned().collect(),
                TradeView::Mine => s.prs.clone(),
            };
            // yours, then reviews asked of you, then the rest; failing checks first in each
            let mine = |p: &survey::Pr| s.prs.iter().any(|m| m.number == p.number);
            prs.sort_by_key(|p| (if mine(p) { 0 } else if p.asks_you { 1 } else { 2 }, match p.ci { "fail" => 0, "pending" => 1, "pass" => 2, _ => 3 }, p.number));
            rows.extend(prs.into_iter().map(|pr| TradeRow { path: path.clone(), pr: Some(pr) }));
        }
        rows
    }

    /// Open the mission composer for a station row: a fitting brief for a PR, a blank one for a project.
    fn trade_mission(&mut self, w: &World, index: usize) {
        let Some(row) = self.trade_rows(w).into_iter().nth(index) else { return };
        let mine = row.pr.as_ref().is_some_and(|pr| self.survey.worlds.get(&row.path).is_some_and(|s| s.prs.iter().any(|m| m.number == pr.number)));
        let mut text = Line::default();
        if let Some(pr) = &row.pr {
            text.set(pr_brief(pr, mine));
        }
        // queue it if someone's there to take it, otherwise send a ship
        let crewed = self.agents.iter().any(|a| a.cwd == row.path && a.dying.is_none());
        self.mode = Mode::Brief { planet: row.path, text, kind: 0, launch: !crewed, role: 0, to: None, attachments: Vec::new(), sending: false, ship: Outfit::default() };
    }

    fn open_trade_row(&mut self, w: &World, index: usize, client: &Client, t: f32) {
        if let Some(row) = self.trade_rows(w).get(index) {
            if let Some(pr) = &row.pr {
                client.open_pr(row.path.clone(), pr.number);
                self.toast(format!("opening PR #{} in browser", pr.number), t);
            } else {
                self.sel = Some(Sel::Planet(row.path.clone()));
                self.mode = Mode::Galaxy;
                self.glide_to_sel(w);
            }
        }
    }

    fn new() -> Self {
        Self {
            agents: Vec::new(),
            online: false,
            err: String::new(),
            loaded: false,
            sel: None,
            seq: 0,
            cam: Cam { x: 0.0, y: 0.0, z: 1.0, tx: 0.0, ty: 0.0, tz: 1.0 },
            fitted: false,
            sys_pos: HashMap::new(),
            fx: Vec::new(),
            log: VecDeque::new(),
            sit: SitFilter::default(),
            campaigns: Default::default(),
            campaign_view: Default::default(),
            campaign_error: String::new(),
            campaign_path: if cfg!(test) { None } else { Some(crate::campaign::path()) },
            campaign_draft: None,
            hyperlanes: Default::default(),
            lane_view: Default::default(),
            dependencies: false,
            lane_error: String::new(),
            lane_path: if cfg!(test) { None } else { Some(crate::hyperlane::path()) },
            designs: Default::default(),
            design_path: if cfg!(test) { None } else { Some(crate::design::path()) },
            skill_home: if cfg!(test) { std::env::temp_dir().join("empire-no-home") } else { home() },
            sectors: Default::default(),
            sector_view: Default::default(),
            sector_focus: None,
            sector_error: String::new(),
            sector_path: if cfg!(test) { None } else { Some(crate::sector::path()) },
            anomalies: Default::default(),
            anomaly_filter: Default::default(),
            anomaly_detail: 0,
            anomaly_error: String::new(),
            anomaly_path: if cfg!(test) { None } else { Some(crate::anomaly::path()) },
            anomaly_return: None,
            toast: None,
            mode: Mode::Galaxy,
            commander: String::new(),
            commander_detail: 0,
            commander_return: false,
            campaign_return: false,
            planet_return: None,
            kinds: detect_kinds(),
            screen: None,
            panel: true,
            bg: Vec::new(),
            bg_key: (0, 0, 0),
            comms_at: Instant::now(),
            scroll: 0,
            out_off: 0,
            out_pin: None,
            log_off: 0,
            log_sort: LogSort::Status,
            advisor: AdvisorState::default(),
            trade_view: TradeView::All,
            pick: None,
            survey: survey::Snapshot::default(),
            missions: Vec::new(),
            prestige: if cfg!(test) { Default::default() } else { std::fs::read_to_string(prestige_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default() },
            fanfares: Vec::new(),
            history: if cfg!(test) { Default::default() } else { std::fs::read_to_string(history_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default() },
            history_saved: Instant::now(),
            click: ((-1, -1), Instant::now(), 0),
        }
    }

    /// What the world at `key` needs from you, most pressing first.
    pub fn needs(&self, key: &str) -> Vec<Need> {
        let mut v = Vec::new();
        let here: Vec<&Agent> = self.agents.iter().filter(|a| a.cwd == key && a.dying.is_none()).collect();
        for a in here.iter().filter(|a| a.status == Status::Blocked) {
            let q = a.ask.as_ref().map_or("a question".to_string(), |q| format!("\"{}\"", clip(&q.question, 40)));
            v.push(Need { badge: "▲".into(), label: format!("{} is waiting on you: {q}", a.kind), col: Status::Blocked.color(), act: Some(Act::Comms(a.id)) });
        }
        if let Some(a) = here.iter().find(|a| !a.war.is_empty()) {
            let files = a.war.iter().flat_map(|w| w.files.clone()).collect::<Vec<_>>().join(", ");
            v.push(Need { badge: "⚔".into(), label: format!("border conflict over {files}: give {} priority", a.kind), col: RED, act: Some(Act::Priority(a.id)) });
        }
        let s = self.survey.worlds.get(key);
        let prs = s.and_then(|w| w.host.as_ref()).and_then(|h| self.survey.worlds.get(h)).or(s).map(|w| w.prs.clone()).unwrap_or_default();
        for pr in prs.iter().filter(|p| p.ci == "fail") {
            let text = format!("CI is failing on PR #{} ({}). Find out why with `gh pr checks {}` and fix it.", pr.number, pr.title, pr.number);
            v.push(Need { badge: format!("✖#{}", pr.number), label: format!("CI failing on #{}: send someone to fix it", pr.number), col: RED, act: Some(Act::Mission(text)) });
        }
        if let Some(w) = s.filter(|w| w.git) {
            if w.dirty > 0 {
                let text = "Commit the uncommitted changes in this repo with clear messages, split into logical commits where it makes sense.".to_string();
                v.push(Need { badge: format!("±{}", w.dirty), label: format!("{} uncommitted file{}: get them committed", w.dirty, if w.dirty == 1 { "" } else { "s" }), col: AMBER, act: Some(Act::Mission(text)) });
            }
            if w.behind > 0 {
                let text = format!("Pull the latest changes from upstream into {} and sort out anything that comes up.", w.branch);
                v.push(Need { badge: format!("↓{}", w.behind), label: format!("{} commit{} behind upstream: pull", w.behind, if w.behind == 1 { "" } else { "s" }), col: AMBER, act: Some(Act::Mission(text)) });
            }
            if w.ahead > 0 {
                let text = format!("Push the unpushed commits on {} to the remote.", w.branch);
                v.push(Need { badge: format!("↑{}", w.ahead), label: format!("{} unpushed commit{}: push", w.ahead, if w.ahead == 1 { "" } else { "s" }), col: AMBER, act: Some(Act::Mission(text)) });
            }
        }
        let queued = self.missions.iter().filter(|m| m.cwd == key && m.state == "queued").count();
        if queued > 0 && here.is_empty() {
            v.push(Need { badge: format!("○{queued}"), label: format!("{queued} mission{} queued with no one here to take {}: press e to send a ship", if queued == 1 { "" } else { "s" }, if queued == 1 { "it" } else { "them" }), col: AMBER, act: None });
        }
        for a in here.iter().filter(|a| a.status == Status::Done) {
            v.push(Need { badge: "✔".into(), label: format!("{} finished{}: take a look", a.kind, if a.title.is_empty() { String::new() } else { format!(" ({})", clip(&a.title, 30)) }), col: Status::Done.color(), act: Some(Act::Comms(a.id)) });
        }
        v
    }

    /// Carry out a quick action for the world at `key`.
    fn act(&mut self, key: &str, act: Act, client: &Client, l: &Lay, t: f32) {
        match act {
            Act::Mission(text) => {
                // a free agent there takes it; with no one there, a ship is sent with it
                let crewed = self.agents.iter().any(|a| a.cwd == key && a.dying.is_none() && a.status != Status::Exited);
                let kind = self.kinds.first().cloned().unwrap_or_else(|| "claude".into());
                client.send(Cmd::Mission { cwd: key.to_string(), text: text.clone(), kind, launch: !crewed, attachments: Vec::new(), draft: false, role: String::new(), to: None, campaign: None, spec: ShipSpec::default() });
                self.mode = Mode::Galaxy;
                self.toast(if crewed { format!("mission queued: {}", clip(&text, 40)) } else { format!("ship launched: {}", clip(&text, 40)) }, t);
            }
            Act::Comms(id) => {
                self.sel = Some(Sel::Agent(id));
                self.open_comms(id, client, l);
            }
            Act::Priority(id) => {
                client.send(Cmd::Priority(id));
                self.mode = Mode::Galaxy;
                self.toast("priority given: its rivals were told to stand down", t);
            }
        }
    }

    /// The structure of `kind` standing on the world at `key`, if any.
    pub fn structure(&self, key: &str, kind: Kind) -> Option<&Built> {
        self.survey.worlds.get(key)?.structures.iter().find(|b| b.kind == kind)
    }

    /// Draft the order to build (or renovate) a structure, in the mission composer, for a
    /// builder: the player reads it, edits it if they like, and sends it.
    fn construct(&mut self, key: &str, kind: Kind) {
        let mut text = Line::default();
        text.set(kind.brief(self.structure(key, kind)));
        let crewed = self.agents.iter().any(|a| a.cwd == key && a.dying.is_none() && a.status != Status::Exited);
        let role = ROLES.iter().position(|r| *r == Role::Builder).unwrap_or(0);
        self.mode = Mode::Brief { planet: key.to_string(), text, kind: 0, launch: !crewed, role, to: None, attachments: Vec::new(), sending: false, ship: Outfit::default() };
    }

    /// Missions in log order, each with its group heading. Within a group: under way,
    /// then queued (in dispatch order), then done (newest first).
    pub fn mission_log(&self, world: &str) -> Vec<(String, &Mission)> {
        let rank = |m: &Mission| match m.state.as_str() {
            "active" | "sent" => 0,
            "queued" => 1,
            _ => 2,
        };
        // (sort key, heading) of a mission's group
        let group = |m: &Mission| -> (String, String) {
            match self.log_sort {
                LogSort::Status => (rank(m).to_string(), ["UNDER WAY", "QUEUED", "DONE"][rank(m)].into()),
                LogSort::Planet => (m.cwd.clone(), format!("● {}", base_name(&m.cwd).to_uppercase())),
                LogSort::Fleet => match m.fleet {
                    Some(f) => (format!("0{f:010}"), format!("⚑ FLEET ORDER #{f}")),
                    None => ("1".into(), "SINGLE MISSIONS".into()),
                },
            }
        };
        let ours = |m: &&Mission| m.cwd == world || self.moon_host(&m.cwd).as_deref() == Some(world);
        let mut v: Vec<((String, String), &Mission)> = self.missions.iter().filter(ours).map(|m| (group(m), m)).collect();
        v.sort_by_key(|(g, m)| (g.0.clone(), rank(m), if rank(m) == 2 { std::cmp::Reverse(m.done_at.unwrap_or(m.at)) } else { std::cmp::Reverse(0) }, m.id));
        v.into_iter().map(|((_, head), m)| (head, m)).collect()
    }

    fn level(&self, key: &str) -> u8 {
        level_of(self.prestige.worlds.get(key).copied().unwrap_or(0))
    }

    /// Prestige for a world, once per event `id`; a new development level gets a fanfare.
    fn award(&mut self, key: &str, id: String, pts: u32, why: &str, cues: &mut Vec<Cue>) {
        // moons earn for their world
        let key = self.moon_host(key).unwrap_or_else(|| key.to_string());
        if !self.prestige.counted.insert(id) {
            return;
        }
        let before = self.prestige.worlds.get(&key).copied().unwrap_or(0);
        let after = before + pts;
        self.prestige.worlds.insert(key.clone(), after);
        let _ = std::fs::create_dir_all(crate::daemon::dirs_state());
        let _ = std::fs::write(prestige_file(), serde_json::to_string(&self.prestige).unwrap_or_default());
        let place = base_name(&key);
        self.say(Ev::Prestige, format!("✦ +{pts} prestige · {place}: {why}"), v3(1.0, 0.85, 0.4)).on(&key);
        let (l0, l1) = (level_of(before), level_of(after));
        if l1 > l0 {
            let name = LEVELS[l1 as usize].1;
            self.say(Ev::Prestige, format!("♛ {place} is now a{} {name}!", if name.starts_with(['a', 'e', 'i', 'o', 'u']) { "n" } else { "" }), v3(1.0, 0.9, 0.5)).on(&key);
            self.fanfares.push(key);
            cues.push(Cue::Fanfare);
        }
    }

    /// How long a world has been left alone: (dormant, fog, last sign of life).
    fn fog(&self, key: &str, live: usize, now: u64) -> (bool, f32, u64) {
        if live > 0 {
            return (false, 0.0, now);
        }
        let commit = self.survey.worlds.get(key).and_then(|w| w.last_commit).unwrap_or(0).max(0) as u64;
        let last = self.history.get(key).copied().unwrap_or(0).max(commit);
        let days = now.saturating_sub(last) as f32 / 86_400.0;
        (true, 0.25 + 0.75 * smoothstep(2.0, 21.0, days), last)
    }

    /// Rings and sparks: docking, collisions.
    fn burst(&mut self, (x, y): (f32, f32), col: V3, t: f32, n: u32) {
        self.fx.push(Fx::Ring { x, y, t0: t, col, speed: 45.0, dur: 1.2 });
        self.fx.push(Fx::Ring { x, y, t0: t + 0.15, col, speed: 25.0, dur: 1.0 });
        for i in 0..n {
            let a = rnd(i * 7 + (t * 100.0) as u32) * TAU;
            let sp = 10.0 + rnd(i * 13 + 5) * 30.0;
            self.fx.push(Fx::Spark { x, y, vx: a.cos() * sp, vy: a.sin() * sp, t0: t, life: 0.6 + rnd(i * 3) * 1.0, col });
        }
    }

    /// The world a selection is a moon of, if it is one.
    /// The ship card's menu; priority only matters in a border conflict.
    pub fn ship_acts(&self, id: u32) -> Vec<ShipAct> {
        let Some(a) = self.agent(id) else { return Vec::new() };
        let mut v = vec![ShipAct::Comms, ShipAct::Mission];
        if !a.war.is_empty() {
            v.push(ShipAct::Priority);
        }
        v.push(ShipAct::Refit);
        v.push(ShipAct::Decommission);
        v
    }

    /// The role and outfit the shipyard's `i`th design would give a deployed ship.
    pub fn refit_outfit(&self, i: usize) -> Option<(Role, ShipSpec)> {
        let d = self.designs.designs.get(i)?;
        Some((Role::parse(&d.role), ShipSpec { orders: d.orders.clone(), skills: d.skills.clone(), design: d.name.clone() }))
    }

    fn ship_act(&mut self, id: u32, act: ShipAct, client: &Client, l: &Lay, t: f32) {
        let Some(a) = self.agent(id) else {
            self.mode = Mode::Galaxy;
            return;
        };
        let cwd = a.cwd.clone();
        match act {
            ShipAct::Comms => self.open_comms(id, client, l),
            ShipAct::Mission => self.mode = Mode::Brief { planet: cwd, text: Line::default(), kind: 0, launch: false, role: 0, to: None, attachments: Vec::new(), sending: false, ship: Outfit::default() },
            ShipAct::Priority => self.act(&cwd, Act::Priority(id), client, l, t),
            ShipAct::Refit => self.mode = Mode::ShipRefit { id, sel: 0, confirm: false },
            ShipAct::Decommission => self.mode = Mode::Confirm { id },
        }
    }

    pub fn moon_host(&self, key: &str) -> Option<String> {
        self.survey.worlds.get(key)?.host.clone()
    }

    /// Live agents in a system or on a world.
    pub fn fleet(&self, w: &World, target: &Sel) -> Vec<&Agent> {
        let planets: Vec<&Planet> = match target {
            Sel::Sys(k) => w
                .systems
                .iter()
                .filter(|s| &s.key == k)
                .flat_map(|s| s.planets.iter().flat_map(|&i| std::iter::once(i).chain(w.planets[i].moons.iter().copied())))
                .map(|i| &w.planets[i])
                .collect(),
            Sel::Planet(k) => w.planet(k).into_iter().collect(),
            Sel::Agent(_) | Sel::GitHub | Sel::Sector(_, _) | Sel::Lane(_) => Vec::new(),
        };
        planets.iter().flat_map(|p| p.agents.iter().map(|&i| &self.agents[i])).filter(|a| a.dying.is_none() && a.status != Status::Exited).collect()
    }

    /// New mission log from the daemon; announce what changed.
    fn track(&mut self, missions: Vec<Mission>, cues: &mut Vec<Cue>) {
        for m in missions.iter().filter(|m| m.state == "done") {
            self.award(&m.cwd, format!("mission-{}-{}", m.id, m.at), 2, &format!("mission accomplished ({})", clip(&m.text, 30)), cues);
        }
        if self.loaded && !self.missions.is_empty() {
            for m in &missions {
                let old = self.missions.iter().find(|o| o.id == m.id).map(|o| o.state.as_str());
                if old == Some(m.state.as_str()) {
                    continue;
                }
                let place = base_name(&m.cwd);
                let what = clip(&m.text, 40);
                match m.state.as_str() {
                    "sent" | "active" if !matches!(old, Some("sent") | Some("active")) => {
                        self.say(Ev::Mission, format!("➤ mission to {place}: {what}"), v3(0.6, 0.85, 1.0)).on(&m.cwd).by(m.agent);
                    }
                    "done" => {
                        self.say(Ev::Mission, format!("★ mission complete on {place}: {what}"), v3(1.0, 0.85, 0.4)).on(&m.cwd).by(m.agent);
                    }
                    _ => {}
                }
            }
        }
        // a fleet order is done when all its missions are
        let fleets: std::collections::BTreeSet<u32> = missions.iter().filter_map(|m| m.fleet).collect();
        for f in fleets {
            let all: Vec<&Mission> = missions.iter().filter(|m| m.fleet == Some(f)).collect();
            let was_done = self.missions.iter().filter(|m| m.fleet == Some(f)).all(|m| m.state == "done") && self.missions.iter().any(|m| m.fleet == Some(f));
            if self.loaded && all.iter().all(|m| m.state == "done") && !was_done {
                let what = clip(&all[0].text, 40);
                self.say(Ev::Mission, format!("★ fleet order complete · {} ships · {what}", all.len()), v3(1.0, 0.85, 0.4));
            }
        }
        let at = survey::now().max(0) as u64;
        let _ = self.sync_campaigns(|b| { b.refresh(&missions, at); Ok(0) });
        self.missions = missions;
    }

    /// The blocked agent whose question is on screen: the selected one if it's
    /// asking, otherwise whoever has been waiting longest.
    pub fn transmission(&self) -> Option<&Agent> {
        let asking = |a: &&Agent| a.dying.is_none() && a.status == Status::Blocked && a.ask.as_ref().is_some_and(|q| !q.options.is_empty());
        if let Some(Sel::Agent(id)) = self.sel
            && let Some(a) = self.agents.iter().filter(asking).find(|a| a.id == id)
        {
            return Some(a);
        }
        self.agents.iter().filter(asking).min_by(|a, b| a.since.total_cmp(&b.since))
    }

    pub fn agent(&self, id: u32) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id && a.dying.is_none())
    }

    /// Report to the situation log; tag the entry with `.on(world)` and `.by(agent)`.
    fn say(&mut self, kind: Ev, msg: String, col: V3) -> &mut LogEntry {
        self.seq += 1;
        self.log.push_front(LogEntry { id: self.seq, at: survey::now(), clock: clock(), msg, col, kind, world: None, agent: None });
        self.log.truncate(LOG_KEEP);
        &mut self.log[0]
    }

    /// The situation log as filtered by `sit`, newest first.
    pub fn situation(&self) -> Vec<&LogEntry> {
        let q = self.sit.query.to_lowercase();
        self.log
            .iter()
            .filter(|e| self.sit.kind.is_none_or(|k| k == e.kind))
            .filter(|e| match (&self.sit.world, &e.world) {
                (None, _) => true,
                (Some(w), Some(ew)) => ew == w || self.moon_host(ew).as_ref() == Some(w),
                (Some(_), None) => false,
            })
            .filter(|e| q.is_empty() || e.msg.to_lowercase().contains(&q))
            .collect()
    }

    fn toast(&mut self, msg: impl Into<String>, t: f32) {
        self.toast = Some((msg.into(), t));
    }

    fn apply(&mut self, list: Vec<AgentView>, t: f32, cues: &mut Vec<Cue>) {
        self.online = true;
        let first = !self.loaded;
        self.loaded = true;
        if first {
            let worlds = list.iter().map(|v| &v.cwd).collect::<std::collections::HashSet<_>>().len();
            self.say(Ev::World, format!("◆ uplink established · {} agents on {worlds} worlds", list.len()), v3(0.6, 0.85, 1.0));
        }
        for v in &list {
            let st = Status::parse(&v.status);
            // what it was last asked to do; the window title often stays on its first prompt
            let title = if v.task.is_empty() { clean_title(&v.title) } else { v.task.clone() };
            if let Some(i) = self.agents.iter().position(|a| a.id == v.id && a.dying.is_none()) {
                let old = self.agents[i].status;
                if old != st {
                    let a = &self.agents[i];
                    let (kind, cwd, title, pos) = (a.kind.clone(), a.cwd.clone(), a.title.clone(), a.wpos);
                    // no sounds for the agent you're already talking to, or for quick little turns
                    let watching = self.talking_to() == Some(v.id);
                    let quick = st == Status::Done && t - a.busy_at < SHORT_TURN;
                    let mut heard = Vec::new();
                    self.transition(old, st, (v.id, &kind, &cwd), &title, pos, t, &mut heard);
                    if !watching && !quick {
                        cues.extend(heard);
                    }
                    let a = &mut self.agents[i];
                    a.status = st;
                    if st == Status::Blocked {
                        a.last_ping = t;
                    }
                    if st == Status::Working && old != Status::Blocked {
                        a.busy_at = t;
                    }
                }
                let a = &mut self.agents[i];
                a.since = t - v.age;
                a.turns = v.turns;
                a.report = v.report.clone();
                a.ask = v.ask.clone();
                // what's aboard can change under a refit; say how one ended
                let was = a.refit.as_ref().map(|r| r.state.clone());
                (a.role, a.design, a.spec, a.refit) = (Role::parse(&v.role), v.design.clone(), v.spec.clone(), v.refit.clone());
                if let Some(r) = v.refit.as_ref().filter(|r| was.as_deref() != Some(r.state.as_str()) && was.is_some()) {
                    let (kind, planet) = (a.kind.clone(), base_name(&a.cwd));
                    let line = match r.state.as_str() {
                        "active" => Some((format!("⚒ {kind} on {planet} refitted: {} aboard", refit_name(r)), v3(0.5, 0.9, 0.6))),
                        "failed" => Some((format!("✖ refit of {kind} on {planet} failed: {}", r.note), v3(1.0, 0.4, 0.3))),
                        "unsupported" => Some((format!("⊘ {kind} on {planet} can't be refitted: {}", r.note), v3(0.7, 0.7, 0.75))),
                        _ => None,
                    };
                    if let Some((msg, col)) = line {
                        self.say(Ev::Agent, msg, col).on(&v.cwd).by(v.id);
                    }
                }
                let a = &mut self.agents[i];
                // a new border conflict (reported once, by the lower id of the pair)
                let (old_war, kind, planet, pos) = (a.war.clone(), a.kind.clone(), base_name(&a.cwd), a.wpos);
                for w in &v.war {
                    if w.rival > v.id && !old_war.iter().any(|o| o.rival == w.rival) {
                        let rival = self.agents.iter().find(|r| r.id == w.rival).map_or("another agent".into(), |r| r.kind.clone());
                        self.say(Ev::Alert, format!("⚔ border conflict on {planet}: {kind} and {rival} both editing {}", w.files.join(", ")), v3(1.0, 0.45, 0.3)).on(&v.cwd).by(v.id);
                        self.fx.push(Fx::Ring { x: pos.0, y: pos.1, t0: t, col: v3(1.0, 0.4, 0.2), speed: 50.0, dur: 1.2 });
                        cues.push(Cue::Blocked);
                    }
                }
                let a = &mut self.agents[i];
                a.war = v.war.clone();
                if !title.is_empty() {
                    a.title = title;
                }
            } else {
                if !first {
                    if self.talking_to() != Some(v.id) {
                        cues.push(Cue::Spawn);
                    }
                    let role = Role::parse(&v.role);
                    let what = if role == Role::None { v.kind.clone() } else { format!("{} {}", v.kind, role.name()) };
                    self.say(Ev::Agent, format!("✦ {what} arrived at {}", base_name(&v.cwd)), kind_color(&v.kind)).on(&v.cwd).by(v.id);
                }
                self.agents.push(Agent {
                    id: v.id,
                    kind: v.kind.clone(),
                    cwd: v.cwd.clone(),
                    title,
                    status: st,
                    since: t - v.age,
                    born: if first { t - 10.0 } else { t },
                    dying: None,
                    seed: rnd(v.id.wrapping_mul(2_654_435_761)),
                    last_ping: t,
                    busy_at: t,
                    turns: v.turns,
                    report: v.report.clone(),
                    ask: v.ask.clone(),
                    war: v.war.clone(),
                    role: Role::parse(&v.role),
                    design: v.design.clone(),
                    spec: v.spec.clone(),
                    refit: v.refit.clone(),
                    wpos: (0.0, 0.0),
                    front: true,
                });
            }
        }
        let mut gone = Vec::new();
        for a in &mut self.agents {
            if a.dying.is_none() && !list.iter().any(|v| v.id == a.id) {
                a.dying = Some(t);
                gone.push((a.id, a.kind.clone(), a.cwd.clone(), a.status == Status::Exited));
            }
        }
        for (id, k, cwd, dark) in gone {
            if !first && !dark {
                cues.push(Cue::Gone);
                self.say(Ev::Agent, format!("✝ {k} left {}", base_name(&cwd)), s3(0.5)).on(&cwd).by(id);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn transition(&mut self, from: Status, to: Status, (id, kind, cwd): (u32, &str, &str), title: &str, (x, y): (f32, f32), t: f32, cues: &mut Vec<Cue>) {
        let planet = base_name(cwd);
        let ring = |col: V3, speed: f32| Fx::Ring { x, y, t0: t, col, speed, dur: 1.0 };
        match (from, to) {
            (_, Status::Blocked) => {
                cues.push(Cue::Blocked);
                self.fx.push(ring(to.color(), 40.0));
                self.say(Ev::Alert, format!("▲ {kind} on {planet} awaits orders"), to.color()).on(cwd).by(id);
            }
            (_, Status::Done) => {
                cues.push(Cue::Done);
                self.fx.push(ring(to.color(), 30.0));
                for i in 0..36 {
                    let a = rnd(i * 7 + (t * 100.0) as u32) * TAU;
                    let sp = 8.0 + rnd(i * 13 + 5) * 20.0;
                    self.fx.push(Fx::Spark { x, y, vx: a.cos() * sp, vy: a.sin() * sp, t0: t, life: 0.6 + rnd(i * 3) * 0.8, col: to.color() });
                }
                let what = if title.is_empty() { String::new() } else { format!(": {title}") };
                self.say(Ev::Agent, format!("✔ {kind} finished on {planet}{what}"), to.color()).on(cwd).by(id);
            }
            (_, Status::Exited) => {
                cues.push(Cue::Gone);
                self.say(Ev::Agent, format!("✝ {kind} on {planet} went dark"), s3(0.55)).on(cwd).by(id);
            }
            (_, Status::Working) => self.fx.push(ring(to.color(), 20.0)),
            _ => {}
        }
    }

    fn tick(&mut self, t: f32, cues: &mut Vec<Cue>) {
        let talking = self.talking_to();
        for a in &mut self.agents {
            let watching = talking == Some(a.id);
            if a.status == Status::Blocked && a.dying.is_none() && !watching && t - a.last_ping > NAG {
                a.last_ping = t;
                cues.push(Cue::Blocked);
                self.fx.push(Fx::Ring { x: a.wpos.0, y: a.wpos.1, t0: t, col: Status::Blocked.color(), speed: 40.0, dur: 1.2 });
            }
        }
        self.fx.retain(|f| match f {
            Fx::Ring { t0, dur, .. } => t - t0 < *dur,
            Fx::Spark { t0, life, .. } => t - t0 < *life,
        });
        self.agents.retain(|a| a.dying.is_none_or(|d| t - d < 1.0));
    }

    /// Group agents into planets and systems, and place everything in orbit.
    fn world(&mut self, t: f32, dt: f32) -> World {
        let mut pmap: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, a) in self.agents.iter().enumerate() {
            pmap.entry(a.cwd.clone()).or_default().push(i);
        }
        // the empire remembers: worlds with no one there now stay on the map, dormant
        let now = survey::now().max(0) as u64;
        for a in self.agents.iter().filter(|a| a.dying.is_none()) {
            if self.survey.worlds.get(&a.cwd).is_some_and(|w| w.host.is_none()) || !self.survey.worlds.contains_key(&a.cwd) {
                self.history.insert(a.cwd.clone(), now);
            }
        }
        if self.history_saved.elapsed() > Duration::from_secs(10) {
            self.history_saved = Instant::now();
            let _ = std::fs::create_dir_all(crate::daemon::dirs_state());
            let _ = std::fs::write(history_file(), serde_json::to_string(&self.history).unwrap_or_default());
        }
        let mut past: Vec<(&String, &u64)> = self.history.iter().filter(|(k, _)| !pmap.contains_key(*k)).collect();
        past.sort_by_key(|(_, t)| std::cmp::Reverse(**t));
        let past: Vec<String> = past.into_iter().take(HISTORY_WORLDS).map(|(k, _)| k.clone()).filter(|k| Path::new(k).is_dir()).collect();
        for k in past {
            if !self.survey.worlds.get(&k).is_some_and(|w| w.host.is_some()) {
                pmap.entry(k).or_default();
            }
        }
        // sector members remain charted even without ships or a recent history entry
        for key in self.sectors.sectors.iter().flat_map(|s| s.worlds.iter()) {
            if Path::new(key).is_dir() { pmap.entry(key.clone()).or_default(); }
        }
        for key in self.hyperlanes.lanes.iter().flat_map(|l| [&l.from, &l.to]) {
            if Path::new(key).is_dir() { pmap.entry(key.clone()).or_default(); }
        }
        for key in self.campaigns.campaigns.iter().filter(|c| !c.archived).flat_map(|c| c.stages.iter().map(|s| &s.world)) {
            if Path::new(key).is_dir() { pmap.entry(key.clone()).or_default(); }
        }
        // worktrees are moons of the world they were split from (which shows up even if empty)
        let host_of: BTreeMap<String, String> = pmap.keys().filter_map(|k| Some((k.clone(), self.survey.worlds.get(k)?.host.clone()?))).collect();
        for h in host_of.values() {
            pmap.entry(h.clone()).or_default();
        }
        let mut smap: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for k in pmap.keys().filter(|k| !host_of.contains_key(*k)) {
            smap.entry(parent(k)).or_default().push(k.clone());
        }
        let n = smap.len();
        let k = 1.0 - (-dt * 3.0).exp();
        // space systems by the size of the biggest one, so nothing overlaps
        let widest = smap.values().map(|p| p.len()).max().unwrap_or(1) as f32;
        let spacing = (ORBIT0 + ORBIT_STEP * (widest - 1.0) + 40.0) * 1.25;
        let mut systems = Vec::new();
        let mut planets: Vec<Planet> = Vec::new();
        for (i, (skey, pkeys)) in smap.iter().enumerate() {
            let target = if n == 1 {
                (0.0, 0.0)
            } else {
                let a = i as f32 * 2.399_963 + 0.6;
                let r = spacing * (i as f32 + 0.6).sqrt();
                (a.cos() * r, a.sin() * r * 0.7)
            };
            let p = self.sys_pos.entry(skey.clone()).or_insert(target);
            *p = (mixf(p.0, target.0, k), mixf(p.1, target.1, k));
            let spos = *p;
            let sh = hash(skey);
            let star = mix(v3(1.0, 0.5, 0.28), v3(0.55, 0.78, 1.0), rnd(sh ^ 173));
            let mut sys = System { key: skey.clone(), name: base_name(skey), wpos: spos, planets: Vec::new(), orbits: Vec::new(), star };
            for (j, pkey) in pkeys.iter().enumerate() {
                let h = hash(pkey);
                let seed = rnd(h);
                let orbit = ORBIT0 + ORBIT_STEP * j as f32;
                let ang = seed * TAU + t * 0.015 / (1.0 + 0.4 * j as f32);
                let mut members = pmap[pkey].clone();
                members.sort_by_key(|&i| self.agents[i].id);
                let live = members.iter().filter(|&&i| self.agents[i].dying.is_none()).count();
                let r = 10.0 + 2.2 * live.min(6) as f32;
                let wpos = (spos.0 + ang.cos() * orbit, spos.1 + ang.sin() * orbit * 0.55);
                let m = members.len().max(1) as f32;
                for (q, &ai) in members.iter().enumerate() {
                    let a = &mut self.agents[ai];
                    let ang = t * (0.15 + a.seed * 0.1) + q as f32 * TAU / m;
                    let orb = r + 8.0 + (q % 2) as f32 * 4.0;
                    a.wpos = (wpos.0 + ang.cos() * orb, wpos.1 + ang.sin() * orb * 0.55);
                    a.front = ang.sin() > 0.0;
                }
                sys.orbits.push(orbit);
                sys.planets.push(planets.len());
                let (dormant, fog, last_seen) = self.fog(pkey, live, now);
                planets.push(Planet { key: pkey.clone(), name: base_name(pkey), agents: members, terrain: (h >> 8) as u8 % 7, seed, wpos, r, host: None, moons: Vec::new(), dormant, fog, last_seen, level: self.level(pkey) });
            }
            systems.push(sys);
        }
        // moons, in close orbit around their worlds
        for (mkey, hkey) in &host_of {
            let Some(hi) = planets.iter().position(|p| &p.key == hkey) else { continue };
            let q = planets[hi].moons.len() as f32;
            let (hpos, hr) = (planets[hi].wpos, planets[hi].r);
            let h = hash(mkey);
            let seed = rnd(h);
            let orbit = hr + 22.0 + 12.0 * q;
            let ang = seed * TAU + t * 0.08 / (1.0 + 0.5 * q);
            let wpos = (hpos.0 + ang.cos() * orbit, hpos.1 + ang.sin() * orbit * 0.55);
            let mut members = pmap[mkey].clone();
            members.sort_by_key(|&i| self.agents[i].id);
            let live = members.iter().filter(|&&i| self.agents[i].dying.is_none()).count();
            let r = 5.0 + 1.0 * live.min(4) as f32;
            let m = members.len().max(1) as f32;
            for (q, &ai) in members.iter().enumerate() {
                let a = &mut self.agents[ai];
                let ang = t * (0.2 + a.seed * 0.1) + q as f32 * TAU / m;
                let orb = r + 5.0;
                a.wpos = (wpos.0 + ang.cos() * orb, wpos.1 + ang.sin() * orb * 0.55);
                a.front = ang.sin() > 0.0;
            }
            let name = self.survey.worlds.get(mkey).map(|w| w.branch.clone()).filter(|b| !b.is_empty()).unwrap_or_else(|| base_name(mkey));
            let idx = planets.len();
            planets[hi].moons.push(idx);
            // moons are grey, cratered rock
            planets.push(Planet { key: mkey.clone(), name, agents: members, terrain: 3, seed, wpos, r, host: Some(hi), moons: Vec::new(), dormant: live == 0, fog: 0.0, last_seen: now, level: 0 });
        }
        let mut lanes = Vec::new();
        for i in 0..systems.len() {
            let mut d: Vec<(f32, usize)> = (0..systems.len())
                .filter(|&j| j != i)
                .map(|j| {
                    let (a, b) = (systems[i].wpos, systems[j].wpos);
                    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2), j)
                })
                .collect();
            d.sort_by(|a, b| a.0.total_cmp(&b.0));
            for &(_, j) in d.iter().take(2) {
                let e = (i.min(j), i.max(j));
                if !lanes.contains(&e) {
                    lanes.push(e);
                }
            }
        }
        // the trade station keeps to the edge of known space, up and to the right
        let traders = planets.iter().any(|p| self.survey.worlds.get(&p.key).is_some_and(|w| w.github));
        let station = (traders && !systems.is_empty()).then(|| {
            let maxx = systems.iter().map(|s| s.wpos.0).fold(f32::MIN, f32::max);
            let miny = systems.iter().map(|s| s.wpos.1).fold(f32::MAX, f32::min);
            let target = (maxx + spacing * 0.75, miny - spacing * 0.35);
            let p = self.sys_pos.entry("\0station".into()).or_insert(target);
            *p = (mixf(p.0, target.0, k), mixf(p.1, target.1, k));
            *p
        });
        World { systems, planets, lanes, station }
    }

    /// Outliner order: systems, their planets, their agents (most urgent first).
    pub fn rows(&self, w: &World) -> Vec<Sel> {
        let mut v = Vec::new();
        if w.station.is_some() { v.push(Sel::GitHub); }
        for s in &w.systems {
            v.push(Sel::Sys(s.key.clone()));
            if self.sector_map() {
                v.extend(self.sectors.sectors.iter().filter(|g| s.planets.iter().any(|&i| self.sector_has(g, &w.planets[i].key))).map(|g| Sel::Sector(g.id, s.key.clone())));
            }
            for &pi in &s.planets {
                let p = &w.planets[pi];
                for p in std::iter::once(p).chain(p.moons.iter().map(|&m| &w.planets[m])) {
                    v.push(Sel::Planet(p.key.clone()));
                    let mut ags: Vec<&Agent> = p.agents.iter().map(|&i| &self.agents[i]).filter(|a| a.dying.is_none()).collect();
                    ags.sort_by_key(|a| (a.status, a.id));
                    v.extend(ags.iter().map(|a| Sel::Agent(a.id)));
                }
            }
        }
        v
    }

    pub fn sel_pos(&self, w: &World) -> Option<(f32, f32)> {
        match self.sel.as_ref()? {
            Sel::GitHub => w.station,
            Sel::Lane(id) => {
                let lane = self.hyperlanes.lanes.iter().find(|l| l.id == *id)?;
                let (a, b) = (w.planet(&lane.from)?, w.planet(&lane.to)?);
                Some(((a.wpos.0 + b.wpos.0) * 0.5, (a.wpos.1 + b.wpos.1) * 0.5))
            }
            Sel::Sector(id, k) => self.sector_regions(w).into_iter().find(|r| r.id == *id && r.system == *k).map(|r| r.center()),
            Sel::Sys(k) => w.systems.iter().find(|s| &s.key == k).map(|s| s.wpos),
            Sel::Planet(k) => w.planet(k).map(|p| p.wpos),
            Sel::Agent(id) => self.agent(*id).map(|a| a.wpos),
        }
    }

    /// Keep a selected ship or moon associated with its star in the distant overview.
    pub fn selected_system<'a>(&self, w: &'a World) -> Option<&'a System> {
        if let Some(Sel::Sys(k) | Sel::Sector(_, k)) = &self.sel {
            return w.systems.iter().find(|s| &s.key == k);
        }
        let key = self.sel_planet()?;
        w.planets.iter().position(|p| p.key == key).and_then(|i| w.system_of(i))
    }

    /// The planet (folder) the selection belongs to.
    pub fn sel_planet(&self) -> Option<String> {
        match self.sel.as_ref()? {
            Sel::Planet(k) => Some(k.clone()),
            Sel::Agent(id) => self.agent(*id).map(|a| a.cwd.clone()),
            Sel::Sys(_) | Sel::GitHub | Sel::Sector(_, _) | Sel::Lane(_) => None,
        }
    }

    fn fit(&mut self, w: &World, l: &Lay) {
        if w.systems.is_empty() {
            (self.cam.tx, self.cam.ty, self.cam.tz) = (0.0, 0.0, 1.0);
            return;
        }
        let pad = 60.0;
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for s in &w.systems {
            let r = s.orbits.last().copied().unwrap_or(0.0) + 30.0;
            x0 = x0.min(s.wpos.0 - r);
            x1 = x1.max(s.wpos.0 + r);
            y0 = y0.min(s.wpos.1 - r * 0.55);
            y1 = y1.max(s.wpos.1 + r * 0.55);
        }
        if let Some((x, y)) = w.station {
            (x0, x1, y0, y1) = (x0.min(x - 40.0), x1.max(x + 40.0), y0.min(y - 30.0), y1.max(y + 30.0));
        }
        self.cam.tx = (x0 + x1) * 0.5;
        self.cam.ty = (y0 + y1) * 0.5;
        self.cam.tz = (l.vw / (x1 - x0 + pad)).min(l.vh / (y1 - y0 + pad)).clamp(0.12, 2.2);
    }

    fn glide_to_sel(&mut self, w: &World) {
        if let Some((x, y)) = self.sel_pos(w) {
            (self.cam.tx, self.cam.ty) = (x, y);
        }
    }

    fn zoom_at(&mut self, l: &Lay, factor: f32, at: (f32, f32)) {
        let before = l.to_world(&self.cam, at);
        let z = (self.cam.z * factor).clamp(0.12, 6.0);
        self.cam.z = z;
        self.cam.tz = z;
        let after = l.to_world(&self.cam, at);
        self.cam.x += before.0 - after.0;
        self.cam.y += before.1 - after.1;
        (self.cam.tx, self.cam.ty) = (self.cam.x, self.cam.y);
    }

    /// The most urgent agent on a planet.
    fn lead_agent(&self, planet: &str) -> Option<u32> {
        self.agents.iter().filter(|a| a.cwd == planet && a.dying.is_none()).min_by_key(|a| (a.status, a.id)).map(|a| a.id)
    }

    /// Ask Advisor about the mission being written once the text has rested for a second.
    fn consult_advisor(&mut self, client: &Client) {
        let Mode::Brief { planet, text, sending: false, .. } = &self.mode else {
            if !matches!(self.mode, Mode::Brief { .. }) {
                self.advisor = AdvisorState::default();
            }
            return;
        };
        let key = format!("{planet}\0{}", text.s.trim());
        if key != self.advisor.seen {
            self.advisor.seen = key;
            self.advisor.seen_at = Some(Instant::now());
            return;
        }
        let rested = self.advisor.seen_at.is_some_and(|at| at.elapsed() > Duration::from_millis(1000));
        if !rested || key == self.advisor.asked || text.s.trim().chars().count() < 8 || !crate::advisor::available() {
            return;
        }
        let ships = brief_ships(&self.agents, planet)
            .into_iter()
            .filter_map(|id| self.agent(id))
            .map(|a| crate::advisor::Ship { id: a.id, kind: a.kind.clone(), role: a.role, status: a.status.label().to_string(), task: a.title.clone(), turns: a.turns, ctx: a.report.ctx })
            .collect();
        self.advisor.asked = key.clone();
        self.advisor.thinking = true;
        client.ask_advisor(key, text.s.trim().to_string(), base_name(planet), ships);
    }

    /// Advisor's answer: shown, and preselected when it's sure and the player hasn't chosen.
    fn advisor_answered(&mut self, key: String, result: Result<crate::advisor::Pick, String>) {
        if key != self.advisor.asked {
            return; // the text has moved on
        }
        self.advisor.thinking = false;
        match result {
            Ok(pick) => {
                self.advisor.err = None;
                if !self.advisor.touched
                    && pick.ship_conf >= ADVISOR_SURE
                    && let Mode::Brief { planet, launch, role, to, .. } = &mut self.mode
                {
                    match pick.ship {
                        Some(id) if brief_ships(&self.agents, planet).contains(&id) => {
                            *launch = false;
                            *to = Some(id);
                        }
                        Some(_) => {}
                        None => {
                            *launch = true;
                            *to = None;
                            if pick.role_conf >= ADVISOR_SURE {
                                *role = ROLES.iter().position(|r| *r == pick.role).unwrap_or(0);
                            }
                        }
                    }
                }
                self.advisor.pick = Some(pick);
            }
            Err(e) => self.advisor.err = Some(e),
        }
    }

    /// The agent whose terminal is on screen, in the chat popup or full screen.
    pub fn talking_to(&self) -> Option<u32> {
        match self.mode {
            Mode::Chat { id, .. } | Mode::Comms(id) => Some(id),
            _ => None,
        }
    }

    /// Talk to an agent: the chat popup first; ⇥ there goes full screen.
    fn open_comms(&mut self, id: u32, client: &Client, l: &Lay) {
        self.mode = Mode::Chat { id, text: Line::default(), attachments: Vec::new(), sending: false };
        self.comms_at = Instant::now();
        self.screen = None;
        self.scroll = 0;
        self.pick = None;
        client.watch(Some(id));
        self.fit_comms(client, l);
        client.send(Cmd::Seen(id));
    }

    /// From the chat popup to the full-screen terminal.
    fn fullscreen_comms(&mut self, id: u32, client: &Client, l: &Lay) {
        self.mode = Mode::Comms(id);
        self.screen = None;
        self.scroll_by(-(self.scroll as isize), client);
        self.fit_comms(client, l);
    }

    /// Size the agent's terminal to the window it's shown in.
    fn fit_comms(&self, client: &Client, l: &Lay) {
        let Some(id) = self.talking_to() else { return };
        let (_, _, cols, rows) = self.terminal_rect(l);
        client.send(Cmd::Resize(id, cols.max(2) as u16, rows.max(2) as u16));
    }

    pub fn terminal_rect(&self, l: &Lay) -> (i32, i32, i32, i32) {
        match &self.mode {
            Mode::Chat { text, attachments, .. } => draw::chat_terminal(l, text, attachments.len()),
            _ => (1, 2, l.cols - 2, l.rows - 3),
        }
    }

    /// Click and drag in comms selects text, like a terminal: double-click a word,
    /// triple-click a line, and letting go copies it.
    fn select(&mut self, me: event::MouseEvent, l: &Lay, t: f32) {
        let (x, y, iw, ih) = self.terminal_rect(l);
        let cell = ((me.row as i32 - y).clamp(0, ih - 1), (me.column as i32 - x).clamp(0, iw - 1));
        match me.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let (at, when, n) = self.click;
                let n = if at == cell && when.elapsed() < Duration::from_millis(400) { n % 3 + 1 } else { 1 };
                self.click = (cell, Instant::now(), n);
                self.pick = match n {
                    1 => Some(Pick { a: cell, b: cell }),
                    2 => self.word_at(cell),
                    _ => Some(Pick { a: (cell.0, 0), b: (cell.0, iw - 1) }),
                };
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(p) = &mut self.pick {
                    p.b = cell;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => match self.pick {
                // a plain click selects nothing
                Some(p) if p.a == p.b && self.click.2 == 1 => self.pick = None,
                Some(_) => self.copy(t),
                None => {}
            },
            _ => {}
        }
    }

    /// The comms screen as rows of characters (wide-glyph continuations dropped).
    fn screen_rows(&self) -> Vec<Vec<char>> {
        let Some((_, s)) = &self.screen else { return Vec::new() };
        s.lines.iter().map(|l| l.iter().flat_map(|r| r.t.chars()).collect()).collect()
    }

    fn word_at(&self, (r, c): (i32, i32)) -> Option<Pick> {
        let rows = self.screen_rows();
        let row = rows.get(r as usize)?;
        let word = |ch: char| !ch.is_whitespace() && !"│┃|\"'`()[]{}<>,;".contains(ch);
        if !row.get(c as usize).is_some_and(|&ch| word(ch)) {
            return None;
        }
        let mut a = c as usize;
        while a > 0 && word(row[a - 1]) {
            a -= 1;
        }
        let mut b = c as usize;
        while b + 1 < row.len() && word(row[b + 1]) {
            b += 1;
        }
        Some(Pick { a: (r, a as i32), b: (r, b as i32) })
    }

    fn copy(&mut self, t: f32) {
        let text = self.selected_text();
        if text.trim().is_empty() { return; }
        let n = text.chars().count();
        self.toast(if copy_to_clipboard(&text) { format!("copied {n} characters") } else { "✖ no clipboard (install wl-copy or xclip)".into() }, t);
    }

    fn selected_text(&self) -> String {
        let Some(p) = self.pick else { return String::new() };
        let ((r0, c0), (r1, c1)) = p.span();
        let rows = self.screen_rows();
        let mut lines = Vec::new();
        for r in r0..=r1 {
            let row = rows.get(r as usize).map_or(&[][..], |v| v.as_slice());
            let from = if r == r0 { c0 as usize } else { 0 };
            let to = if r == r1 { (c1 as usize + 1).min(row.len()) } else { row.len() };
            let s: String = row.get(from.min(to)..to).unwrap_or(&[]).iter().filter(|&&c| c != '\0').collect();
            lines.push(s.trim_end().to_string());
        }
        lines.join("\n")
    }

    /// Scroll the comms window back (positive) or toward live (negative).
    fn scroll_by(&mut self, lines: isize, client: &Client) {
        let max = self.screen.as_ref().map_or(0, |(_, s)| s.history);
        let next = (self.scroll as isize).saturating_add(lines).clamp(0, max as isize) as usize;
        if next != self.scroll {
            self.scroll = next;
            self.pick = None;
            client.scroll(next);
        }
    }

    fn close_comms(&mut self, client: &Client) {
        if let Mode::Chat { attachments, sending, .. } = &self.mode {
            if *sending { return; }
            screenshots::discard(attachments);
        }
        client.watch(None);
        self.screen = None;
        self.mode = if std::mem::take(&mut self.campaign_return) { Mode::Campaigns } else if std::mem::take(&mut self.commander_return) { Mode::Commander } else { self.planet_return.take().map_or(Mode::Galaxy, Mode::Planet) };
    }

    fn chat_sent(&mut self, id: u32, result: Result<(), String>, client: &Client, l: &Lay, t: f32) {
        let Mode::Chat { id: current, text, attachments, sending } = &mut self.mode else { return };
        if *current != id || !*sending { return; }
        *sending = false;
        match result {
            Ok(()) => {
                // Submitted image copies must remain available for the agent to read.
                attachments.clear();
                *text = Line::default();
                self.fit_comms(client, l);
                self.toast("Reply sent", t);
            }
            Err(e) => self.toast(format!("Not sent: {e}. Your draft is kept."), t),
        }
    }

    /// Scroll either the embedded application or the emulator's history, using
    /// coordinates relative to the terminal, never the outer popup.
    fn terminal_scroll(&mut self, client: &Client, l: &Lay, lines: isize, at: Option<(i32, i32)>) {
        let Some(id) = self.talking_to() else { return };
        let Some((_, s)) = &self.screen else { return };
        let (tx, ty, width, height) = self.terminal_rect(l);
        let (c, r) = at.unwrap_or((tx + width / 2, ty + height / 2));
        if c < tx || c >= tx + width || r < ty || r >= ty + height { return; }
        if s.mouse && self.scroll == 0 {
            let (x, y) = (c - tx + 1, r - ty + 1);
            let b = if lines > 0 { 64 } else { 65 };
            let sequence = if s.sgr { format!("\x1b[<{b};{x};{y}M") } else {
                let enc = |v: i32| char::from_u32((32 + v).min(255) as u32).unwrap_or(' ');
                format!("\x1b[M{}{}{}", enc(b), enc(x), enc(y))
            };
            self.pick = None;
            // A wheel event is one tick; page keys send enough ticks for a page.
            client.send(Cmd::Input(id, sequence.repeat(if at.is_some() { 1 } else { (lines.unsigned_abs() / 3).max(1) })));
        } else if !s.alt {
            self.scroll_by(lines, client);
        }
    }

    /// Enter on the current selection.
    fn activate(&mut self, w: &World, l: &Lay, _client: &Client) {
        match self.sel.clone() {
            Some(Sel::GitHub) => self.mode = Mode::Trade { sel: 0 },
            Some(Sel::Lane(_)) => self.open_hyperlanes(),
            Some(Sel::Agent(id)) => self.mode = Mode::Ship { id, sel: 0 },
            Some(Sel::Planet(k)) => self.mode = Mode::Planet(PlanetView::new(k)),
            Some(Sel::Sector(id, k)) => {
                if let Some(r) = self.sector_regions(w).into_iter().find(|r| r.id == id && r.system == k) {
                    (self.cam.tx, self.cam.ty) = r.center();
                    let (x0, y0, x1, y1) = r.bounds;
                    self.cam.tz = (l.vw / (x1 - x0 + 60.0)).min(l.vh / (y1 - y0 + 60.0)).clamp(SECTOR_ZOOM + 0.15, 2.5);
                    self.sector_focus = Some(id);
                }
            }
            Some(Sel::Sys(k)) => {
                if let Some(s) = w.systems.iter().find(|s| s.key == k) {
                    let r = s.orbits.last().copied().unwrap_or(40.0) + 40.0;
                    (self.cam.tx, self.cam.ty) = s.wpos;
                    self.cam.tz = if self.cam.z < OVERVIEW_ZOOM && (!self.sectors.sectors.is_empty() || self.dependencies) {
                        (OVERVIEW_ZOOM + SECTOR_ZOOM) * 0.5
                    } else {
                        let min = if self.sectors.sectors.is_empty() && !self.dependencies { OVERVIEW_ZOOM + 0.1 } else { SECTOR_ZOOM + 0.15 };
                        (l.vw / (2.0 * r)).min(l.vh / (1.2 * r)).clamp(min, 4.0)
                    };
                }
            }
            None => {}
        }
    }

    fn spawn(&mut self, kind: &str, cwd: &Path, client: &Client, t: f32) {
        let cwd = cwd.to_string_lossy().trim_end_matches('/').to_string();
        let cwd = if cwd.is_empty() { "/".into() } else { cwd };
        client.send(Cmd::Spawn { kind: kind.into(), cwd: cwd.clone() });
        self.toast(format!("◆ launching {kind} to {}…", base_name(&cwd)), t);
        self.mode = Mode::Galaxy;
    }
}

// ───────────────────────────────── keys ────────────────────────────────

/// Encode a key press for a terminal application.
fn key_bytes(k: &KeyEvent) -> Option<String> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let s = match k.code {
        KeyCode::Char(c) if ctrl => {
            let b = (c.to_ascii_lowercase() as u8) & 0x1f;
            (b as char).to_string()
        }
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "\r".into(),
        KeyCode::Backspace => "\x7f".into(),
        KeyCode::Tab => "\t".into(),
        KeyCode::BackTab => "\x1b[Z".into(),
        KeyCode::Esc => "\x1b".into(),
        KeyCode::Up => "\x1b[A".into(),
        KeyCode::Down => "\x1b[B".into(),
        KeyCode::Right => "\x1b[C".into(),
        KeyCode::Left => "\x1b[D".into(),
        KeyCode::Home => "\x1b[H".into(),
        KeyCode::End => "\x1b[F".into(),
        KeyCode::PageUp => "\x1b[5~".into(),
        KeyCode::PageDown => "\x1b[6~".into(),
        KeyCode::Delete => "\x1b[3~".into(),
        KeyCode::Insert => "\x1b[2~".into(),
        KeyCode::F(n @ 1..=4) => format!("\x1bO{}", (b'P' + n - 1) as char),
        _ => return None,
    };
    Some(if alt { format!("\x1b{s}") } else { s })
}

/// Something a world needs from you, and the quick action that deals with it.
pub struct Need {
    /// Short form for the map ("±7"), and the full sentence for cards and menus.
    pub badge: String,
    pub label: String,
    pub col: V3,
    pub act: Option<Act>,
}

pub enum AttentionTarget {
    Anomaly(u64),
    Agent(u32),
    Needs,
    Pr(survey::Pr),
    Deploy,
}

pub struct Attention {
    pub id: String,
    pub world: String,
    pub kind: &'static str,
    pub title: String,
    pub detail: String,
    pub target: AttentionTarget,
    rank: u8,
}

impl App {
    /// Only actionable work, across every charted system; ordinary git dirt is not an alert.
    pub fn attention(&self, w: &World) -> Vec<Attention> {
        let mut rows = Vec::new();
        for a in self.agents.iter().filter(|a| a.dying.is_none() && a.status.needs_you()) {
            let waiting = a.status == Status::Blocked;
            let title = a.ask.as_ref().filter(|_| waiting).map_or_else(|| {
                if a.title.is_empty() { format!("{} #{} {}", a.kind, a.id, if waiting { "needs your input" } else { "finished work" }) }
                else { a.title.clone() }
            }, |q| q.question.clone());
            let mut detail = format!("{} #{} · {}\n{}", a.kind, a.id, a.status.label(), a.title);
            if let Some(q) = a.ask.as_ref().filter(|_| waiting) {
                detail.push_str(&format!("\n{}\n{}", q.context.join("\n"), q.question));
                for o in &q.options { detail.push_str(&format!("\n{}: {}", o.key, o.label)); }
            }
            rows.push(Attention { id: format!("agent:{}", a.id), world: a.cwd.clone(), kind: if waiting { "Input" } else { "Finished" }, title, detail, target: AttentionTarget::Agent(a.id), rank: if waiting { 0 } else { 5 } });
        }
        for p in &w.planets {
            let here: Vec<_> = self.agents.iter().filter(|a| a.cwd == p.key && a.dying.is_none()).collect();
            let files: std::collections::BTreeSet<_> = here.iter().flat_map(|a| a.war.iter()).flat_map(|war| war.files.iter().cloned()).collect();
            if !files.is_empty() {
                rows.push(Attention { id: format!("conflict:{}", p.key), world: p.key.clone(), kind: "Conflict", title: "Agents are editing the same files".into(), detail: format!("Choose which agent should have priority.\n{}", files.into_iter().collect::<Vec<_>>().join("\n")), target: AttentionTarget::Needs, rank: 1 });
            }
            let queued = self.missions.iter().filter(|m| m.cwd == p.key && m.state == "queued").count();
            if queued > 0 && !here.iter().any(|a| a.status != Status::Exited) {
                rows.push(Attention { id: format!("uncrewed:{}", p.key), world: p.key.clone(), kind: "Unstaffed", title: format!("{queued} queued missions need an agent"), detail: self.missions.iter().filter(|m| m.cwd == p.key && m.state == "queued").map(|m| m.text.as_str()).collect::<Vec<_>>().join("\n\n"), target: AttentionTarget::Deploy, rank: 4 });
            }
            // Moons share their host's PRs: show each request only once.
            if p.host.is_some() { continue; }
            let Some(s) = self.survey.worlds.get(&p.key) else { continue };
            for (pr, mine) in s.prs.iter().filter(|pr| pr.ci == "fail").map(|pr| (pr, true))
                .chain(s.others.iter().filter(|pr| pr.asks_you).map(|pr| (pr, false))) {
                let freshness = if let Some(error) = &s.prs_error { format!("Cached results · {error}") }
                    else if let Some(at) = s.prs_checked { format!("Checked {} seconds ago", (survey::now() - at).max(0)) }
                    else { "Waiting for GitHub survey".into() };
                rows.push(Attention { id: format!("pr:{}:{}", p.key, pr.number), world: p.key.clone(), kind: if mine { "CI failed" } else { "Review" }, title: format!("#{} {}", pr.number, pr.title), detail: format!("{}\n{}\n{freshness}", if mine { "Your pull request has failing checks." } else { "Your review has been requested." }, pr.title), target: AttentionTarget::Pr(pr.clone()), rank: if mine { 2 } else { 3 } });
            }
        }
        for f in self.anomalies.findings.iter().filter(|f| f.state == crate::anomaly::State::Open && f.kind != "CI") {
            rows.push(Attention { id: format!("anomaly:{}", f.id), world: f.world.clone(), kind: "Anomaly", title: f.title.clone(), detail: f.evidence.clone(), target: AttentionTarget::Anomaly(f.id), rank: 4 });
        }
        rows.sort_by(|a, b| (a.rank, &a.world, &a.id).cmp(&(b.rank, &b.world, &b.id)));
        rows
    }

    pub fn attention_selection(&self, rows: &[Attention]) -> usize {
        rows.iter().position(|r| r.id == self.commander).unwrap_or(0)
    }

    fn open_commander(&mut self, w: &World) {
        let rows = self.attention(w);
        let id = rows.get(self.attention_selection(&rows)).map_or(String::new(), |r| r.id.clone());
        if self.commander != id { self.commander_detail = 0; }
        self.commander = id;
        self.mode = Mode::Commander;
    }
}

impl App {
    fn edit_anomalies(&mut self, change: impl FnOnce(&mut crate::anomaly::Book)) {
        let result = match &self.anomaly_path {
            Some(path) => crate::anomaly::edit(path, change),
            None => { change(&mut self.anomalies); Ok(self.anomalies.clone()) }
        };
        match result {
            Ok(book) => { self.anomalies = book; self.anomaly_error.clear(); }
            Err(e) => {
                let msg = format!("Anomalies could not be saved or loaded: {e}");
                if self.anomaly_error != msg { self.say(Ev::Error, msg.clone(), RED); }
                self.anomaly_error = msg;
            }
        }
    }

    fn refresh_anomalies(&mut self) {
        use crate::anomaly::{Observation, State, Event};
        let mut seen = Vec::new();
        for (path, w) in &self.survey.worlds {
            let host = w.host.as_ref().unwrap_or(path);
            let obs = |key: String, kind: &str, title: String, evidence: String, at: i64, present: bool| Observation {
                key, kind: kind.into(), world: path.clone(), host: host.clone(), title, evidence, at, present,
            };
            if let Some(at) = w.checked {
                seen.push(obs("conflicts".into(), "Git", "Unresolved merge conflicts".into(), format!("git status reports unmerged files:\n{}", w.conflicts.join("\n")), at, !w.conflicts.is_empty()));
                if w.host.is_some() && self.loaded && self.online {
                    let crewed = self.agents.iter().any(|a| a.cwd == *path && a.dying.is_none() && a.status != Status::Exited);
                    seen.push(obs("uncrewed".into(), "Moon", "Worktree moon has no ship".into(), format!("Branch: {}\nHost: {host}\nNo running agent is assigned here. This does not establish that the worktree is abandoned.", w.branch), at, !crewed));
                }
            }
            if let Some(at) = w.prs_checked.filter(|_| w.prs_error.is_none() && w.host.is_none()) {
                for pr in w.prs.iter().filter(|p| p.ci == "fail" || p.ci == "pass") {
                    seen.push(obs(format!("ci:{}", pr.number), "CI", format!("PR #{} has failing checks", pr.number), format!("{}\nAuthor: {}\nGitHub check rollup: {}", pr.title, pr.author, pr.ci), at, pr.ci == "fail"));
                }
            }
        }
        let missions = self.missions.clone();
        self.edit_anomalies(|book| {
            for o in seen { book.observe(o); }
            for f in &mut book.findings {
                let tag = format!("{} ", f.tag());
                for m in &missions {
                    if m.cwd != f.world || !m.text.starts_with(&tag) || (m.at as i64) < f.first { continue; }
                    if !f.missions.contains(&m.id) {
                        f.missions.push(m.id);
                        f.history.push(Event { at: m.at as i64, note: format!("Investigation mission #{} linked", m.id) });
                        if f.state == State::Open { f.state = State::Investigating; }
                    }
                    if m.state == "done" {
                        let note = format!("Mission #{} finished its turn; finding still needs verification", m.id);
                        if !f.history.iter().any(|h| h.note == note) { f.history.push(Event { at: m.done_at.unwrap_or(m.at) as i64, note }); }
                    }
                }
            }
        });
    }

    pub fn anomaly_count(&self, key: &str, system: bool) -> usize {
        self.anomalies.findings.iter().filter(|f| f.state.active() && if system { f.system == key } else { f.world == key || f.host == key }).count()
    }

    pub fn anomaly_rows(&self) -> Vec<&crate::anomaly::Finding> { self.anomaly_filter.rows(&self.anomalies) }

    fn open_anomalies(&mut self) {
        self.anomaly_filter = crate::anomaly::Filter::default();
        self.anomaly_detail = 0;
        self.mode = Mode::Anomalies { id: self.anomaly_rows().first().map_or(0, |f| f.id), typing: false };
    }

    fn investigate_anomaly(&mut self, id: u64, t: f32) {
        let Some(f) = self.anomalies.findings.iter().find(|f| f.id == id) else { return };
        if !Path::new(&f.world).is_dir() { self.toast("this world's folder is unavailable", t); return; }
        let mut text = Line::default();
        text.set(format!("{} Investigate: {}\n\nWorld: {}\nSource: {}\nObserved evidence:\n{}\n\nInvestigate read-only. Verify whether this still holds and report supporting evidence, possible causes and a concrete next step. Do not fix, commit, merge, push or delete anything. Report uncertainty explicitly.\n\nInclude any additional findings in your reply with their world, evidence and uncertainty. This finding already exists; report its outcome in your reply.", f.tag(), f.title, f.world, f.kind, f.evidence));
        let planet = f.world.clone();
        let role = ROLES.iter().position(|r| *r == Role::Scout).unwrap_or(0);
        self.anomaly_return = Some(id);
        self.advisor.touched = true;
        self.mode = Mode::Brief { planet, text, kind: 0, launch: true, role, to: None, attachments: vec![], sending: false, ship: Outfit::default() };
    }
}

fn anomaly_input(app: &mut App, w: &World, l: &Lay, ev: &Event, id: u64, mut typing: bool, t: f32) {
    use crate::anomaly::{State, Filter, KINDS, STATES};
    let ids: Vec<_> = app.anomaly_rows().iter().map(|f| f.id).collect();
    let mut sel = ids.iter().position(|&n| n == id).unwrap_or(0);
    let mut action = None;
    match ev {
        Event::Key(k) if k.kind == KeyEventKind::Release => return,
        Event::Paste(s) if typing => app.anomaly_filter.query.extend(s.chars().filter(|c| !c.is_control())),
        Event::Key(k) if typing => match k.code {
            KeyCode::Esc | KeyCode::Enter => typing = false,
            KeyCode::Backspace => { app.anomaly_filter.query.pop(); }
            KeyCode::Char(c) => app.anomaly_filter.query.push(c),
            _ => {}
        },
        Event::Key(k) => match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('A') => { app.mode = Mode::Galaxy; return; }
            KeyCode::Up | KeyCode::Char('k') => sel = sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => sel = (sel + 1).min(ids.len().saturating_sub(1)),
            KeyCode::Home => sel = 0,
            KeyCode::End => sel = ids.len().saturating_sub(1),
            KeyCode::PageUp => app.anomaly_detail = app.anomaly_detail.saturating_sub(4),
            KeyCode::PageDown => app.anomaly_detail = app.anomaly_detail.saturating_add(4),
            KeyCode::Char('/') => typing = true,
            KeyCode::Char('f') => app.anomaly_filter.kind = (app.anomaly_filter.kind + 1) % KINDS.len(),
            KeyCode::Char('v') => app.anomaly_filter.state = (app.anomaly_filter.state + 1) % STATES.len(),
            KeyCode::Char('w') => {
                if app.anomaly_filter.world.take().is_none() {
                    app.anomaly_filter.world = ids.get(sel).and_then(|id| app.anomalies.findings.iter().find(|f| f.id == *id)).map(|f| f.host.clone());
                }
            }
            KeyCode::Char('s') => {
                if app.anomaly_filter.system.take().is_none() {
                    app.anomaly_filter.system = ids.get(sel).and_then(|id| app.anomalies.findings.iter().find(|f| f.id == *id)).map(|f| f.system.clone());
                }
            }
            KeyCode::Char('x') => app.anomaly_filter = Filter::default(),
            KeyCode::Enter => action = Some('l'),
            KeyCode::Char(c @ ('t' | 'd' | 'i' | 'r' | 'o')) => action = Some(c),
            _ => {}
        },
        Event::Mouse(m) => match m.kind {
            MouseEventKind::ScrollUp => sel = sel.saturating_sub(1),
            MouseEventKind::ScrollDown => sel = (sel + 1).min(ids.len().saturating_sub(1)),
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(i) = draw::anomaly_hit(l, ids.len(), sel, m.column as i32, m.row as i32) {
                    if i == sel { action = Some('l'); }
                    sel = i;
                }
            }
            _ => {}
        },
        _ => {}
    }
    let rows: Vec<_> = app.anomaly_rows().iter().map(|f| f.id).collect();
    let selected = if rows == ids { rows.get(sel).copied() } else { rows.iter().copied().find(|n| *n == id).or(rows.first().copied()) }.unwrap_or(0);
    if selected != id { app.anomaly_detail = 0; }
    app.mode = Mode::Anomalies { id: selected, typing };
    let Some(f) = app.anomalies.findings.iter().find(|f| f.id == selected) else { return };
    let world = f.world.clone();
    match action {
        Some('t') => app.investigate_anomaly(selected, t),
        Some('l') => {
            if w.planet(&world).is_none() && !Path::new(&world).is_dir() { app.toast("this world's folder is unavailable", t); return; }
            app.history.insert(world.clone(), survey::now().max(0) as u64);
            app.sel = Some(Sel::Planet(world));
            let map = app.world(t, 0.0);
            app.glide_to_sel(&map);
            app.cam.tz = 1.6;
            app.mode = Mode::Galaxy;
        }
        Some(c) => {
            let state = match c { 'd' => State::Deferred, 'i' => State::Dismissed, 'r' => State::Resolved, _ => State::Open };
            app.edit_anomalies(|b| b.set_state(selected, state, survey::now()));
            app.anomaly_detail = 0;
        }
        None => {}
    }
}


impl App {
    pub fn sector_has(&self, sector: &crate::sector::Sector, path: &str) -> bool {
        sector.contains(path, self.survey.worlds.get(path).and_then(|w| w.host.as_deref()))
    }

    pub fn focused_sector(&self) -> Option<&crate::sector::Sector> {
        self.sector_focus.and_then(|id| self.sectors.sectors.iter().find(|s| s.id == id))
    }

    pub fn selected_sector(&self) -> Option<&crate::sector::Sector> {
        self.sectors.sectors.iter().find(|s| s.id == self.sector_view.id).or(self.sectors.sectors.first())
    }

    pub fn sector_worlds(&self, w: &World) -> Vec<String> {
        let mut worlds: std::collections::BTreeSet<String> = w.planets.iter().map(|p| p.host.map_or_else(|| p.key.clone(), |i| w.planets[i].key.clone())).collect();
        if let Some(s) = self.selected_sector() { worlds.extend(s.worlds.iter().cloned()); }
        worlds.into_iter().collect()
    }

    pub fn sector_stats(&self, sector: &crate::sector::Sector) -> SectorStats {
        let mut stats = SectorStats { worlds: sector.worlds.len(), ..Default::default() };
        for a in self.agents.iter().filter(|a| a.dying.is_none() && a.status != Status::Exited && self.sector_has(sector, &a.cwd)) {
            stats.ships += 1;
            match a.status { Status::Working => stats.working += 1, Status::Blocked => stats.blocked += 1, Status::Done => stats.finished += 1, _ => {} }
        }
        for m in self.missions.iter().filter(|m| self.sector_has(sector, &m.cwd)) {
            match m.state.as_str() { "active" | "sent" => stats.missions += 1, "queued" => stats.queued += 1, _ => {} }
        }
        stats.anomalies = self.anomalies.findings.iter().filter(|f| f.state.active() && sector.contains(&f.world, Some(&f.host))).count();
        stats
    }

    fn sync_sectors(&mut self, change: impl FnOnce(&mut crate::sector::Book) -> Result<u64, String>) -> Result<u64, String> {
        let mut result = Ok(0);
        let apply = |b: &mut crate::sector::Book| { result = change(b); };
        let loaded = match &self.sector_path {
            Some(path) => crate::sector::edit(path, apply),
            None => { apply(&mut self.sectors); Ok(self.sectors.clone()) }
        };
        match loaded {
            Ok(book) => {
                self.sectors = book;
                self.sector_error.clear();
                if self.sector_focus.is_some() && self.focused_sector().is_none() { self.sector_focus = None; }
                result
            }
            Err(e) => {
                let msg = format!("Sectors could not be saved or loaded: {e}");
                if self.sector_error != msg { self.say(Ev::Error, msg.clone(), RED); }
                self.sector_error = msg.clone();
                Err(msg)
            }
        }
    }

    fn open_sectors(&mut self) {
        let _ = self.sync_sectors(|_| Ok(0));
        let picked = self.sel_planet();
        let selected = match &self.sel { Some(Sel::Sector(id, _)) => self.sectors.sectors.iter().find(|s| s.id == *id), _ => None };
        self.sector_view.id = selected.or_else(|| self.focused_sector()).or_else(|| picked.as_ref().and_then(|p| self.sectors.sectors.iter().find(|s| self.sector_has(s, p))))
            .or_else(|| self.selected_sector()).map_or(0, |s| s.id);
        if let Some(p) = picked { self.sector_view.world = self.moon_host(&p).unwrap_or(p); }
        self.sector_view.edit = None;
        self.sector_view.error.clear();
        self.mode = Mode::Sectors;
    }

    fn focus_sector(&mut self, l: &Lay, t: f32) {
        let Some(s) = self.selected_sector().cloned() else { return };
        let w = self.world(t, 0.0);
        let mut points = Vec::new();
        for (i, p) in w.planets.iter().enumerate().filter(|(_, p)| self.sector_has(&s, &p.key)) {
            points.push(p.wpos);
            if let Some(sys) = w.system_of(i) { points.push(sys.wpos); }
        }
        if points.is_empty() { self.sector_view.error = "Add an available world before focusing this sector".into(); return; }
        let bounds = points.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)));
        self.cam.tx = (bounds.0 + bounds.2) * 0.5;
        self.cam.ty = (bounds.1 + bounds.3) * 0.5;
        self.cam.tz = (l.vw / (bounds.2 - bounds.0 + 100.0)).min(l.vh / (bounds.3 - bounds.1 + 100.0)).clamp(0.12, (OVERVIEW_ZOOM + SECTOR_ZOOM) * 0.5);
        self.sector_focus = Some(s.id);
        self.sel = None;
        self.mode = Mode::Galaxy;
    }

    fn sector_anomalies(&mut self) {
        let Some(s) = self.selected_sector().cloned() else { return };
        self.open_anomalies();
        self.anomaly_filter.sector = Some((s.name, s.worlds));
        self.mode = Mode::Anomalies { id: self.anomaly_rows().first().map_or(0, |f| f.id), typing: false };
    }
}

// ─────────────────────────────── hyperlanes ───────────────────────────────

impl App {
    pub fn selected_lane(&self) -> Option<&crate::hyperlane::Lane> {
        self.hyperlanes.lanes.iter().find(|p| p.id == self.lane_view.id).or(self.hyperlanes.lanes.first())
    }

    pub fn lane_worlds(&self, w: &World) -> Vec<String> {
        let mut paths: std::collections::BTreeSet<_> = w.planets.iter().filter(|p| p.host.is_none()).map(|p| p.key.clone()).collect();
        for lane in &self.hyperlanes.lanes { paths.extend([lane.from.clone(), lane.to.clone()]); }
        if self.lane_view.edit == Some(LaneEdit::To) { paths.remove(&self.lane_view.from); }
        paths.into_iter().collect()
    }

    fn sync_designs(&mut self, change: impl FnOnce(&mut crate::design::Book) -> Result<u64, String>) -> Result<u64, String> {
        let mut result = Ok(0);
        let apply = |b: &mut crate::design::Book| { result = change(b); };
        let loaded = match &self.design_path {
            Some(path) => crate::design::edit(path, apply),
            None => { apply(&mut self.designs); Ok(self.designs.clone()) }
        };
        match loaded {
            Ok(book) => { self.designs = book; result }
            Err(e) => {
                let msg = format!("Ship designs could not be saved or loaded: {e}");
                self.say(Ev::Error, msg.clone(), RED);
                Err(msg)
            }
        }
    }

    pub fn open_shipyard(&mut self) {
        let _ = self.sync_designs(|_| Ok(0));
        self.mode = Mode::Shipyard(YardView::default());
    }

    /// Outfit the composer's new ship from the next design, then from none again.
    fn next_design(&mut self, t: f32) {
        let _ = self.sync_designs(|_| Ok(0));
        let Mode::Brief { kind, launch, role, ship, .. } = &mut self.mode else { return };
        let list = &self.designs.designs;
        let next = ship.design.and_then(|id| list.iter().position(|d| d.id == id)).map_or(0, |i| i + 1);
        let mut note = None;
        match list.get(next) {
            Some(d) => {
                match self.kinds.iter().position(|k| *k == d.kind) {
                    Some(i) => *kind = i,
                    None => note = Some(format!("{} isn't installed here; {} launches as {}", d.kind, d.name, self.kinds.get(*kind).map_or("", String::as_str))),
                }
                *role = ROLES.iter().position(|&r| r == Role::parse(&d.role)).unwrap_or(0);
                *ship = Outfit { design: Some(d.id), orders: d.orders.clone(), skills: d.skills.clone() };
                *launch = true; // a design is for a new ship
            }
            None => {
                *ship = Outfit::default();
                if list.is_empty() { note = Some("No ship designs yet: Ctrl+R refits this ship, Ctrl+S there saves it".into()); }
            }
        }
        self.advisor.touched = true;
        if let Some(n) = note { self.toast(n, t); }
    }

    /// Open the refit dialog over the composer.
    fn open_refit(&mut self) {
        let Mode::Brief { planet, ship, .. } = &self.mode else { return };
        let mut skills = crate::design::available(Path::new(planet), &self.skill_home);
        skills.extend(ship.skills.iter().filter(|s| !skills.contains(s)).cloned().collect::<Vec<_>>());
        let orders = Line { cur: ship.orders.chars().count(), s: ship.orders.clone() };
        let picked = ship.skills.clone();
        let brief = std::mem::replace(&mut self.mode, Mode::Galaxy);
        self.mode = Mode::Refit(RefitView { brief: Box::new(brief), orders, skills, picked, sel: 0, naming: None, error: String::new() });
    }

    /// Close the refit dialog, keeping its changes for this launch or not.
    fn close_refit(&mut self, keep: bool) {
        let Mode::Refit(v) = std::mem::replace(&mut self.mode, Mode::Galaxy) else { return };
        let mut brief = *v.brief;
        if keep && let Mode::Brief { ship, .. } = &mut brief {
            ship.orders = v.orders.s.trim().to_string();
            ship.skills = v.picked;
        }
        self.mode = brief;
    }

    /// Save the refit ship as a design under `name` (over a design of that name, if there is one).
    fn save_design(&mut self, name: &str) -> Result<u64, String> {
        let Mode::Refit(v) = &self.mode else { return Err("Nothing to save".into()) };
        let Mode::Brief { kind, role, .. } = &*v.brief else { return Err("Nothing to save".into()) };
        let d = crate::design::Design {
            id: 0, name: name.into(), kind: self.kinds.get(*kind).cloned().unwrap_or_default(), role: ROLES[*role].name().into(),
            orders: v.orders.s.clone(), skills: v.picked.clone(),
        };
        let id = self.sync_designs(|b| b.save(d))?;
        if let Mode::Refit(v) = &mut self.mode && let Mode::Brief { ship, launch, .. } = &mut *v.brief {
            ship.design = Some(id);
            *launch = true;
        }
        Ok(id)
    }

    fn sync_hyperlanes(&mut self, change: impl FnOnce(&mut crate::hyperlane::Book) -> Result<u64, String>) -> Result<u64, String> {
        let mut result = Ok(0);
        let apply = |b: &mut crate::hyperlane::Book| { result = change(b); };
        let loaded = match &self.lane_path {
            Some(path) => crate::hyperlane::edit(path, apply),
            None => { apply(&mut self.hyperlanes); Ok(self.hyperlanes.clone()) }
        };
        match loaded {
            Ok(book) => {
                self.hyperlanes = book;
                self.lane_error.clear();
                if matches!(self.sel, Some(Sel::Lane(id)) if !self.hyperlanes.lanes.iter().any(|l| l.id == id)) { self.sel = None; }
                result
            }
            Err(e) => {
                let msg = format!("Hyperlanes could not be saved or loaded: {e}");
                if self.lane_error != msg { self.say(Ev::Error, msg.clone(), RED); }
                self.lane_error = msg.clone();
                Err(msg)
            }
        }
    }

    fn open_hyperlanes(&mut self) {
        let picked = self.sel.clone();
        let _ = self.sync_hyperlanes(|_| Ok(0));
        if let Some(Sel::Lane(id)) = picked { self.lane_view.id = id; }
        else if let Some(p) = self.sel_planet() {
            let p = self.moon_host(&p).unwrap_or(p);
            if let Some(lane) = self.hyperlanes.lanes.iter().find(|l| l.from == p || l.to == p) { self.lane_view.id = lane.id; }
        }
        self.lane_view.edit = None;
        self.lane_view.error.clear();
        self.mode = Mode::Hyperlanes;
    }

    fn draft_lane(&mut self, w: &World, edit: Option<LaneEdit>) {
        self.lane_view.error.clear();
        if let Some(step) = edit {
            let Some(lane) = self.selected_lane().cloned() else { return };
            self.lane_view.draft_id = Some(lane.id);
            self.lane_view.from = lane.from;
            self.lane_view.to = lane.to;
            self.lane_view.text.set(lane.label);
            self.lane_view.edit = Some(step);
        } else {
            self.lane_view.draft_id = None;
            self.lane_view.edit = Some(LaneEdit::From);
            let paths = self.lane_worlds(w);
            let picked = self.sel_planet().map(|p| self.moon_host(&p).unwrap_or(p));
            self.lane_view.from = picked.filter(|p| paths.contains(p)).or_else(|| paths.first().cloned()).unwrap_or_default();
            self.lane_view.to.clear();
            self.lane_view.text.set(String::new());
        }
    }

    fn advance_lane(&mut self, w: &World) {
        match self.lane_view.edit {
            Some(LaneEdit::From) => {
                if self.lane_view.from.is_empty() { self.lane_view.error = "Chart a world before connecting it".into(); return; }
                self.lane_view.edit = Some(LaneEdit::To);
                let paths = self.lane_worlds(w);
                if !paths.contains(&self.lane_view.to) { self.lane_view.to = paths.first().cloned().unwrap_or_default(); }
            }
            Some(LaneEdit::To) => {
                if self.lane_view.to.is_empty() { self.lane_view.error = "Chart another world to connect to".into(); return; }
                self.lane_view.edit = Some(LaneEdit::Label);
            }
            Some(LaneEdit::Label) => {
                let (id, from, to, label) = (self.lane_view.draft_id, self.lane_view.from.clone(), self.lane_view.to.clone(), self.lane_view.text.s.clone());
                match self.sync_hyperlanes(|b| b.save(id, &from, &to, &label)) {
                    Ok(id) => { self.lane_view.id = id; self.lane_view.edit = None; self.lane_view.error.clear(); }
                    Err(e) => self.lane_view.error = e,
                }
            }
            _ => {}
        }
    }

    fn focus_lane(&mut self, w: &World, l: &Lay) {
        let Some(lane) = self.selected_lane().cloned() else { return };
        let (Some(a), Some(b)) = (w.planet(&lane.from), w.planet(&lane.to)) else {
            self.lane_view.error = "An endpoint is unavailable; the saved relationship is kept".into(); return;
        };
        self.cam.tx = (a.wpos.0 + b.wpos.0) * 0.5;
        self.cam.ty = (a.wpos.1 + b.wpos.1) * 0.5;
        // Keep world endpoints visible even when they span several systems.
        self.cam.tz = (l.vw / ((a.wpos.0 - b.wpos.0).abs() + 100.0)).min(l.vh / ((a.wpos.1 - b.wpos.1).abs() + 100.0)).clamp(0.08, 0.8);
        self.dependencies = true;
        self.sel = Some(Sel::Lane(lane.id));
        self.mode = Mode::Galaxy;
    }

    fn toggle_dependencies(&mut self) {
        self.dependencies = !self.dependencies;
        if !self.dependencies && matches!(self.sel, Some(Sel::Lane(_))) { self.sel = None; }
    }
}

// ─────────────────────────────── campaigns ───────────────────────────────

impl App {
    pub fn selected_campaign(&self) -> Option<&crate::campaign::Campaign> {
        self.campaigns.campaigns.iter().find(|c| c.id == self.campaign_view.id).or(self.campaigns.campaigns.first())
    }
    pub fn campaign_stage(&self) -> Option<&crate::campaign::Stage> {
        self.selected_campaign().and_then(|c| c.stages.iter().find(|s| s.id == self.campaign_view.stage).or(c.stages.first()))
    }
    /// The stage's waiting ship first, then its most recent live mission's ship.
    pub fn campaign_ship(&self, stage: &crate::campaign::Stage) -> Option<&Agent> {
        stage.missions.iter().rev().filter(|l| l.available).filter_map(|l| {
            self.agent(l.mission.agent?).filter(|a| a.cwd == l.mission.cwd && a.dying.is_none() && a.status != Status::Exited)
        }).min_by_key(|a| if a.status == Status::Blocked { 0 } else { 1 })
    }
    pub fn campaign_worlds(&self, w: &World) -> Vec<String> {
        let mut paths: std::collections::BTreeSet<_> = w.planets.iter().map(|p| p.key.clone()).collect();
        if let Some(c) = self.selected_campaign() { paths.extend(c.stages.iter().map(|s| s.world.clone())); }
        paths.into_iter().collect()
    }
    pub fn campaign_missions(&self) -> Vec<&Mission> {
        let c = self.selected_campaign();
        self.missions.iter().filter(|m| !c.is_some_and(|c| c.stages.iter().any(|s| s.missions.iter().any(|l| l.mission.id == m.id && l.mission.at == m.at)))).collect()
    }
    fn sync_campaigns(&mut self, change: impl FnOnce(&mut crate::campaign::Book) -> Result<u64, String>) -> Result<u64, String> {
        let mut result = Ok(0);
        let apply = |b: &mut crate::campaign::Book| { result = change(b); };
        let loaded = match &self.campaign_path {
            Some(path) => crate::campaign::edit(path, apply),
            None => { apply(&mut self.campaigns); Ok(self.campaigns.clone()) }
        };
        match loaded {
            Ok(book) => { self.campaigns = book; self.campaign_error.clear(); result }
            Err(e) => {
                let msg = format!("Campaigns could not be saved or loaded: {e}");
                if self.campaign_error != msg { self.say(Ev::Error, msg.clone(), RED); }
                self.campaign_error = msg.clone(); Err(msg)
            }
        }
    }
    fn open_campaigns(&mut self) {
        let _ = self.sync_campaigns(|_| Ok(0));
        self.campaign_view.edit = None; self.campaign_view.error.clear();
        self.mode = Mode::Campaigns;
    }
    fn campaign_draft(&mut self, w: &World) {
        let (Some(c), Some(s)) = (self.selected_campaign(), self.campaign_stage()) else { return };
        let link = crate::proto::CampaignRef { campaign: c.id, stage: s.id };
        let world = s.world.clone();
        let context = match self.campaigns.brief(c.id, s.id) { Ok(s) => s, Err(e) => { self.campaign_view.error = e; return; } };
        if w.planet(&world).is_none() { self.campaign_view.error = "This world's folder is unavailable; the stage is kept".into(); return; }
        self.campaign_view.id = link.campaign; self.campaign_view.stage = link.stage;
        let mut text = Line::default(); text.set(context.clone());
        self.campaign_draft = Some(CampaignDraft { link, context });
        // Queueing is the safe default; the normal composer still offers every ship and role.
        self.mode = Mode::Brief { planet: world, text, kind: 0, launch: false, role: 0, to: None, attachments: vec![], sending: false, ship: Outfit::default() };
    }
    fn campaign_edit(&mut self, edit: CampaignEdit) {
        let value = match edit {
            CampaignEdit::Objective => self.selected_campaign().map(|c| c.objective.clone()).unwrap_or_default(),
            CampaignEdit::Task if self.campaign_view.stage_edit => self.campaign_stage().map(|s| s.task.clone()).unwrap_or_default(),
            CampaignEdit::Handoff => self.campaign_stage().map(|s| s.handoff.clone()).unwrap_or_default(),
            CampaignEdit::Verify => self.campaign_stage().map(|s| s.verification.clone()).unwrap_or_default(),
            _ => String::new(),
        };
        self.campaign_view.edit = Some(edit); self.campaign_view.text.set(value); self.campaign_view.scroll = 0; self.campaign_view.error.clear();
    }
    fn campaign_save(&mut self, w: &World) {
        let view = &self.campaign_view;
        let (id, stage) = (self.selected_campaign().map_or(0, |c| c.id), self.campaign_stage().map_or(0, |s| s.id));
        let (edit, text, at) = (view.edit, view.text.s.clone(), survey::now().max(0) as u64);
        let result = match edit {
            Some(CampaignEdit::Objective) => {
                if text.trim().is_empty() || text.trim().chars().count() > 120 || text.chars().any(char::is_control) {
                    self.campaign_view.error = "Use a single-line objective of 1–120 characters".into(); return;
                }
                self.campaign_view.objective = text;
                let criteria = if self.campaign_view.new { String::new() } else { self.selected_campaign().map_or(String::new(), |c| c.criteria.clone()) };
                self.campaign_view.text.set(criteria); self.campaign_view.edit = Some(CampaignEdit::Criteria); return;
            }
            Some(CampaignEdit::Criteria) => {
                let objective = self.campaign_view.objective.clone(); let new = self.campaign_view.new;
                self.sync_campaigns(|b| if new { b.create(&objective, &text, at) } else { b.update(id, &objective, &text, at).map(|_| id) })
            }
            Some(CampaignEdit::World) => {
                if self.campaign_view.world.is_empty() || !self.campaign_worlds(w).contains(&self.campaign_view.world) { self.campaign_view.error = "Choose a charted world".into(); return; }
                self.campaign_edit(CampaignEdit::Task); return;
            }
            Some(CampaignEdit::Task) => {
                let world = self.campaign_view.world.clone();
                let editing = self.campaign_view.stage_edit;
                self.sync_campaigns(|b| if editing { b.update_stage(id, stage, &world, &text, at).map(|_| stage) } else { b.add_stage(id, &world, &text, at) })
            }
            Some(CampaignEdit::Handoff) => self.sync_campaigns(|b| b.handoff(id, stage, &text, at).map(|_| stage)),
            Some(CampaignEdit::Verify) => self.sync_campaigns(|b| b.verify(id, stage, &text, at).map(|_| stage)),
            Some(CampaignEdit::Link) => {
                let Some(m) = self.campaign_missions().get(self.campaign_view.mission).copied().cloned() else { return };
                self.sync_campaigns(|b| b.link(id, stage, &m, at).map(|_| stage))
            }
            _ => return,
        };
        match result {
            Ok(key) => {
                if edit == Some(CampaignEdit::Criteria) { self.campaign_view.id = key; self.campaign_view.stages = true; }
                if edit == Some(CampaignEdit::Task) { self.campaign_view.stage = key; self.campaign_view.stages = true; }
                self.campaign_view.edit = None; self.campaign_view.error.clear(); self.campaign_view.scroll = 0;
            }
            Err(e) => self.campaign_view.error = e,
        }
    }
}

fn campaign_input(app: &mut App, w: &World, l: &Lay, client: &Client, ev: &Event) {
    if let Event::Key(k) = ev && k.kind == KeyEventKind::Release { return; }
    let edit = app.campaign_view.edit;
    if let Event::Key(k) = ev && k.code == KeyCode::Esc {
        if edit.is_some() { app.campaign_view.edit = None; app.campaign_view.error.clear(); }
        else { app.mode = Mode::Galaxy; }
        return;
    }
    let (id, stage) = (app.selected_campaign().map_or(0, |c| c.id), app.campaign_stage().map_or(0, |s| s.id));
    let at = survey::now().max(0) as u64;
    if edit == Some(CampaignEdit::Complete) {
        if let Event::Key(k) = ev {
            if matches!(k.code, KeyCode::Char('y' | 'Y')) {
                match app.sync_campaigns(|b| b.complete(id, at).map(|_| id)) {
                    Ok(_) => { app.campaign_view.edit = None; app.campaign_view.error.clear(); }
                    Err(e) => app.campaign_view.error = e,
                }
            } else { app.campaign_view.edit = None; }
        }
        return;
    }
    let picking = matches!(edit, Some(CampaignEdit::World | CampaignEdit::Link));
    if edit.is_some() && !picking {
        match ev {
            Event::Key(k) if (k.code == KeyCode::Enter && k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT))
                || (k.code == KeyCode::Char('j') && k.modifiers.contains(KeyModifiers::CONTROL)) => app.campaign_view.text.paste_lines("\n"),
            Event::Key(k) if k.code == KeyCode::Enter => app.campaign_save(w),
            Event::Key(k) if matches!(k.code, KeyCode::Up | KeyCode::Down) => { app.campaign_view.text.vertical(k.code == KeyCode::Up); }
            Event::Key(k) => { app.campaign_view.text.key(k); }
            Event::Paste(s) => app.campaign_view.text.paste_lines(s),
            _ => {}
        }
        return;
    }
    let worlds = app.campaign_worlds(w);
    let ids: Vec<_> = app.campaigns.campaigns.iter().map(|c| c.id).collect();
    let stages: Vec<_> = app.selected_campaign().map_or(vec![], |c| c.stages.iter().map(|s| s.id).collect());
    let (pos, len) = match edit {
        Some(CampaignEdit::World) => (worlds.iter().position(|p| p == &app.campaign_view.world).unwrap_or(0), worlds.len()),
        Some(CampaignEdit::Link) => (app.campaign_view.mission, app.campaign_missions().len()),
        _ if app.campaign_view.stages => (stages.iter().position(|s| *s == stage).unwrap_or(0), stages.len()),
        _ => (ids.iter().position(|c| *c == id).unwrap_or(0), ids.len()),
    };
    let mut next = pos;
    match ev {
        Event::Key(k) => match k.code {
            KeyCode::Up | KeyCode::Char('k') => next = pos.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => next = (pos + 1).min(len.saturating_sub(1)),
            KeyCode::Home => next = 0,
            KeyCode::End => next = len.saturating_sub(1),
            KeyCode::PageUp => { app.campaign_view.scroll = app.campaign_view.scroll.saturating_sub(8); return; }
            KeyCode::PageDown => { app.campaign_view.scroll = app.campaign_view.scroll.saturating_add(8); return; }
            KeyCode::Enter if picking => { app.campaign_save(w); return; }
            KeyCode::Tab | KeyCode::BackTab if !picking => { app.campaign_view.stages = !app.campaign_view.stages; return; }
            KeyCode::Char('n') if !picking => { app.campaign_view.new = true; app.campaign_edit(CampaignEdit::Objective); app.campaign_view.text.set(String::new()); return; }
            KeyCode::Char('e') if !picking && id != 0 => { app.campaign_view.new = false; app.campaign_edit(CampaignEdit::Objective); return; }
            KeyCode::Char('s') if !picking && id != 0 => {
                app.campaign_view.stage_edit = false;
                app.campaign_edit(CampaignEdit::World);
                app.campaign_view.world = app.sel_planet().or_else(|| worlds.first().cloned()).unwrap_or_default(); return;
            }
            KeyCode::Char('t') if !picking && stage != 0 => {
                app.campaign_view.stage_edit = true;
                app.campaign_view.world = app.campaign_stage().unwrap().world.clone();
                app.campaign_edit(CampaignEdit::World); return;
            }
            KeyCode::Char('h' | 'v') if !picking && stage != 0 => { app.campaign_edit(if k.code == KeyCode::Char('h') { CampaignEdit::Handoff } else { CampaignEdit::Verify }); return; }
            KeyCode::Char('l') if !picking && stage != 0 => { app.campaign_view.mission = 0; app.campaign_edit(CampaignEdit::Link); return; }
            KeyCode::Char('m') if !picking => { app.campaign_draft(w); return; }
            KeyCode::Char('d') if !picking => { app.campaign_view.details = !app.campaign_view.details; app.campaign_view.scroll = 0; return; }
            KeyCode::Char('o') if !picking => {
                if let Some(id) = app.campaign_stage().and_then(|s| app.campaign_ship(s)).map(|a| a.id) {
                    app.campaign_return = true;
                    app.open_comms(id, client, l);
                } else { app.campaign_view.error = "No live ship linked to this stage; m drafts a mission, Enter opens its log".into(); }
                return;
            }
            KeyCode::Enter if !picking => {
                if let Some(s) = app.campaign_stage() { app.mode = Mode::Log { world: s.world.clone(), sel: 0, open: HashSet::new() }; }
                return;
            }
            KeyCode::Char('a' | 'r' | 'x') if !picking && id != 0 => {
                let result = app.sync_campaigns(|b| match k.code {
                    KeyCode::Char('a') => b.accept(id, stage, at), KeyCode::Char('r') => b.reopen(id, stage, at), _ => b.archive(id, at),
                }.map(|_| id));
                app.campaign_view.error = result.err().unwrap_or_default(); return;
            }
            KeyCode::Char('c') if !picking && id != 0 => { app.campaign_edit(CampaignEdit::Complete); return; }
            _ => {}
        },
        Event::Mouse(me) if matches!(me.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) => {
            let down = me.kind == MouseEventKind::ScrollDown;
            let (_, y, _, _, _, room) = draw::campaign_layout(app, l);
            if !picking && me.row as i32 >= y + 4 + room as i32 { app.campaign_view.scroll = app.campaign_view.scroll.saturating_add_signed(if down { 3 } else { -3 }); return; }
            next = if down { (pos + 3).min(len.saturating_sub(1)) } else { pos.saturating_sub(3) };
        }
        Event::Mouse(me) if me.kind == MouseEventKind::Up(MouseButton::Left) => {
            if let Some((right, index)) = draw::campaign_hit(app, w, l, me.column as i32, me.row as i32) {
                if !picking { app.campaign_view.stages = right; }
                next = index;
            }
        }
        _ => {}
    }
    if edit == Some(CampaignEdit::World) { if let Some(p) = worlds.get(next) { app.campaign_view.world = p.clone(); } }
    else if edit == Some(CampaignEdit::Link) { app.campaign_view.mission = next; }
    else if app.campaign_view.stages { if let Some(s) = stages.get(next) { app.campaign_view.stage = *s; } }
    else if let Some(c) = ids.get(next) { app.campaign_view.id = *c; }
    if next != pos { app.campaign_view.scroll = 0; }
}

/// The refit dialog: write the new ship's standing orders, pick its skills, save it as a design.
fn refit_input(app: &mut App, ev: &Event, t: f32) {
    if let Event::Key(k) = ev && k.kind == KeyEventKind::Release { return; }
    let Mode::Refit(v) = &mut app.mode else { return };
    if let Some(name) = &mut v.naming {
        match ev {
            Event::Key(k) if k.code == KeyCode::Esc => { v.naming = None; v.error.clear(); }
            Event::Key(k) if k.code == KeyCode::Enter => {
                let name = name.s.clone();
                match app.save_design(&name) {
                    Ok(_) => {
                        if let Mode::Refit(v) = &mut app.mode { v.naming = None; v.error.clear(); }
                        app.toast(format!("Design {} saved to the shipyard", name.trim()), t);
                    }
                    Err(e) => if let Mode::Refit(v) = &mut app.mode { v.error = e },
                }
            }
            Event::Key(k) => { name.key(k); }
            Event::Paste(s) => name.paste_lines(&s.chars().filter(|c| !c.is_control()).collect::<String>()),
            _ => {}
        }
        return;
    }
    let n = v.skills.len();
    match ev {
        Event::Paste(s) if v.sel == 0 => v.orders.paste_lines(s),
        Event::Mouse(me) if me.kind == MouseEventKind::ScrollUp => v.sel = v.sel.saturating_sub(1),
        Event::Mouse(me) if me.kind == MouseEventKind::ScrollDown => v.sel = (v.sel + 1).min(n),
        Event::Key(k) => {
            let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
            match k.code {
                KeyCode::Esc => app.close_refit(false),
                KeyCode::Char('s') if ctrl => {
                    let name = match &*v.brief { Mode::Brief { ship, .. } => ship.design.and_then(|id| app.designs.get(id)).map(|d| d.name.clone()).unwrap_or_default(), _ => String::new() };
                    v.naming = Some(Line { cur: name.chars().count(), s: name });
                }
                // new lines in the orders
                KeyCode::Enter if v.sel == 0 && k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => v.orders.paste_lines("\n"),
                KeyCode::Char('j') if v.sel == 0 && ctrl => v.orders.paste_lines("\n"),
                KeyCode::Enter => app.close_refit(true),
                KeyCode::Tab => v.sel = (v.sel + 1) % (n + 1),
                KeyCode::BackTab => v.sel = (v.sel + n) % (n + 1),
                KeyCode::Up if v.sel == 0 => { v.orders.vertical(true); }
                KeyCode::Down if v.sel == 0 => if !v.orders.vertical(false) { v.sel = 1.min(n); },
                KeyCode::Up | KeyCode::Char('k') if v.sel > 0 => v.sel -= 1,
                KeyCode::Down | KeyCode::Char('j') if v.sel > 0 => v.sel = (v.sel + 1).min(n),
                KeyCode::Char(' ') if v.sel > 0 => {
                    let s = v.skills[v.sel - 1].clone();
                    if let Some(i) = v.picked.iter().position(|p| *p == s) { v.picked.remove(i); } else { v.picked.push(s); }
                }
                _ if v.sel == 0 => { v.orders.key(k); }
                _ => {}
            }
        }
        _ => {}
    }
}

/// The shipyard: the saved designs, renamed or scrapped. Ships launched from them keep their own copy.
fn shipyard_input(app: &mut App, ev: &Event) {
    if let Event::Key(k) = ev && k.kind == KeyEventKind::Release { return; }
    let n = app.designs.designs.len();
    let Mode::Shipyard(v) = &mut app.mode else { return };
    v.sel = v.sel.min(n.saturating_sub(1));
    let id = app.designs.designs.get(v.sel).map(|d| d.id);
    if let Some(name) = &mut v.rename {
        match ev {
            Event::Key(k) if k.code == KeyCode::Esc => { v.rename = None; v.error.clear(); }
            Event::Key(k) if k.code == KeyCode::Enter => {
                let (name, id) = (name.s.clone(), id.unwrap_or(0));
                let r = app.sync_designs(|b| b.rename(id, &name));
                if let Mode::Shipyard(v) = &mut app.mode { match r { Ok(_) => { v.rename = None; v.error.clear(); } Err(e) => v.error = e } }
            }
            Event::Key(k) => { name.key(k); }
            Event::Paste(s) => name.paste_lines(&s.chars().filter(|c| !c.is_control()).collect::<String>()),
            _ => {}
        }
        return;
    }
    if v.remove {
        if let Event::Key(k) = ev {
            v.remove = false;
            if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) && let Some(id) = id {
                let r = app.sync_designs(|b| b.remove(id));
                if let (Err(e), Mode::Shipyard(v)) = (r, &mut app.mode) { v.error = e; }
            }
        }
        return;
    }
    match ev {
        Event::Key(k) => match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Y') => app.mode = Mode::Galaxy,
            KeyCode::Up | KeyCode::Char('k') => v.sel = v.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => v.sel = (v.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('r') if id.is_some() => {
                let name = app.designs.designs[v.sel].name.clone();
                v.rename = Some(Line { cur: name.chars().count(), s: name });
                v.error.clear();
            }
            KeyCode::Char('x') if id.is_some() => { v.remove = true; v.error.clear(); }
            _ => {}
        },
        Event::Mouse(me) if me.kind == MouseEventKind::ScrollUp => v.sel = v.sel.saturating_sub(1),
        Event::Mouse(me) if me.kind == MouseEventKind::ScrollDown => v.sel = (v.sel + 1).min(n.saturating_sub(1)),
        _ => {}
    }
}

fn hyperlane_input(app: &mut App, w: &World, l: &Lay, ev: &Event) {
    if let Event::Key(k) = ev && k.kind == KeyEventKind::Release { return; }
    let step = app.lane_view.edit;
    if let Event::Key(k) = ev {
        if k.code == KeyCode::Esc {
            if step.is_some() { app.lane_view.edit = None; app.lane_view.error.clear(); }
            else { app.mode = Mode::Galaxy; }
            return;
        }
        if step == Some(LaneEdit::Remove) {
            if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                let id = app.lane_view.id;
                match app.sync_hyperlanes(|b| b.remove(id).map(|_| 0)) {
                    Ok(_) => app.lane_view = LaneView::default(),
                    Err(e) => app.lane_view.error = e,
                }
            } else { app.lane_view.edit = None; }
            return;
        }
        if step == Some(LaneEdit::Label) {
            if k.code == KeyCode::Enter { app.advance_lane(w); }
            else { app.lane_view.text.key(k); }
            return;
        }
    }
    if let Event::Paste(s) = ev && step == Some(LaneEdit::Label) {
        app.lane_view.text.paste_lines(&s.chars().filter(|c| !c.is_control()).collect::<String>());
        return;
    }
    if matches!(step, Some(LaneEdit::Label | LaneEdit::Remove)) { return; }
    let paths = app.lane_worlds(w);
    let picking = matches!(step, Some(LaneEdit::From | LaneEdit::To));
    let current = if step == Some(LaneEdit::To) { &app.lane_view.to } else { &app.lane_view.from };
    let (at, count) = if picking { (paths.iter().position(|p| p == current).unwrap_or(0), paths.len()) }
        else { (app.selected_lane().and_then(|p| app.hyperlanes.lanes.iter().position(|l| l.id == p.id)).unwrap_or(0), app.hyperlanes.lanes.len()) };
    let room = draw::hyperlane_layout(l).4;
    let mut next = at;
    match ev {
        Event::Key(k) => match k.code {
            KeyCode::Up | KeyCode::Char('k') => next = at.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => next = (at + 1).min(count.saturating_sub(1)),
            KeyCode::PageUp => next = at.saturating_sub(room),
            KeyCode::PageDown => next = (at + room).min(count.saturating_sub(1)),
            KeyCode::Home => next = 0,
            KeyCode::End => next = count.saturating_sub(1),
            KeyCode::Enter if picking => { app.advance_lane(w); return; }
            KeyCode::Backspace if step == Some(LaneEdit::To) => { app.lane_view.edit = Some(LaneEdit::From); return; }
            KeyCode::Char('n') if !picking => { app.draft_lane(w, None); return; }
            KeyCode::Char('e' | 'r') if !picking => { app.draft_lane(w, Some(if k.code == KeyCode::Char('r') { LaneEdit::Label } else { LaneEdit::From })); return; }
            KeyCode::Char('x') if !picking => {
                if let Some(lane) = app.selected_lane() { app.lane_view.id = lane.id; app.lane_view.edit = Some(LaneEdit::Remove); }
                return;
            }
            KeyCode::Char('f') | KeyCode::Enter if !picking => { app.focus_lane(w, l); return; }
            KeyCode::Char('h') if !picking => { app.toggle_dependencies(); return; }
            KeyCode::Char('1' | '2') if !picking => {
                if let Some(lane) = app.selected_lane().cloned() {
                    let path = if k.code == KeyCode::Char('1') { lane.from } else { lane.to };
                    if w.planet(&path).is_some() {
                        app.sel = Some(Sel::Planet(path)); app.glide_to_sel(w); app.cam.tz = 1.3; app.mode = Mode::Galaxy;
                    } else { app.lane_view.error = "This world's folder is unavailable".into(); }
                }
                return;
            }
            _ => {}
        },
        Event::Mouse(me) if me.kind == MouseEventKind::ScrollUp => next = at.saturating_sub(3),
        Event::Mouse(me) if me.kind == MouseEventKind::ScrollDown => next = (at + 3).min(count.saturating_sub(1)),
        Event::Mouse(me) if me.kind == MouseEventKind::Up(MouseButton::Left) => {
            if let Some(i) = draw::hyperlane_hit(l, count, at, me.column as i32, me.row as i32) { next = i; }
        }
        _ => {}
    }
    if picking {
        if let Some(path) = paths.get(next) {
            if step == Some(LaneEdit::To) { app.lane_view.to = path.clone(); } else { app.lane_view.from = path.clone(); }
        }
    } else if let Some(lane) = app.hyperlanes.lanes.get(next) { app.lane_view.id = lane.id; }
}

fn sector_input(app: &mut App, w: &World, l: &Lay, ev: &Event, t: f32) {
    if let Event::Key(k) = ev && k.kind == KeyEventKind::Release { return; }
    if let Some(edit) = app.sector_view.edit {
        match ev {
            Event::Key(k) if k.code == KeyCode::Esc => { app.sector_view.edit = None; app.sector_view.error.clear(); }
            Event::Key(k) if matches!(edit, SectorEdit::Remove) => {
                if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    let id = app.sector_view.id;
                    match app.sync_sectors(|b| b.remove(id).map(|_| 0)) {
                        Ok(_) => { app.sector_view = SectorView::default(); }
                        Err(e) => app.sector_view.error = e,
                    }
                } else { app.sector_view.edit = None; }
            }
            Event::Key(k) if k.code == KeyCode::Enter => {
                let id = app.sector_view.id;
                let text = app.sector_view.text.s.clone();
                let result = match edit {
                    SectorEdit::New => app.sync_sectors(|b| b.create(&text)),
                    SectorEdit::Rename => app.sync_sectors(|b| b.rename(id, &text).map(|_| id)),
                    SectorEdit::Path => {
                        let path = expand(text.trim()).canonicalize();
                        match path {
                            Ok(path) if path.is_dir() => {
                                let path = path.to_string_lossy().into_owned();
                                let host = survey::world_host(&path).unwrap_or(path);
                                let result = app.sync_sectors(|b| {
                                    if !b.sectors.iter().any(|s| s.id == id && s.contains(&host, None)) { b.toggle(id, &host)?; }
                                    Ok(id)
                                });
                                if result.is_ok() { app.sector_view.world = host; }
                                result
                            }
                            _ => Err("Choose an existing folder; sectors do not create or move folders".into()),
                        }
                    }
                    SectorEdit::Remove => unreachable!(),
                };
                match result {
                    Ok(id) => { app.sector_view.id = id; app.sector_view.edit = None; app.sector_view.worlds = true; app.sector_view.error.clear(); }
                    Err(e) => app.sector_view.error = e,
                }
            }
            Event::Key(k) => { app.sector_view.text.key(k); }
            Event::Paste(s) if !matches!(edit, SectorEdit::Remove) => {
                app.sector_view.text.paste_lines(&s.chars().filter(|c| !c.is_control()).collect::<String>());
            }
            _ => {}
        }
        return;
    }
    let sectors: Vec<_> = app.sectors.sectors.iter().map(|s| s.id).collect();
    let worlds = app.sector_worlds(w);
    let mut si = sectors.iter().position(|id| *id == app.sector_view.id).unwrap_or(0);
    let mut wi = worlds.iter().position(|p| *p == app.sector_view.world).unwrap_or(0);
    let mut pane = app.sector_view.worlds;
    let mut action = None;
    let page = (l.rows - 14).max(1) as usize;
    match ev {
        Event::Key(k) => match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('V') => { app.mode = Mode::Galaxy; return; }
            KeyCode::Tab | KeyCode::BackTab => pane = !pane,
            KeyCode::Left => pane = false,
            KeyCode::Right => pane = true,
            KeyCode::Up | KeyCode::Char('k') => if pane { wi = wi.saturating_sub(1); } else { si = si.saturating_sub(1); },
            KeyCode::Down | KeyCode::Char('j') => if pane { wi = (wi + 1).min(worlds.len().saturating_sub(1)); } else { si = (si + 1).min(sectors.len().saturating_sub(1)); },
            KeyCode::Home => if pane { wi = 0; } else { si = 0; },
            KeyCode::End => if pane { wi = worlds.len().saturating_sub(1); } else { si = sectors.len().saturating_sub(1); },
            KeyCode::PageUp => if pane { wi = wi.saturating_sub(page); } else { si = si.saturating_sub(page); },
            KeyCode::PageDown => if pane { wi = (wi + page).min(worlds.len().saturating_sub(1)); } else { si = (si + page).min(sectors.len().saturating_sub(1)); },
            KeyCode::Enter => action = Some(if pane { 'l' } else { 'f' }),
            KeyCode::Char(c @ ('n' | 'r' | 'x' | 'p' | 'f' | 'a' | ' ')) => action = Some(c),
            _ => {}
        },
        Event::Mouse(m) => match m.kind {
            MouseEventKind::ScrollUp => if pane { wi = wi.saturating_sub(1); } else { si = si.saturating_sub(1); },
            MouseEventKind::ScrollDown => if pane { wi = (wi + 1).min(worlds.len().saturating_sub(1)); } else { si = (si + 1).min(sectors.len().saturating_sub(1)); },
            MouseEventKind::Up(MouseButton::Left) => match draw::sector_hit(l, sectors.len(), worlds.len(), si, wi, m.column as i32, m.row as i32) {
                Some((false, i)) => { pane = false; si = i; }
                Some((true, i)) => { pane = true; wi = i; }
                None => {}
            },
            _ => {}
        },
        _ => {}
    }
    let id = sectors.get(si).copied().unwrap_or(0);
    app.sector_view.id = id;
    app.sector_view.world = worlds.get(wi).cloned().unwrap_or_default();
    app.sector_view.worlds = pane;
    match action {
        Some('n') => { app.sector_view.edit = Some(SectorEdit::New); app.sector_view.text.set(String::new()); app.sector_view.error.clear(); }
        Some(c @ ('r' | 'p' | 'x')) if id != 0 => {
            app.sector_view.edit = Some(match c { 'r' => SectorEdit::Rename, 'p' => SectorEdit::Path, _ => SectorEdit::Remove });
            let value = if c == 'r' { app.selected_sector().unwrap().name.clone() } else { String::new() };
            app.sector_view.text.set(value);
            app.sector_view.error.clear();
        }
        Some(' ') if pane && id != 0 => {
            if let Some(world) = worlds.get(wi) {
                match app.sync_sectors(|b| b.toggle(id, world).map(|_| id)) {
                    Ok(_) => app.sector_view.error.clear(),
                    Err(e) => app.sector_view.error = e,
                }
            }
        }
        Some('f') => app.focus_sector(l, t),
        Some('a') => app.sector_anomalies(),
        Some('l') => {
            if let Some(path) = worlds.get(wi) {
                if w.planet(path).is_none() { app.sector_view.error = "This world's folder is unavailable or is still being charted".into(); return; }
                app.sel = Some(Sel::Planet(path.clone()));
                app.glide_to_sel(w); app.cam.tz = 1.6; app.mode = Mode::Galaxy;
            }
        }
        _ => {}
    }
}


fn commander_input(app: &mut App, w: &World, l: &Lay, client: &Client, ev: &Event) {
    let rows = app.attention(w);
    let mut sel = app.attention_selection(&rows);
    let mut open = false;
    let mut mission = false;
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('C') => { app.mode = Mode::Galaxy; return; }
            KeyCode::Up | KeyCode::Char('k') => sel = sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => sel = (sel + 1).min(rows.len().saturating_sub(1)),
            KeyCode::Home => sel = 0,
            KeyCode::End => sel = rows.len().saturating_sub(1),
            KeyCode::PageUp => app.commander_detail = app.commander_detail.saturating_sub(4),
            KeyCode::PageDown => app.commander_detail = app.commander_detail.saturating_add(4),
            KeyCode::Enter => open = true,
            KeyCode::Char('t') => mission = true,
            _ => {}
        },
        Event::Mouse(me) => match me.kind {
            MouseEventKind::ScrollUp => sel = sel.saturating_sub(1),
            MouseEventKind::ScrollDown => sel = (sel + 1).min(rows.len().saturating_sub(1)),
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(i) = draw::commander_hit(l, rows.len(), sel, me.column as i32, me.row as i32) {
                    open = i == sel;
                    sel = i;
                }
            }
            _ => {}
        },
        _ => {}
    }
    let Some(row) = rows.get(sel) else { app.commander.clear(); app.commander_detail = 0; return };
    if app.commander != row.id { app.commander_detail = 0; }
    app.commander = row.id.clone();
    if mission && let AttentionTarget::Pr(pr) = &row.target {
        let mine = app.survey.worlds.get(&row.world).is_some_and(|s| s.prs.iter().any(|p| p.number == pr.number));
        let mut text = Line::default();
        text.set(pr_brief(pr, mine));
        let crewed = app.agents.iter().any(|a| a.cwd == row.world && a.dying.is_none() && a.status != Status::Exited);
        app.mode = Mode::Brief { planet: row.world.clone(), text, kind: 0, launch: !crewed, role: 0, to: None, attachments: vec![], sending: false, ship: Outfit::default() };
    } else if open {
        app.sel = Some(Sel::Planet(row.world.clone()));
        match &row.target {
            AttentionTarget::Agent(id) => {
                app.sel = Some(Sel::Agent(*id));
                app.commander_return = true;
                app.open_comms(*id, client, l);
            }
            AttentionTarget::Anomaly(id) => {
                app.open_anomalies();
                app.mode = Mode::Anomalies { id: *id, typing: false };
            }
            AttentionTarget::Needs => app.mode = Mode::Actions { planet: row.world.clone(), sel: 0 },
            AttentionTarget::Deploy => app.mode = Mode::Deploy { planet: row.world.clone(), kind: 0 },
            AttentionTarget::Pr(pr) => client.open_pr(row.world.clone(), pr.number),
        }
    }
}

#[derive(Clone)]
pub enum Act {
    /// Hand the world a mission (queued for a free agent, or with a new ship if none).
    Mission(String),
    /// Open comms with an agent.
    Comms(u32),
    Priority(u32),
}

/// What the ship card offers.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ShipAct {
    Comms,
    Mission,
    Priority,
    Refit,
    Decommission,
}

impl ShipAct {
    pub fn label(self) -> &'static str {
        match self {
            Self::Comms => "open comms",
            Self::Mission => "give its world a mission",
            Self::Priority => "give priority in its border conflict",
            Self::Refit => "refit from a saved design",
            Self::Decommission => "decommission",
        }
    }
}

const AMBER: V3 = v3(1.0, 0.75, 0.3);

/// What a refit is to put aboard, by design name or role.
pub fn refit_name(r: &RefitState) -> String {
    if !r.spec.design.is_empty() { r.spec.design.clone() } else { Role::parse(&r.role).label().to_string() }
}

/// Can this species be relaunched on its own session? (The daemon has the last word.)
pub fn resumes(kind: &str) -> bool { matches!(kind, "claude" | "codex" | "grok") }
const RED: V3 = v3(1.0, 0.4, 0.3);

/// Points each world has earned, and which events were already counted.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Prestige {
    pub worlds: HashMap<String, u32>,
    pub counted: std::collections::HashSet<String>,
}

fn prestige_file() -> PathBuf {
    crate::daemon::dirs_state().join("prestige.json")
}

/// Prestige needed for each development level, and its name.
pub const LEVELS: [(u32, &str); 4] = [(0, "outpost"), (4, "colony"), (12, "city world"), (25, "ecumenopolis")];

pub fn level_of(points: u32) -> u8 {
    LEVELS.iter().rposition(|(p, _)| points >= *p).unwrap_or(0) as u8
}

fn history_file() -> PathBuf {
    crate::daemon::dirs_state().join("worlds.json")
}

/// The most worlds kept on the map from history.
const HISTORY_WORLDS: usize = 40;

/// Below this scale, individual ships and worlds no longer have room on screen.
pub const OVERVIEW_ZOOM: f32 = 0.45;
/// Between the galaxy overview and planetary detail, sectors trace their member worlds.
pub const SECTOR_ZOOM: f32 = 1.05;

/// One sector's territory within a star system; moons belong to their host marker.
pub struct SectorRegion {
    pub id: u64,
    pub system: String,
    pub worlds: Vec<usize>,
    pub hull: Vec<(f32, f32)>,
    pub bounds: (f32, f32, f32, f32),
}

impl SectorRegion {
    pub fn center(&self) -> (f32, f32) { ((self.bounds.0 + self.bounds.2) * 0.5, (self.bounds.1 + self.bounds.3) * 0.5) }
    pub fn contains(&self, p: (f32, f32)) -> bool {
        (0..self.hull.len()).all(|i| {
            let (a, b) = (self.hull[i], self.hull[(i + 1) % self.hull.len()]);
            (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0) >= 0.0
        })
    }
}

impl App {
    pub fn sector_map(&self) -> bool { self.cam.z < SECTOR_ZOOM && (self.dependencies || (self.cam.z >= OVERVIEW_ZOOM && !self.sectors.sectors.is_empty())) }

    pub fn sector_regions(&self, w: &World) -> Vec<SectorRegion> {
        let mut regions = Vec::new();
        for g in &self.sectors.sectors {
            for s in &w.systems {
                let worlds: Vec<_> = s.planets.iter().copied().filter(|&i| self.sector_has(g, &w.planets[i].key)).collect();
                if worlds.is_empty() { continue; }
                let mut points: Vec<_> = worlds.iter().flat_map(|&i| {
                    let p = &w.planets[i];
                    (0..12).map(move |n| {
                        let a = n as f32 * TAU / 12.0;
                        (p.wpos.0 + (p.r + 16.0) * a.cos(), p.wpos.1 + (p.r + 16.0) * a.sin())
                    })
                }).collect();
                points.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
                let bounds = points.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)));
                // Rounded hulls hug the worlds instead of enclosing an entire system.
                let mut hull: Vec<(f32, f32)> = Vec::new();
                for reverse in [false, true] {
                    let start = hull.len();
                    for n in 0..points.len() {
                        let p = points[if reverse { points.len() - 1 - n } else { n }];
                        while hull.len() >= start + 2 {
                            let (a, b) = (hull[hull.len() - 2], hull[hull.len() - 1]);
                            if (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0) > 0.0 { break; }
                            hull.pop();
                        }
                        hull.push(p);
                    }
                    hull.pop();
                }
                regions.push(SectorRegion { id: g.id, system: s.key.clone(), worlds, hull, bounds });
            }
        }
        regions
    }
}

/// `s`, cut to `n` chars with an ellipsis.
pub fn clip(s: &str, n: usize) -> String {
    let s = s.lines().next().unwrap_or("");
    if s.chars().count() > n { format!("{}…", s.chars().take(n - 1).collect::<String>()) } else { s.to_string() }
}

/// Hand text to the system clipboard: wl-copy, xclip or pbcopy, else the
/// terminal itself via OSC 52.
fn copy_to_clipboard(text: &str) -> bool {
    use std::process::{Command, Stdio};
    let tools: [&[&str]; 3] = [&["wl-copy"], &["xclip", "-selection", "clipboard"], &["pbcopy"]];
    for tool in tools {
        let Ok(mut child) = Command::new(tool[0]).args(&tool[1..]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
            continue;
        };
        let written = child.stdin.take().is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        // wl-copy and xclip fork their clipboard owner; their parent reports
        // connection errors. Do not call a failed Wayland connection a copy.
        let start = Instant::now();
        let success = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if start.elapsed() < Duration::from_secs(2) => std::thread::sleep(Duration::from_millis(10)),
                _ => { let _ = child.kill(); let _ = child.wait(); break false; }
            }
        };
        if written && success { return true; }
    }
    let mut out = io::stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes())).and_then(|_| out.flush()).is_ok()
}

fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for ch in b.chunks(3) {
        let n = ch.iter().enumerate().fold(0u32, |n, (i, &c)| n | (c as u32) << (16 - 8 * i));
        for i in 0..4 {
            s.push(if i <= ch.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

/// Ctrl-] (or F12) leaves the comms window; everything else goes to the agent.
fn is_leave(k: &KeyEvent) -> bool {
    k.code == KeyCode::F(12)
        || (k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char(']') | KeyCode::Char('5')))
}

// ─────────────────────────────── main loop ─────────────────────────────

pub fn run() -> io::Result<()> {
    // the daemon must be up before we take over the terminal (it may log)
    crate::daemon::ensure_running()?;
    let stale = crate::daemon::ensure_current();
    let client = Client::start();
    let sound = Sound::start(true);

    let mut out = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide, EnableMouseCapture, EnableBracketedPaste, terminal::Clear(terminal::ClearType::All))?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));
    let r = game_loop(&mut out, &client, sound.as_ref(), stale);
    restore();
    r
}

fn restore() {
    let mut o = io::stdout();
    let _ = execute!(o, DisableBracketedPaste, DisableMouseCapture, style::ResetColor, cursor::Show, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

fn game_loop(o: &mut io::Stdout, client: &Client, sound: Option<&Sound>, stale: Option<String>) -> io::Result<()> {
    let mut app = App::new();
    if let Some(e) = stale {
        app.say(Ev::Error, format!("✖ {e}"), v3(1.0, 0.4, 0.3));
    }
    let (c, r) = terminal::size()?;
    let mut fb = Fb::new(c as usize, r as usize);
    let mut out = Out::new(fb.cols * fb.rows);
    let t0 = Instant::now();
    let mut last = t0;
    let budget = Duration::from_micros(1_000_000 / FPS);
    // left-button drag: (start cell, camera at start, moved?)
    let mut drag: Option<((u16, u16), (f32, f32), bool)> = None;
    let mut last_cue = Instant::now() - Duration::from_secs(10);
    let survey = survey::Survey::start();
    let mut surveyed = Instant::now() - Duration::from_secs(10);

    loop {
        let now = Instant::now();
        let t = (now - t0).as_secs_f32();
        let dt = (now - last).as_secs_f32().min(0.1);
        last = now;

        let mut cues = Vec::new();
        while let Ok(m) = client.rx.try_recv() {
            match m {
                Msg::ChatSent(id, result) => app.chat_sent(id, result, client, &Lay::new(&fb, app.panel), t),
                Msg::MissionResult(result) => {
                    match result {
                        Ok(()) => {
                            let launched = matches!(app.mode, Mode::Brief { launch: true, .. });
                            app.mode = if app.campaign_draft.take().is_some() { Mode::Campaigns } else { app.anomaly_return.take().map_or_else(|| app.planet_return.take().map_or(Mode::Galaxy, Mode::Planet), |id| Mode::Anomalies { id, typing: false }) };
                            app.toast(if launched { "mission launched" } else { "mission queued" }, t);
                        }
                        Err(e) => {
                            if let Mode::Brief { sending, .. } = &mut app.mode { *sending = false; }
                            app.toast(e, t);
                        }
                    }
                }
                Msg::Agents(list, missions) => {
                    app.apply(list, t, &mut cues);
                    app.track(missions, &mut cues);
                }
                Msg::Down(e) => {
                    app.online = false;
                    app.err = e;
                }
                Msg::Screen(id, s) => {
                    if app.talking_to() == Some(id) {
                        app.screen = Some((id, s));
                    }
                }
                Msg::Spawned { id, kind, cwd } => {
                    app.say(Ev::Agent, format!("◆ {kind} launched to {}", base_name(&cwd)), kind_color(&kind)).on(&cwd).by(id);
                    app.sel = Some(Sel::Agent(id));
                    if matches!(app.mode, Mode::Galaxy) {
                        app.open_comms(id, client, &Lay::new(&fb, app.panel));
                    }
                }
                Msg::Advisor { key, result } => app.advisor_answered(key, result),
                Msg::Docked { moon, result } => {
                    let at = app.agents.iter().find(|a| a.cwd == moon).map_or((0.0, 0.0), |a| a.wpos);
                    let name = app.survey.worlds.get(&moon).map(|w| w.branch.clone()).unwrap_or_else(|| base_name(&moon));
                    let host_path = app.survey.worlds.get(&moon).and_then(|w| w.host.clone()).unwrap_or_default();
                    let host = base_name(&host_path);
                    match result {
                        Ok(()) => {
                            app.burst(at, v3(0.6, 0.85, 1.0), t, 48);
                            app.say(Ev::World, format!("⊕ moon {name} docked with {host}: its branch is merged"), v3(0.6, 0.85, 1.0)).on(&host_path);
                            app.toast(format!("⊕ {name} docked"), t);
                            if app.sel == Some(Sel::Planet(moon.clone())) {
                                app.sel = None;
                            }
                        }
                        Err(e) => {
                            let crash = e.starts_with("collision");
                            if crash {
                                app.burst(at, v3(1.0, 0.35, 0.2), t, 80);
                                cues.push(Cue::Blocked);
                            }
                            app.say(Ev::Error, format!("{} {name}: {e}", if crash { "✸" } else { "✖" }), v3(1.0, 0.4, 0.3)).on(&moon);
                            app.toast(format!("docking failed: {e}"), t);
                        }
                    }
                }
                Msg::Error(e) => {
                    app.say(Ev::Error, format!("✖ {e}"), v3(1.0, 0.4, 0.3));
                    app.toast(format!("✖ {e}"), t);
                }
            }
        }
        app.tick(t, &mut cues);
        if let Some(s) = sound
            && let Some(&c) = cues.iter().min_by_key(|c| c.rank())
            && (c.rank() == 0 || last_cue.elapsed() > Duration::from_secs(3))
        {
            s.play(c);
            last_cue = Instant::now();
        }

        let lay = Lay::new(&fb, app.panel);
        let world = app.world(t, dt);
        for key in std::mem::take(&mut app.fanfares) {
            if let Some(p) = world.planet(&key) {
                let pos = p.wpos;
                app.burst(pos, v3(1.0, 0.85, 0.35), t, 120);
                app.fx.push(Fx::Ring { x: pos.0, y: pos.1, t0: t + 0.3, col: v3(1.0, 0.95, 0.7), speed: 70.0, dur: 1.8 });
                app.toast(format!("♛ {} levelled up", p.name), t);
            }
        }
        if !app.fitted && app.loaded {
            app.fit(&world, &lay);
            (app.cam.x, app.cam.y, app.cam.z) = (app.cam.tx, app.cam.ty, app.cam.tz);
            app.fitted = true;
        }
        if surveyed.elapsed() > Duration::from_millis(500) {
            survey.watch(world.planets.iter().map(|p| p.key.clone()).collect());
            let snap = survey.snapshot();
            let mut awards: Vec<(String, String, u32, String)> = Vec::new();
            for (path, w) in &snap.worlds {
                let old = app.survey.worlds.get(path);
                // everything committed again, with agents at work there
                if w.git && w.dirty == 0 && old.is_some_and(|o| o.git && o.dirty > 0) && app.agents.iter().any(|a| &a.cwd == path && a.dying.is_none()) {
                    let n = old.map_or(0, |o| o.dirty);
                    awards.push((path.clone(), format!("clean-{path}-{}", survey::now()), 1, format!("all committed ({n} file{})", if n == 1 { "" } else { "s" })));
                }
                for (n, title) in &w.merged {
                    awards.push((path.clone(), format!("merged-{path}-{n}"), 5, format!("PR #{n} merged: {}", clip(title, 30))));
                }
                for pr in w.prs.iter().filter(|p| p.ci == "pass") {
                    if old.and_then(|o| o.prs.iter().find(|p| p.number == pr.number)).is_some_and(|p| p.ci != "pass") {
                        awards.push((path.clone(), format!("green-{path}-{}", pr.number), 1, format!("CI green on #{}", pr.number)));
                    }
                }
                // structures raised or torn down since the last survey
                if let Some(had) = old.map(|o| o.structures.iter().map(|b| b.kind).collect::<Vec<Kind>>()) {
                    for b in w.structures.iter().filter(|b| !had.contains(&b.kind)) {
                        let k = b.kind;
                        app.say(Ev::World, format!("{} {} raised on {} ({})", k.glyph(), k.name(), base_name(path), b.files.join(", ")), k.color()).on(path);
                        let host = app.moon_host(path).unwrap_or_else(|| path.clone());
                        awards.push((path.clone(), format!("built-{host}-{}", k.name()), 1, format!("{} built ({})", k.name(), k.what())));
                    }
                    for k in had.into_iter().filter(|k| !w.structures.iter().any(|b| b.kind == *k)) {
                        app.say(Ev::World, format!("{} {} torn down on {}", k.glyph(), k.name(), base_name(path)), DIM_LOG).on(path);
                    }
                }
                for pr in &w.prs {
                    let before = app.survey.worlds.get(path).and_then(|o| o.prs.iter().find(|p| p.number == pr.number)).map(|p| p.ci);
                    match (before, pr.ci) {
                        (Some(b), "fail") if b != "fail" => {
                            app.say(Ev::Alert, format!("✖ CI failed on #{} ({}): {}", pr.number, base_name(path), pr.title), v3(1.0, 0.4, 0.3)).on(path);
                            cues.push(Cue::Blocked);
                        }
                        (Some(b), "pass") if b != "pass" => {
                            app.say(Ev::Trade, format!("✔ CI green on #{} ({}): {}", pr.number, base_name(path), pr.title), v3(0.4, 0.95, 0.6)).on(path);
                        }
                        (None, _) if app.survey.worlds.contains_key(path) && app.survey.worlds[path].github => {
                            app.say(Ev::Trade, format!("▸ PR #{} docked at the station ({}): {}", pr.number, base_name(path), pr.title), v3(0.6, 0.85, 1.0)).on(path);
                        }
                        _ => {}
                    }
                }
            }
            app.survey = snap;
            app.refresh_anomalies();
            let _ = app.sync_sectors(|_| Ok(0));
            let _ = app.sync_hyperlanes(|_| Ok(0));
            for (path, id, pts, why) in awards {
                app.award(&path, id, pts, &why, &mut cues);
            }
            surveyed = Instant::now();
        }
        // a comms target that's gone for good drops you back to the galaxy
        // (a freshly launched agent takes a moment to show up in the list)
        if let Some(id) = app.talking_to()
            && !app.agents.iter().any(|a| a.id == id)
            && app.comms_at.elapsed() > Duration::from_secs(4)
        {
            app.close_comms(client);
        }

        if let Mode::Ship { id, .. } = app.mode
            && app.agent(id).is_none()
        {
            app.mode = Mode::Galaxy;
        }
        app.consult_advisor(client);

        while event::poll(Duration::ZERO)? {
            let ev = event::read()?;
            if let Event::Resize(c, r) = ev {
                fb = Fb::new(c as usize, r as usize);
                out = Out::new(fb.cols * fb.rows);
                app.bg_key = (0, 0, 0);
                execute!(o, terminal::Clear(terminal::ClearType::All))?;
                app.fit_comms(client, &Lay::new(&fb, app.panel));
                continue;
            }
            if handle(&mut app, &world, &lay, client, sound, ev, t, &mut drag) {
                if app.talking_to().is_some() {
                    app.close_comms(client);
                }
                return Ok(());
            }
        }

        // camera easing
        let k = 1.0 - (-dt * 6.0).exp();
        app.cam.x = mixf(app.cam.x, app.cam.tx, k);
        app.cam.y = mixf(app.cam.y, app.cam.ty, k);
        app.cam.z = mixf(app.cam.z, app.cam.tz, k);

        draw::frame(&mut fb, &mut app, &world, &lay, t, sound);
        out.present(&fb, o)?;

        let el = now.elapsed();
        if el < budget {
            std::thread::sleep(budget - el);
        }
    }
}

fn attach_paths(attachments: &mut Vec<Attachment>, paths: &[PathBuf], dir: &Path) -> Result<(), String> {
    if attachments.len() + paths.len() > screenshots::MAX_ATTACHMENTS {
        return Err("Attach up to 8 screenshots per mission".into());
    }
    attachments.extend(screenshots::import(paths, dir)?);
    Ok(())
}

fn paste_brief(text: &mut Line, attachments: &mut Vec<Attachment>, pasted: &str, dir: &Path) -> Result<(), String> {
    if let Some(paths) = screenshots::dropped_paths(pasted) {
        attach_paths(attachments, &paths, dir)
    } else {
        text.paste_lines(pasted);
        Ok(())
    }
}

/// What's written in the chat popup.
fn chat_text(app: &mut App) -> Option<&mut Line> {
    match &mut app.mode {
        Mode::Chat { text, .. } => Some(text),
        _ => None,
    }
}

fn chat_paste(app: &mut App, pasted: &str, dir: &Path) -> Result<(), String> {
    if let Mode::Chat { text, attachments, sending: false, .. } = &mut app.mode {
        paste_brief(text, attachments, pasted, dir)?;
    }
    Ok(())
}

fn handle_chat(app: &mut App, l: &Lay, client: &Client, ev: Event, t: f32) {
    let Mode::Chat { id, ref text, ref attachments, sending } = app.mode else { return };
    let (empty, images) = (text.s.is_empty(), attachments.len());
    let asking = app.agent(id).is_some_and(|a| a.status == Status::Blocked);
    let before = app.terminal_rect(l);
    let page = (before.3 - 1).max(1) as isize;
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => {
            let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
            match k.code {
                _ if is_leave(&k) || k.code == KeyCode::Esc => {
                    if sending { app.toast("Sending reply…", t); } else { app.close_comms(client); }
                }
                KeyCode::Tab => {
                    if empty && images == 0 && !sending { app.fullscreen_comms(id, client, l); }
                    else { app.toast("Send or clear your draft before opening full screen", t); }
                }
                KeyCode::PageUp => app.terminal_scroll(client, l, page, None),
                KeyCode::PageDown => app.terminal_scroll(client, l, -page, None),
                KeyCode::Home if ctrl => app.scroll_by(isize::MAX, client),
                KeyCode::End if ctrl => app.scroll_by(-(app.scroll as isize), client),
                KeyCode::Char('c' | 'C') if ctrl => app.copy(t),
                KeyCode::Char('v' | 'V') | KeyCode::Insert
                    if !sending && (ctrl || k.code == KeyCode::Insert && k.modifiers.contains(KeyModifiers::SHIFT)) => {
                    let result = screenshots::clipboard_content(&screenshots::directory());
                    match result {
                        Ok(screenshots::Clipboard::Text(s)) => {
                            if let Err(e) = chat_paste(app, &s, &screenshots::directory()) { app.toast(e, t); }
                        }
                        Ok(screenshots::Clipboard::Image(image)) => {
                            if let Mode::Chat { attachments, .. } = &mut app.mode {
                                if attachments.len() < screenshots::MAX_ATTACHMENTS { attachments.push(image); }
                                else { screenshots::discard(&[image]); app.toast("Attach up to 8 screenshots per reply", t); }
                            }
                        }
                        Err(e) => app.toast(e, t),
                    }
                }
                KeyCode::Char('x') if ctrl && !sending => {
                    if let Mode::Chat { attachments, .. } = &mut app.mode {
                        if let Some(image) = attachments.pop() { screenshots::discard(&[image]); }
                    }
                }
                KeyCode::Char('o') if ctrl && !sending => {
                    if let Mode::Chat { text, attachments, .. } = &mut app.mode {
                        if let Some(paths) = screenshots::dropped_paths(&text.s) {
                            match attach_paths(attachments, &paths, &screenshots::directory()) {
                                Ok(()) => *text = Line::default(),
                                Err(e) => app.toast(e, t),
                            }
                        } else { app.toast("Type an image path, then Ctrl+O to attach it", t); }
                    }
                }
                KeyCode::Enter if !sending && k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
                    chat_text(app).unwrap().paste_lines("\n");
                }
                KeyCode::Char('j') if ctrl && !sending => chat_text(app).unwrap().paste_lines("\n"),
                KeyCode::Enter if !sending => {
                    app.scroll_by(-(app.scroll as isize), client);
                    if let Mode::Chat { text, attachments, sending, .. } = &mut app.mode {
                        if text.s.trim().is_empty() && attachments.is_empty() {
                            client.send(Cmd::Input(id, "\r".into()));
                        } else if let Err(e) = screenshots::validate(attachments) {
                            app.toast(e, t);
                        } else {
                            let text = if text.s.trim().is_empty() { "Please inspect the attached screenshots." } else { &text.s };
                            client.send(Cmd::Say(id, screenshots::prompt(text, attachments)));
                            *sending = true;
                        }
                    }
                }
                KeyCode::Up | KeyCode::Down if empty && images == 0 && !sending => {
                    client.send(Cmd::Input(id, if k.code == KeyCode::Up { "\x1b[A" } else { "\x1b[B" }.into()));
                }
                KeyCode::Up | KeyCode::Down if !sending => { chat_text(app).unwrap().vertical(k.code == KeyCode::Up); }
                KeyCode::Char(c) if empty && images == 0 && asking && c.is_ascii_digit() && k.modifiers.is_empty() && !sending => {
                    client.send(Cmd::Answer(id, c.to_string()));
                }
                _ if !sending => { chat_text(app).unwrap().key(&k); }
                _ => {}
            }
        }
        Event::Paste(p) if !sending => {
            if let Err(e) = chat_paste(app, &p, &screenshots::directory()) { app.toast(e, t); }
        }
        Event::Mouse(me) => {
            let (x, y, width, height) = app.terminal_rect(l);
            let (c, r) = (me.column as i32, me.row as i32);
            let in_term = c >= x && c < x + width && r >= y && r < y + height;
            match me.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    app.terminal_scroll(client, l, if me.kind == MouseEventKind::ScrollUp { 3 } else { -3 }, Some((c, r)));
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if in_term { app.select(me, l, t); }
                    else if c == x + width && r >= y && r < y + height {
                        let history = app.screen.as_ref().map_or(0, |(_, s)| s.history);
                        let target = history * (height - 1 - (r - y)) as usize / (height - 1).max(1) as usize;
                        app.scroll_by(target as isize - app.scroll as isize, client);
                    } else {
                        let (bx, by, bw, bh) = draw::chat_box(l);
                        if c < bx || c >= bx + bw || r < by || r >= by + bh { app.close_comms(client); }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) if app.pick.is_some() => app.select(me, l, t),
                _ => {}
            }
        }
        _ => {}
    }
    if matches!(app.mode, Mode::Chat { .. }) && before != app.terminal_rect(l) { app.fit_comms(client, l); }
}

/// Handle one input event. Returns true to quit.
#[allow(clippy::too_many_arguments)]
/// Navigate the buildings without sending any order until its draft is submitted.
fn planet_input(app: &mut App, l: &Lay, client: &Client, ev: &Event) {
    let Mode::Planet(mut v) = std::mem::replace(&mut app.mode, Mode::Galaxy) else { return };
    let g = draw::PlanetLayout::new(l);
    let before = (v.sel, v.file);
    let mut delta: isize = 0;
    let mut edge = None;
    let mut build = false;
    let mut comms = false;
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return,
            KeyCode::Tab | KeyCode::Right => v.focus = (v.focus + 1) % 3,
            KeyCode::BackTab | KeyCode::Left => v.focus = (v.focus + 2) % 3,
            KeyCode::Up | KeyCode::Char('k') => delta = -1,
            KeyCode::Down | KeyCode::Char('j') => delta = 1,
            KeyCode::PageUp => { v.focus = 2; delta = -(g.preview_rows as isize); }
            KeyCode::PageDown => { v.focus = 2; delta = g.preview_rows as isize; }
            KeyCode::Home => edge = Some(false),
            KeyCode::End => edge = Some(true),
            KeyCode::Char(d @ '1'..='8') => { v.sel = d as usize - '1' as usize; v.focus = 0; }
            KeyCode::Enter => {
                if v.files().is_empty() { build = true; }
                else { v.focus = (v.focus + 1).min(2); }
            }
            KeyCode::Char('B' | 'b') => build = true,
            KeyCode::Char('c') => comms = true,
            KeyCode::Char('R') => {
                let path = v.files().get(v.file).cloned();
                v.built = crate::structure::survey(&v.key);
                v.file = path.and_then(|p| v.files().iter().position(|f| f == &p)).unwrap_or(0);
                v.read();
            }
            _ => {}
        },
        Event::Mouse(me) => {
            let (x, y) = (me.column as i32, me.row as i32);
            if me.kind == MouseEventKind::Up(MouseButton::Left) && y == 0 && x < 14 { return; }
            let focus = if y >= g.y && y < l.rows - 2 {
                Some(if x < g.split { 0 } else if y < g.preview_y - 1 { 1 } else { 2 })
            } else { None };
            if let Some(focus) = focus {
                match me.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        v.focus = focus;
                        delta = if me.kind == MouseEventKind::ScrollUp { -1 } else { 1 };
                        if focus == 2 { delta *= 3; }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        v.focus = focus;
                        if focus == 0 {
                            let i = v.sel.saturating_sub(g.buildings - 1) + ((y - g.y) / g.step) as usize;
                            if i < KINDS.len() { v.sel = i; }
                        } else if focus == 1 {
                            let i = v.file.saturating_sub(g.files - 1) + (y - g.y) as usize;
                            if i < v.files().len() { v.file = i; }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let max_scroll = v.lines(g.width as usize).len().saturating_sub(g.preview_rows as usize);
    let max = match v.focus { 0 => KINDS.len() - 1, 1 => v.files().len().saturating_sub(1), _ => max_scroll };
    let pos = match v.focus { 0 => &mut v.sel, 1 => &mut v.file, _ => &mut v.scroll };
    *pos = edge.map_or_else(|| pos.saturating_add_signed(delta).min(max), |end| if end { max } else { 0 });
    if before.0 != v.sel { v.file = 0; }
    if before != (v.sel, v.file) { v.read(); }
    if build {
        let (key, kind) = (v.key.clone(), KINDS[v.sel]);
        app.survey.worlds.entry(key.clone()).or_default().structures = v.built.clone();
        app.planet_return = Some(v);
        app.construct(&key, kind);
    } else if comms {
        if let Some(id) = app.lead_agent(&v.key) {
            app.planet_return = Some(v);
            app.open_comms(id, client, l);
        } else {
            v.preview = "No ships in orbit. Return to the galaxy and press e to deploy one.".into();
            app.mode = Mode::Planet(v);
        }
    } else { app.mode = Mode::Planet(v); }
}

fn handle(
    app: &mut App,
    w: &World,
    l: &Lay,
    client: &Client,
    sound: Option<&Sound>,
    ev: Event,
    t: f32,
    drag: &mut Option<((u16, u16), (f32, f32), bool)>,
) -> bool {
    if matches!(app.mode, Mode::Planet(_)) {
        planet_input(app, l, client, &ev);
        return false;
    }
    if matches!(app.mode, Mode::Chat { .. }) {
        handle_chat(app, l, client, ev, t);
        return false;
    }

    // ── comms: you're talking to the agent directly
    if let Mode::Comms(id) = app.mode {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                let page = (l.rows - 4).max(1) as isize;
                if is_leave(&k) {
                    app.close_comms(client);
                } else if k.modifiers.contains(KeyModifiers::SHIFT) && k.code == KeyCode::PageUp {
                    app.scroll_by(page, client);
                } else if k.modifiers.contains(KeyModifiers::SHIFT) && k.code == KeyCode::PageDown {
                    app.scroll_by(-page, client);
                } else if let Some(s) = key_bytes(&k) {
                    app.pick = None;
                    app.scroll_by(-(app.scroll as isize), client);
                    client.send(Cmd::Input(id, s));
                }
            }
            Event::Mouse(me) if matches!(me.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) => {
                let up = me.kind == MouseEventKind::ScrollUp;
                let (mouse, sgr, alt) = app.screen.as_ref().map_or((false, false, false), |(_, s)| (s.mouse, s.sgr, s.alt));
                // inside the box: 1-based terminal coordinates for the agent
                let (x, y) = ((me.column as i32).max(1), (me.row as i32 - 1).max(1));
                if mouse && app.scroll == 0 {
                    // the agent handles the wheel itself
                    let b = if up { 64 } else { 65 };
                    let seq = if sgr {
                        format!("\x1b[<{b};{x};{y}M")
                    } else {
                        let enc = |v: i32| char::from_u32((32 + v).min(255) as u32).unwrap_or(' ');
                        format!("\x1b[M{}{}{}", enc(b), enc(x), enc(y))
                    };
                    client.send(Cmd::Input(id, seq));
                } else if !alt {
                    // our own history; a full-screen app without mouse support has none, and
                    // arrow keys would page through its prompt history, so it gets nothing
                    app.scroll_by(if up { 3 } else { -3 }, client);
                }
            }
            Event::Mouse(me) => app.select(me, l, t),
            Event::Paste(p) => {
                app.pick = None;
                app.scroll_by(-(app.scroll as isize), client);
                client.send(Cmd::Input(id, format!("\x1b[200~{p}\x1b[201~")));
            }
            _ => {}
        }
        return false;
    }

    if matches!(app.mode, Mode::Campaigns) {
        campaign_input(app, w, l, client, &ev);
        return false;
    }
    if matches!(app.mode, Mode::Hyperlanes) {
        hyperlane_input(app, w, l, &ev);
        return false;
    }
    if matches!(app.mode, Mode::Refit(_)) {
        refit_input(app, &ev, t);
        return false;
    }
    if matches!(app.mode, Mode::Shipyard(_)) {
        shipyard_input(app, &ev);
        return false;
    }
    if matches!(app.mode, Mode::Sectors) {
        sector_input(app, w, l, &ev, t);
        return false;
    }
    if let Mode::Anomalies { id, typing } = app.mode {
        anomaly_input(app, w, l, &ev, id, typing, t);
        return false;
    }
    if let Event::Paste(pasted) = &ev {
        if let Mode::Brief { text, attachments, sending: false, .. } = &mut app.mode {
            if let Err(e) = paste_brief(text, attachments, pasted, &screenshots::directory()) {
                app.toast(e, t);
            }
        }
        return false;
    }
    if matches!(app.mode, Mode::Commander) {
        commander_input(app, w, l, client, &ev);
        return false;
    }
    if let Mode::Situation { id, typing } = app.mode {
        situation_input(app, w, l, client, &ev, id, typing, t);
        return false;
    }
    if let Mode::Trade { sel } = app.mode {
        let rows = app.trade_rows(w);
        let max = rows.len().saturating_sub(1);
        let mut selected = sel.min(max);
        let mut activate = false;
        match &ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => { app.mode = Mode::Galaxy; return false; }
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(max),
                KeyCode::Home => selected = 0,
                KeyCode::End => selected = max,
                KeyCode::PageUp => selected = selected.saturating_sub((l.rows - 10).max(1) as usize),
                KeyCode::PageDown => selected = (selected + (l.rows - 10).max(1) as usize).min(max),
                KeyCode::Enter => activate = true,
                KeyCode::Tab | KeyCode::Char('v') => {
                    app.trade_view = app.trade_view.next();
                    selected = 0;
                }
                KeyCode::Char('t') => {
                    app.trade_mission(w, selected);
                    return false;
                }
                _ => {}
            },
            Event::Mouse(me) => match me.kind {
                MouseEventKind::ScrollUp => selected = selected.saturating_sub(1),
                MouseEventKind::ScrollDown => selected = (selected + 1).min(max),
                MouseEventKind::Up(MouseButton::Left) => {
                    if let Some(row) = draw::trade_hit(l, rows.len(), selected, me.column as i32, me.row as i32) {
                        activate = row == selected;
                        selected = row;
                    }
                }
                _ => {}
            },
            _ => {}
        }
        app.mode = Mode::Trade { sel: selected };
        if activate { app.open_trade_row(w, selected, client, t); }
        return false;
    }
    // a click on the mission composer's options picks them
    if let Event::Mouse(me) = &ev
        && me.kind == MouseEventKind::Down(MouseButton::Left)
        && let Mode::Brief { planet, text, attachments, launch, role, to, sending: false, .. } = &mut app.mode
    {
        let ships = brief_ships(&app.agents, planet);
        match draw::brief_hit(l, text, attachments.len(), ships.len(), me.column as i32, me.row as i32) {
            Some(draw::BriefHit::Launch) => *launch = true,
            Some(draw::BriefHit::Ship(i)) => {
                *launch = false;
                *to = ships.get(i).copied();
            }
            Some(draw::BriefHit::Queue) => {
                *launch = false;
                *to = None;
            }
            Some(draw::BriefHit::Role(i)) => {
                *role = i;
                *launch = true;
            }
            Some(draw::BriefHit::Design) => app.next_design(t),
            Some(draw::BriefHit::Refit) => app.open_refit(),
            None => return false,
        }
        app.advisor.touched = true;
        return false;
    }
    let Event::Key(k) = ev else {
        if let Event::Mouse(me) = ev
            && !matches!(app.mode, Mode::Brief { .. })
        {
            mouse(app, w, l, client, me, drag, t);
        }
        return false;
    };
    if k.kind == KeyEventKind::Release {
        return false;
    }
    let nk = app.kinds.len().max(1);

    // ── dialogs
    if matches!(app.mode, Mode::Brief { sending: false, .. }) && k.code == KeyCode::Enter && !k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
        && let Some(draft) = &app.campaign_draft
    {
        let (link, context) = (draft.link, draft.context.clone());
        let result = app.sync_campaigns(|_| Ok(0)).and_then(|_| app.campaigns.brief(link.campaign, link.stage));
        match result {
            Ok(now) if now == context => {}
            Ok(_) => { app.toast("Campaign changed; cancel and reopen the mission to refresh its handoff", t); return false; }
            Err(e) => { app.toast(e, t); return false; }
        }
    }
    // a design outfits the composer's new ship; a refit adjusts its orders and skills
    if matches!(app.mode, Mode::Brief { sending: false, .. }) && k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('d' | 'r')) {
        if k.code == KeyCode::Char('d') { app.next_design(t); } else { app.open_refit(); }
        return false;
    }
    match &mut app.mode {
        Mode::Help => {
            app.mode = Mode::Galaxy;
            return false;
        }
        Mode::Confirm { id } => {
            let id = *id;
            if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                client.send(Cmd::Kill(id));
                if let Some(a) = app.agent(id) {
                    let (msg, cwd) = (format!("✖ decommissioned {} on {}", a.kind, base_name(&a.cwd)), a.cwd.clone());
                    app.say(Ev::Agent, msg, v3(1.0, 0.5, 0.35)).on(&cwd).by(id);
                }
            }
            app.mode = Mode::Galaxy;
            return false;
        }
        Mode::Ship { id, sel } => {
            let (id, mut s) = (*id, *sel);
            let acts = app.ship_acts(id);
            let pick = match k.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.mode = Mode::Galaxy;
                    None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    s = s.saturating_sub(1);
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    s = (s + 1).min(acts.len().saturating_sub(1));
                    None
                }
                KeyCode::Char(d @ '1'..='9') => acts.get(d as usize - '1' as usize).copied(),
                KeyCode::Enter | KeyCode::Char('c') => acts.get(s).copied(),
                KeyCode::Char('t') => Some(ShipAct::Mission),
                KeyCode::Char('x') => Some(ShipAct::Decommission),
                KeyCode::Char('r') => Some(ShipAct::Refit),
                KeyCode::Char('P') if acts.contains(&ShipAct::Priority) => Some(ShipAct::Priority),
                _ => None,
            };
            if let Mode::Ship { sel, .. } = &mut app.mode {
                *sel = s;
            }
            if let Some(a) = pick {
                app.ship_act(id, a, client, l, t);
            }
            return false;
        }
        Mode::ShipRefit { id, sel, confirm } => {
            let (id, mut s, asking) = (*id, *sel, *confirm);
            let Some(a) = app.agent(id) else {
                app.mode = Mode::Galaxy;
                return false;
            };
            let (kind, planet, cwd, aboard, pending) = (a.kind.clone(), base_name(&a.cwd), a.cwd.clone(), (a.role, a.spec.clone()), a.refit.is_some());
            let plan = app.refit_outfit(s);
            if asking {
                // nothing is sent until the player says yes
                match (k.code, plan) {
                    (KeyCode::Char('y') | KeyCode::Char('Y'), Some((role, spec))) => {
                        let name = spec.design.clone();
                        client.send(Cmd::Refit { id, role: role.name().into(), spec });
                        app.say(Ev::Agent, format!("⚒ refit ordered: {kind} on {planet} → {name}; restarts on its own session when idle"), v3(1.0, 0.75, 0.3)).on(&cwd).by(id);
                        app.mode = Mode::Ship { id, sel: 0 };
                    }
                    _ => app.mode = Mode::ShipRefit { id, sel: s, confirm: false },
                }
                return false;
            }
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.mode = Mode::Ship { id, sel: 0 };
                    return false;
                }
                KeyCode::Up | KeyCode::Char('k') => s = s.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => s = (s + 1).min(app.designs.designs.len().saturating_sub(1)),
                KeyCode::Enter | KeyCode::Char('y') => match plan {
                    Some(p) if p == aboard => app.toast("It already carries that outfit", t),
                    Some(_) => {
                        app.mode = Mode::ShipRefit { id, sel: s, confirm: true };
                        return false;
                    }
                    None => app.toast("No designs yet: save one from a mission composer with ^S", t),
                },
                KeyCode::Char('c') if pending => {
                    client.send(Cmd::CancelRefit(id));
                    app.say(Ev::Agent, format!("⚒ refit of {kind} on {planet} called off"), DIM_LOG).on(&cwd).by(id);
                }
                _ => {}
            }
            if let Mode::ShipRefit { sel, .. } = &mut app.mode {
                *sel = s;
            }
            return false;
        }
        Mode::Actions { planet, sel } => {
            let (planet, mut s) = (planet.clone(), *sel);
            let acts: Vec<Act> = app.needs(&planet).into_iter().filter_map(|n| n.act).collect();
            let pick = match k.code {
                KeyCode::Esc | KeyCode::Char('r') => {
                    app.mode = Mode::Galaxy;
                    None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    s = s.saturating_sub(1);
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    s = (s + 1).min(acts.len().saturating_sub(1));
                    None
                }
                KeyCode::Char(d @ '1'..='9') => acts.get(d as usize - '1' as usize).cloned(),
                KeyCode::Enter => acts.get(s).cloned(),
                _ => None,
            };
            if let Mode::Actions { sel, .. } = &mut app.mode {
                *sel = s;
            }
            if let Some(a) = pick {
                app.act(&planet, a, client, l, t);
            }
            return false;
        }
        Mode::Build { planet, sel } => {
            let (planet, mut s) = (planet.clone(), *sel);
            let pick = match k.code {
                KeyCode::Esc | KeyCode::Char('B') => {
                    app.mode = Mode::Galaxy;
                    None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    s = s.saturating_sub(1);
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    s = (s + 1).min(KINDS.len() - 1);
                    None
                }
                KeyCode::Char(d @ '1'..='9') => KINDS.get(d as usize - '1' as usize).copied(),
                KeyCode::Enter => KINDS.get(s).copied(),
                _ => None,
            };
            if let Mode::Build { sel, .. } = &mut app.mode {
                *sel = s;
            }
            if let Some(kind) = pick {
                app.construct(&planet, kind);
            }
            return false;
        }
        Mode::Log { world, sel, open } => {
            let (world, mut s) = (world.clone(), *sel);
            let mut open = std::mem::take(open);
            let log: Vec<(u32, Option<u32>)> = app.mission_log(&world).iter().map(|(_, m)| (m.id, m.agent)).collect();
            match k.code {
                KeyCode::Esc | KeyCode::Char('L') | KeyCode::Char('q') => app.mode = Mode::Galaxy,
                KeyCode::Up | KeyCode::Char('k') => s = s.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => s = (s + 1).min(log.len().saturating_sub(1)),
                // strike it: a queued mission is called off, a finished one leaves the log
                KeyCode::Char('x') | KeyCode::Delete => {
                    if let Some((id, _)) = log.get(s) {
                        client.send(Cmd::Scrub(*id));
                        app.missions.retain(|m| m.id != *id);
                        s = s.saturating_sub(1);
                    }
                }
                KeyCode::Enter => {
                    if let Some(&(_, Some(aid))) = log.get(s)
                        && app.agent(aid).is_some()
                    {
                        app.sel = Some(Sel::Agent(aid));
                        app.open_comms(aid, client, l);
                    }
                }
                // expand / collapse: one mission, or all of them
                KeyCode::Char(' ') => {
                    if let Some((id, _)) = log.get(s)
                        && !open.remove(id)
                    {
                        open.insert(*id);
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => open.extend(log.get(s).map(|m| m.0)),
                KeyCode::Left | KeyCode::Char('h') => {
                    if let Some((id, _)) = log.get(s) {
                        open.remove(id);
                    }
                }
                KeyCode::Char('e') => {
                    if open.len() < log.len() {
                        open.extend(log.iter().map(|m| m.0));
                    } else {
                        open.clear();
                    }
                }
                // regroup, keeping the same mission selected
                KeyCode::Char('s') | KeyCode::Tab => {
                    app.log_sort = app.log_sort.next();
                    let id = log.get(s).map(|m| m.0);
                    s = app.mission_log(&world).iter().position(|(_, m)| Some(m.id) == id).unwrap_or(0);
                }
                _ => {}
            }
            if let Mode::Log { sel, open: o, .. } = &mut app.mode {
                *sel = s;
                *o = open;
            }
            return false;
        }
        Mode::Moon { planet, branch, kind } => {
            match k.code {
                KeyCode::Esc => app.mode = Mode::Galaxy,
                KeyCode::Up => *kind = (*kind + nk - 1) % nk,
                KeyCode::Down => *kind = (*kind + 1) % nk,
                KeyCode::Enter if !branch.s.trim().is_empty() => {
                    let cmd = Cmd::Moon { cwd: planet.clone(), branch: branch.s.trim().to_string(), kind: app.kinds[*kind].clone() };
                    let msg = format!("☾ founding moon {}…", branch.s.trim());
                    client.send(cmd);
                    app.mode = Mode::Galaxy;
                    app.toast(msg, t);
                }
                _ => {
                    branch.key(&k);
                }
            }
            return false;
        }
        Mode::Dock { moon } => {
            if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                client.send(Cmd::Dock(moon.clone()));
                app.toast("⊕ docking…", t);
            }
            app.mode = Mode::Galaxy;
            return false;
        }
        Mode::Fleet { target, text } => {
            match k.code {
                KeyCode::Esc => app.mode = Mode::Galaxy,
                KeyCode::Enter if !text.s.trim().is_empty() => {
                    let (target, text) = (target.clone(), text.clone());
                    let ids: Vec<u32> = app.fleet(w, &target).iter().filter(|a| a.status != Status::Blocked).map(|a| a.id).collect();
                    let n = ids.len();
                    client.send(Cmd::Broadcast(ids, text.s.trim().to_string()));
                    app.mode = Mode::Galaxy;
                    app.toast(format!("fleet order sent to {n} ship{}", if n == 1 { "" } else { "s" }), t);
                }
                _ => {
                    text.key(&k);
                }
            }
            return false;
        }
        Mode::Brief { planet, text, kind, launch, role, to, attachments, sending, ship } => {
            if *sending { return false; }
            match k.code {
                KeyCode::Esc => {
                    screenshots::discard(attachments);
                    app.mode = if app.campaign_draft.take().is_some() { Mode::Campaigns } else { app.anomaly_return.take().map_or_else(|| app.planet_return.take().map_or(Mode::Galaxy, Mode::Planet), |id| Mode::Anomalies { id, typing: false }) };
                }
                KeyCode::Char('v') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    let result = if attachments.len() >= screenshots::MAX_ATTACHMENTS {
                        Err("Attach up to 8 screenshots per mission".into())
                    } else { screenshots::clipboard(&screenshots::directory()) };
                    match result {
                        Ok(image) => attachments.push(image),
                        Err(e) => app.toast(e, t),
                    }
                }
                KeyCode::Char('x') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(image) = attachments.pop() { screenshots::discard(&[image]); }
                }
                KeyCode::Char('o') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(paths) = screenshots::dropped_paths(&text.s) {
                        match attach_paths(attachments, &paths, &screenshots::directory()) {
                            Ok(()) => *text = Line::default(),
                            Err(e) => app.toast(e, t),
                        }
                    } else { app.toast("Type an image file path, then Ctrl+O to attach it", t); }
                }
                // a new line: Shift/Alt+Enter, or Ctrl+J where the terminal can't tell those apart
                KeyCode::Enter if k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => text.paste_lines("\n"),
                KeyCode::Char('j') if k.modifiers.contains(KeyModifiers::CONTROL) => text.paste_lines("\n"),
                // ↑↓ move through the lines; past the first or last they pick the species
                KeyCode::Up | KeyCode::Down => {
                    let up = k.code == KeyCode::Up;
                    if !text.vertical(up) {
                        *kind = if up { (*kind + nk - 1) % nk } else { (*kind + 1) % nk };
                    }
                }
                // launch → each ship here → queue for the first free one → launch
                KeyCode::Tab => {
                    let ships = brief_ships(&app.agents, planet);
                    let at = if *launch { 0 } else { to.and_then(|id| ships.iter().position(|&s| s == id)).map_or(ships.len() + 1, |i| i + 1) };
                    let next = (at + 1) % (ships.len() + 2);
                    *launch = next == 0;
                    *to = if next >= 1 { ships.get(next - 1).copied() } else { None };
                    app.advisor.touched = true;
                }
                // a role is for a new ship, so picking one means launching
                KeyCode::BackTab => {
                    *role = (*role + 1) % ROLES.len();
                    *launch = true;
                    app.advisor.touched = true;
                }
                KeyCode::Enter if !text.s.trim().is_empty() || !attachments.is_empty() => {
                    let prompt = if text.s.trim().is_empty() { "Review the attached screenshots." } else { text.s.trim() };
                    let role = if *launch { ROLES[*role].name().into() } else { String::new() };
                    let to = if *launch { None } else { *to };
                    let species = app.kinds[*kind].clone();
                    let spec = if *launch {
                        let design = ship.design.and_then(|id| app.designs.get(id)).map(|d| d.name.clone()).unwrap_or_default();
                        ShipSpec { orders: ship.orders.clone(), skills: ship.skills.clone(), design }
                    } else { ShipSpec::default() };
                    // launch anyway, but say which skills this species won't find
                    let missing: Vec<String> = spec.skills.iter().filter(|s| !crate::design::reachable(&species, s, Path::new(planet), &app.skill_home)).cloned().collect();
                    let cmd = Cmd::Mission { cwd: planet.clone(), text: prompt.to_string(), kind: species.clone(), launch: *launch, attachments: attachments.clone(), draft: true, role, to, campaign: app.campaign_draft.as_ref().map(|d| d.link), spec };
                    *sending = true;
                    client.send(cmd);
                    if !missing.is_empty() { app.toast(format!("{species} can't find skill {}; launching without it", missing.join(", ")), t); }
                }
                _ => {
                    text.key(&k);
                }
            }
            return false;
        }
        Mode::Deploy { planet, kind } => {
            match k.code {
                KeyCode::Esc => app.mode = Mode::Galaxy,
                KeyCode::Up | KeyCode::Left | KeyCode::Char('k') | KeyCode::Char('h') => *kind = (*kind + nk - 1) % nk,
                KeyCode::Down | KeyCode::Right | KeyCode::Tab | KeyCode::Char('j') | KeyCode::Char('l') => *kind = (*kind + 1) % nk,
                KeyCode::Enter => {
                    let (p, kd) = (planet.clone(), app.kinds[*kind].clone());
                    app.spawn(&kd, Path::new(&p), client, t);
                }
                _ => {}
            }
            return false;
        }
        Mode::Colonize(c) => {
            match k.code {
                KeyCode::Esc => app.mode = Mode::Galaxy,
                KeyCode::Up => c.kind = (c.kind + nk - 1) % nk,
                KeyCode::Down => c.kind = (c.kind + 1) % nk,
                KeyCode::Tab | KeyCode::BackTab => {
                    if !c.comps.is_empty() {
                        if k.code == KeyCode::BackTab {
                            c.comp_i = if c.comp_i == 0 { c.comps.len() - 1 } else { (c.comp_i + c.comps.len() - 2) % c.comps.len() };
                        }
                        let pick = c.comps[c.comp_i % c.comps.len()].clone();
                        c.comp_i += 1;
                        if c.comps.len() == 1 {
                            c.path.set(pick);
                            c.comps = complete(&c.path.s);
                            c.comp_i = 0;
                        } else {
                            c.path.set(pick);
                        }
                        c.mkdir = false;
                    }
                }
                KeyCode::Enter => {
                    let path = expand(c.path.s.trim());
                    let kind = app.kinds[c.kind].clone();
                    if path.is_dir() {
                        app.spawn(&kind, &path, client, t);
                    } else if path.exists() {
                        app.toast("✖ that's a file, not a folder", t);
                    } else if !c.mkdir {
                        c.mkdir = true;
                    } else {
                        match std::fs::create_dir_all(&path) {
                            Ok(()) => {
                                app.say(Ev::World, format!("◆ charted new world {}", tilde(&path.to_string_lossy())), v3(0.6, 0.9, 1.0)).on(&path.to_string_lossy());
                                app.spawn(&kind, &path, client, t);
                            }
                            Err(e) => app.toast(format!("✖ {e}"), t),
                        }
                    }
                }
                _ => {
                    if c.path.key(&k) {
                        c.comps = complete(&c.path.s);
                        c.comp_i = 0;
                        c.mkdir = false;
                    }
                }
            }
            return false;
        }
        Mode::Galaxy | Mode::Refit(_) | Mode::Shipyard(_) | Mode::Sectors | Mode::Hyperlanes | Mode::Campaigns | Mode::Planet(_) | Mode::Anomalies { .. } | Mode::Commander | Mode::Comms(_) | Mode::Chat { .. } | Mode::Trade { .. } | Mode::Situation { .. } => {}
    }

    // ── galaxy
    let rows = app.rows(w);
    let cur = app.sel.as_ref().and_then(|s| rows.iter().position(|r| r == s));
    let pick = |app: &mut App, i: usize| {
        if let Some(s) = rows.get(i) {
            app.sel = Some(s.clone());
            app.glide_to_sel(w);
        }
    };
    let pan = 60.0 / app.cam.z;
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('C') => app.open_commander(w),
        KeyCode::Char('A') => app.open_anomalies(),
        KeyCode::Char('V') => app.open_sectors(),
        KeyCode::Char('H') => app.open_hyperlanes(),
        KeyCode::Char('O') => app.open_campaigns(),
        KeyCode::Char('Y') => app.open_shipyard(),
        KeyCode::Char('h') => app.toggle_dependencies(),
        KeyCode::Char('c') if ctrl => return true,
        KeyCode::Esc => { app.sel = None; app.sector_focus = None; },
        KeyCode::Down | KeyCode::Char('j') => pick(app, cur.map_or(0, |c| (c + 1).min(rows.len().saturating_sub(1)))),
        KeyCode::Up | KeyCode::Char('k') => pick(app, cur.map_or(0, |c| c.saturating_sub(1))),
        KeyCode::Home | KeyCode::Char('g') => pick(app, 0),
        KeyCode::End | KeyCode::Char('G') => pick(app, rows.len().saturating_sub(1)),
        // the transmission on screen takes Enter, unless you've picked another agent
        KeyCode::Enter
            if let Some(id) = app.transmission().map(|a| a.id)
                && app.sel != Some(Sel::GitHub)
                && !matches!(app.sel, Some(Sel::Planet(_) | Sel::Sector(_, _) | Sel::Lane(_)))
                && !(app.cam.z < SECTOR_ZOOM && matches!(app.sel, Some(Sel::Sys(_))))
                && !matches!(app.sel, Some(Sel::Agent(s)) if s != id) =>
        {
            app.sel = Some(Sel::Agent(id));
            app.open_comms(id, client, l);
        }
        KeyCode::Enter | KeyCode::Char('c') | KeyCode::Char(' ') => app.activate(w, l, client),
        KeyCode::Tab => {
            let alerts: Vec<u32> = rows
                .iter()
                .filter_map(|r| match r {
                    Sel::Agent(id) if app.agent(*id).is_some_and(|a| a.status.needs_you()) => Some(*id),
                    _ => None,
                })
                .collect();
            if alerts.is_empty() {
                app.toast("all quiet across the empire ✓", t);
            } else {
                let at = match app.sel {
                    Some(Sel::Agent(id)) => alerts.iter().position(|&a| a == id).map_or(0, |p| (p + 1) % alerts.len()),
                    _ => 0,
                };
                app.sel = Some(Sel::Agent(alerts[at]));
                app.glide_to_sel(w);
                app.cam.tz = app.cam.tz.max(1.6);
            }
        }
        KeyCode::Char(d @ '1'..='9') if app.transmission().is_some() => {
            let a = app.transmission().unwrap();
            let (id, kind, planet, cwd) = (a.id, a.kind.clone(), base_name(&a.cwd), a.cwd.clone());
            let opts = a.ask.as_ref().map(|q| q.options.clone()).unwrap_or_default();
            if let Some(o) = opts.get(d as usize - '1' as usize) {
                client.send(Cmd::Answer(id, o.key.clone()));
                app.say(Ev::Mission, format!("➤ {kind} on {planet}: {}", o.label), v3(0.6, 0.85, 1.0)).on(&cwd).by(id);
                app.toast(format!("orders sent: {}", o.label), t);
            }
        }
        KeyCode::Char('n') => {
            let start = app.sel_planet().map(|p| parent(&p)).unwrap_or_else(|| home().to_string_lossy().into_owned());
            let mut path = Line::default();
            path.set(format!("{}/", tilde(&start).trim_end_matches('/')));
            let kind = 0;
            let comps = complete(&path.s);
            app.mode = Mode::Colonize(Colonize { path, kind, comps, comp_i: 0, mkdir: false });
        }
        KeyCode::Char('b') => match app.sel.clone() {
            Some(s @ (Sel::Sys(_) | Sel::Planet(_))) if !app.fleet(w, &s).is_empty() => app.mode = Mode::Fleet { target: s, text: Line::default() },
            _ => app.toast("select a system or world with ships in orbit to give a fleet order", t),
        },
        KeyCode::Char('u') => match app.sel_planet() {
            Some(p) => {
                let host = app.moon_host(&p).unwrap_or(p);
                if app.survey.worlds.get(&host).is_some_and(|w| w.git) {
                    let taken: Vec<String> = app.survey.worlds.values().filter(|w| w.host.as_ref() == Some(&host)).map(|w| w.branch.clone()).collect();
                    let name = MOONS.iter().find(|m| !taken.iter().any(|b| b == *m)).copied().unwrap_or("moon");
                    let mut branch = Line::default();
                    branch.set(name.to_string());
                    app.mode = Mode::Moon { planet: host, branch, kind: 0 };
                } else {
                    app.toast("only git worlds can have moons", t);
                }
            }
            None => app.toast("select a world to found a moon (git worktree) off it", t),
        },
        KeyCode::Char('D') => match app.sel_planet() {
            Some(p) if app.moon_host(&p).is_some() => app.mode = Mode::Dock { moon: p },
            _ => app.toast("select a moon to dock it", t),
        },
        KeyCode::Char('r') => match app.sel_planet() {
            Some(p) if app.needs(&p).iter().any(|n| n.act.is_some()) => app.mode = Mode::Actions { planet: p, sel: 0 },
            Some(p) => app.toast(format!("{} needs nothing from you right now ✓", base_name(&p)), t),
            None => app.toast("select a world to see what it needs", t),
        },
        KeyCode::Char('B') => match app.sel_planet() {
            Some(p) => app.mode = Mode::Build { planet: p, sel: 0 },
            None => app.toast("select a world to see its structures and build more", t),
        },
        KeyCode::Char('L') => match app.sel_planet() {
            // a moon's log is its world's, so the family's work reads together
            Some(p) => app.mode = Mode::Log { world: app.moon_host(&p).unwrap_or(p), sel: 0, open: HashSet::new() },
            None => app.toast("select a world to see its mission log", t),
        },
        KeyCode::Char('S') => app.mode = Mode::Situation { id: app.log.front().map_or(0, |e| e.id), typing: false },
        KeyCode::Char('X') => match app.sel_planet() {
            Some(p) if w.planet(&p).is_some_and(|p| p.dormant) && app.campaigns.campaigns.iter().any(|c| !c.archived && c.stages.iter().any(|s| s.world == p)) => {
                app.toast("this world is kept by a campaign · O to archive it first", t);
            }
            Some(p) if w.planet(&p).is_some_and(|p| p.dormant) && app.hyperlanes.lanes.iter().any(|l| l.from == p || l.to == p) => {
                app.toast("this world is kept by a hyperlane · H to remove its links first", t);
            }
            Some(p) if w.planet(&p).is_some_and(|p| p.dormant) && app.sectors.sectors.iter().any(|s| s.contains(&p, None)) => {
                app.toast("this world is kept by a sector · V to remove its membership first", t);
            }
            Some(p) if w.planet(&p).is_some_and(|p| p.dormant) => {
                app.history.remove(&p);
                let _ = std::fs::write(history_file(), serde_json::to_string(&app.history).unwrap_or_default());
                app.say(Ev::World, format!("◌ {} forgotten", base_name(&p)), DIM_LOG).on(&p);
                app.sel = None;
            }
            _ => app.toast("select a dormant world (no agents) to forget it", t),
        },
        KeyCode::Char('P') => match app.sel {
            Some(Sel::Agent(id)) if app.agent(id).is_some_and(|a| !a.war.is_empty()) => {
                client.send(Cmd::Priority(id));
                let a = app.agent(id).unwrap();
                let (msg, cwd) = (format!("☮ {} on {} has priority: its rivals were told to stand down", a.kind, base_name(&a.cwd)), a.cwd.clone());
                app.say(Ev::Alert, msg, v3(0.6, 0.85, 1.0)).on(&cwd).by(id);
            }
            _ => app.toast("select an agent in a border conflict to give it priority", t),
        },
        KeyCode::Char('t') => match app.sel_planet() {
            Some(planet) => {
                // queue it if someone's there to take it, otherwise send a ship
                let crewed = app.agents.iter().any(|a| a.cwd == planet && a.dying.is_none());
                app.mode = Mode::Brief { planet, text: Line::default(), kind: 0, launch: !crewed, role: 0, to: None, attachments: Vec::new(), sending: false, ship: Outfit::default() };
            }
            None => app.toast("select a world to give it a mission", t),
        },
        KeyCode::Char('T') => {
            // strike finished missions on the selected world from the log
            if let Some(p) = app.sel_planet() {
                for m in app.missions.iter().filter(|m| m.cwd == p && m.state == "done") {
                    client.send(Cmd::Scrub(m.id));
                }
            }
        }
        KeyCode::Char('e') => match app.sel_planet() {
            Some(planet) => app.mode = Mode::Deploy { planet, kind: 0 },
            None => app.toast("select a planet first, or press n to colonize a new one", t),
        },
        KeyCode::Char('x') => match app.sel {
            Some(Sel::Agent(id)) if app.agent(id).is_some() => app.mode = Mode::Confirm { id },
            _ => app.toast("select an agent to decommission", t),
        },
        KeyCode::Char('w') => app.cam.ty -= pan,
        KeyCode::Char('s') => app.cam.ty += pan,
        KeyCode::Char('a') | KeyCode::Left => app.cam.tx -= pan,
        KeyCode::Char('d') | KeyCode::Right => app.cam.tx += pan,
        KeyCode::Char('+') | KeyCode::Char('=') => {
            app.cam.tz = (app.cam.tz * 1.3).min(6.0);
            if app.cam.tz >= 3.0 && let Some(Sel::Planet(k)) = app.sel.clone() {
                app.mode = Mode::Planet(PlanetView::new(k));
            }
        }
        KeyCode::Char('-') | KeyCode::Char('_') => app.cam.tz = (app.cam.tz / 1.3).max(0.12),
        KeyCode::Char('z') => app.fit(w, l),
        KeyCode::Char('f') => {
            if let Some(Sel::Lane(id)) = app.sel {
                app.lane_view.id = id; app.focus_lane(w, l);
            } else if matches!(app.sel, Some(Sel::Sector(_, _))) {
                app.activate(w, l, client);
            } else if let Some(Sel::Planet(k)) = app.sel.clone() {
                app.mode = Mode::Planet(PlanetView::new(k));
            } else {
                app.glide_to_sel(w);
                app.cam.tz = 2.4;
            }
        }
        KeyCode::Char('o') => {
            app.panel = !app.panel;
            app.bg_key = (0, 0, 0);
        }
        KeyCode::Char('M') => {
            if let Some(s) = sound {
                let muted = !s.music_muted.fetch_xor(true, Relaxed);
                app.toast(if muted { "music muted" } else { "music on" }, t);
            }
        }
        KeyCode::Char('m') => {
            if let Some(s) = sound {
                let muted = !s.muted.fetch_xor(true, Relaxed);
                app.toast(if muted { "comms chatter muted" } else { "comms chatter on" }, t);
            }
        }
        KeyCode::Char('?') => app.mode = Mode::Help,
        _ => {}
    }
    false
}

/// Keys and clicks in the situation log view.
#[allow(clippy::too_many_arguments)]
fn situation_input(app: &mut App, w: &World, l: &Lay, client: &Client, ev: &Event, id: u64, mut typing: bool, t: f32) {
    let ids: Vec<u64> = app.situation().iter().map(|e| e.id).collect();
    let max = ids.len().saturating_sub(1);
    let at = ids.iter().position(|&i| i == id).unwrap_or(0);
    let mut sel = at;
    let page = (l.rows - 20).max(1) as usize;
    let (mut go, mut comms) = (false, false);
    match ev {
        Event::Key(k) if k.kind == KeyEventKind::Release => {}
        // the search line takes every key until it's done
        Event::Key(k) if typing => match k.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Down | KeyCode::Up | KeyCode::Tab => typing = false,
            KeyCode::Backspace => {
                app.sit.query.pop();
            }
            KeyCode::Char(c) => app.sit.query.push(c),
            _ => {}
        },
        Event::Key(k) => match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('S') => {
                app.mode = Mode::Galaxy;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => sel = sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => sel = (sel + 1).min(max),
            KeyCode::Home | KeyCode::Char('g') => sel = 0,
            KeyCode::End | KeyCode::Char('G') => sel = max,
            KeyCode::PageUp => sel = sel.saturating_sub(page),
            KeyCode::PageDown => sel = (sel + page).min(max),
            KeyCode::Char('/') => typing = true,
            // one kind of news: by number, or cycling through them
            KeyCode::Char('0') => app.sit.kind = None,
            KeyCode::Char(d @ '1'..='7') => {
                let k = Ev::ALL[d as usize - '1' as usize];
                app.sit.kind = (app.sit.kind != Some(k)).then_some(k);
            }
            KeyCode::Char('f') | KeyCode::Tab | KeyCode::BackTab => {
                let i = app.sit.kind.and_then(|k| Ev::ALL.iter().position(|&e| e == k)).map_or(0, |i| i + 1);
                let n = Ev::ALL.len() + 1;
                let i = if k.code == KeyCode::BackTab { (i + n - 1) % n } else { (i + 1) % n };
                app.sit.kind = i.checked_sub(1).map(|i| Ev::ALL[i]);
            }
            // just the selected entry's world (and its moons), or every world again
            KeyCode::Char('w') => {
                if app.sit.world.take().is_none() {
                    let world = app.log.iter().find(|e| e.id == id).and_then(|e| e.world.clone());
                    match world {
                        Some(p) => app.sit.world = Some(app.moon_host(&p).unwrap_or(p)),
                        None => app.toast("this entry isn't about a world", t),
                    }
                }
            }
            KeyCode::Char('x') | KeyCode::Backspace => app.sit = SitFilter::default(),
            KeyCode::Enter => go = true,
            KeyCode::Char('c') => comms = true,
            _ => {}
        },
        Event::Mouse(me) => match me.kind {
            MouseEventKind::ScrollUp => sel = sel.saturating_sub(1),
            MouseEventKind::ScrollDown => sel = (sel + 1).min(max),
            MouseEventKind::Up(MouseButton::Left) => match draw::situation_hit(app, l, at, me.column as i32, me.row as i32) {
                Some(draw::SitHit::Row(i)) => {
                    go = i == sel;
                    sel = i;
                }
                Some(draw::SitHit::Kind(k)) => app.sit.kind = k,
                None => {}
            },
            _ => {}
        },
        _ => {}
    }
    // a changed filter keeps the same entry if it's still shown
    let list: Vec<u64> = app.situation().iter().map(|e| e.id).collect();
    let id = if list != ids { list.iter().copied().find(|&i| i == id).or(list.first().copied()) } else { ids.get(sel).copied() };
    let id = id.unwrap_or(0);
    app.mode = Mode::Situation { id, typing };
    let Some(e) = app.log.iter().find(|e| e.id == id) else { return };
    let (agent, world) = (e.agent.filter(|&a| app.agent(a).is_some()), e.world.clone().filter(|p| w.planet(p).is_some()));
    if comms {
        match agent {
            Some(a) => {
                app.sel = Some(Sel::Agent(a));
                app.open_comms(a, client, l);
            }
            None => app.toast("no agent to talk to here: it has left", t),
        }
    } else if go {
        app.sel = match (agent, world) {
            (Some(a), _) => Some(Sel::Agent(a)),
            (None, Some(p)) => Some(Sel::Planet(p)),
            _ => return app.toast("nothing to go to: its world and agent are gone", t),
        };
        app.mode = Mode::Galaxy;
        app.glide_to_sel(w);
    }
}

fn mouse(app: &mut App, w: &World, l: &Lay, client: &Client, me: event::MouseEvent, drag: &mut Option<((u16, u16), (f32, f32), bool)>, t: f32) {
    let (mc, mr) = (me.column, me.row);
    let px = (mc as f32 * 2.0 + 1.0, mr as f32 * 4.0 + 2.0);
    let in_panel = l.pw > 0 && mc as i32 >= l.cols - l.pw;
    match me.kind {
        MouseEventKind::ScrollUp if !in_panel => {
            let hit = pick_at(app, w, l, px);
            app.zoom_at(l, 1.15, px);
            if app.cam.z >= 3.0 && let Some(Sel::Planet(k)) = hit {
                app.sel = Some(Sel::Planet(k.clone()));
                app.mode = Mode::Planet(PlanetView::new(k));
            }
        }
        MouseEventKind::ScrollDown if !in_panel => app.zoom_at(l, 1.0 / 1.15, px),
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => draw::panel_scroll(app, w, l, mr as i32, if me.kind == MouseEventKind::ScrollUp { -3 } else { 3 }),
        MouseEventKind::Down(MouseButton::Left) => *drag = Some(((mc, mr), (app.cam.x, app.cam.y), false)),
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(((c0, r0), (x0, y0), moved)) = drag.as_mut() {
                let (dx, dy) = ((mc as f32 - *c0 as f32) * 2.0, (mr as f32 - *r0 as f32) * 4.0);
                if dx.abs() + dy.abs() > 2.0 {
                    *moved = true;
                }
                if *moved && !in_panel {
                    app.cam.x = *x0 - dx / app.cam.z;
                    app.cam.y = *y0 - dy / app.cam.z;
                    (app.cam.tx, app.cam.ty) = (app.cam.x, app.cam.y);
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) if matches!(app.mode, Mode::Ship { .. }) => {
            drag.take();
            let Mode::Ship { id, .. } = app.mode else { return };
            match draw::ship_hit(app, id, l, mc as i32, mr as i32) {
                Some(Some(i)) => {
                    if let Some(&a) = app.ship_acts(id).get(i) {
                        app.ship_act(id, a, client, l, t);
                    }
                }
                Some(None) => {}
                None => app.mode = Mode::Galaxy,
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let was_drag = drag.take().is_some_and(|d| d.2);
            if was_drag {
                return;
            }
            if mr as i32 == l.rows - 1 && (mc as usize) < draw::commander_button(app, w).chars().count() {
                app.open_commander(w);
                return;
            }
            if in_panel && let Some(id) = draw::panel_log_hit(app, w, l, mr as i32) {
                // show it even if the view's filters would hide it
                if !app.situation().iter().any(|e| e.id == id) {
                    app.sit = SitFilter::default();
                }
                app.mode = Mode::Situation { id, typing: false };
                return;
            }
            let hit = if in_panel { draw::panel_hit(app, w, l, mr as i32) } else { pick_at(app, w, l, px) };
            if let Some(h) = hit {
                if h == Sel::GitHub {
                    app.sel = Some(h);
                    app.mode = Mode::Trade { sel: 0 };
                    return;
                }
                if app.sel.as_ref() == Some(&h) {
                    app.activate(w, l, client);
                } else {
                    app.sel = Some(h);
                    if !in_panel {
                        return;
                    }
                    app.glide_to_sel(w);
                }
            }
        }
        _ => {}
    }
}

/// Pick visible objects and labels at the current map scale; ships lead in close-up.
fn pick_at(app: &App, w: &World, l: &Lay, (px, py): (f32, f32)) -> Option<Sel> {
    let z = app.cam.z;
    let dist = |p: (f32, f32)| {
        let (sx, sy) = l.to_screen(&app.cam, p);
        ((sx - px).powi(2) + (sy - py).powi(2)).sqrt()
    };
    let nearest = |c: Vec<(f32, f32, Sel)>| c.into_iter().filter(|(d, r, _)| d < r).min_by(|a, b| a.0.total_cmp(&b.0)).map(|c| c.2);
    if z < OVERVIEW_ZOOM && !app.sector_map() {
        // Only the visible system and station markers are targets at this scale.
        let mut nodes: Vec<_> = w.systems.iter().map(|s| (dist(s.wpos), 8.0, Sel::Sys(s.key.clone()))).collect();
        if let Some(p) = w.station { nodes.push((dist(p), 8.0, Sel::GitHub)); }
        return nearest(nodes).or_else(|| draw::overview_labels(app, w, l).into_iter().find_map(|label| {
            let width = label.lines.iter().map(|(s, _)| s.chars().count()).max().unwrap_or(0) as f32;
            (px >= label.col as f32 * 2.0 && px < (label.col as f32 + width) * 2.0
                && py >= label.row as f32 * 4.0 && py < (label.row as f32 + label.lines.len() as f32) * 4.0).then_some(label.target)
        }));
    }
    if app.sector_map() {
        let mut nodes: Vec<_> = w.planets.iter().filter(|p| p.host.is_none()).map(|p| (dist(p.wpos), 5.0, Sel::Planet(p.key.clone()))).collect();
        nodes.extend(w.systems.iter().map(|s| (dist(s.wpos), 5.0, Sel::Sys(s.key.clone()))));
        if let Some(p) = w.station { nodes.push((dist(p), 8.0, Sel::GitHub)); }
        return nearest(nodes).or_else(|| draw::sector_labels(app, w, l).into_iter().find_map(|label| {
            let width = label.lines.iter().map(|(s, _)| s.chars().count()).max().unwrap_or(0) as f32;
            (px >= label.col as f32 * 2.0 && px < (label.col as f32 + width) * 2.0
                && py >= label.row as f32 * 4.0 && py < (label.row as f32 + label.lines.len() as f32) * 4.0).then_some(label.target)
        })).or_else(|| draw::lane_pick(app, w, l, (px, py))).or_else(|| {
            let p = ((px - l.vw * 0.5) / z + app.cam.x, (py - l.vy0 - l.vh * 0.5) / z + app.cam.y);
            app.sector_regions(w).into_iter().find(|r| r.contains(p)).map(|r| Sel::Sector(r.id, r.system))
        });
    }
    let ships = app.agents.iter().filter(|a| a.dying.is_none()).map(|a| (dist(a.wpos), 6.0 * z.clamp(0.7, 2.2), Sel::Agent(a.id))).collect();
    let planets = w.planets.iter().map(|p| (dist(p.wpos), (p.r * z).max(4.0) + 3.0, Sel::Planet(p.key.clone()))).collect();
    let stars = w.systems.iter().map(|s| (dist(s.wpos), (9.0 * z).max(4.0) + 4.0, Sel::Sys(s.key.clone()))).collect();
    nearest(ships).or_else(|| nearest(planets)).or_else(|| nearest(stars)).or_else(|| {
        if !app.dependencies { return None; }
        draw::sector_labels(app, w, l).into_iter().find_map(|label| {
            let width = label.lines.iter().map(|(s, _)| s.chars().count()).max().unwrap_or(0) as f32;
            (px >= label.col as f32 * 2.0 && px < (label.col as f32 + width) * 2.0
                && py >= label.row as f32 * 4.0 && py < (label.row as f32 + label.lines.len() as f32) * 4.0).then_some(label.target)
        })
    }).or_else(|| draw::lane_pick(app, w, l, (px, py))).or_else(|| {
        let station = w.station?;
        let (c, r, label) = draw::station_label(app, w, l)?;
        let label_hit = !app.dependencies && px >= c as f32 * 2.0 && px < (c + label.chars().count() as i32) as f32 * 2.0
            && py >= r as f32 * 4.0 && py < (r + 1) as f32 * 4.0;
        (dist(station) < (13.0 * z).max(5.0) * 2.2 + 3.0 || label_hit).then_some(Sel::GitHub)
    })
}

#[cfg(test)]
mod screenshot_tests {
    use super::*;

    #[test]
    fn mission_paste_keeps_text_and_imports_dropped_images() {
        let dir = std::env::temp_dir().join(format!("empire-mission-paste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("screen shot.png");
        std::fs::write(&source, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let mut text = Line::default();
        text.set("Fix æ layout".into());
        text.cur = 5;
        let mut images = Vec::new();
        paste_brief(&mut text, &mut images, "new\r\n", &dir.join("stored")).unwrap();
        assert_eq!(text.s, "Fix ænew\n layout");
        paste_brief(&mut text, &mut images, &format!("'{}'", source.display()), &dir.join("stored")).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(text.s, "Fix ænew\n layout");
        assert_ne!(images[0].path, source.to_str().unwrap());
        let original = images.clone();
        assert!(paste_brief(&mut text, &mut images, "/missing/shot.png", &dir).is_err());
        assert_eq!(images, original);
        screenshots::discard(&images);
        assert!(!Path::new(&images[0].path).exists());
        assert!(source.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mission_composer_keeps_controls_visible_with_eight_images() {
        let mut app = App::new();
        let attachments = (0..8).map(|i| Attachment { name: format!("screen-{i}.png"), path: format!("/saved/{i}.png") }).collect();
        let mut text = Line::default();
        text.set("Fix the spacing".into());
        app.mode = Mode::Brief { planet: "/world".into(), text, kind: 0, launch: false, role: 0, to: None, attachments, sending: false, ship: Outfit::default() };
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        let mut fb = Fb::new(80, 24);
        let layout = Lay::new(&fb, false);
        crate::draw::frame(&mut fb, &mut app, &world, &layout, 1.0, None);
        let rows: Vec<String> = fb.txt.chunks(fb.cols).map(|row| row.iter().map(|c| c.map_or(' ', |c| c.ch)).collect()).collect();
        assert!(rows.iter().any(|r| r.contains("8 total")));
        assert!(rows.iter().any(|r| r.contains("screen-7.png")));
        assert!(rows.iter().any(|r| r.contains("queue it")));
        assert!(rows.iter().any(|r| r.contains("Ctrl+V") && r.contains("Ctrl+X")));
        assert!(rows.iter().any(|r| r.contains("give orders")));
    }
}

#[cfg(test)]
mod chat_input_tests {
    use super::*;

    fn setup() -> (App, World, Lay, Client, std::sync::mpsc::Receiver<Cmd>) {
        let mut app = App::new();
        app.agents.push(ship_card_tests::ship(1, false));
        let w = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        let l = Lay::new(&Fb::new(120, 40), false);
        let (client, commands) = Client::recording();
        app.open_comms(1, &client, &l);
        while commands.try_recv().is_ok() {}
        app.screen = Some((1, ScreenView { cols: 98, rows: 29, cursor: (0, 0), hide_cursor: true,
            lines: vec![vec![crate::proto::Run { t: "hello world".into(), fg: None, bg: None, b: false }]],
            scroll: 0, history: 100, mouse: false, sgr: false, alt: false }));
        (app, w, l, client, commands)
    }

    fn event(app: &mut App, w: &World, l: &Lay, client: &Client, ev: Event) {
        handle(app, w, l, client, None, ev, 1.0, &mut None);
    }

    fn mouse(kind: MouseEventKind, c: i32, r: i32) -> Event {
        Event::Mouse(event::MouseEvent { kind, column: c as u16, row: r as u16, modifiers: KeyModifiers::NONE })
    }

    #[test]
    fn popup_drag_selects_terminal_cells_at_the_popup_offset() {
        let (mut app, w, l, client, _) = setup();
        let (x, y, _, _) = draw::chat_box(&l);
        event(&mut app, &w, &l, &client, mouse(MouseEventKind::Down(MouseButton::Left), x + 1, y + 1));
        event(&mut app, &w, &l, &client, mouse(MouseEventKind::Drag(MouseButton::Left), x + 5, y + 1));
        assert_eq!(app.pick.map(|p| p.span()), Some(((0, 0), (0, 4))));
        assert_eq!(app.selected_text(), "hello");
    }

    #[test]
    fn popup_paste_keeps_newlines_and_does_not_send_before_enter() {
        let (mut app, w, l, client, commands) = setup();
        event(&mut app, &w, &l, &client, Event::Paste("first\r\nsecond".into()));
        assert_eq!(chat_text(&mut app).unwrap().s, "first\nsecond");
        assert!(!commands.try_iter().any(|c| matches!(c, Cmd::Say(..) | Cmd::Input(..))));
    }

    #[test]
    fn popup_wheel_uses_agent_mouse_protocol_when_the_agent_owns_scrolling() {
        let (mut app, w, l, client, commands) = setup();
        let (_, s) = app.screen.as_mut().unwrap();
        s.mouse = true; s.sgr = true; s.alt = true; s.history = 0;
        let (x, y, _, _) = draw::chat_box(&l);
        event(&mut app, &w, &l, &client, mouse(MouseEventKind::ScrollUp, x + 4, y + 3));
        assert!(commands.try_iter().any(|c| matches!(c, Cmd::Input(1, data) if data == "\x1b[<64;4;3M")));
    }

    #[test]
    fn popup_wheel_and_page_keys_scroll_normal_terminal_history() {
        let (mut app, w, l, client, _) = setup();
        let (x, y, _, _) = draw::chat_box(&l);
        event(&mut app, &w, &l, &client, mouse(MouseEventKind::ScrollUp, x + 4, y + 3));
        assert_eq!(app.scroll, 3);
        event(&mut app, &w, &l, &client, Event::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)));
        assert!(app.scroll > 3);
        event(&mut app, &w, &l, &client, Event::Key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)));
        assert_eq!(app.scroll, 3);
    }

    #[test]
    fn dropped_image_is_validated_instead_of_becoming_message_text() {
        let (mut app, w, l, client, _) = setup();
        event(&mut app, &w, &l, &client, Event::Paste("/nonexistent/chat-image.png".into()));
        assert!(app.toast.is_some());
        assert!(chat_text(&mut app).unwrap().s.is_empty());
    }

    #[test]
    fn screenshot_reply_keeps_the_draft_on_failure_and_the_image_after_delivery() {
        let (mut app, w, l, client, commands) = setup();
        let dir = std::env::temp_dir().join(format!("empire-chat-delivery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("test shot.png");
        std::fs::write(&source, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        chat_paste(&mut app, "Look at this\nPlease fix it", &dir.join("copies")).unwrap();
        chat_paste(&mut app, source.to_str().unwrap(), &dir.join("copies")).unwrap();
        let saved = match &app.mode { Mode::Chat { attachments, .. } => attachments[0].path.clone(), _ => unreachable!() };
        assert_ne!(saved, source.to_str().unwrap());
        event(&mut app, &w, &l, &client, Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(commands.try_iter().any(|c| matches!(c, Cmd::Say(1, text) if text.contains("Look at this\nPlease fix it") && text.contains(&saved))));
        app.chat_sent(1, Err("connection lost".into()), &client, &l, 1.0);
        assert!(matches!(&app.mode, Mode::Chat { text, attachments, sending: false, .. } if text.s.contains("Please fix it") && attachments.len() == 1));
        event(&mut app, &w, &l, &client, Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        app.chat_sent(1, Ok(()), &client, &l, 1.0);
        assert!(matches!(&app.mode, Mode::Chat { text, attachments, sending: false, .. } if text.s.is_empty() && attachments.is_empty()));
        assert!(Path::new(&saved).is_file());
        app.close_comms(&client);
        assert!(Path::new(&saved).is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn closing_a_draft_cleans_up_only_its_owned_images() {
        let (mut app, _, _, client, _) = setup();
        let dir = std::env::temp_dir().join(format!("empire-chat-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("shot.png");
        std::fs::write(&source, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        chat_paste(&mut app, source.to_str().unwrap(), &dir.join("copies")).unwrap();
        let saved = match &app.mode { Mode::Chat { attachments, .. } => attachments[0].path.clone(), _ => unreachable!() };
        app.close_comms(&client);
        assert!(!Path::new(&saved).exists());
        assert!(source.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn popup_renders_multiline_drafts_attachments_and_errors_inside_the_frame() {
        let (mut app, w, _, client, _) = setup();
        let mut fb = Fb::new(80, 24);
        let l = Lay::new(&fb, false);
        if let Mode::Chat { text, attachments, .. } = &mut app.mode {
            text.set("first line\nsecond line\nlast line".into());
            attachments.push(Attachment { path: "/not-sent.png".into(), name: "sample.png".into() });
        }
        app.toast("Clipboard feedback is visible", 1.0);
        draw::frame(&mut fb, &mut app, &w, &l, 1.0, None);
        let rows: Vec<String> = fb.txt.chunks(fb.cols).map(|r| r.iter().map(|c| c.map_or(' ', |c| c.ch)).collect()).collect();
        for text in ["first line", "second line", "last line", "sample.png", "Clipboard feedback is visible"] {
            assert!(rows.iter().any(|r| r.contains(text)), "missing {text}");
        }
        event(&mut app, &w, &l, &client, Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        assert!(matches!(app.mode, Mode::Chat { .. }), "Tab must not silently discard a draft");
    }

    #[test]
    fn scrollbar_and_history_keys_can_reach_top_and_bottom() {
        let (mut app, w, l, client, _) = setup();
        let (x, y, width, height) = app.terminal_rect(&l);
        event(&mut app, &w, &l, &client, mouse(MouseEventKind::Down(MouseButton::Left), x + width, y));
        assert_eq!(app.scroll, 100);
        event(&mut app, &w, &l, &client, mouse(MouseEventKind::Down(MouseButton::Left), x + width, y + height - 1));
        assert_eq!(app.scroll, 0);
        event(&mut app, &w, &l, &client, Event::Key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL)));
        assert_eq!(app.scroll, 100);
        event(&mut app, &w, &l, &client, Event::Key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL)));
        assert_eq!(app.scroll, 100);
        event(&mut app, &w, &l, &client, Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL)));
        assert_eq!(app.scroll, 0);
    }
}

#[cfg(test)]
mod overview_tests {
    use super::*;


    #[test]
    fn middle_zoom_reveals_sectors_and_does_not_pick_hidden_ships() {
        let (mut app, w, l) = fixture();
        let id = app.sectors.create("Orion").unwrap();
        app.sectors.toggle(id, "/empire/api").unwrap();
        app.sectors.toggle(id, "/empire/web").unwrap();
        app.cam.z = 0.7;
        (app.cam.x, app.cam.y) = w.systems[0].wpos;
        let mut fb = Fb::new(100, 36);
        draw::frame(&mut fb, &mut app, &w, &l, 4.0, None);
        let text: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
        assert!(text.contains("ORION"), "sector names must appear without opening or focusing a sector");
        app.agents[0].wpos = w.planets[0].wpos;
        let point = l.to_screen(&app.cam, app.agents[0].wpos);
        assert_eq!(pick_at(&app, &w, &l, point), Some(Sel::Planet(w.planets[0].key.clone())));
    }

    #[test]
    fn sector_labels_select_local_region_and_enter_reveals_its_worlds() {
        let (mut app, w, l) = fixture();
        let id = app.sectors.create("Orion").unwrap();
        for p in ["/empire/api", "/empire/web"] { app.sectors.toggle(id, p).unwrap(); }
        app.cam.z = 0.7;
        (app.cam.x, app.cam.y) = w.systems[0].wpos;
        let target = Sel::Sector(id, "/empire".into());
        let labels = draw::sector_labels(&app, &w, &l);
        let label = labels.iter().find(|p| p.target == target).unwrap();
        assert_eq!(pick_at(&app, &w, &l, (label.col as f32 * 2.0 + 1.0, label.row as f32 * 4.0 + 2.0)), Some(target.clone()));
        assert!(app.rows(&w).contains(&target));
        app.agents[2].ask = Some(Ask { question: "Allow this?".into(), options: vec![crate::proto::Choice { key: "y".into(), label: "Yes".into() }], ..Default::default() });
        app.sel = Some(target);
        let center = app.sector_regions(&w)[0].center();
        handle(&mut app, &w, &l, &Client::isolated(), None, Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), 4.0, &mut None);
        assert!(matches!(app.mode, Mode::Galaxy));
        assert_eq!((app.cam.tx, app.cam.ty), center);
        assert!(app.cam.tz > SECTOR_ZOOM);
        assert_eq!(app.sector_focus, Some(id));
        app.sector_focus = Some(app.sectors.create("Other").unwrap());
        app.open_sectors();
        assert_eq!(app.sector_view.id, id);
    }

    #[test]
    fn sector_regions_are_local_and_moons_share_the_host_marker() {
        let (mut app, _, _) = fixture();
        let mut other = ship_card_tests::ship(4, false);
        other.cwd = "/elsewhere/runtime".into();
        app.agents.push(other);
        let id = app.sectors.create("Orion").unwrap();
        for p in ["/empire/api", "/elsewhere/runtime"] { app.sectors.toggle(id, p).unwrap(); }
        let w = app.world(4.0, 0.0);
        let regions = app.sector_regions(&w);
        assert_eq!(regions.len(), 2);
        assert_ne!(regions[0].system, regions[1].system);
        for r in &regions {
            assert_eq!(r.id, id);
            assert_eq!(r.worlds.len(), 1);
            let p = &w.planets[r.worlds[0]];
            assert!(p.host.is_none());
            assert!(r.contains(p.wpos));
            assert!(!r.contains((r.bounds.2 + 1.0, r.bounds.3 + 1.0)));
        }
        let shared = app.sectors.create("Shared").unwrap();
        app.sectors.toggle(shared, "/empire/api").unwrap();
        assert_eq!(app.sector_regions(&w).iter().filter(|r| r.system == "/empire").count(), 2);
    }

    #[test]
    fn system_enter_visits_sector_layer_before_planetary_detail() {
        let (mut app, w, l) = fixture();
        let id = app.sectors.create("Orion").unwrap();
        app.sectors.toggle(id, "/empire/api").unwrap();
        app.sel = Some(Sel::Sys("/empire".into()));
        app.activate(&w, &l, &Client::isolated());
        app.cam.z = app.cam.tz;
        assert!(app.sector_map());
        app.activate(&w, &l, &Client::isolated());
        app.cam.z = app.cam.tz;
        assert!(!app.sector_map());
        assert!(app.cam.z >= SECTOR_ZOOM);
    }

    fn fixture() -> (App, World, Lay) {
        let mut app = App::new();
        app.history.clear();
        app.online = true;
        for (id, cwd, status) in [(1, "/empire/api", Status::Working), (2, "/empire/web", Status::Done), (3, "/moons/patch", Status::Blocked)] {
            let mut a = ship_card_tests::ship(id, false);
            a.cwd = cwd.into();
            a.status = status;
            app.agents.push(a);
        }
        app.survey.worlds.insert("/moons/patch".into(), survey::World { host: Some("/empire/api".into()), ..Default::default() });
        let w = app.world(4.0, 0.0);
        app.cam.z = 0.2;
        let l = Lay::new(&Fb::new(100, 36), false);
        (app, w, l)
    }

    #[test]
    fn distant_system_can_be_picked_without_hitting_its_hidden_ships() {
        let (mut app, w, l) = fixture();
        app.agents[0].wpos = w.systems[0].wpos;
        let point = l.to_screen(&app.cam, w.systems[0].wpos);
        assert_eq!(pick_at(&app, &w, &l, point), Some(Sel::Sys("/empire".into())));
        app.cam.z = 1.0;
        let point = l.to_screen(&app.cam, app.agents[0].wpos);
        assert_eq!(pick_at(&app, &w, &l, point), Some(Sel::Agent(1)));
    }

    #[test]
    fn enter_on_a_distant_system_zooms_even_when_an_agent_is_asking() {
        let (mut app, w, l) = fixture();
        app.agents[2].ask = Some(Ask { question: "Allow this?".into(), options: vec![crate::proto::Choice { key: "y".into(), label: "Yes".into() }], ..Default::default() });
        app.sel = Some(Sel::Sys("/empire".into()));
        handle(&mut app, &w, &l, &Client::isolated(), None, Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), 4.0, &mut None);
        assert!(matches!(app.mode, Mode::Galaxy));
        assert!(app.cam.tz >= 0.5);
    }

    #[test]
    fn overview_shows_world_and_ship_totals_including_attention_on_moons() {
        let (mut app, w, l) = fixture();
        let mut fb = Fb::new(100, 36);
        draw::frame(&mut fb, &mut app, &w, &l, 4.0, None);
        let text = fb.txt.chunks(fb.cols).map(|r| r.iter().map(|c| c.map_or(' ', |c| c.ch)).collect::<String>()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("2 worlds · 3 ships"));
        assert!(text.contains("1 waiting"));
        assert!(text.contains("1 finished"));
    }

    #[test]
    fn entering_a_large_system_always_reveals_its_worlds() {
        let (mut app, mut w, l) = fixture();
        w.systems[0].orbits.push(600.0);
        app.sel = Some(Sel::Sys(w.systems[0].key.clone()));
        app.activate(&w, &l, &Client::isolated());
        assert!(app.cam.tz >= 0.5, "enter must leave the distant overview");
    }

    #[test]
    fn distant_labels_are_clickable_and_do_not_cover_each_other() {
        let (mut app, mut w, l) = fixture();
        for i in 1..8 {
            w.systems.push(System { key: format!("/system{i}"), name: format!("system {i}"), wpos: (i as f32 * 60.0 - 240.0, (i % 3) as f32 * 80.0 - 80.0), planets: vec![], orbits: vec![], star: s3(0.8) });
        }
        w.station = Some((260.0, 80.0));
        app.sel = Some(Sel::Agent(3)); // the blocked agent is on a moon
        let labels = draw::overview_labels(&app, &w, &l);
        assert!(!labels.is_empty());
        assert_eq!(labels[0].target, Sel::Sys("/empire".into()));
        for (i, label) in labels.iter().enumerate() {
            let width = label.lines.iter().map(|(s, _)| s.chars().count() as i32).max().unwrap();
            let point = (label.col as f32 * 2.0 + 1.0, label.row as f32 * 4.0 + 2.0);
            assert_eq!(pick_at(&app, &w, &l, point), Some(label.target.clone()));
            assert!(label.col >= 0 && label.col + width <= l.cols - l.pw);
            for other in labels.iter().skip(i + 1) {
                let ow = other.lines.iter().map(|(s, _)| s.chars().count() as i32).max().unwrap();
                assert!(label.col + width <= other.col || other.col + ow <= label.col
                    || label.row + label.lines.len() as i32 <= other.row || other.row + other.lines.len() as i32 <= label.row);
            }
        }
    }

    #[test]
    fn distant_graphics_do_not_accumulate_ship_effects_but_close_graphics_keep_them() {
        let (mut app, w, l) = fixture();
        for zoom in [0.12, 0.3, 1.0] {
            app.cam.z = zoom;
            app.fx.clear();
            let mut before = Fb::new(100, 36);
            draw::frame(&mut before, &mut app, &w, &l, 4.0, None);
            app.fx.push(Fx::Ring { x: 0.0, y: 0.0, t0: 3.5, col: s3(1.0), speed: 40.0, dur: 2.0 });
            let mut after = Fb::new(100, 36);
            draw::frame(&mut after, &mut app, &w, &l, 4.0, None);
            if zoom < 0.45 { assert_eq!(before.px, after.px); }
            else { assert_ne!(before.px, after.px); }
        }
    }
}

#[cfg(test)]
mod station_tests {
    use super::*;

    fn fixture() -> (App, World) {
        let mut app = App::new();
        let mut world = World { systems: vec![], planets: vec![], lanes: vec![], station: Some((0.0, 0.0)) };
        for (key, github, ci, host) in [("/alpha", true, "pass", None), ("/beta", true, "fail", None), ("/local", false, "none", None), ("/moon", true, "fail", Some(0))] {
            world.planets.push(Planet { key: key.into(), name: base_name(key), agents: vec![], terrain: 0, seed: 0.5, wpos: (80.0, 70.0), r: 10.0, host, moons: vec![], level: 0, dormant: false, fog: 0.0, last_seen: 0 });
            app.survey.worlds.insert(key.into(), survey::World { github, prs: vec![survey::Pr { number: 7, title: format!("Fix {key}"), ci, ..Default::default() }], prs_checked: Some(survey::now()), ..Default::default() });
        }
        app.survey.worlds.insert("/forgotten".into(), survey::World { github: true, ..Default::default() });
        (app, world)
    }

    #[test]
    fn station_shows_only_galaxy_projects_and_prioritizes_failed_prs() {
        let (app, world) = fixture();
        let rows = app.trade_rows(&world);
        let projects: Vec<_> = rows.iter().filter(|r| r.pr.is_none()).map(|r| r.path.as_str()).collect();
        assert_eq!(projects, ["/beta", "/alpha"]);
        assert_eq!(rows[1].pr.as_ref().unwrap().ci, "fail");
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn views_split_your_prs_from_reviews_asked_of_you() {
        let (mut app, world) = fixture();
        let pr = |number, asks_you| survey::Pr { number, title: format!("Theirs {number}"), ci: "pass", author: "ana".into(), asks_you, draft: false };
        app.survey.worlds.get_mut("/alpha").unwrap().others = vec![pr(20, false), pr(21, true)];
        let numbers = |app: &App| -> Vec<u32> { app.trade_rows(&world).iter().filter(|r| r.path == "/alpha").filter_map(|r| r.pr.as_ref().map(|p| p.number)).collect() };
        // all: yours, then the one waiting on your review, then the rest
        assert_eq!(numbers(&app), [7, 21, 20]);
        app.trade_view = TradeView::Review;
        assert_eq!(numbers(&app), [21]);
        app.trade_view = TradeView::Mine;
        assert_eq!(numbers(&app), [7]);
        // a project with a review waiting on you outranks a quiet one
        app.trade_view = TradeView::All;
        let projects: Vec<_> = app.trade_rows(&world).iter().filter(|r| r.pr.is_none()).map(|r| r.path.clone()).collect();
        assert_eq!(projects, ["/beta", "/alpha"]);
    }

    #[test]
    fn t_on_a_pr_opens_a_fitting_mission_for_its_world() {
        let (mut app, world) = fixture();
        app.survey.worlds.get_mut("/alpha").unwrap().others = vec![survey::Pr { number: 21, title: "Theirs".into(), ci: "pass", author: "ana".into(), asks_you: true, draft: false }];
        let rows = app.trade_rows(&world);
        let brief = |app: &mut App, number: u32| {
            let i = rows.iter().position(|r| r.pr.as_ref().is_some_and(|p| p.number == number) && r.path == "/alpha").unwrap();
            app.trade_mission(&world, i);
            match &app.mode {
                Mode::Brief { planet, text, launch, .. } => (planet.clone(), text.s.clone(), *launch),
                _ => panic!("no composer"),
            }
        };
        let (planet, text, launch) = brief(&mut app, 21);
        assert_eq!(planet, "/alpha");
        assert!(text.starts_with("Review PR #21") && text.contains("@ana") && text.contains("Don't post"));
        assert!(launch, "no one is on /alpha, so a ship is sent");
        let (_, text, _) = brief(&mut app, 7);
        assert!(text.starts_with("Address the review feedback on my PR #7"));
        let failing = survey::Pr { number: 9, title: "Broken".into(), ci: "fail", ..Default::default() };
        assert!(pr_brief(&failing, true).starts_with("Fix the failing checks on my PR #9"));
        // a project row opens a blank composer
        let p = rows.iter().position(|r| r.pr.is_none() && r.path == "/alpha").unwrap();
        app.trade_mission(&world, p);
        assert!(matches!(&app.mode, Mode::Brief { text, .. } if text.s.is_empty()));
    }

    #[test]
    fn station_label_click_and_keyboard_activation_open_the_overview() {
        let (mut app, world) = fixture();
        let fb = Fb::new(120, 40);
        let l = Lay::new(&fb, false);
        let client = Client::isolated();
        let (col, row, _) = draw::station_label(&app, &world, &l).unwrap();
        assert_eq!(pick_at(&app, &world, &l, (col as f32 * 2.0 + 1.0, row as f32 * 4.0 + 2.0)), Some(Sel::GitHub));
        mouse(&mut app, &world, &l, &client, event::MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: col as u16, row: row as u16, modifiers: KeyModifiers::NONE }, &mut None, 1.0);
        assert!(matches!(app.mode, Mode::Trade { sel: 0 }));
        app.mode = Mode::Galaxy;
        assert!(app.rows(&world).contains(&Sel::GitHub));
        app.sel = Some(Sel::GitHub);
        app.activate(&world, &l, &client);
        assert!(matches!(app.mode, Mode::Trade { sel: 0 }));
        app.open_trade_row(&world, 0, &client, 0.0);
        assert_eq!(app.sel, Some(Sel::Planet("/beta".into())));
        assert!(matches!(app.mode, Mode::Galaxy));
    }

    #[test]
    fn overview_displays_ci_and_distinguishes_failed_fetch_from_empty_prs() {
        let (mut app, world) = fixture();
        app.survey.worlds.get_mut("/beta").unwrap().prs_error = Some("GitHub refresh failed".into());
        app.mode = Mode::Trade { sel: 0 };
        let mut fb = Fb::new(100, 30);
        let l = Lay::new(&fb, false);
        draw::frame(&mut fb, &mut app, &world, &l, 1.0, None);
        let rendered: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
        assert!(rendered.contains("2 projects · 2 PRs"));
        assert!(rendered.contains("PRs unavailable"));
        assert!(rendered.contains("GitHub refresh failed"));
        assert!(rendered.contains("failing"));
        assert!(rendered.contains("passing"));
    }

    #[test]
    fn station_overview_scrolls_to_last_row_and_hits_the_visible_row() {
        let (mut app, world) = fixture();
        app.survey.worlds.get_mut("/beta").unwrap().prs = (1..=40).map(|number| survey::Pr { number, title: format!("Change {number}"), ci: "fail", ..Default::default() }).collect();
        app.mode = Mode::Trade { sel: 0 };
        let mut fb = Fb::new(80, 24);
        let l = Lay::new(&fb, false);
        let client = Client::isolated();
        handle(&mut app, &world, &l, &client, None, Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)), 1.0, &mut None);
        let last = app.trade_rows(&world).len() - 1;
        assert!(matches!(app.mode, Mode::Trade { sel } if sel == last));
        draw::frame(&mut fb, &mut app, &world, &l, 1.0, None);
        let row = fb.txt.chunks(fb.cols).position(|r| r.iter().map(|c| c.map_or(' ', |c| c.ch)).collect::<String>().contains("Fix /alpha")).unwrap();
        assert_eq!(draw::trade_hit(&l, last + 1, last, 6, row as i32), Some(last));
        handle(&mut app, &world, &l, &client, None, Event::Key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)), 1.0, &mut None);
        assert!(matches!(app.mode, Mode::Trade { sel: 0 }));
        handle(&mut app, &world, &l, &client, None, Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), 1.0, &mut None);
        assert!(matches!(app.mode, Mode::Galaxy));
    }

    #[test]
    fn github_station_body_is_a_pick_target_at_every_zoom() {
        let mut app = App::new();
        let fb = Fb::new(120, 40);
        let l = Lay::new(&fb, false);
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: Some((25.0, -10.0)) };
        for zoom in [0.12, 1.0, 6.0] {
            app.cam.z = zoom;
            let point = l.to_screen(&app.cam, world.station.unwrap());
            assert_eq!(pick_at(&app, &world, &l, point), Some(Sel::GitHub), "station is not clickable at zoom {zoom}");
        }
    }
}

#[cfg(test)]
mod ship_card_tests {
    use super::*;

    pub fn ship(id: u32, war: bool) -> Agent {
        Agent {
            id,
            kind: "claude".into(),
            cwd: "/home/me/world".into(),
            title: "fixing the outliner".into(),
            status: Status::Working,
            since: 0.0,
            born: 0.0,
            dying: None,
            seed: 0.5,
            last_ping: 0.0,
            busy_at: 0.0,
            turns: 3,
            report: Default::default(),
            ask: None,
            war: if war { vec![crate::proto::War { rival: 9, files: vec!["src/a.rs".into()] }] } else { vec![] },
            role: Role::None,
            design: String::new(),
            spec: ShipSpec::default(),
            refit: None,
            wpos: (0.0, 0.0),
            front: true,
        }
    }

    fn screen(app: &mut App) -> (Lay, Vec<String>) {
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        let mut fb = Fb::new(100, 30);
        let l = Lay::new(&fb, false);
        crate::draw::frame(&mut fb, app, &world, &l, 1.0, None);
        (l, fb.txt.chunks(fb.cols).map(|row| row.iter().map(|c| c.map_or(' ', |c| c.ch)).collect()).collect())
    }

    #[test]
    fn priority_is_offered_only_in_a_border_conflict() {
        let mut app = App::new();
        app.agents = vec![ship(1, false), ship(2, true)];
        assert_eq!(app.ship_acts(1), vec![ShipAct::Comms, ShipAct::Mission, ShipAct::Refit, ShipAct::Decommission]);
        assert_eq!(app.ship_acts(2), vec![ShipAct::Comms, ShipAct::Mission, ShipAct::Priority, ShipAct::Refit, ShipAct::Decommission]);
        assert!(app.ship_acts(7).is_empty());
    }

    #[test]
    fn card_shows_the_ship_and_clicks_land_on_its_menu() {
        let mut app = App::new();
        app.agents = vec![ship(1, false)];
        app.mode = Mode::Ship { id: 1, sel: 0 };
        let (l, rows) = screen(&mut app);
        assert!(rows.iter().any(|r| r.contains("CLAUDE · WORLD")));
        assert!(rows.iter().any(|r| r.contains("fixing the outliner")));
        let comms = rows.iter().position(|r| r.contains("open comms")).unwrap() as i32;
        let col = rows[comms as usize].chars().position(|c| c == 'o').unwrap() as i32;
        assert_eq!(crate::draw::ship_hit(&app, 1, &l, col, comms), Some(Some(0)));
        assert_eq!(crate::draw::ship_hit(&app, 1, &l, col, comms + 2), Some(Some(2)));
        assert_eq!(crate::draw::ship_hit(&app, 1, &l, col, comms - 2), Some(None));
        assert_eq!(crate::draw::ship_hit(&app, 1, &l, 0, 0), None);
    }
}

#[cfg(test)]
mod commander_tests {
    use super::*;

    fn fixture() -> (App, World, Lay) {
        let mut app = App::new();
        app.history.clear();
        app.online = true;
        for (id, cwd, status) in [(1, "/alpha/api", Status::Blocked), (2, "/beta/web", Status::Done), (3, "/beta/worker", Status::Working)] {
            let mut a = ship_card_tests::ship(id, false);
            a.cwd = cwd.into();
            a.status = status;
            a.title = format!("task for ship {id}");
            app.agents.push(a);
        }
        app.agents[0].ask = Some(Ask { question: "May I run the migration?".into(), context: vec!["Database changes need approval".into()], options: vec![] });
        app.sel = Some(Sel::Sys("/beta".into()));
        let world = app.world(0.0, 0.0);
        let l = Lay::new(&Fb::new(120, 40), false);
        (app, world, l)
    }

    fn key(app: &mut App, w: &World, l: &Lay, code: KeyCode) {
        handle(app, w, l, &Client::isolated(), None, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), 1.0, &mut None);
    }

    fn rendered(app: &mut App, w: &World) -> String {
        let mut fb = Fb::new(120, 40);
        let l = Lay::new(&fb, false);
        draw::frame(&mut fb, app, w, &l, 1.0, None);
        fb.txt.chunks(fb.cols).map(|row| row.iter().map(|c| c.map_or(' ', |c| c.ch)).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn commander_opens_across_systems_and_returns_after_comms() {
        let (mut app, w, l) = fixture();
        key(&mut app, &w, &l, KeyCode::Char('C'));
        let screen = rendered(&mut app, &w);
        assert!(screen.contains("COMMANDER"));
        assert!(screen.contains("May I run the migration?"));
        assert!(screen.contains("task for ship 2"));
        key(&mut app, &w, &l, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Chat { id: 1, .. }));
        key(&mut app, &w, &l, KeyCode::Esc);
        assert!(rendered(&mut app, &w).contains("COMMANDER"));
    }

    #[test]
    fn commander_empty_state_reflects_resolved_work() {
        let (mut app, w, l) = fixture();
        key(&mut app, &w, &l, KeyCode::Char('C'));
        app.agents[0].status = Status::Working;
        app.agents[1].status = Status::Idle;
        assert!(rendered(&mut app, &w).contains("Nothing needs your attention"));
    }

    #[test]
    fn attention_prioritizes_requests_without_duplicate_moon_prs_or_routine_work() {
        let (mut app, _, _) = fixture();
        app.agents[2].war = vec![War { rival: 8, files: vec!["src/main.rs".into()] }];
        app.survey.worlds.insert("/alpha/api".into(), survey::World {
            github: true, dirty: 7, ahead: 2,
            prs: vec![survey::Pr { number: 7, title: "Broken checks".into(), ci: "fail", ..Default::default() }],
            others: vec![survey::Pr { number: 8, title: "Please review".into(), asks_you: true, ..Default::default() }],
            ..Default::default()
        });
        let mut moon = ship_card_tests::ship(4, false);
        moon.cwd = "/moons/api".into();
        app.agents.push(moon);
        let mut survey = app.survey.worlds["/alpha/api"].clone();
        survey.host = Some("/alpha/api".into());
        app.survey.worlds.insert("/moons/api".into(), survey);
        app.survey.worlds.insert("/unmapped".into(), app.survey.worlds["/alpha/api"].clone());
        let w = app.world(0.0, 0.0);
        let rows = app.attention(&w);
        assert_eq!(rows.iter().map(|r| r.kind).collect::<Vec<_>>(), ["Input", "Conflict", "CI failed", "Review", "Finished"]);
        assert!(rows.iter().filter(|r| matches!(r.target, AttentionTarget::Pr(_))).all(|r| r.world == "/alpha/api"));
        app.agents[0].dying = Some(0.0);
        assert!(!app.attention(&w).iter().any(|r| r.id == "agent:1"));
    }

    #[test]
    fn selection_stays_on_the_same_request_when_alerts_arrive() {
        let (mut app, w, l) = fixture();
        key(&mut app, &w, &l, KeyCode::Char('C'));
        key(&mut app, &w, &l, KeyCode::Down);
        let mut newcomer = ship_card_tests::ship(9, false);
        newcomer.cwd = "/aaa/new".into();
        newcomer.status = Status::Blocked;
        app.agents.push(newcomer);
        assert_eq!(app.attention(&w)[app.attention_selection(&app.attention(&w))].id, "agent:2");
        key(&mut app, &w, &l, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Chat { id: 2, .. }));
    }

    #[test]
    fn commander_lists_unstaffed_missions_and_prepares_pr_work_without_sending_it() {
        let (mut app, w, l) = fixture();
        app.agents[2].status = Status::Exited;
        app.missions.push(Mission { cwd: "/beta/worker".into(), text: "Build the service".into(), state: "queued".into(), ..Default::default() });
        assert!(app.attention(&w).iter().any(|r| r.kind == "Unstaffed"));
        app.survey.worlds.insert("/alpha/api".into(), survey::World {
            others: vec![survey::Pr { number: 42, asks_you: true, title: "Review authentication".into(), ..Default::default() }],
            ..Default::default()
        });
        key(&mut app, &w, &l, KeyCode::Char('C'));
        key(&mut app, &w, &l, KeyCode::Down);
        key(&mut app, &w, &l, KeyCode::Char('t'));
        assert!(matches!(&app.mode, Mode::Brief { planet, text, sending: false, .. } if planet == "/alpha/api" && text.s.contains("42") && text.s.contains("review")));
    }

    #[test]
    fn long_queue_scrolls_and_mouse_hit_matches_the_visible_request() {
        let (mut app, w, _) = fixture();
        for id in 10..40 {
            let mut a = ship_card_tests::ship(id, false);
            a.status = Status::Blocked;
            a.title = format!("Request number {id}");
            app.agents.push(a);
        }
        let mut fb = Fb::new(80, 24);
        let l = Lay::new(&fb, false);
        key(&mut app, &w, &l, KeyCode::Char('C'));
        key(&mut app, &w, &l, KeyCode::End);
        draw::frame(&mut fb, &mut app, &w, &l, 1.0, None);
        let items = app.attention(&w);
        let sel = items.len() - 1;
        let text: Vec<String> = fb.txt.chunks(fb.cols).map(|r| r.iter().map(|c| c.map_or(' ', |c| c.ch)).collect()).collect();
        let row = text.iter().position(|r| r.contains("task for ship 2")).unwrap();
        assert_eq!(draw::commander_hit(&l, items.len(), sel, 20, row as i32), Some(sel));
        assert!(text.iter().any(|r| r.contains("Esc galaxy")));
        assert_eq!(draw::commander_hit(&l, items.len(), sel, 20, l.rows - 4), None);
    }

    #[test]
    fn long_question_details_scroll_to_the_end_and_back() {
        let (mut app, w, l) = fixture();
        app.agents[0].ask.as_mut().unwrap().context = (0..40).map(|i| format!("context line {i}")).collect();
        key(&mut app, &w, &l, KeyCode::Char('C'));
        for _ in 0..20 { key(&mut app, &w, &l, KeyCode::PageDown); rendered(&mut app, &w); }
        assert!(rendered(&mut app, &w).contains("context line 39"));
        for _ in 0..20 { key(&mut app, &w, &l, KeyCode::PageUp); }
        assert!(rendered(&mut app, &w).contains("context line 0"));
    }
}

#[cfg(test)]
mod mission_log_tests {
    use super::*;

    #[test]
    fn a_worlds_log_shows_only_its_own_missions() {
        let mut app = App::new();
        let m = |id, cwd: &str| Mission { id, cwd: cwd.into(), text: format!("job {id}"), state: "queued".into(), ..Mission::default() };
        app.missions = vec![m(1, "/a"), m(2, "/b"), m(3, "/a")];
        let ids: Vec<u32> = app.mission_log("/a").iter().map(|(_, m)| m.id).collect();
        assert_eq!(ids, vec![1, 3]);
        assert!(app.mission_log("/c").is_empty());
    }
}

#[cfg(test)]
mod situation_tests {
    use super::*;

    fn key(app: &mut App, w: &World, l: &Lay, c: KeyCode) {
        let ev = Event::Key(KeyEvent::new(c, KeyModifiers::NONE));
        situation_input(app, w, l, &Client::isolated(), &ev, match app.mode { Mode::Situation { id, .. } => id, _ => 0 }, matches!(app.mode, Mode::Situation { typing: true, .. }), 0.0);
    }

    fn shown(app: &App) -> Vec<String> {
        app.situation().iter().map(|e| e.msg.clone()).collect()
    }

    #[test]
    fn the_situation_log_filters_by_kind_world_and_text() {
        let mut app = App::new();
        app.say(Ev::Alert, "claude awaits orders".into(), DIM_LOG).on("/a").by(1);
        app.say(Ev::Mission, "mission to b".into(), DIM_LOG).on("/b");
        app.say(Ev::Error, "uplink lost".into(), DIM_LOG);
        assert_eq!(shown(&app), ["uplink lost", "mission to b", "claude awaits orders"]);
        app.sit.kind = Some(Ev::Alert);
        assert_eq!(shown(&app), ["claude awaits orders"]);
        app.sit = SitFilter { world: Some("/b".into()), ..SitFilter::default() };
        assert_eq!(shown(&app), ["mission to b"]);
        app.sit = SitFilter { query: "UPLINK".into(), ..SitFilter::default() };
        assert_eq!(shown(&app), ["uplink lost"]);
    }

    #[test]
    fn keys_filter_search_and_keep_the_selected_entry() {
        let mut app = App::new();
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        let l = Lay::new(&Fb::new(120, 40), false);
        app.say(Ev::Alert, "first alert".into(), DIM_LOG).on("/a");
        app.say(Ev::Mission, "a mission".into(), DIM_LOG).on("/a");
        app.say(Ev::Alert, "second alert".into(), DIM_LOG).on("/b");
        app.mode = Mode::Situation { id: app.log[0].id, typing: false };
        key(&mut app, &world, &l, KeyCode::Down);
        key(&mut app, &world, &l, KeyCode::Down);
        // the selected alert survives filtering to alerts only
        key(&mut app, &world, &l, KeyCode::Char('1'));
        assert_eq!(app.sit.kind, Some(Ev::Alert));
        assert!(matches!(app.mode, Mode::Situation { id, .. } if app.log.iter().find(|e| e.id == id).unwrap().msg == "first alert"));
        // w narrows to its world
        key(&mut app, &world, &l, KeyCode::Char('w'));
        assert_eq!(shown(&app), ["first alert"]);
        key(&mut app, &world, &l, KeyCode::Char('x'));
        key(&mut app, &world, &l, KeyCode::Char('/'));
        for c in "miss".chars() {
            key(&mut app, &world, &l, KeyCode::Char(c));
        }
        assert_eq!(shown(&app), ["a mission"]);
        key(&mut app, &world, &l, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Situation { typing: false, .. }));
        key(&mut app, &world, &l, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Galaxy));
    }

    #[test]
    fn the_view_renders_entries_and_details() {
        let mut app = App::new();
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        app.say(Ev::Alert, "codex on alpha awaits orders".into(), DIM_LOG).on("/alpha").by(9);
        app.mode = Mode::Situation { id: app.log[0].id, typing: false };
        let mut fb = Fb::new(120, 40);
        let l = Lay::new(&fb, false);
        draw::frame(&mut fb, &mut app, &world, &l, 1.0, None);
        let rendered: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
        assert!(rendered.contains("SITUATION LOG · 1 of 1 entries"));
        assert!(rendered.contains("1 alerts 1"));
        assert!(rendered.contains("codex on alpha awaits orders"));
        assert!(rendered.contains("#9 · has left"));
    }
}

#[cfg(test)]
mod multiline_tests {
    use super::*;

    #[test]
    fn mission_text_moves_between_lines_keeping_the_column() {
        let mut text = Line::default();
        text.paste_lines("first line\r\nab\n\tthird");
        assert_eq!(text.s, "first line\nab\n    third");
        text.cur = 8; // "first li|ne"
        assert!(!text.vertical(true));
        assert!(text.vertical(false));
        assert_eq!(text.cur, 13); // clamped to the end of "ab"
        assert!(text.vertical(false));
        assert_eq!(text.cur, 16); // column 2 of "    third"
        assert!(!text.vertical(false));
    }

    #[test]
    fn wrapped_text_places_the_cursor_after_breaks_and_wraps() {
        let pos = draw::layout("abcd\nef", 3);
        assert_eq!(pos, vec![(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (2, 0), (2, 1), (2, 2)]);
    }
}

#[cfg(test)]
mod path_menu_tests {
    use super::*;

    #[test]
    fn completion_cycles_beyond_eight_results_and_can_go_backwards() {
        let dir = std::env::temp_dir().join(format!("empire-completion-cycle-{}", std::process::id()));
        for i in 0..12 { std::fs::create_dir_all(dir.join(format!("Project{i:02}"))).unwrap(); }
        let mut path = Line::default();
        path.set(format!("{}/pjt", dir.display()));
        let comps = complete(&path.s);
        let mut app = App::new();
        app.mode = Mode::Colonize(Colonize { path, comps, comp_i: 0, kind: 0, mkdir: false });
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        let mut fb = Fb::new(100, 30);
        let l = Lay::new(&fb, false);
        let client = Client::isolated();
        for _ in 0..12 { handle(&mut app, &world, &l, &client, None, Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)), 1.0, &mut None); }
        assert!(matches!(&app.mode, Mode::Colonize(c) if c.path.s.ends_with("Project11/")));
        draw::frame(&mut fb, &mut app, &world, &l, 1.0, None);
        let rows: Vec<String> = fb.txt.chunks(fb.cols).map(|r| r.iter().map(|c| c.map_or(' ', |c| c.ch)).collect()).collect();
        assert!(rows.iter().filter(|r| r.contains("Project11")).count() >= 2, "selected completion must remain visible below the input");
        handle(&mut app, &world, &l, &client, None, Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)), 1.0, &mut None);
        assert!(matches!(&app.mode, Mode::Colonize(c) if c.path.s.ends_with("Project10/")));
        assert!(fuzzy_score("øp", "ØnskeProsjekt").is_some());
        assert!(fuzzy_score("zz", "project").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn completion_matches_case_insensitively_and_ranks_gaps_after_prefixes() {
        let dir = std::env::temp_dir().join(format!("empire-fuzzy-{}", std::process::id()));
        for name in ["Docs", "Documents", "my-docs", "downloads", ".docs"] { std::fs::create_dir_all(dir.join(name)).unwrap(); }
        let got = complete(&format!("{}/dOCS", dir.display()));
        let names: Vec<_> = got.iter().map(|s| Path::new(s).file_name().unwrap().to_str().unwrap()).collect();
        assert_eq!(names, ["Docs", "my-docs", "Documents"]);
        assert!(complete(&format!("{}/dwn", dir.display())).iter().any(|s| s.ends_with("downloads/")));
        assert!(complete(&format!("{}/.DOC", dir.display())).iter().any(|s| s.ends_with(".docs/")));
        assert!(complete(&format!("{}/zzz", dir.display())).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn modified_backspace_removes_a_path_segment_and_preserves_the_tail() {
        for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let mut line = Line::default();
            line.set("code/somethi".into());
            assert!(line.key(&KeyEvent::new(KeyCode::Backspace, modifier)));
            assert_eq!(line.s, "code/");
            line.set("~/code/æøå/tail".into());
            line.cur = "~/code/æøå".chars().count();
            line.key(&KeyEvent::new(KeyCode::Backspace, modifier));
            assert_eq!(line.s, "~/code//tail");
            assert_eq!(line.cur, 7);
            line.set("code/folder/".into());
            line.key(&KeyEvent::new(KeyCode::Backspace, modifier));
            assert_eq!(line.s, "code/");
        }
        let mut line = Line::default();
        line.set("code/somethi".into());
        line.key(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(line.s, "code/someth");
    }
}

#[cfg(test)]
mod role_tests {
    use super::*;

    #[test]
    fn composer_offers_roles_and_a_click_picks_one_and_launches() {
        let mut app = App::new();
        app.mode = Mode::Brief { planet: "/world".into(), text: Line::default(), kind: 0, launch: false, role: 0, to: None, attachments: Vec::new(), sending: false, ship: Outfit::default() };
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        let mut fb = Fb::new(100, 30);
        let l = Lay::new(&fb, false);
        crate::draw::frame(&mut fb, &mut app, &world, &l, 1.0, None);
        let rows: Vec<String> = fb.txt.chunks(fb.cols).map(|row| row.iter().map(|c| c.map_or(' ', |c| c.ch)).collect()).collect();
        let (r, line) = rows.iter().enumerate().find(|(_, r)| r.contains("scout · builder")).expect("role row");
        let c = line.chars().position(|ch| ch == 'b').unwrap() as u16; // "builder"
        let click = event::MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: c, row: r as u16, modifiers: KeyModifiers::NONE };
        let client = Client::isolated();
        let mut drag = None;
        handle(&mut app, &world, &l, &client, None, Event::Mouse(click), 1.0, &mut drag);
        assert!(matches!(app.mode, Mode::Brief { launch: true, role, .. } if ROLES[role] == Role::Builder));
    }
}

#[cfg(test)]
mod advisor_tests {
    use super::*;
    use crate::advisor::Pick;

    fn composer() -> (App, World, Lay) {
        let mut app = App::new();
        app.agents = vec![ship_card_tests::ship(4, false), ship_card_tests::ship(7, false)];
        app.mode = Mode::Brief { planet: "/home/me/world".into(), text: Line::default(), kind: 0, launch: true, role: 0, to: None, attachments: Vec::new(), sending: false, ship: Outfit::default() };
        let fb = Fb::new(100, 34);
        (app, World { systems: vec![], planets: vec![], lanes: vec![], station: None }, Lay::new(&fb, false))
    }

    fn choice(app: &App) -> (bool, Option<u32>, Role) {
        match &app.mode {
            Mode::Brief { launch, to, role, .. } => (*launch, *to, ROLES[*role]),
            _ => panic!("composer closed"),
        }
    }

    #[test]
    fn tab_goes_launch_then_each_ship_then_queue() {
        let (mut app, world, l) = composer();
        let client = Client::isolated();
        let tab = || Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let mut seen = Vec::new();
        for _ in 0..4 {
            handle(&mut app, &world, &l, &client, None, tab(), 1.0, &mut None);
            seen.push(choice(&app));
        }
        let picks: Vec<(bool, Option<u32>)> = seen.iter().map(|c| (c.0, c.1)).collect();
        assert_eq!(picks, vec![(false, Some(4)), (false, Some(7)), (false, None), (true, None)]);
    }

    #[test]
    fn a_sure_pick_is_preselected_until_the_player_chooses() {
        let (mut app, ..) = composer();
        app.advisor.asked = "k".into();
        app.advisor_answered("k".into(), Ok(Pick { ship: Some(7), ship_conf: 0.9, role: Role::None, role_conf: 0.2 }));
        assert_eq!(choice(&app), (false, Some(7), Role::None));
        // a new ship, with the role it's sure of
        app.advisor_answered("k".into(), Ok(Pick { ship: None, ship_conf: 0.8, role: Role::Scout, role_conf: 0.7 }));
        assert_eq!(choice(&app), (true, None, Role::Scout));
        // unsure picks, stale answers and the player's own choice leave it alone
        app.advisor_answered("k".into(), Ok(Pick { ship: Some(4), ship_conf: 0.4, role: Role::None, role_conf: 0.0 }));
        app.advisor_answered("old".into(), Ok(Pick { ship: Some(4), ship_conf: 0.99, role: Role::None, role_conf: 0.0 }));
        assert_eq!(choice(&app), (true, None, Role::Scout));
        app.advisor.touched = true;
        app.advisor_answered("k".into(), Ok(Pick { ship: Some(4), ship_conf: 0.99, role: Role::None, role_conf: 0.0 }));
        assert_eq!(choice(&app), (true, None, Role::Scout));
        assert_eq!(app.advisor.pick.as_ref().and_then(|p| p.ship), Some(4)); // still shown
    }
}

#[cfg(test)]
mod structure_tests {
    use super::*;

    #[test]
    fn landing_takes_precedence_over_transmissions_and_focus_enters_the_planet() {
        let mut app = App::new();
        let mut ship = ship_card_tests::ship(1, false);
        ship.status = Status::Blocked;
        ship.ask = Some(Ask { question: "Allow this?".into(), ..Default::default() });
        app.agents.push(ship);
        app.sel = Some(Sel::Planet("/uninhabited/world".into()));
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        let l = Lay::new(&Fb::new(100, 30), false);
        for key in [KeyCode::Enter, KeyCode::Char('f'), KeyCode::Char('+')] {
            app.mode = Mode::Galaxy;
            app.cam.tz = 2.9;
            handle(&mut app, &world, &l, &Client::isolated(), None, Event::Key(KeyEvent::new(key, KeyModifiers::NONE)), 1.0, &mut None);
            assert!(matches!(&app.mode, Mode::Planet(v) if v.key == "/uninhabited/world"));
        }
    }

    #[test]
    fn planet_files_scroll_refresh_and_return_from_construction() {
        let dir = std::env::temp_dir().join(format!("empire-planet-view-{}", std::process::id()));
        for i in 0..12 {
            let p = dir.join(format!(".agents/skills/skill-{i:02}"));
            std::fs::create_dir_all(&p).unwrap();
            let text = (0..40).map(|n| format!("Skill {i}, line {n}\n")).collect::<String>();
            std::fs::write(p.join("SKILL.md"), text).unwrap();
        }
        let mut app = App::new();
        app.history.clear();
        let key = dir.to_string_lossy().to_string();
        app.history.insert(key.clone(), survey::now() as u64);
        app.sel = Some(Sel::Planet(key.clone()));
        let world = app.world(1.0, 0.0);
        let mut fb = Fb::new(100, 30);
        let l = Lay::new(&fb, true);
        let (client, commands) = Client::recording();
        let press = |app: &mut App, code| { handle(app, &world, &l, &client, None, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), 1.0, &mut None); };
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::End);
        assert!(matches!(&app.mode, Mode::Planet(v) if v.file == 11 && v.preview.contains("Skill 11, line 0")));
        press(&mut app, KeyCode::PageDown);
        assert!(matches!(&app.mode, Mode::Planet(v) if v.scroll > 0));
        draw::frame(&mut fb, &mut app, &world, &l, 1.0, None);
        let text: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
        assert!(text.contains("skill-11") && text.contains("Skill 11, line 20"));
        press(&mut app, KeyCode::Char('B'));
        assert!(matches!(&app.mode, Mode::Brief { text, .. } if text.s.starts_with("Renovate") && text.s.contains("skill-11")));
        press(&mut app, KeyCode::Esc);
        assert!(matches!(&app.mode, Mode::Planet(v) if v.file == 11 && v.sel == 2));
        std::fs::remove_file(dir.join(".agents/skills/skill-11/SKILL.md")).unwrap();
        press(&mut app, KeyCode::Char('R'));
        assert!(matches!(&app.mode, Mode::Planet(v) if v.files().len() == 11 && v.file == 0 && v.preview.contains("Skill 0, line 0")));
        // the same layout drives mouse targets, even when the galaxy outliner is enabled
        let g = draw::PlanetLayout::new(&l);
        let me = event::MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: (g.split + 3) as u16, row: (g.y + 1) as u16, modifiers: KeyModifiers::NONE };
        handle(&mut app, &world, &l, &client, None, Event::Mouse(me), 1.0, &mut None);
        assert!(matches!(&app.mode, Mode::Planet(v) if v.file == 1 && v.preview.contains("Skill 1, line 0")));
        for (cols, rows) in [(80, 24), (48, 18), (30, 12)] {
            let mut small = Fb::new(cols, rows);
            draw::frame(&mut small, &mut app, &world, &Lay::new(&Fb::new(cols, rows), false), 1.0, None);
            let text: String = small.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
            assert!(text.contains("Esc galaxy"));
            if cols >= 48 { assert!(text.contains("BUILDINGS") && text.contains("SKILL.md")); }
        }
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Galaxy));
        assert_eq!(app.sel, Some(Sel::Planet(key)));
        assert!(commands.try_iter().next().is_none(), "browsing and cancelling must never dispatch an order");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn planet_preview_reports_missing_files_and_limits_large_files() {
        let dir = std::env::temp_dir().join(format!("empire-planet-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "x".repeat(70_000)).unwrap();
        let mut v = PlanetView::new(dir.to_string_lossy().into_owned());
        assert!(v.preview.ends_with("[Preview limited to 64 KiB]"));
        std::fs::remove_file(dir.join("AGENTS.md")).unwrap();
        v.read();
        assert!(v.preview.starts_with("Cannot read this file:"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn entering_a_world_opens_its_buildings_even_without_ships() {
        let mut app = App::new();
        app.sel = Some(Sel::Planet("/uninhabited/world".into()));
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        let mut fb = Fb::new(100, 30);
        let l = Lay::new(&fb, false);
        let client = Client::isolated();
        handle(&mut app, &world, &l, &client, None, Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), 1.0, &mut None);
        draw::frame(&mut fb, &mut app, &world, &l, 1.0, None);
        let text: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
        assert!(text.contains("PLANET") && text.contains("BUILDINGS") && text.contains("AGENTS.md"));
    }

    #[test]
    fn b_shows_structures_and_drafts_a_construction_order_for_a_builder() {
        let mut app = App::new();
        let key = "/home/me/world".to_string();
        let charter = Built { kind: Kind::Charter, count: 1, files: vec!["AGENTS.md".into()] };
        app.survey.worlds.insert(key.clone(), survey::World { structures: vec![charter], ..Default::default() });
        app.sel = Some(Sel::Planet(key.clone()));
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        let mut fb = Fb::new(120, 40);
        let l = Lay::new(&fb, false);
        let client = Client::isolated();
        let key_ev = |c| Event::Key(KeyEvent::new(c, KeyModifiers::NONE));
        handle(&mut app, &world, &l, &client, None, key_ev(KeyCode::Char('B')), 1.0, &mut None);
        assert!(matches!(app.mode, Mode::Build { .. }));
        draw::frame(&mut fb, &mut app, &world, &l, 1.0, None);
        let text: String = fb.txt.chunks(fb.cols).map(|row| row.iter().map(|c| c.map_or(' ', |c| c.ch)).collect::<String>() + "\n").collect();
        assert!(text.contains("STRUCTURES") && text.contains("renovate") && text.contains("construct"));
        // the charter stands, so it's renovated; the archive is an empty plot, so it's built
        handle(&mut app, &world, &l, &client, None, key_ev(KeyCode::Char('1')), 1.0, &mut None);
        match &app.mode {
            Mode::Brief { text, launch, role, .. } => {
                assert!(text.s.starts_with("Renovate") && text.s.contains("AGENTS.md"));
                assert!(*launch);
                assert_eq!(ROLES[*role], Role::Builder);
            }
            _ => panic!("no composer"),
        }
        app.mode = Mode::Build { planet: key, sel: 0 };
        handle(&mut app, &world, &l, &client, None, key_ev(KeyCode::Down), 1.0, &mut None);
        handle(&mut app, &world, &l, &client, None, key_ev(KeyCode::Enter), 1.0, &mut None);
        assert!(matches!(&app.mode, Mode::Brief { text, .. } if text.s.starts_with("Write a CLAUDE.md")));
    }
}

#[cfg(test)]
mod panel_scroll_tests {
    use super::*;

    fn wheel(app: &mut App, w: &World, l: &Lay, row: u16, down: bool) {
        let kind = if down { MouseEventKind::ScrollDown } else { MouseEventKind::ScrollUp };
        let me = event::MouseEvent { kind, column: (l.cols - 5) as u16, row, modifiers: KeyModifiers::NONE };
        mouse(app, w, l, &Client::isolated(), me, &mut None, 0.0);
    }

    fn sys(i: usize) -> System {
        System { key: format!("/s{i}"), name: format!("s{i}"), wpos: (0.0, 0.0), planets: vec![], orbits: vec![], star: s3(0.8) }
    }

    #[test]
    fn the_wheel_scrolls_the_outliner_until_the_selection_moves() {
        let mut app = App::new();
        let w = World { systems: (0..60).map(sys).collect(), planets: vec![], lanes: vec![], station: None };
        let l = Lay::new(&Fb::new(120, 40), true);
        app.sel = Some(Sel::Sys("/s0".into()));
        assert_eq!(draw::panel_hit(&app, &w, &l, 3), Some(Sel::Sys("/s0".into())));
        wheel(&mut app, &w, &l, 5, true);
        wheel(&mut app, &w, &l, 5, true);
        assert_eq!(draw::panel_hit(&app, &w, &l, 3), Some(Sel::Sys("/s6".into())));
        wheel(&mut app, &w, &l, 5, false);
        assert_eq!(draw::panel_hit(&app, &w, &l, 3), Some(Sel::Sys("/s3".into())));
        // scrolling never runs past either end
        for _ in 0..40 { wheel(&mut app, &w, &l, 5, true); }
        assert_eq!(draw::panel_hit(&app, &w, &l, 3).is_some(), true);
        assert_eq!(draw::panel_hit(&app, &w, &l, 2), None);
        // a new selection is brought back into view
        app.sel = Some(Sel::Sys("/s1".into()));
        assert_eq!(draw::panel_hit(&app, &w, &l, 3), Some(Sel::Sys("/s1".into())));
    }

    #[test]
    fn the_wheel_scrolls_the_side_panel_situation_log() {
        let mut app = App::new();
        let w = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        for i in 0..30 { app.say(Ev::Mission, format!("entry {i}"), DIM_LOG); }
        let mut fb = Fb::new(120, 40);
        let l = Lay::new(&fb, true);
        let last = (l.rows - 3) as u16;
        let msg = |app: &App, row| draw::panel_log_hit(app, &w, &l, row).and_then(|id| app.log.iter().find(|e| e.id == id)).map(|e| e.msg.clone());
        let top = last as i32 - (msg(&app, last as i32).map(|m| 29 - m[6..].parse::<i32>().unwrap()).unwrap());
        assert_eq!(msg(&app, top).as_deref(), Some("entry 29"));
        wheel(&mut app, &w, &l, last, true);
        assert_eq!(msg(&app, top).as_deref(), Some("entry 26"));
        draw::frame(&mut fb, &mut app, &w, &l, 1.0, None);
        let rendered: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
        assert!(rendered.contains("entry 26") && !rendered.contains("entry 27"));
        for _ in 0..20 { wheel(&mut app, &w, &l, last, false); }
        assert_eq!(msg(&app, top).as_deref(), Some("entry 29"));
    }
}

#[cfg(test)]
mod anomaly_tests {
    use super::*;
    use crate::anomaly::{Book, Observation, State};

    fn fixture() -> (App, World, Lay) {
        let mut app = App::new();
        app.history.clear();
        app.online = true;
        app.loaded = true;
        let mut a = ship_card_tests::ship(1, false);
        a.cwd = "/alpha/api".into();
        app.agents = vec![a];
        app.survey.worlds.insert("/alpha/api".into(), survey::World { git: true, checked: Some(10), conflicts: vec!["src/login.rs".into()], ..Default::default() });
        app.refresh_anomalies();
        let w = app.world(0.0, 0.0);
        let l = Lay::new(&Fb::new(120, 40), false);
        (app, w, l)
    }

    fn key(app: &mut App, w: &World, l: &Lay, code: KeyCode) {
        handle(app, w, l, &Client::isolated(), None, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), 1.0, &mut None);
    }

    fn text(app: &mut App, w: &World, cols: usize, rows: usize) -> String {
        let mut fb = Fb::new(cols, rows);
        let l = Lay::new(&fb, false);
        draw::frame(&mut fb, app, w, &l, 1.0, None);
        fb.txt.chunks(cols).map(|row| row.iter().map(|c| c.map_or(' ', |c| c.ch)).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn browser_filters_and_dismissal_keep_the_evidence_and_history() {
        let (mut app, w, l) = fixture();
        key(&mut app, &w, &l, KeyCode::Char('A'));
        let screen = text(&mut app, &w, 120, 40);
        assert!(screen.contains("ANOMALIES"));
        assert!(screen.contains("src/login.rs"));
        assert!(screen.contains("System: /alpha"));
        key(&mut app, &w, &l, KeyCode::Char('/'));
        handle(&mut app, &w, &l, &Client::isolated(), None, Event::Paste("not found".into()), 1.0, &mut None);
        assert!(app.anomaly_rows().is_empty());
        key(&mut app, &w, &l, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Anomalies { .. }));
        key(&mut app, &w, &l, KeyCode::Char('x'));
        key(&mut app, &w, &l, KeyCode::Char('i'));
        app.refresh_anomalies();
        assert!(app.anomaly_rows().is_empty());
        assert_eq!(app.anomalies.findings[0].state, State::Dismissed);
        key(&mut app, &w, &l, KeyCode::Char('v'));
        assert_eq!(app.anomaly_rows().len(), 1);
        assert!(text(&mut app, &w, 120, 40).contains("Marked dismissed"));
        key(&mut app, &w, &l, KeyCode::Char('o'));
        assert_eq!(app.anomalies.findings[0].state, State::Open);
        assert!(text(&mut app, &w, 80, 24).contains("ANOMALIES"));
    }

    #[test]
    fn long_anomaly_lists_mouse_selection_and_details_scroll() {
        let (mut app, w, l) = fixture();
        for i in 0..40 {
            app.anomalies.observe(Observation { key: format!("report:{i}"), world: "/alpha/api".into(), host: "/alpha/api".into(), kind: "Report".into(),
                title: format!("finding {i}"), evidence: (0..60).map(|n| format!("evidence line {n}\n")).collect(), at: 100 + i, present: true });
        }
        key(&mut app, &w, &l, KeyCode::Char('A'));
        key(&mut app, &w, &l, KeyCode::End);
        let screen = text(&mut app, &w, 120, 40);
        let row = screen.lines().position(|s| s.contains("open · Git · api · #1")).unwrap();
        assert_eq!(draw::anomaly_hit(&l, 41, 40, 5, row as i32), Some(40));
        key(&mut app, &w, &l, KeyCode::Home);
        for _ in 0..30 { key(&mut app, &w, &l, KeyCode::PageDown); }
        assert!(text(&mut app, &w, 120, 40).contains("evidence line 59"));
        key(&mut app, &w, &l, KeyCode::Char('d'));
        assert_eq!(app.anomalies.findings.last().unwrap().state, State::Deferred);
        assert!(!app.attention(&w).iter().any(|r| r.title == "finding 39"));
        assert!(app.attention(&w).iter().any(|r| r.title == "finding 38"));
    }

    #[test]
    fn stale_or_missing_surveys_do_not_resolve_findings() {
        let (mut app, _, _) = fixture();
        let s = app.survey.worlds.get_mut("/alpha/api").unwrap();
        s.prs_checked = Some(10);
        s.prs.push(survey::Pr { number: 42, ci: "fail", title: "Login".into(), ..Default::default() });
        app.refresh_anomalies();
        assert_eq!(app.anomalies.findings.len(), 2);
        let s = app.survey.worlds.get_mut("/alpha/api").unwrap();
        s.prs[0].ci = "pass";
        s.prs_checked = Some(20);
        s.prs_error = Some("offline".into());
        s.checked = None;
        s.conflicts.clear();
        app.refresh_anomalies();
        assert!(app.anomalies.findings.iter().all(|f| f.state == State::Open));
        app.survey.worlds.clear();
        app.refresh_anomalies();
        assert!(app.anomalies.findings.iter().all(|f| f.state == State::Open));
    }

    #[test]
    fn a_moons_anomaly_counts_once_in_its_host_system() {
        let (mut app, w, l) = fixture();
        app.survey.worlds.insert("/moons/api-fix".into(), survey::World { git: true, checked: Some(20), host: Some("/alpha/api".into()), branch: "fix".into(), ..Default::default() });
        app.refresh_anomalies();
        assert_eq!(app.anomaly_count("/alpha", true), 2);
        assert_eq!(app.anomaly_count("/alpha/api", false), 2);
        assert_eq!(app.anomaly_count("/moons/api-fix", false), 1);
        app.cam.z = 0.3;
        let labels = draw::overview_labels(&app, &w, &l);
        assert!(labels.iter().flat_map(|l| l.lines.iter()).any(|(s, _)| s.contains("2 anomalies")));
        app.anomaly_filter.system = Some("/alpha".into());
        assert_eq!(app.anomaly_rows().len(), 2);
        app.anomaly_filter.world = Some("/alpha/api".into());
        app.anomaly_filter.kind = 3;
        assert_eq!(app.anomaly_rows()[0].world, "/moons/api-fix");
        app.online = false;
        app.agents.clear();
        app.refresh_anomalies();
        assert_eq!(app.anomalies.findings.len(), 2);
    }

    #[test]
    fn investigation_is_a_scout_draft_and_cancel_does_not_start_it() {
        let (mut app, w, l) = fixture();
        app.anomalies = Book::default();
        app.anomalies.observe(Observation { key: "report".into(), world: "/tmp".into(), host: "/tmp".into(), kind: "Report".into(), title: "Check fixture".into(), evidence: "Explicit report".into(), at: 10, present: true });
        key(&mut app, &w, &l, KeyCode::Char('A'));
        key(&mut app, &w, &l, KeyCode::Char('t'));
        assert!(matches!(&app.mode, Mode::Brief { launch: true, role, text, .. } if ROLES[*role] == Role::Scout && text.s.starts_with("[anomaly #1] ")));
        assert!(app.advisor.touched);
        assert_eq!(app.anomalies.findings[0].state, State::Open);
        key(&mut app, &w, &l, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Anomalies { id: 1, .. }));
        assert_eq!(app.anomalies.findings[0].state, State::Open);
        app.missions.push(Mission { id: 7, cwd: "/tmp".into(), text: "[anomaly #1] Investigate".into(), at: 20, state: "active".into(), ..Default::default() });
        app.survey.worlds.clear();
        app.refresh_anomalies();
        assert_eq!(app.anomalies.findings[0].state, State::Investigating);
        app.missions[0].state = "done".into();
        app.refresh_anomalies();
        app.refresh_anomalies();
        let f = &app.anomalies.findings[0];
        assert_eq!(f.missions, vec![7]);
        assert_eq!(f.state, State::Investigating, "a finished turn does not prove the anomaly resolved");
        assert_eq!(f.history.iter().filter(|e| e.note.contains("finished its turn")).count(), 1);
    }
}

#[cfg(test)]
mod sector_tests {
    use super::*;

    fn fixture() -> App {
        let mut app = App::new();
        app.online = true;
        app.loaded = true;
        let id = app.sectors.create("Orion").unwrap();
        for world in ["/work/runtime", "/other/frontend", "/services/gateway"] { app.sectors.toggle(id, world).unwrap(); }
        for (i, (path, status)) in [("/work/runtime", Status::Working), ("/other/frontend", Status::Blocked), ("/moons/frontend-fix", Status::Done), ("/unrelated/world", Status::Working)].into_iter().enumerate() {
            let mut a = ship_card_tests::ship(i as u32 + 1, false);
            a.cwd = path.into(); a.status = status;
            app.agents.push(a);
        }
        app.survey.worlds.insert("/moons/frontend-fix".into(), survey::World { host: Some("/other/frontend".into()), ..Default::default() });
        app.missions.push(Mission { id: 1, cwd: "/moons/frontend-fix".into(), state: "queued".into(), ..Default::default() });
        app.missions.push(Mission { id: 2, cwd: "/work/runtime".into(), state: "active".into(), ..Default::default() });
        app.missions.push(Mission { id: 3, cwd: "/unrelated/world".into(), state: "queued".into(), ..Default::default() });
        for (path, host, present) in [("/other/frontend", "/other/frontend", true), ("/moons/frontend-fix", "/other/frontend", true), ("/unrelated/world", "/unrelated/world", true)] {
            app.anomalies.observe(crate::anomaly::Observation { key: "report".into(), world: path.into(), host: host.into(), kind: "Report".into(), title: "Finding".into(), evidence: "Evidence".into(), at: 10, present });
        }
        app
    }

    #[test]
    fn sector_totals_follow_moons_and_exclude_unrelated_worlds() {
        let mut app = fixture();
        let stats = app.sector_stats(&app.sectors.sectors[0]);
        assert_eq!(stats, SectorStats { worlds: 3, ships: 3, working: 1, blocked: 1, finished: 1, missions: 1, queued: 1, anomalies: 2 });
        app.agents[0].dying = Some(1.0);
        app.anomalies.set_state(1, crate::anomaly::State::Dismissed, 20);
        let stats = app.sector_stats(&app.sectors.sectors[0]);
        assert_eq!(stats.ships, 2);
        assert_eq!(stats.working, 0);
        assert_eq!(stats.anomalies, 1);
    }

    fn key(app: &mut App, w: &World, l: &Lay, code: KeyCode) {
        handle(app, w, l, &Client::isolated(), None, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), 1.0, &mut None);
    }

    fn paste(app: &mut App, w: &World, l: &Lay, value: &str) {
        handle(app, w, l, &Client::isolated(), None, Event::Paste(value.into()), 1.0, &mut None);
    }

    fn screen(app: &mut App, w: &World, cols: usize, rows: usize) -> String {
        let mut fb = Fb::new(cols, rows);
        let l = Lay::new(&fb, false);
        draw::frame(&mut fb, app, w, &l, 1.0, None);
        fb.txt.chunks(cols).map(|r| r.iter().map(|c| c.map_or(' ', |c| c.ch)).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn sector_editor_creates_renames_and_assigns_worlds_without_sending_orders() {
        let mut app = fixture();
        app.sectors = Default::default();
        let w = app.world(0.0, 0.0);
        let l = Lay::new(&Fb::new(120, 40), false);
        key(&mut app, &w, &l, KeyCode::Char('V'));
        assert!(matches!(app.mode, Mode::Sectors));
        key(&mut app, &w, &l, KeyCode::Char('n'));
        paste(&mut app, &w, &l, "Orion");
        key(&mut app, &w, &l, KeyCode::Enter);
        assert_eq!(app.sectors.sectors[0].name, "Orion");
        let candidates = app.sector_worlds(&w);
        assert!(!candidates.iter().any(|p| p.starts_with("/moons")));
        for path in ["/other/frontend", "/work/runtime"] {
            app.sector_view.world = path.into();
            key(&mut app, &w, &l, KeyCode::Char(' '));
        }
        let rendered = screen(&mut app, &w, 120, 40);
        assert!(rendered.contains("Orion · 2 worlds"));
        assert!(rendered.contains("3 ships · 1 working · 1 waiting · 1 finished"));
        assert!(rendered.contains("1 missions under way · 1 queued · 2 anomalies"));
        assert!(rendered.contains("[+] /other/frontend"));
        assert!(screen(&mut app, &w, 80, 24).contains("Space membership"));
        key(&mut app, &w, &l, KeyCode::Char('r'));
        app.sector_view.text.set("Orion platform".into());
        key(&mut app, &w, &l, KeyCode::Enter);
        assert_eq!(app.sectors.sectors[0].name, "Orion platform");
        assert_eq!(app.agents.len(), 4);
        assert_eq!(app.missions.len(), 3);
    }

    #[test]
    fn sector_focus_and_anomaly_filter_keep_the_rest_of_the_galaxy_available() {
        let mut app = fixture();
        let w = app.world(0.0, 0.0);
        let l = Lay::new(&Fb::new(120, 40), false);
        key(&mut app, &w, &l, KeyCode::Char('V'));
        key(&mut app, &w, &l, KeyCode::Char('a'));
        assert_eq!(app.anomaly_rows().len(), 2);
        assert!(app.anomaly_rows().iter().all(|f| f.host == "/other/frontend"));
        assert!(screen(&mut app, &w, 120, 40).contains("Orion sector"));
        key(&mut app, &w, &l, KeyCode::Char('x'));
        assert_eq!(app.anomaly_rows().len(), 3);
        key(&mut app, &w, &l, KeyCode::Esc);
        key(&mut app, &w, &l, KeyCode::Char('V'));
        key(&mut app, &w, &l, KeyCode::Char('f'));
        assert!(matches!(app.mode, Mode::Galaxy));
        assert_eq!(app.sector_focus, Some(1));
        assert!(app.world(0.0, 0.0).planet("/unrelated/world").is_some());
        assert!(screen(&mut app, &w, 120, 40).contains("Orion · 3 worlds"));
        app.cam.z = 0.3;
        let labels = draw::overview_labels(&app, &w, &l);
        assert!(labels.iter().flat_map(|l| &l.lines).any(|(s, _)| s == "▱ WORK"));
        key(&mut app, &w, &l, KeyCode::Esc);
        assert!(app.sector_focus.is_none());
    }

    #[test]
    fn sector_lists_scroll_and_mouse_hits_match_the_visible_rows() {
        let mut app = fixture();
        for n in 0..35 { app.sectors.create(&format!("Sector {n}")).unwrap(); }
        for n in 0..35 { app.sectors.toggle(1, &format!("/archive/world-{n:02}")).unwrap(); }
        let w = app.world(0.0, 0.0);
        let l = Lay::new(&Fb::new(120, 40), false);
        key(&mut app, &w, &l, KeyCode::Char('V'));
        key(&mut app, &w, &l, KeyCode::End);
        let text = screen(&mut app, &w, 120, 40);
        let row = text.lines().position(|r| r.contains("Sector 34") && !r.contains("worlds")).unwrap();
        assert_eq!(draw::sector_hit(&l, 36, 3, 35, 0, 5, row as i32), Some((false, 35)));
        key(&mut app, &w, &l, KeyCode::Home);
        key(&mut app, &w, &l, KeyCode::Tab);
        key(&mut app, &w, &l, KeyCode::End);
        let worlds = app.sector_worlds(&w);
        let text = screen(&mut app, &w, 120, 40);
        let row = text.lines().position(|r| r.contains("[+] /work/runtime")).unwrap();
        assert_eq!(draw::sector_hit(&l, 36, worlds.len(), 0, worlds.len() - 1, 50, row as i32), Some((true, worlds.len() - 1)));
    }

    #[test]
    fn path_membership_persists_and_dissolving_leaves_the_folder_untouched() {
        let dir = std::env::temp_dir().join(format!("empire-sector-ui-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("frontend")).unwrap();
        std::fs::write(dir.join("frontend/keep.txt"), "keep me").unwrap();
        let mut app = fixture();
        app.sectors = Default::default();
        app.sector_path = Some(dir.join("sectors.json"));
        let w = app.world(0.0, 0.0);
        let l = Lay::new(&Fb::new(100, 30), false);
        key(&mut app, &w, &l, KeyCode::Char('V'));
        key(&mut app, &w, &l, KeyCode::Char('n'));
        paste(&mut app, &w, &l, "Orion");
        key(&mut app, &w, &l, KeyCode::Enter);
        key(&mut app, &w, &l, KeyCode::Char('p'));
        paste(&mut app, &w, &l, dir.join("missing").to_str().unwrap());
        key(&mut app, &w, &l, KeyCode::Enter);
        assert!(app.sector_view.error.contains("existing folder"));
        assert!(!dir.join("missing").exists());
        app.sector_view.text.set(dir.join("frontend").to_string_lossy().into_owned());
        key(&mut app, &w, &l, KeyCode::Enter);
        let path = dir.join("frontend").canonicalize().unwrap().to_string_lossy().into_owned();
        let saved = crate::sector::edit(&dir.join("sectors.json"), |_| {}).unwrap();
        assert_eq!(saved.sectors[0].worlds, vec![path.clone()]);
        let mapped = app.world(0.0, 0.0);
        assert!(mapped.planet(&path).is_some());
        key(&mut app, &mapped, &l, KeyCode::Char('x'));
        key(&mut app, &mapped, &l, KeyCode::Esc);
        assert_eq!(app.sectors.sectors.len(), 1);
        key(&mut app, &mapped, &l, KeyCode::Char('x'));
        key(&mut app, &mapped, &l, KeyCode::Char('y'));
        assert!(crate::sector::edit(&dir.join("sectors.json"), |_| {}).unwrap().sectors.is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("frontend/keep.txt")).unwrap(), "keep me");
        assert_eq!(app.agents.len(), 4);
        std::fs::remove_dir_all(dir).unwrap();
    }

}

#[cfg(test)]
mod hyperlane_tests {
    use super::*;

    fn key(app: &mut App, w: &World, l: &Lay, code: KeyCode) {
        handle(app, w, l, &Client::isolated(), None, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), 1.0, &mut None);
    }

    #[test]
    fn hyperlane_editor_opens_from_the_galaxy() {
        let mut app = App::new();
        let w = app.world(0.0, 0.0);
        let mut fb = Fb::new(110, 40);
        let l = Lay::new(&fb, false);
        key(&mut app, &w, &l, KeyCode::Char('H'));
        draw::frame(&mut fb, &mut app, &w, &l, 1.0, None);
        let text: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
        assert!(text.contains("HYPERLANES"));
    }
    fn fixture() -> (App, World, Lay) {
        let mut app = App::new();
        app.loaded = true; app.online = true;
        for (i, path) in ["/front/app", "/back/gateway", "/moons/fix"].into_iter().enumerate() {
            let mut a = ship_card_tests::ship(i as u32 + 1, false); a.cwd = path.into(); app.agents.push(a);
        }
        app.survey.worlds.insert("/moons/fix".into(), survey::World { host: Some("/front/app".into()), ..Default::default() });
        let mut w = app.world(0.0, 0.0);
        for p in &mut w.planets { p.wpos = if p.key == "/front/app" { (-65.0, 0.0) } else { (65.0, 0.0) }; }
        for s in &mut w.systems { s.wpos = (0.0, -60.0); }
        for a in &mut app.agents { a.wpos = (1000.0, 1000.0); }
        app.cam.z = 0.75;
        (app, w, Lay::new(&Fb::new(110, 40), false))
    }

    #[test]
    fn editor_creates_a_directional_link_from_a_moons_host_and_cancels_drafts() {
        let (mut app, w, l) = fixture();
        app.sel = Some(Sel::Planet("/moons/fix".into()));
        for code in [KeyCode::Char('H'), KeyCode::Char('n'), KeyCode::Enter, KeyCode::Enter] { key(&mut app, &w, &l, code); }
        assert_eq!(app.lane_view.from, "/front/app");
        assert_eq!(app.lane_view.to, "/back/gateway");
        key(&mut app, &w, &l, KeyCode::Enter);
        assert!(app.hyperlanes.lanes.is_empty());
        assert!(app.lane_view.error.contains("1–48"));
        handle(&mut app, &w, &l, &Client::isolated(), None, Event::Paste("calls".into()), 1.0, &mut None);
        key(&mut app, &w, &l, KeyCode::Enter);
        let lane = app.hyperlanes.lanes[0].clone();
        assert_eq!((lane.from.as_str(), lane.label.as_str(), lane.to.as_str()), ("/front/app", "calls", "/back/gateway"));
        key(&mut app, &w, &l, KeyCode::Char('r'));
        app.lane_view.text.set("depends on".into());
        key(&mut app, &w, &l, KeyCode::Enter);
        assert_eq!(app.hyperlanes.lanes[0].id, lane.id);
        assert_eq!(app.hyperlanes.lanes[0].label, "depends on");
        key(&mut app, &w, &l, KeyCode::Char('e'));
        key(&mut app, &w, &l, KeyCode::Esc);
        key(&mut app, &w, &l, KeyCode::Char('n'));
        key(&mut app, &w, &l, KeyCode::Esc);
        assert_eq!(app.hyperlanes.lanes.len(), 1);
        key(&mut app, &w, &l, KeyCode::Char('x'));
        key(&mut app, &w, &l, KeyCode::Char('n'));
        assert_eq!(app.hyperlanes.lanes.len(), 1);
        key(&mut app, &w, &l, KeyCode::Char('x'));
        key(&mut app, &w, &l, KeyCode::Char('y'));
        assert!(app.hyperlanes.lanes.is_empty());
        assert_eq!(app.agents.len(), 3);
        assert!(app.missions.is_empty());
    }

    #[test]
    fn dependency_overlay_picks_curves_and_labels_and_enter_opens_the_link() {
        let (mut app, w, l) = fixture();
        let id = app.hyperlanes.save(None, "/front/app", "/back/gateway", "calls").unwrap();
        assert!(draw::lane_paths(&app, &w, &l).is_empty());
        key(&mut app, &w, &l, KeyCode::Char('h'));
        assert!(app.sector_map()); // usable even without a sector
        for z in [0.2, 0.75, 1.4] {
            app.cam.z = z;
            let route = draw::lane_paths(&app, &w, &l).remove(0);
            assert!(route.points.first().unwrap().0 < route.points.last().unwrap().0);
            assert_eq!(pick_at(&app, &w, &l, route.points[12]), Some(Sel::Lane(id)));
            let label = draw::sector_labels(&app, &w, &l).into_iter().find(|p| p.target == Sel::Lane(id)).unwrap();
            assert_eq!(pick_at(&app, &w, &l, (label.col as f32 * 2.0 + 1.0, label.row as f32 * 4.0 + 2.0)), Some(Sel::Lane(id)));
        }
        app.sel = Some(Sel::Lane(id));
        key(&mut app, &w, &l, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Hyperlanes));
        assert_eq!(app.lane_view.id, id);
        key(&mut app, &w, &l, KeyCode::Char('f'));
        assert!(matches!(app.mode, Mode::Galaxy));
        assert_eq!((app.cam.tx, app.cam.ty), (0.0, 0.0));
        key(&mut app, &w, &l, KeyCode::Char('h'));
        assert!(app.sel.is_none());
        assert!(draw::lane_paths(&app, &w, &l).is_empty());
    }

    #[test]
    fn reciprocal_and_parallel_links_have_separate_curves() {
        let (mut app, w, l) = fixture();
        for (a, b, label) in [("/front/app", "/back/gateway", "calls"), ("/back/gateway", "/front/app", "notifies"), ("/front/app", "/back/gateway", "depends on")] {
            app.hyperlanes.save(None, a, b, label).unwrap();
        }
        app.dependencies = true;
        let paths = draw::lane_paths(&app, &w, &l);
        for (i, a) in paths.iter().enumerate() {
            for b in paths.iter().skip(i + 1) { assert_ne!(a.points[12], b.points[12]); }
            assert_eq!(draw::lane_pick(&app, &w, &l, a.points[12]), Some(Sel::Lane(a.id)));
        }
        let labels = draw::sector_labels(&app, &w, &l);
        for (i, a) in labels.iter().enumerate() {
            let width = a.lines.iter().map(|p| p.0.chars().count()).max().unwrap() as i32;
            assert!(a.col >= 0 && a.col + width <= l.cols && a.row >= 1 && a.row + (a.lines.len() as i32) < l.rows - 1);
            for b in labels.iter().skip(i + 1) {
                let bw = b.lines.iter().map(|p| p.0.chars().count()).max().unwrap() as i32;
                assert!(a.col + width <= b.col || b.col + bw <= a.col || a.row + a.lines.len() as i32 <= b.row || b.row + b.lines.len() as i32 <= a.row);
            }
        }
    }

    #[test]
    fn links_keep_worlds_charted_and_missing_endpoints_and_save_errors_visible() {
        let dir = std::env::temp_dir().join(format!("empire-hyperlane-ui-{}", std::process::id()));
        for p in ["a", "b"] { std::fs::create_dir_all(dir.join(p)).unwrap(); }
        let (a, b) = (dir.join("a").to_string_lossy().into_owned(), dir.join("b").to_string_lossy().into_owned());
        let path = dir.join("hyperlanes.json");
        crate::hyperlane::edit(&path, |book| { book.save(None, &a, &b, "calls").unwrap(); }).unwrap();
        let mut app = App::new(); app.lane_path = Some(path.clone()); app.open_hyperlanes();
        let w = app.world(0.0, 0.0);
        let l = Lay::new(&Fb::new(100, 36), false);
        assert!(w.planet(&a).is_some() && w.planet(&b).is_some());
        std::fs::remove_dir(dir.join("b")).unwrap();
        let w = app.world(0.0, 0.0);
        app.dependencies = true;
        assert!(draw::lane_paths(&app, &w, &l).is_empty());
        key(&mut app, &w, &l, KeyCode::Char('f'));
        assert!(app.lane_view.error.contains("unavailable"));
        assert_eq!(app.hyperlanes.lanes[0].to, b);
        key(&mut app, &w, &l, KeyCode::Char('r'));
        app.lane_view.text.set("depends on".into());
        std::fs::write(&path, "broken").unwrap();
        key(&mut app, &w, &l, KeyCode::Enter);
        assert!(app.lane_view.edit == Some(LaneEdit::Label));
        assert!(app.lane_view.error.contains("could not be saved"));
        assert_eq!(app.hyperlanes.lanes[0].label, "calls");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn long_link_lists_scroll_and_mouse_hits_the_visible_row() {
        let (mut app, w, l) = fixture();
        for i in 0..40 { app.hyperlanes.save(None, "/front/app", "/back/gateway", &format!("relationship {i}")).unwrap(); }
        app.open_hyperlanes();
        key(&mut app, &w, &l, KeyCode::End);
        let (x, y, _, _, room) = draw::hyperlane_layout(&l);
        let hit = draw::hyperlane_hit(&l, 40, 39, x + 3, y + 4).unwrap();
        assert_eq!(hit, 40 - room);
        handle(&mut app, &w, &l, &Client::isolated(), None, Event::Mouse(event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left), column: (x + 3) as u16, row: (y + 4) as u16, modifiers: KeyModifiers::NONE,
        }), 1.0, &mut None);
        assert_eq!(app.lane_view.id, app.hyperlanes.lanes[hit].id);
        for (cols, rows) in [(80, 24), (110, 40)] {
            let mut fb = Fb::new(cols, rows); let l = Lay::new(&fb, false);
            draw::frame(&mut fb, &mut app, &w, &l, 1.0, None);
            let text: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
            assert!(text.contains("From: /front/app") && text.contains("To:   /back/gateway"));
            assert!(text.contains("n new") && text.contains("Esc back"));
        }
    }

}

#[cfg(test)]
mod campaign_tests {
    use super::*;

    fn fixture() -> (App, World, Lay) {
        let mut app = App::new(); app.loaded = true; app.online = true; app.kinds = vec!["codex".into()];
        let mut a = ship_card_tests::ship(1, false); a.cwd = "/world/app".into(); app.agents.push(a);
        let mut moon = ship_card_tests::ship(2, false); moon.cwd = "/moons/auth".into(); app.agents.push(moon);
        app.survey.worlds.insert("/moons/auth".into(), survey::World { host: Some("/world/app".into()), ..Default::default() });
        let w = app.world(0.0, 0.0);
        app.sel = Some(Sel::Planet("/world/app".into()));
        (app, w, Lay::new(&Fb::new(120, 42), false))
    }
    fn key(app: &mut App, w: &World, l: &Lay, client: &Client, code: KeyCode) {
        handle(app, w, l, client, None, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), 1.0, &mut None);
    }
    fn paste(app: &mut App, w: &World, l: &Lay, client: &Client, text: &str) {
        handle(app, w, l, client, None, Event::Paste(text.into()), 1.0, &mut None);
    }
    fn plan(app: &mut App, w: &World, l: &Lay, client: &Client) {
        key(app, w, l, client, KeyCode::Char('O'));
        key(app, w, l, client, KeyCode::Char('n'));
        paste(app, w, l, client, "Ship authentication"); key(app, w, l, client, KeyCode::Enter);
        paste(app, w, l, client, "Anonymous access is rejected\nExisting sessions still work"); key(app, w, l, client, KeyCode::Enter);
        key(app, w, l, client, KeyCode::Char('s')); key(app, w, l, client, KeyCode::Enter);
        paste(app, w, l, client, "Scout auth routes and propose a plan"); key(app, w, l, client, KeyCode::Enter);
    }

    #[test]
    fn campaign_opens_waiting_ship_and_returns_without_sending_a_reply() {
        let (mut app, w, l) = fixture(); let (client, commands) = Client::recording(); plan(&mut app, &w, &l, &client);
        for (id, agent, cwd) in [(8, 1, "/world/app"), (9, 2, "/moons/auth")] {
            app.campaigns.link(1, 1, &Mission { id, agent: Some(agent), cwd: cwd.into(), state: "active".into(), at: id as u64, ..Default::default() }, 10).unwrap();
        }
        app.agents[0].status = Status::Blocked;
        app.agents[0].ask = Some(Ask { question: "Which storage format?".into(), ..Default::default() });
        key(&mut app, &w, &l, &client, KeyCode::Char('o'));
        assert!(matches!(app.mode, Mode::Chat { id: 1, .. }), "Open the waiting ship before another linked ship");
        key(&mut app, &w, &l, &client, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Campaigns));
        assert_eq!(app.campaign_stage().unwrap().id, 1);
        key(&mut app, &w, &l, &client, KeyCode::Char('o'));
        key(&mut app, &w, &l, &client, KeyCode::Tab);
        assert!(matches!(app.mode, Mode::Comms(1)));
        key(&mut app, &w, &l, &client, KeyCode::F(12));
        assert!(matches!(app.mode, Mode::Campaigns));
        assert!(commands.try_iter().all(|c| matches!(c, Cmd::Seen(_) | Cmd::Resize { .. })));
        app.campaigns.campaigns[0].stages[0].missions[0].available = false;
        key(&mut app, &w, &l, &client, KeyCode::Char('o'));
        assert!(matches!(app.mode, Mode::Chat { id: 2, .. }), "An unavailable snapshot must not point to a reused ship id");
        key(&mut app, &w, &l, &client, KeyCode::Esc);
        for a in &mut app.agents { a.status = Status::Exited; }
        key(&mut app, &w, &l, &client, KeyCode::Char('o'));
        assert!(matches!(app.mode, Mode::Campaigns));
        assert!(app.campaign_view.error.contains("No live ship"));
    }

    #[test]
    fn campaign_summary_shows_question_without_repeating_the_saved_prompt() {
        let (mut app, w, _) = fixture(); let client = Client::isolated();
        let mut fb = Fb::new(120, 60); let l = Lay::new(&fb, false); plan(&mut app, &w, &l, &client);
        app.campaigns.link(1, 1, &Mission { id: 8, agent: Some(1), cwd: "/world/app".into(), state: "active".into(), at: 8,
            text: "ARCHIVED PROMPT SENTINEL".into(), ..Default::default() }, 10).unwrap();
        app.agents[0].status = Status::Blocked;
        app.agents[0].ask = Some(Ask { question: "Which storage format?".into(), ..Default::default() });
        draw::frame(&mut fb, &mut app, &w, &l, 1.0, None);
        let text: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
        assert!(text.contains("Waiting for your input") && text.contains("Which storage format?"));
        assert!(text.contains("o open ship") && text.contains("Objective") && text.contains("Acceptance criteria"));
        assert!(!text.contains("ARCHIVED PROMPT SENTINEL") && !text.contains("Campaign history"));
        key(&mut app, &w, &l, &client, KeyCode::Char('d'));
        app.campaign_view.scroll = usize::MAX;
        draw::frame(&mut fb, &mut app, &w, &l, 1.0, None);
        let text: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
        assert!(text.contains("ARCHIVED PROMPT SENTINEL") && text.contains("Campaign history"));
    }

    #[test]
    fn campaign_planning_and_cancel_do_not_send_orders_and_submission_keeps_metadata() {
        let (mut app, w, l) = fixture(); let (client, commands) = Client::recording();
        plan(&mut app, &w, &l, &client);
        assert_eq!(app.campaigns.campaigns[0].criteria, "Anonymous access is rejected\nExisting sessions still work");
        assert_eq!(app.campaigns.campaigns[0].stages[0].world, "/world/app");
        assert!(commands.try_recv().is_err());
        key(&mut app, &w, &l, &client, KeyCode::Char('m'));
        assert!(matches!(&app.mode, Mode::Brief { text, launch: false, .. } if text.s.contains("Acceptance criteria") && text.s.contains("Scout auth routes")));
        key(&mut app, &w, &l, &client, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Campaigns));
        assert!(app.campaign_draft.is_none());
        assert!(commands.try_recv().is_err());
        key(&mut app, &w, &l, &client, KeyCode::Char('m'));
        key(&mut app, &w, &l, &client, KeyCode::Enter);
        assert!(matches!(commands.try_recv().unwrap(), Cmd::Mission { cwd, text, launch: false, campaign: Some(crate::proto::CampaignRef { campaign: 1, stage: 1 }), .. }
            if cwd == "/world/app" && text.contains("Anonymous access is rejected")));
        assert!(matches!(app.mode, Mode::Brief { sending: true, .. }));
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn existing_missions_need_handoff_verification_and_explicit_campaign_acceptance() {
        let (mut app, w, l) = fixture(); let (client, commands) = Client::recording(); plan(&mut app, &w, &l, &client);
        app.missions.push(Mission { id: 9, cwd: "/moons/auth".into(), text: "Inspect auth".into(), state: "done".into(), at: 10, ..Default::default() });
        key(&mut app, &w, &l, &client, KeyCode::Char('l')); key(&mut app, &w, &l, &client, KeyCode::Enter);
        assert_eq!(app.campaign_stage().unwrap().status(), "reported finished");
        key(&mut app, &w, &l, &client, KeyCode::Char('a'));
        assert!(app.campaign_view.error.contains("handoff"));
        for (action, text) in [('h', "Found an unguarded route. Open question: cookie expiry."), ('v', "Inspected the route and reproduced the missing-session test")] {
            key(&mut app, &w, &l, &client, KeyCode::Char(action)); paste(&mut app, &w, &l, &client, text); key(&mut app, &w, &l, &client, KeyCode::Enter);
        }
        assert_eq!(app.campaign_stage().unwrap().status(), "verified");
        key(&mut app, &w, &l, &client, KeyCode::Char('a'));
        assert_eq!(app.campaign_stage().unwrap().status(), "accepted");
        assert!(app.selected_campaign().unwrap().completed.is_none());
        key(&mut app, &w, &l, &client, KeyCode::Char('c')); key(&mut app, &w, &l, &client, KeyCode::Char('n'));
        assert!(app.selected_campaign().unwrap().completed.is_none());
        key(&mut app, &w, &l, &client, KeyCode::Char('c')); key(&mut app, &w, &l, &client, KeyCode::Char('y'));
        assert!(app.selected_campaign().unwrap().completed.is_some());
        key(&mut app, &w, &l, &client, KeyCode::Char('x'));
        assert!(app.selected_campaign().unwrap().archived);
        assert_eq!(app.campaign_stage().unwrap().missions[0].mission.cwd, "/moons/auth");
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn changed_handoff_blocks_a_stale_draft_without_losing_the_prompt() {
        let (mut app, w, l) = fixture(); let (client, commands) = Client::recording(); plan(&mut app, &w, &l, &client);
        key(&mut app, &w, &l, &client, KeyCode::Char('m'));
        app.campaigns.update(1, "Ship revised authentication", "Expired sessions also fail", 100).unwrap();
        key(&mut app, &w, &l, &client, KeyCode::Enter);
        assert!(commands.try_recv().is_err());
        assert!(matches!(&app.mode, Mode::Brief { text, sending: false, .. } if text.s.contains("Anonymous access is rejected")));
        assert!(app.toast.as_ref().unwrap().0.contains("Campaign changed"));
    }

    #[test]
    fn campaign_history_scrolls_and_mouse_selects_visible_stage_rows() {
        let (mut app, w, l) = fixture(); let client = Client::isolated(); plan(&mut app, &w, &l, &client);
        for i in 1..30 { app.campaigns.add_stage(1, "/world/app", &format!("Stage {i}"), i).unwrap(); }
        key(&mut app, &w, &l, &client, KeyCode::End);
        let (x, y, _, _, left, room) = draw::campaign_layout(&app, &l);
        assert_eq!(draw::campaign_hit(&app, &w, &l, x + left + 2, y + 4), Some((true, 30 - room)));
        handle(&mut app, &w, &l, &client, None, Event::Mouse(event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left), column: (x + left + 2) as u16, row: (y + 4) as u16, modifiers: KeyModifiers::NONE,
        }), 1.0, &mut None);
        assert_eq!(app.campaign_view.stage, (30 - room + 1) as u64);
        app.campaign_view.stage = 1;
        app.campaigns.handoff(1, 1, &format!("{}END OF HANDOFF", "finding\n".repeat(40)), 40).unwrap();
        for (cols, rows) in [(80, 24), (120, 42)] {
            let mut fb = Fb::new(cols, rows); let l = Lay::new(&fb, false);
            draw::frame(&mut fb, &mut app, &w, &l, 1.0, None);
            let text: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
            assert!(text.contains("CAMPAIGNS") && text.contains("h handoff") && text.contains("n new"));
            app.campaign_view.scroll = usize::MAX;
            draw::frame(&mut fb, &mut app, &w, &l, 1.0, None);
            let text: String = fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect();
            assert!(text.contains("END OF HANDOFF"));
        }
    }
}

#[cfg(test)]
mod design_tests {
    use super::*;
    use crate::design::Design;

    fn press(app: &mut App, w: &World, l: &Lay, client: &Client, code: KeyCode, m: KeyModifiers) {
        handle(app, w, l, client, None, Event::Key(KeyEvent::new(code, m)), 1.0, &mut None);
    }
    fn typed(app: &mut App, w: &World, l: &Lay, client: &Client, s: &str) {
        for c in s.chars() { press(app, w, l, client, KeyCode::Char(c), KeyModifiers::NONE); }
    }
    fn screen(app: &mut App, w: &World, l: &Lay) -> String {
        let mut fb = Fb::new(120, 40);
        draw::frame(&mut fb, app, w, l, 1.0, None);
        fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect()
    }
    /// A world with one Claude skill (which Codex can't see), and a composer open on it.
    fn setup(name: &str) -> (App, World, Lay, PathBuf) {
        let dir = std::env::temp_dir().join(format!("empire-design-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude/skills/empire-test-release")).unwrap();
        std::fs::write(dir.join(".claude/skills/empire-test-release/SKILL.md"), "---\nname: empire-test-release\n---").unwrap();
        let mut app = App::new();
        app.kinds = vec!["claude".into(), "codex".into()];
        app.mode = Mode::Brief { planet: dir.to_string_lossy().into(), text: Line::default(), kind: 0, launch: false, role: 0, to: None, attachments: vec![], sending: false, ship: Outfit::default() };
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        (app, world, Lay::new(&Fb::new(120, 40), false), dir)
    }

    #[test]
    fn a_design_outfits_the_new_ship_and_can_be_adjusted_before_launch() {
        let (mut app, w, l, dir) = setup("pick");
        let (client, commands) = Client::recording();
        let ctrl = KeyModifiers::CONTROL;
        let id = app.designs.save(Design { name: "Frigate".into(), kind: "codex".into(), role: "builder".into(), orders: "Keep diffs small.".into(), skills: vec!["empire-test-release".into()], ..Default::default() }).unwrap();
        press(&mut app, &w, &l, &client, KeyCode::Char('d'), ctrl);
        assert!(matches!(&app.mode, Mode::Brief { kind: 1, launch: true, role, ship, .. } if ROLES[*role] == Role::Builder && ship.design == Some(id) && ship.orders == "Keep diffs small."));
        // inspect: the design, its orders, and a skill Codex can't find from here
        let s = screen(&mut app, &w, &l);
        assert!(s.contains("Frigate") && s.contains("“Keep diffs small.”") && s.contains("✗empire-test-release"), "{s}");
        // switch species: Claude finds it
        press(&mut app, &w, &l, &client, KeyCode::Up, KeyModifiers::NONE);
        assert!(screen(&mut app, &w, &l).contains("✓empire-test-release"));
        // refit for this launch only
        press(&mut app, &w, &l, &client, KeyCode::Char('r'), ctrl);
        assert!(matches!(app.mode, Mode::Refit(_)));
        typed(&mut app, &w, &l, &client, " Run the tests.");
        press(&mut app, &w, &l, &client, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(&app.mode, Mode::Brief { ship, .. } if ship.orders == "Keep diffs small. Run the tests."));
        assert_eq!(app.designs.get(id).unwrap().orders, "Keep diffs small.");
        typed(&mut app, &w, &l, &client, "Fix the bug");
        press(&mut app, &w, &l, &client, KeyCode::Enter, KeyModifiers::NONE);
        match commands.try_recv().unwrap() {
            Cmd::Mission { kind, launch, role, spec, text, .. } => {
                assert_eq!((kind.as_str(), launch, role.as_str(), text.as_str()), ("claude", true, "builder", "Fix the bug"));
                assert_eq!(spec, ShipSpec { orders: "Keep diffs small. Run the tests.".into(), skills: vec!["empire-test-release".into()], design: "Frigate".into() });
            }
            _ => panic!("expected a mission"),
        }
        // the design cycle ends back at none
        let (mut app2, w2, l2, _) = setup("pick2");
        app2.designs = app.designs.clone();
        press(&mut app2, &w2, &l2, &client, KeyCode::Char('d'), ctrl);
        press(&mut app2, &w2, &l2, &client, KeyCode::Char('d'), ctrl);
        assert!(matches!(&app2.mode, Mode::Brief { ship, .. } if *ship == Outfit::default()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("empire-design-pick2-{}", std::process::id())));
    }

    #[test]
    fn without_a_design_the_composer_launches_as_before() {
        let (mut app, w, l, dir) = setup("bare");
        let (client, commands) = Client::recording();
        assert!(screen(&mut app, &w, &l).contains("no orders or skills of its own"));
        press(&mut app, &w, &l, &client, KeyCode::Tab, KeyModifiers::NONE); // queue → launch
        typed(&mut app, &w, &l, &client, "Fix it");
        press(&mut app, &w, &l, &client, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(commands.try_recv().unwrap(), Cmd::Mission { role, spec, .. } if role.is_empty() && spec == ShipSpec::default()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refit_is_saved_to_the_shipyard_and_kept_across_restarts() {
        let (mut app, w, l, dir) = setup("yard");
        app.design_path = Some(dir.join("state/designs.json"));
        let (client, _commands) = Client::recording();
        let ctrl = KeyModifiers::CONTROL;
        press(&mut app, &w, &l, &client, KeyCode::BackTab, KeyModifiers::NONE); // scout
        press(&mut app, &w, &l, &client, KeyCode::Char('r'), ctrl);
        typed(&mut app, &w, &l, &client, "Only read docs.");
        press(&mut app, &w, &l, &client, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut app, &w, &l, &client, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(screen(&mut app, &w, &l).contains("[x] ✓ empire-test-release"));
        press(&mut app, &w, &l, &client, KeyCode::Char('s'), ctrl);
        typed(&mut app, &w, &l, &client, "Survey boat");
        press(&mut app, &w, &l, &client, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut app, &w, &l, &client, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(&app.mode, Mode::Brief { ship, launch: true, .. } if ship.design.is_some() && ship.skills == ["empire-test-release"]));
        // a fresh galaxy finds it in its shipyard
        let mut later = App::new();
        later.design_path = app.design_path.clone();
        press(&mut later, &w, &l, &client, KeyCode::Char('Y'), KeyModifiers::NONE);
        assert!(matches!(later.mode, Mode::Shipyard(_)));
        let d = later.designs.designs[0].clone();
        assert_eq!((d.name.as_str(), d.kind.as_str(), d.role.as_str(), d.orders.as_str()), ("Survey boat", "claude", "scout", "Only read docs."));
        assert!(screen(&mut later, &w, &l).contains("Survey boat"));
        // rename and scrap it
        press(&mut later, &w, &l, &client, KeyCode::Char('r'), KeyModifiers::NONE);
        press(&mut later, &w, &l, &client, KeyCode::Char('u'), ctrl);
        typed(&mut later, &w, &l, &client, "Tender");
        press(&mut later, &w, &l, &client, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(crate::design::edit(later.design_path.as_ref().unwrap(), |_| {}).unwrap().designs[0].name, "Tender");
        press(&mut later, &w, &l, &client, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(screen(&mut later, &w, &l).contains("Scrap Tender?"));
        press(&mut later, &w, &l, &client, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(crate::design::edit(later.design_path.as_ref().unwrap(), |_| {}).unwrap().designs.is_empty());
        // the composer that launched from it shows the design as scrapped, its outfit intact
        let _ = app.sync_designs(|_| Ok(0));
        assert!(matches!(&app.mode, Mode::Brief { ship, .. } if ship.orders == "Only read docs."));
        assert!(screen(&mut app, &w, &l).contains("scrapped"));
        // small terminals squeeze the dialogs without panicking
        for (cols, rows) in [(80, 24), (48, 14), (30, 8)] {
            let small = Lay::new(&Fb::new(cols, rows), false);
            for mode in [Mode::Shipyard(YardView::default()), Mode::Refit(RefitView { brief: Box::new(Mode::Galaxy), orders: Line::default(), skills: vec!["a".into(); 9], picked: vec![], sel: 4, naming: Some(Line::default()), error: "oops".into() })] {
                later.mode = mode;
                draw::frame(&mut Fb::new(cols, rows), &mut later, &w, &small, 1.0, None);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod ship_refit_tests {
    use super::*;
    use crate::design::Design;
    use crate::proto::RefitState;

    fn press(app: &mut App, w: &World, l: &Lay, client: &Client, code: KeyCode) {
        handle(app, w, l, client, None, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), 1.0, &mut None);
    }
    fn screen(app: &mut App, w: &World, l: &Lay) -> String {
        let mut fb = Fb::new(120, 40);
        draw::frame(&mut fb, app, w, l, 1.0, None);
        fb.txt.iter().map(|c| c.map_or(' ', |c| c.ch)).collect()
    }
    /// A deployed builder on a world with one Claude skill, and two designs in the shipyard.
    fn setup(name: &str, kind: &str) -> (App, World, Lay, PathBuf) {
        let dir = std::env::temp_dir().join(format!("empire-shiprefit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude/skills/empire-test-release")).unwrap();
        std::fs::write(dir.join(".claude/skills/empire-test-release/SKILL.md"), "---\nname: empire-test-release\n---").unwrap();
        let mut app = App::new();
        let mut a = crate::game::ship_card_tests::ship(1, false);
        a.kind = kind.into();
        a.cwd = dir.to_string_lossy().into();
        a.status = Status::Idle;
        a.role = Role::Builder;
        a.spec = ShipSpec { orders: "Old orders.".into(), skills: vec!["old-skill".into()], design: "Frigate".into() };
        a.design = "Frigate".into();
        app.agents = vec![a];
        app.designs.save(Design { name: "Warden".into(), kind: "codex".into(), role: "inspector".into(), orders: "Review first.".into(), skills: vec!["empire-test-release".into(), "missing-skill".into()], ..Default::default() }).unwrap();
        app.designs.save(Design { name: "Probe".into(), kind: "claude".into(), role: "scout".into(), ..Default::default() }).unwrap();
        app.mode = Mode::Ship { id: 1, sel: 0 };
        let world = World { systems: vec![], planets: vec![], lanes: vec![], station: None };
        (app, world, Lay::new(&Fb::new(120, 40), false), dir)
    }

    #[test]
    fn the_preview_shows_what_changes_and_nothing_is_sent_before_y() {
        let (mut app, w, l, dir) = setup("preview", "claude");
        let (client, commands) = Client::recording();
        press(&mut app, &w, &l, &client, KeyCode::Char('r'));
        assert!(matches!(app.mode, Mode::ShipRefit { id: 1, sel: 0, confirm: false }));
        let s = screen(&mut app, &w, &l);
        assert!(s.contains("REFIT · CLAUDE") && s.contains("aboard   builder · Frigate design"), "{s}");
        assert!(s.contains("builder → inspector"), "{s}");
        assert!(s.contains("− “Old orders.”") && s.contains("+ “Review first.”"), "{s}");
        assert!(s.contains("+✓empire-test-release") && s.contains("+✗missing-skill") && s.contains("−old-skill"), "{s}");
        assert!(s.contains("Restarts on its own session when idle") && s.contains("old outfit stays active"), "{s}");
        // a scout design: read-only gained, orders and skills dropped
        press(&mut app, &w, &l, &client, KeyCode::Down);
        let s = screen(&mut app, &w, &l);
        assert!(s.contains("builder → scout") && s.contains("gains a read-only launch") && s.contains("(dropped)"), "{s}");
        assert!(commands.try_recv().is_err(), "previewing sends nothing");
        // ⏎ asks first; anything but y backs out
        press(&mut app, &w, &l, &client, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::ShipRefit { confirm: true, sel: 1, .. }));
        assert!(screen(&mut app, &w, &l).contains("Refit as Probe? y refit when idle"));
        press(&mut app, &w, &l, &client, KeyCode::Char('n'));
        assert!(matches!(app.mode, Mode::ShipRefit { confirm: false, sel: 1, .. }));
        assert!(commands.try_recv().is_err());
        press(&mut app, &w, &l, &client, KeyCode::Enter);
        press(&mut app, &w, &l, &client, KeyCode::Char('y'));
        match commands.try_recv().unwrap() {
            Cmd::Refit { id, role, spec } => {
                assert_eq!((id, role.as_str()), (1, "scout"));
                assert_eq!(spec, ShipSpec { orders: String::new(), skills: vec![], design: "Probe".into() });
            }
            _ => panic!("expected a refit"),
        }
        assert!(commands.try_recv().is_err());
        assert!(matches!(app.mode, Mode::Ship { id: 1, .. }));
        // the ship still shows what's actually aboard until the daemon says otherwise
        assert_eq!(app.agents[0].role, Role::Builder);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_scout_refitted_to_a_builder_loses_read_only_and_the_same_outfit_is_no_refit() {
        let (mut app, w, l, dir) = setup("scout", "claude");
        let (client, commands) = Client::recording();
        app.agents[0].role = Role::Scout;
        app.agents[0].spec = ShipSpec { design: "Probe".into(), ..Default::default() };
        app.mode = Mode::ShipRefit { id: 1, sel: 0, confirm: false };
        assert!(screen(&mut app, &w, &l).contains("loses its read-only launch"));
        press(&mut app, &w, &l, &client, KeyCode::Down);
        press(&mut app, &w, &l, &client, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::ShipRefit { confirm: false, .. }), "already aboard: nothing to confirm");
        assert!(commands.try_recv().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c_calls_off_a_refit_and_unresumable_species_are_flagged() {
        let (mut app, w, l, dir) = setup("cancel", "cursor");
        let (client, commands) = Client::recording();
        app.mode = Mode::ShipRefit { id: 1, sel: 0, confirm: false };
        assert!(screen(&mut app, &w, &l).contains("cursor can't resume a session"));
        press(&mut app, &w, &l, &client, KeyCode::Char('c'));
        assert!(commands.try_recv().is_err(), "no refit to cancel");
        app.agents[0].refit = Some(RefitState { role: "inspector".into(), spec: ShipSpec { design: "Warden".into(), ..Default::default() }, state: "pending".into(), note: String::new() });
        assert!(screen(&mut app, &w, &l).contains("c cancel refit"));
        press(&mut app, &w, &l, &client, KeyCode::Char('c'));
        assert!(matches!(commands.try_recv().unwrap(), Cmd::CancelRefit(1)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_ship_card_shows_each_refit_state_apart_from_whats_aboard() {
        let (mut app, w, l, dir) = setup("card", "claude");
        assert!(!screen(&mut app, &w, &l).contains("refit   "));
        for (state, note, label) in [
            ("pending", "", "◌ PENDING Warden: restarts on its own session when idle"),
            ("applying", "", "⟳ APPLYING Warden"),
            ("active", "resumed session s", "✓ ACTIVE Warden aboard"),
            ("failed", "it shut down right after relaunch; relaunched with its old outfit", "✖ FAILED Warden: it shut down right after relaunch"),
            ("unsupported", "no session to resume yet", "⊘ UNSUPPORTED Warden: no session to resume yet"),
        ] {
            app.agents[0].refit = Some(RefitState { role: "inspector".into(), spec: ShipSpec { design: "Warden".into(), ..Default::default() }, state: state.into(), note: note.into() });
            let s = screen(&mut app, &w, &l);
            assert!(s.contains(label), "{state}: {s}");
            // the outfit row is what's actually running
            assert!(s.contains("outfit    builder · Frigate design · own orders · skills old-skill"), "{s}");
        }
        // the menu moved down a row and clicks still land on it
        let mut fb = Fb::new(120, 40);
        draw::frame(&mut fb, &mut app, &w, &l, 1.0, None);
        let rows: Vec<String> = fb.txt.chunks(fb.cols).map(|r| r.iter().map(|c| c.map_or(' ', |c| c.ch)).collect()).collect();
        let r = rows.iter().position(|r| r.contains("refit from a saved design")).unwrap();
        let c = rows[r].find("refit from").map(|b| rows[r][..b].chars().count()).unwrap() as i32;
        assert_eq!(draw::ship_hit(&app, 1, &l, c, r as i32), Some(Some(2)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_agent_list_carries_the_refit_and_reports_how_it_ended() {
        let (mut app, _w, _l, dir) = setup("apply", "claude");
        app.loaded = true;
        let cwd = app.agents[0].cwd.clone();
        let view = |state: &str, role: &str, design: &str| AgentView {
            id: 1, kind: "claude".into(), cwd: cwd.clone(), title: String::new(), task: String::new(), role: role.into(), status: "idle".into(), age: 0.0, turns: 0,
            report: Report::default(), ask: None, war: vec![], design: design.into(), spec: ShipSpec { design: design.into(), ..Default::default() },
            refit: Some(RefitState { role: "inspector".into(), spec: ShipSpec { design: "Warden".into(), ..Default::default() }, state: state.into(), note: String::new() }),
        };
        let mut cues = Vec::new();
        app.apply(vec![view("pending", "builder", "Frigate")], 1.0, &mut cues);
        assert_eq!((app.agents[0].role, app.agents[0].refit.as_ref().unwrap().state.as_str()), (Role::Builder, "pending"));
        app.apply(vec![view("active", "inspector", "Warden")], 2.0, &mut cues);
        assert_eq!((app.agents[0].role, app.agents[0].design.as_str()), (Role::Inspector, "Warden"));
        assert!(app.log.iter().any(|e| e.msg.contains("refitted: Warden aboard")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
