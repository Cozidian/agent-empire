//! Advisor (Provider's Advice model) suggests which ship should take a mission: one of the
//! ships already on the world, or a new one, and the role a new one should have. It only
//! suggests; the composer still sends nothing until you do.

use crate::role::{ROLES, Role};
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

const URL: &str = "https://api.provider.ai/v1/advice";
const NEW: &str = "new_ship";

/// A ship on the world, as Advisor sees it.
pub struct Ship {
    pub id: u32,
    pub kind: String,
    pub role: Role,
    pub status: String,
    pub task: String,
    pub turns: u32,
    pub ctx: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    /// The ship to give it to, or `None` for a new one.
    pub ship: Option<u32>,
    pub ship_conf: f32,
    pub role: Role,
    pub role_conf: f32,
}

/// Whether Advisor can be asked at all.
pub fn available() -> bool {
    std::env::var("PROVIDER_API_KEY").is_ok_and(|k| !k.is_empty())
}

fn ship_key(id: u32) -> String {
    format!("ship_{id}")
}

pub fn request(mission: &str, world: &str, ships: &[Ship]) -> Value {
    let describe = |s: &Ship| {
        let role = if s.role == Role::None { "no role".to_string() } else { format!("{} ({})", s.role.name(), s.role.about()) };
        let task = if s.task.is_empty() { "nothing yet".to_string() } else { s.task.clone() };
        json!({ "species": s.kind, "role": role, "status": s.status, "working_on": task, "turns": s.turns,
                "context_used": s.ctx.map_or("unknown".to_string(), |c| format!("{c:.0}%")) })
    };
    let mut fleet = serde_json::Map::new();
    for s in ships {
        fleet.insert(ship_key(s.id), describe(s));
    }
    let mut options = serde_json::Map::new();
    for s in ships {
        let role = if s.role == Role::None { String::new() } else { format!(" {}", s.role.name()) };
        options.insert(ship_key(s.id), json!(format!("give it to this {}{role}, now {}, working on: {}", s.kind, s.status, if s.task.is_empty() { "nothing yet" } else { &s.task })));
    }
    options.insert(NEW.into(), json!("launch a new ship for it, with fresh context"));
    let roles: serde_json::Map<String, Value> = ROLES.iter().map(|r| (r.label().to_string(), json!(r.about()))).collect();
    json!({
        "model": "advisor-latest",
        "state": { "mission": mission, "world": world, "ships": fleet },
        "questions": {
            "ship": {
                "type": "choice",
                "instructions": "Which ship should carry out `mission` on this world? Prefer a ship whose current work (`ships.*.working_on`) the mission continues or builds on, whose role fits the mission, and that is free (idle or done) rather than working or waiting on a question. Choose a new ship when the mission is unrelated to what the ships here are doing, when the fitting ship's context is nearly full, or when no ship's role fits.",
                "criteria": options,
            },
            "role": {
                "type": "choice",
                "instructions": "If a new ship were launched to carry out `mission`, which role fits the mission best? Investigating, exploring or planning without changing anything is a scout; making changes is a builder; testing and reviewing existing changes is an inspector; mixed or unclear work needs no role.",
                "criteria": roles,
            },
        },
    })
}

pub fn parse(v: &Value) -> Option<Pick> {
    let ship = &v["answers"]["ship"];
    let role = &v["answers"]["role"];
    let pick = ship["choice"].as_str()?;
    let id = if pick == NEW { None } else { Some(pick.strip_prefix("ship_")?.parse().ok()?) };
    Some(Pick {
        ship: id,
        ship_conf: ship["confidence"].as_f64().unwrap_or(0.0) as f32,
        role: ROLES.into_iter().find(|r| Some(r.label()) == role["choice"].as_str()).unwrap_or_default(),
        role_conf: role["confidence"].as_f64().unwrap_or(0.0) as f32,
    })
}

/// Ask Advisor (blocking: call it off the UI thread). The key goes to curl on stdin, not argv.
pub fn ask(mission: &str, world: &str, ships: &[Ship]) -> Result<Pick, String> {
    let key = std::env::var("PROVIDER_API_KEY").map_err(|_| "no PROVIDER_API_KEY")?;
    let body = request(mission, world, ships).to_string();
    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", "15", "-K", "-", "-H", "Content-Type: application/json", "--data-binary", &body, URL])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    child.stdin.take().ok_or("curl: no stdin")?.write_all(format!("header = \"Authorization: Bearer {key}\"\n").as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| "unreadable answer from Advisor")?;
    parse(&v).ok_or_else(|| v["detail"].to_string().chars().take(120).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_about_every_ship_and_reads_the_pick() {
        let ships = [Ship { id: 4, kind: "claude".into(), role: Role::Builder, status: "idle".into(), task: "Add roles".into(), turns: 3, ctx: Some(40.0) }];
        let req = request("Fix the role row", "empire", &ships);
        let opts = req["questions"]["ship"]["criteria"].as_object().unwrap();
        assert!(opts.contains_key("ship_4") && opts.contains_key(NEW));
        assert_eq!(req["state"]["ships"]["ship_4"]["working_on"], "Add roles");
        let answer = json!({ "answers": {
            "ship": { "type": "choice", "choice": "ship_4", "confidence": 0.9, "probabilities": { "ship_4": 0.95, "new_ship": 0.05 } },
            "role": { "type": "choice", "choice": "builder", "confidence": 0.7, "probabilities": {} },
        }});
        assert_eq!(parse(&answer), Some(Pick { ship: Some(4), ship_conf: 0.9, role: Role::Builder, role_conf: 0.7 }));
        let fresh = json!({ "answers": { "ship": { "choice": "new_ship", "confidence": 0.8 }, "role": { "choice": "scout", "confidence": 0.6 } } });
        assert_eq!(parse(&fresh).map(|p| (p.ship, p.role)), Some((None, Role::Scout)));
        assert_eq!(parse(&json!({ "detail": [] })), None);
    }

    /// Against the real API: `cargo test advisor_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn advisor_live() {
        let ships = [
            Ship { id: 4, kind: "claude".into(), role: Role::Builder, status: "idle".into(), task: "Add a role row to the mission composer".into(), turns: 6, ctx: Some(35.0) },
            Ship { id: 7, kind: "codex".into(), role: Role::None, status: "working".into(), task: "Rework the sound cues".into(), turns: 2, ctx: Some(20.0) },
        ];
        for mission in ["The role row in the mission composer overflows on narrow terminals; fix it", "Find out why the daemon leaks file descriptors after an upgrade and propose a fix, without changing anything", "Run the tests and review the last three commits"] {
            println!("{mission}\n  → {:?}", ask(mission, "agent-empire", &ships));
        }
    }
}
