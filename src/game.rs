//! The game: galaxy model, camera, input and the main loop.
//!
//! Folders are planets, their parent folders are star systems, and agents are
//! ships in orbit around the folder they work in.

use crate::client::{Client, Cmd, Msg};
use crate::draw;
use crate::gfx::*;
use crate::proto::{AgentView, Ask, Mission, Report, ScreenView};
use crate::survey;
use crate::sound::{Cue, Sound};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode,
    KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::{cursor, execute, style, terminal};
use std::collections::{BTreeMap, HashMap, VecDeque};
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
    pub wpos: (f32, f32),
    /// In front of its planet (drawn after it).
    pub front: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Sel {
    Sys(String),
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
            KeyCode::Char('w') if ctrl => {
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

pub enum Mode {
    Galaxy,
    Comms(u32),
    Colonize(Colonize),
    Deploy { planet: String, kind: usize },
    Confirm { id: u32 },
    /// Founding a moon (git worktree) off a world.
    Moon { planet: String, branch: Line, kind: usize },
    /// Merging a moon back into its world.
    Dock { moon: String },
    /// One order for every agent in a system or on a world.
    Fleet { target: Sel, text: Line },
    /// Writing a mission for a world: hand it to a new ship now, or queue it.
    Brief { planet: String, text: Line, kind: usize, launch: bool },
    Help,
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
    pub cam: Cam,
    pub fitted: bool,
    pub sys_pos: HashMap<String, (f32, f32)>,
    pub fx: Vec<Fx>,
    pub log: VecDeque<(String, String, V3)>,
    pub toast: Option<(String, f32)>,
    pub mode: Mode,
    pub kinds: Vec<String>,
    pub screen: Option<(u32, ScreenView)>,
    pub panel: bool,
    pub bg: Vec<V3>,
    pub bg_key: (usize, usize, i32),
    /// When the comms window was opened.
    pub comms_at: Instant,
    /// Lines scrolled back in the comms window (0 = live).
    pub scroll: usize,
    pub missions: Vec<Mission>,
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
    let mut v: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix) && (prefix.starts_with('.') || !n.starts_with('.')))
        .map(|n| format!("{dir}{n}/"))
        .collect();
    v.sort();
    v.truncate(8);
    v
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
    fn new() -> Self {
        Self {
            agents: Vec::new(),
            online: false,
            err: String::new(),
            loaded: false,
            sel: None,
            cam: Cam { x: 0.0, y: 0.0, z: 1.0, tx: 0.0, ty: 0.0, tz: 1.0 },
            fitted: false,
            sys_pos: HashMap::new(),
            fx: Vec::new(),
            log: VecDeque::new(),
            toast: None,
            mode: Mode::Galaxy,
            kinds: detect_kinds(),
            screen: None,
            panel: true,
            bg: Vec::new(),
            bg_key: (0, 0, 0),
            comms_at: Instant::now(),
            scroll: 0,
            pick: None,
            survey: survey::Snapshot::default(),
            missions: Vec::new(),
            click: ((-1, -1), Instant::now(), 0),
        }
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
            Sel::Agent(_) => Vec::new(),
        };
        planets.iter().flat_map(|p| p.agents.iter().map(|&i| &self.agents[i])).filter(|a| a.dying.is_none() && a.status != Status::Exited).collect()
    }

    /// New mission log from the daemon; announce what changed.
    fn track(&mut self, missions: Vec<Mission>) {
        if self.loaded && !self.missions.is_empty() {
            for m in &missions {
                let old = self.missions.iter().find(|o| o.id == m.id).map(|o| o.state.as_str());
                if old == Some(m.state.as_str()) {
                    continue;
                }
                let place = base_name(&m.cwd);
                let what = clip(&m.text, 40);
                match m.state.as_str() {
                    "sent" | "active" if !matches!(old, Some("sent") | Some("active")) => self.say(format!("➤ mission to {place}: {what}"), v3(0.6, 0.85, 1.0)),
                    "done" => self.say(format!("★ mission complete on {place}: {what}"), v3(1.0, 0.85, 0.4)),
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
                self.say(format!("★ fleet order complete · {} ships · {what}", all.len()), v3(1.0, 0.85, 0.4));
            }
        }
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

    fn say(&mut self, msg: String, col: V3) {
        self.log.push_front((clock(), msg, col));
        self.log.truncate(60);
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
            self.say(format!("◆ uplink established · {} agents on {worlds} worlds", list.len()), v3(0.6, 0.85, 1.0));
        }
        for v in &list {
            let st = Status::parse(&v.status);
            let title = clean_title(&v.title);
            if let Some(i) = self.agents.iter().position(|a| a.id == v.id && a.dying.is_none()) {
                let old = self.agents[i].status;
                if old != st {
                    let a = &self.agents[i];
                    let (kind, planet, title, pos) = (a.kind.clone(), base_name(&a.cwd), a.title.clone(), a.wpos);
                    // no sounds for the agent you're already talking to, or for quick little turns
                    let watching = matches!(self.mode, Mode::Comms(c) if c == v.id);
                    let quick = st == Status::Done && t - a.busy_at < SHORT_TURN;
                    let mut heard = Vec::new();
                    self.transition(old, st, &kind, &planet, &title, pos, t, &mut heard);
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
                if !title.is_empty() {
                    a.title = title;
                }
            } else {
                if !first {
                    if !matches!(self.mode, Mode::Comms(c) if c == v.id) {
                        cues.push(Cue::Spawn);
                    }
                    self.say(format!("✦ {} arrived at {}", v.kind, base_name(&v.cwd)), kind_color(&v.kind));
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
                    wpos: (0.0, 0.0),
                    front: true,
                });
            }
        }
        let mut gone = Vec::new();
        for a in &mut self.agents {
            if a.dying.is_none() && !list.iter().any(|v| v.id == a.id) {
                a.dying = Some(t);
                gone.push((a.kind.clone(), base_name(&a.cwd), a.status == Status::Exited));
            }
        }
        for (k, p, dark) in gone {
            if !first && !dark {
                cues.push(Cue::Gone);
                self.say(format!("✝ {k} left {p}"), s3(0.5));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn transition(&mut self, from: Status, to: Status, kind: &str, planet: &str, title: &str, (x, y): (f32, f32), t: f32, cues: &mut Vec<Cue>) {
        let ring = |col: V3, speed: f32| Fx::Ring { x, y, t0: t, col, speed, dur: 1.0 };
        match (from, to) {
            (_, Status::Blocked) => {
                cues.push(Cue::Blocked);
                self.fx.push(ring(to.color(), 40.0));
                self.say(format!("▲ {kind} on {planet} awaits orders"), to.color());
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
                self.say(format!("✔ {kind} finished on {planet}{what}"), to.color());
            }
            (_, Status::Exited) => {
                cues.push(Cue::Gone);
                self.say(format!("✝ {kind} on {planet} went dark"), s3(0.55));
            }
            (_, Status::Working) => self.fx.push(ring(to.color(), 20.0)),
            _ => {}
        }
    }

    fn tick(&mut self, t: f32, cues: &mut Vec<Cue>) {
        for a in &mut self.agents {
            let watching = matches!(self.mode, Mode::Comms(c) if c == a.id);
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
            let star = [v3(1.0, 0.86, 0.55), v3(0.7, 0.85, 1.0), v3(1.0, 0.62, 0.42), v3(1.0, 0.95, 0.85)][(sh % 4) as usize];
            let mut sys = System { key: skey.clone(), name: base_name(skey), wpos: spos, planets: Vec::new(), orbits: Vec::new(), star };
            for (j, pkey) in pkeys.iter().enumerate() {
                let h = hash(pkey);
                let seed = rnd(h);
                let orbit = ORBIT0 + ORBIT_STEP * j as f32;
                let ang = seed * TAU + t * 0.035 / (1.0 + 0.4 * j as f32);
                let mut members = pmap[pkey].clone();
                members.sort_by_key(|&i| self.agents[i].id);
                let live = members.iter().filter(|&&i| self.agents[i].dying.is_none()).count();
                let r = 10.0 + 2.2 * live.min(6) as f32;
                let wpos = (spos.0 + ang.cos() * orbit, spos.1 + ang.sin() * orbit * 0.55);
                let m = members.len().max(1) as f32;
                for (q, &ai) in members.iter().enumerate() {
                    let a = &mut self.agents[ai];
                    let ang = t * (0.45 + a.seed * 0.25) + q as f32 * TAU / m;
                    let orb = r + 8.0 + (q % 2) as f32 * 4.0;
                    a.wpos = (wpos.0 + ang.cos() * orb, wpos.1 + ang.sin() * orb * 0.55);
                    a.front = ang.sin() > 0.0;
                }
                sys.orbits.push(orbit);
                sys.planets.push(planets.len());
                planets.push(Planet { key: pkey.clone(), name: base_name(pkey), agents: members, terrain: (h >> 8) as u8 % 7, seed, wpos, r, host: None, moons: Vec::new() });
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
            let ang = seed * TAU + t * 0.18 / (1.0 + 0.5 * q);
            let wpos = (hpos.0 + ang.cos() * orbit, hpos.1 + ang.sin() * orbit * 0.55);
            let mut members = pmap[mkey].clone();
            members.sort_by_key(|&i| self.agents[i].id);
            let live = members.iter().filter(|&&i| self.agents[i].dying.is_none()).count();
            let r = 5.0 + 1.0 * live.min(4) as f32;
            let m = members.len().max(1) as f32;
            for (q, &ai) in members.iter().enumerate() {
                let a = &mut self.agents[ai];
                let ang = t * (0.6 + a.seed * 0.3) + q as f32 * TAU / m;
                let orb = r + 5.0;
                a.wpos = (wpos.0 + ang.cos() * orb, wpos.1 + ang.sin() * orb * 0.55);
                a.front = ang.sin() > 0.0;
            }
            let name = self.survey.worlds.get(mkey).map(|w| w.branch.clone()).filter(|b| !b.is_empty()).unwrap_or_else(|| base_name(mkey));
            let idx = planets.len();
            planets[hi].moons.push(idx);
            // moons are grey, cratered rock
            planets.push(Planet { key: mkey.clone(), name, agents: members, terrain: 3, seed, wpos, r, host: Some(hi), moons: Vec::new() });
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
        World { systems, planets, lanes }
    }

    /// Outliner order: systems, their planets, their agents (most urgent first).
    pub fn rows(&self, w: &World) -> Vec<Sel> {
        let mut v = Vec::new();
        for s in &w.systems {
            v.push(Sel::Sys(s.key.clone()));
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
            Sel::Sys(k) => w.systems.iter().find(|s| &s.key == k).map(|s| s.wpos),
            Sel::Planet(k) => w.planet(k).map(|p| p.wpos),
            Sel::Agent(id) => self.agent(*id).map(|a| a.wpos),
        }
    }

    /// The planet (folder) the selection belongs to.
    pub fn sel_planet(&self) -> Option<String> {
        match self.sel.as_ref()? {
            Sel::Planet(k) => Some(k.clone()),
            Sel::Agent(id) => self.agent(*id).map(|a| a.cwd.clone()),
            Sel::Sys(_) => None,
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

    fn open_comms(&mut self, id: u32, client: &Client, l: &Lay) {
        self.mode = Mode::Comms(id);
        self.comms_at = Instant::now();
        self.screen = None;
        self.scroll = 0;
        self.pick = None;
        client.watch(Some(id));
        client.send(Cmd::Resize(id, (l.cols - 2) as u16, (l.rows - 3) as u16));
        client.send(Cmd::Seen(id));
    }

    /// Click and drag in comms selects text, like a terminal: double-click a word,
    /// triple-click a line, and letting go copies it.
    fn select(&mut self, me: event::MouseEvent, l: &Lay, t: f32) {
        let (iw, ih) = (l.cols - 2, l.rows - 3);
        let cell = ((me.row as i32 - 2).clamp(0, ih - 1), (me.column as i32 - 1).clamp(0, iw - 1));
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
        let Some(p) = self.pick else { return };
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
        let text = lines.join("\n");
        if text.trim().is_empty() {
            return;
        }
        let n = text.chars().count();
        self.toast(if copy_to_clipboard(&text) { format!("copied {n} characters") } else { "✖ no clipboard (install wl-copy or xclip)".into() }, t);
    }

    /// Scroll the comms window back (positive) or toward live (negative).
    fn scroll_by(&mut self, lines: isize, client: &Client) {
        let max = self.screen.as_ref().map_or(0, |(_, s)| s.history);
        let next = (self.scroll as isize + lines).clamp(0, max as isize) as usize;
        if next != self.scroll {
            self.scroll = next;
            self.pick = None;
            client.scroll(next);
        }
    }

    fn close_comms(&mut self, client: &Client) {
        client.watch(None);
        self.screen = None;
        self.mode = Mode::Galaxy;
    }

    /// Enter on the current selection.
    fn activate(&mut self, w: &World, l: &Lay, client: &Client) {
        match self.sel.clone() {
            Some(Sel::Agent(id)) => self.open_comms(id, client, l),
            Some(Sel::Planet(k)) => {
                if let Some(id) = self.lead_agent(&k) {
                    self.sel = Some(Sel::Agent(id));
                    self.open_comms(id, client, l);
                }
            }
            Some(Sel::Sys(k)) => {
                if let Some(s) = w.systems.iter().find(|s| s.key == k) {
                    let r = s.orbits.last().copied().unwrap_or(40.0) + 40.0;
                    (self.cam.tx, self.cam.ty) = s.wpos;
                    self.cam.tz = (l.vw / (2.0 * r)).min(l.vh / (1.2 * r)).clamp(0.3, 4.0);
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
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        // wl-copy stays around to serve the clipboard; don't wait for it
        std::thread::spawn(move || child.wait());
        return true;
    }
    let _ = write!(io::stdout(), "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    true
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
    let sound = Sound::start();

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
        app.say(format!("✖ {e}"), v3(1.0, 0.4, 0.3));
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
                Msg::Agents(list, missions) => {
                    app.apply(list, t, &mut cues);
                    app.track(missions);
                }
                Msg::Down(e) => {
                    app.online = false;
                    app.err = e;
                }
                Msg::Screen(id, s) => {
                    if matches!(app.mode, Mode::Comms(c) if c == id) {
                        app.screen = Some((id, s));
                    }
                }
                Msg::Spawned { id, kind, cwd } => {
                    app.say(format!("◆ {kind} launched to {}", base_name(&cwd)), kind_color(&kind));
                    app.sel = Some(Sel::Agent(id));
                    if matches!(app.mode, Mode::Galaxy) {
                        app.open_comms(id, client, &Lay::new(&fb, app.panel));
                    }
                }
                Msg::Docked { moon, result } => {
                    let at = app.agents.iter().find(|a| a.cwd == moon).map_or((0.0, 0.0), |a| a.wpos);
                    let name = app.survey.worlds.get(&moon).map(|w| w.branch.clone()).unwrap_or_else(|| base_name(&moon));
                    let host = app.survey.worlds.get(&moon).and_then(|w| w.host.clone()).map(|h| base_name(&h)).unwrap_or_default();
                    match result {
                        Ok(()) => {
                            app.burst(at, v3(0.6, 0.85, 1.0), t, 48);
                            app.say(format!("⚓ moon {name} docked with {host}: its branch is merged"), v3(0.6, 0.85, 1.0));
                            app.toast(format!("⚓ {name} docked"), t);
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
                            app.say(format!("{} {name}: {e}", if crash { "💥" } else { "✖" }), v3(1.0, 0.4, 0.3));
                            app.toast(format!("docking failed: {e}"), t);
                        }
                    }
                }
                Msg::Error(e) => {
                    app.say(format!("✖ {e}"), v3(1.0, 0.4, 0.3));
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
        if !app.fitted && app.loaded {
            app.fit(&world, &lay);
            (app.cam.x, app.cam.y, app.cam.z) = (app.cam.tx, app.cam.ty, app.cam.tz);
            app.fitted = true;
        }
        if surveyed.elapsed() > Duration::from_millis(500) {
            survey.watch(world.planets.iter().map(|p| p.key.clone()).collect());
            app.survey = survey.snapshot();
            surveyed = Instant::now();
        }
        // a comms target that's gone for good drops you back to the galaxy
        // (a freshly launched agent takes a moment to show up in the list)
        if let Mode::Comms(id) = app.mode
            && !app.agents.iter().any(|a| a.id == id)
            && app.comms_at.elapsed() > Duration::from_secs(4)
        {
            app.close_comms(client);
        }

        while event::poll(Duration::ZERO)? {
            let ev = event::read()?;
            if let Event::Resize(c, r) = ev {
                fb = Fb::new(c as usize, r as usize);
                out = Out::new(fb.cols * fb.rows);
                app.bg_key = (0, 0, 0);
                execute!(o, terminal::Clear(terminal::ClearType::All))?;
                if let Mode::Comms(id) = app.mode {
                    client.send(Cmd::Resize(id, c.saturating_sub(2), r.saturating_sub(3)));
                }
                continue;
            }
            if handle(&mut app, &world, &lay, client, sound, ev, t, &mut drag) {
                if let Mode::Comms(_) = app.mode {
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

/// Handle one input event. Returns true to quit.
#[allow(clippy::too_many_arguments)]
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

    let Event::Key(k) = ev else {
        if let Event::Mouse(me) = ev {
            mouse(app, w, l, client, me, drag);
        }
        return false;
    };
    if k.kind == KeyEventKind::Release {
        return false;
    }
    let nk = app.kinds.len().max(1);

    // ── dialogs
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
                    let msg = format!("✖ decommissioned {} on {}", a.kind, base_name(&a.cwd));
                    app.say(msg, v3(1.0, 0.5, 0.35));
                }
            }
            app.mode = Mode::Galaxy;
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
                app.toast("⚓ docking…", t);
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
        Mode::Brief { planet, text, kind, launch } => {
            match k.code {
                KeyCode::Esc => app.mode = Mode::Galaxy,
                KeyCode::Up => *kind = (*kind + nk - 1) % nk,
                KeyCode::Down => *kind = (*kind + 1) % nk,
                KeyCode::Tab => *launch = !*launch,
                KeyCode::Enter if !text.s.trim().is_empty() => {
                    let cmd = Cmd::Mission { cwd: planet.clone(), text: text.s.trim().to_string(), kind: app.kinds[*kind].clone(), launch: *launch };
                    let msg = if *launch { "mission launched" } else { "mission queued" };
                    client.send(cmd);
                    app.mode = Mode::Galaxy;
                    app.toast(msg, t);
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
                KeyCode::Tab => {
                    if !c.comps.is_empty() {
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
                                app.say(format!("◆ charted new world {}", tilde(&path.to_string_lossy())), v3(0.6, 0.9, 1.0));
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
        Mode::Galaxy | Mode::Comms(_) => {}
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
        KeyCode::Char('c') if ctrl => return true,
        KeyCode::Esc => app.sel = None,
        KeyCode::Down | KeyCode::Char('j') => pick(app, cur.map_or(0, |c| (c + 1).min(rows.len().saturating_sub(1)))),
        KeyCode::Up | KeyCode::Char('k') => pick(app, cur.map_or(0, |c| c.saturating_sub(1))),
        KeyCode::Home | KeyCode::Char('g') => pick(app, 0),
        KeyCode::End | KeyCode::Char('G') => pick(app, rows.len().saturating_sub(1)),
        // the transmission on screen takes Enter, unless you've picked another agent
        KeyCode::Enter
            if let Some(id) = app.transmission().map(|a| a.id)
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
            let (id, kind, planet) = (a.id, a.kind.clone(), base_name(&a.cwd));
            let opts = a.ask.as_ref().map(|q| q.options.clone()).unwrap_or_default();
            if let Some(o) = opts.get(d as usize - '1' as usize) {
                client.send(Cmd::Answer(id, o.key.clone()));
                app.say(format!("➤ {kind} on {planet}: {}", o.label), v3(0.6, 0.85, 1.0));
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
        KeyCode::Char('t') => match app.sel_planet() {
            Some(planet) => {
                // queue it if someone's there to take it, otherwise send a ship
                let crewed = app.agents.iter().any(|a| a.cwd == planet && a.dying.is_none());
                app.mode = Mode::Brief { planet, text: Line::default(), kind: 0, launch: !crewed };
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
        KeyCode::Char('+') | KeyCode::Char('=') => app.cam.tz = (app.cam.tz * 1.3).min(6.0),
        KeyCode::Char('-') | KeyCode::Char('_') => app.cam.tz = (app.cam.tz / 1.3).max(0.12),
        KeyCode::Char('z') => app.fit(w, l),
        KeyCode::Char('f') => {
            app.glide_to_sel(w);
            app.cam.tz = 2.4;
        }
        KeyCode::Char('o') => {
            app.panel = !app.panel;
            app.bg_key = (0, 0, 0);
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

fn mouse(app: &mut App, w: &World, l: &Lay, client: &Client, me: event::MouseEvent, drag: &mut Option<((u16, u16), (f32, f32), bool)>) {
    let (mc, mr) = (me.column, me.row);
    let px = (mc as f32 * 2.0 + 1.0, mr as f32 * 4.0 + 2.0);
    let in_panel = l.pw > 0 && mc as i32 >= l.cols - l.pw;
    match me.kind {
        MouseEventKind::ScrollUp if !in_panel => app.zoom_at(l, 1.15, px),
        MouseEventKind::ScrollDown if !in_panel => app.zoom_at(l, 1.0 / 1.15, px),
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
        MouseEventKind::Up(MouseButton::Left) => {
            let was_drag = drag.take().is_some_and(|d| d.2);
            if was_drag {
                return;
            }
            let hit = if in_panel { draw::panel_hit(app, w, l, mr as i32) } else { pick_at(app, w, l, px) };
            if let Some(h) = hit {
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

/// The object under a screen pixel: ships first, then planets, then stars.
fn pick_at(app: &App, w: &World, l: &Lay, (px, py): (f32, f32)) -> Option<Sel> {
    let z = app.cam.z;
    let dist = |p: (f32, f32)| {
        let (sx, sy) = l.to_screen(&app.cam, p);
        ((sx - px).powi(2) + (sy - py).powi(2)).sqrt()
    };
    let nearest = |c: Vec<(f32, f32, Sel)>| c.into_iter().filter(|(d, r, _)| d < r).min_by(|a, b| a.0.total_cmp(&b.0)).map(|c| c.2);
    let ships = app.agents.iter().filter(|a| a.dying.is_none()).map(|a| (dist(a.wpos), 6.0, Sel::Agent(a.id))).collect();
    let planets = w.planets.iter().map(|p| (dist(p.wpos), (p.r * z).max(4.0) + 3.0, Sel::Planet(p.key.clone()))).collect();
    let stars = w.systems.iter().map(|s| (dist(s.wpos), (9.0 * z).max(4.0) + 4.0, Sel::Sys(s.key.clone()))).collect();
    nearest(ships).or_else(|| nearest(planets)).or_else(|| nearest(stars))
}
