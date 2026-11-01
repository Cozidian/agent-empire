//! Campaigns carry objectives and reviewed handoffs between missions.

use crate::proto::{CampaignRef, Mission};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note { pub at: u64, pub kind: String, pub text: String }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Linked { pub mission: Mission, pub available: bool }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    pub id: u64, pub world: String, pub task: String,
    pub missions: Vec<Linked>, pub handoff: String, pub verification: String,
    pub accepted: Option<u64>, pub history: Vec<Note>,
}

impl Stage {
    pub fn status(&self) -> &'static str {
        if self.accepted.is_some() { "accepted" }
        else if !self.verification.is_empty() { "verified" }
        else if self.missions.is_empty() { "planned" }
        else if self.missions.iter().any(|m| m.available && m.mission.state != "done") { "under way" }
        else if self.missions.iter().any(|m| !m.available && m.mission.state != "done") { "mission unavailable" }
        else { "reported finished" }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Campaign {
    pub id: u64, pub objective: String, pub criteria: String, pub stages: Vec<Stage>,
    pub completed: Option<u64>, pub archived: bool, pub history: Vec<Note>,
    next_stage: u64,
}

impl Campaign {
    pub fn ready(&self, stage: u64) -> Result<usize, String> {
        if self.archived || self.completed.is_some() { return Err("Reopen this campaign before continuing".into()); }
        let i = self.stages.iter().position(|s| s.id == stage).ok_or("This stage is no longer available")?;
        if self.stages[..i].iter().any(|s| s.accepted.is_none()) { return Err("Accept the earlier stages before this handoff".into()); }
        if self.stages[i].accepted.is_some() { return Err("Reopen this stage before changing its outcome".into()); }
        Ok(i)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    #[serde(default)]
    next: u64,
    pub campaigns: Vec<Campaign>,
}

impl Book {
    fn campaign_mut(&mut self, id: u64) -> Result<&mut Campaign, String> {
        self.campaigns.iter_mut().find(|c| c.id == id).ok_or_else(|| "This campaign is no longer available".into())
    }

    pub fn create(&mut self, objective: &str, criteria: &str, at: u64) -> Result<u64, String> {
        let objective = checked(objective, 120, false)?;
        let criteria = checked(criteria, 8000, true)?;
        let id = self.next.max(self.campaigns.iter().map(|c| c.id).max().unwrap_or(0)).checked_add(1).ok_or("No campaign ids left")?;
        self.next = id;
        self.campaigns.push(Campaign { id, objective, criteria, stages: vec![], completed: None, archived: false, next_stage: 0,
            history: vec![note(at, "created", "Campaign created; all handoffs are manual")] });
        Ok(id)
    }

    pub fn update(&mut self, id: u64, objective: &str, criteria: &str, at: u64) -> Result<(), String> {
        let (objective, criteria) = (checked(objective, 120, false)?, checked(criteria, 8000, true)?);
        let c = self.campaign_mut(id)?;
        if c.archived { return Err("Restore this campaign before editing it".into()); }
        if c.objective != objective || c.criteria != criteria {
            c.history.push(note(at, "previous objective", &format!("{}\nAcceptance criteria:\n{}", c.objective, c.criteria)));
            c.objective = objective; c.criteria = criteria;
            invalidate(c, 0, at, "Objective or acceptance criteria changed");
        }
        Ok(())
    }

    pub fn add_stage(&mut self, id: u64, world: &str, task: &str, at: u64) -> Result<u64, String> {
        if !std::path::Path::new(world).is_absolute() || world.chars().any(char::is_control) { return Err("Choose an absolute world path".into()); }
        let task = checked(task, 8000, true)?;
        let c = self.campaign_mut(id)?;
        if c.archived || c.completed.is_some() { return Err("Reopen this campaign before adding a stage".into()); }
        let stage = c.next_stage.max(c.stages.iter().map(|s| s.id).max().unwrap_or(0)).checked_add(1).ok_or("No stage ids left")?;
        c.next_stage = stage;
        c.stages.push(Stage { id: stage, world: world.into(), task, missions: vec![], handoff: String::new(), verification: String::new(), accepted: None,
            history: vec![note(at, "planned", "Stage planned; no mission sent")] });
        Ok(stage)
    }

    pub fn update_stage(&mut self, id: u64, stage: u64, world: &str, task: &str, at: u64) -> Result<(), String> {
        if !std::path::Path::new(world).is_absolute() || world.chars().any(char::is_control) { return Err("Choose an absolute world path".into()); }
        let task = checked(task, 8000, true)?;
        let c = self.campaign_mut(id)?;
        if c.archived { return Err("Restore the campaign before editing its stages".into()); }
        let i = c.stages.iter().position(|s| s.id == stage).ok_or("This stage is no longer available")?;
        if c.stages[i].world != world || c.stages[i].task != task {
            let old = format!("{}: {}", c.stages[i].world, c.stages[i].task);
            c.stages[i].history.push(note(at, "previous stage", &old));
            invalidate(c, i, at, "Stage changed; review affected handoffs again");
            c.stages[i].world = world.into(); c.stages[i].task = task;
        }
        Ok(())
    }

    pub fn link(&mut self, id: u64, stage: u64, mission: &Mission, at: u64) -> Result<(), String> {
        let c = self.campaign_mut(id)?;
        let i = c.ready(stage)?;
        if mission.campaign.is_some_and(|r| r.campaign == id && r.stage != stage) { return Err("This mission belongs to another stage of the campaign".into()); }
        if c.stages.iter().any(|s| s.missions.iter().any(|m| same(&m.mission, mission))) { return Err("This mission is already linked to the campaign".into()); }
        invalidate(c, i, at, "Another mission was linked; review the new outcome");
        c.stages[i].missions.push(Linked { mission: mission.clone(), available: true });
        c.stages[i].history.push(note(at, "mission linked", &format!("Mission #{} on {}", mission.id, mission.cwd)));
        Ok(())
    }

    pub fn handoff(&mut self, id: u64, stage: u64, text: &str, at: u64) -> Result<(), String> {
        let text = checked(text, 8000, true)?;
        let c = self.campaign_mut(id)?; let i = c.ready(stage)?;
        if c.stages[i].handoff != text {
            invalidate(c, i, at, "Handoff updated; verification needs review");
            c.stages[i].handoff = text.clone();
            c.stages[i].history.push(note(at, "handoff", &text));
        }
        Ok(())
    }

    pub fn verify(&mut self, id: u64, stage: u64, text: &str, at: u64) -> Result<(), String> {
        let text = checked(text, 8000, true)?;
        let c = self.campaign_mut(id)?; let i = c.ready(stage)?;
        let s = &mut c.stages[i];
        reviewable(s)?;
        if s.handoff.is_empty() { return Err("Record findings and unresolved questions in the handoff first".into()); }
        s.verification = text.clone(); s.history.push(note(at, "verified by player", &text));
        Ok(())
    }

    pub fn accept(&mut self, id: u64, stage: u64, at: u64) -> Result<(), String> {
        let c = self.campaign_mut(id)?; let i = c.ready(stage)?;
        let s = &mut c.stages[i];
        reviewable(s)?;
        if s.verification.is_empty() || s.handoff.is_empty() { return Err("Record a handoff and verification evidence before accepting".into()); }
        s.accepted = Some(at); s.history.push(note(at, "accepted by player", "Stage accepted; the next handoff is available"));
        Ok(())
    }

    pub fn complete(&mut self, id: u64, at: u64) -> Result<(), String> {
        let c = self.campaign_mut(id)?;
        if c.archived { return Err("Restore the campaign first".into()); }
        if c.stages.is_empty() || c.stages.iter().any(|s| s.accepted.is_none()) { return Err("Accept every stage before completing the campaign".into()); }
        c.completed = Some(at); c.history.push(note(at, "campaign accepted", "Player reviewed the objective and acceptance criteria"));
        Ok(())
    }

    pub fn reopen(&mut self, id: u64, stage: u64, at: u64) -> Result<(), String> {
        let c = self.campaign_mut(id)?;
        let i = c.stages.iter().position(|s| s.id == stage).ok_or("Choose a stage to reopen")?;
        c.archived = false;
        invalidate(c, i, at, "Player reopened this stage and subsequent handoffs");
        Ok(())
    }

    pub fn archive(&mut self, id: u64, at: u64) -> Result<(), String> {
        let c = self.campaign_mut(id)?; c.archived = !c.archived;
        c.history.push(note(at, if c.archived { "archived" } else { "restored" }, "Missions and agents are unchanged"));
        Ok(())
    }

    /// Keep evidence after missions leave the daemon's log. No transition is auto-accepted.
    pub fn refresh(&mut self, missions: &[Mission], at: u64) {
        for c in &mut self.campaigns {
            let mut changed = None;
            for (i, s) in c.stages.iter_mut().enumerate() {
                for m in missions.iter().filter(|m| m.campaign == Some(CampaignRef { campaign: c.id, stage: s.id })) {
                    if !s.missions.iter().any(|l| same(&l.mission, m)) {
                        s.missions.push(Linked { mission: m.clone(), available: true });
                        s.history.push(note(at, "mission linked", &format!("Mission #{} on {}", m.id, m.cwd)));
                        changed = Some(changed.map_or(i, |j: usize| j.min(i)));
                    }
                }
                for linked in &mut s.missions {
                    let fresh = missions.iter().find(|m| same(m, &linked.mission));
                    linked.available = fresh.is_some();
                    if let Some(m) = fresh {
                        if linked.mission.state == "done" && m.state != "done" { changed = Some(changed.map_or(i, |j: usize| j.min(i))); }
                        linked.mission = m.clone();
                    }
                }
            }
            if let Some(i) = changed { invalidate(c, i, at, "Mission evidence changed; review affected handoffs again"); }
        }
    }

    pub fn brief(&self, id: u64, stage: u64) -> Result<String, String> {
        let c = self.campaigns.iter().find(|c| c.id == id).ok_or("This campaign is no longer available")?;
        let i = c.ready(stage)?; let s = &c.stages[i];
        if s.missions.iter().any(|m| m.available && m.mission.state != "done") { return Err("This stage already has a mission under way; inspect it before sending another".into()); }
        let mut text = format!("Campaign: {}\n\nAcceptance criteria:\n{}\n\nCurrent stage on {}:\n{}", c.objective, c.criteria, s.world, s.task);
        for previous in &c.stages[..i] {
            text.push_str(&format!("\n\nAccepted handoff from {}:\n{}\nVerification recorded by the player:\n{}", previous.world, previous.handoff, previous.verification));
        }
        if !s.handoff.is_empty() { text.push_str(&format!("\n\nCurrent findings and unresolved questions:\n{}", s.handoff)); }
        text.push_str("\n\nReport findings, changes, verification evidence, and unresolved questions for the handoff. Campaign acceptance and the next mission are handled by the player.");
        Ok(text)
    }
}

fn checked(text: &str, max: usize, multiline: bool) -> Result<String, String> {
    if text.chars().any(|c| c.is_control() && !(multiline && c == '\n')) { return Err("Use plain text without control characters".into()); }
    let text = text.trim();
    if text.is_empty() || text.chars().count() > max { return Err(format!("Enter 1–{max} characters")); }
    Ok(text.into())
}
fn same(a: &Mission, b: &Mission) -> bool { a.id == b.id && a.at == b.at }
fn note(at: u64, kind: &str, text: &str) -> Note { Note { at, kind: kind.into(), text: text.into() } }
fn reviewable(s: &Stage) -> Result<(), String> {
    if s.missions.is_empty() { return Err("Link a mission before reviewing the outcome".into()); }
    if s.missions.iter().any(|m| m.available && m.mission.state != "done") { return Err("A linked mission is still under way".into()); }
    Ok(())
}
fn invalidate(c: &mut Campaign, i: usize, at: u64, why: &str) {
    c.completed = None;
    for s in c.stages.iter_mut().skip(i) {
        if s.accepted.is_some() || !s.verification.is_empty() { s.history.push(note(at, "review reopened", why)); }
        s.accepted = None; s.verification.clear();
    }
}

pub fn path() -> std::path::PathBuf { crate::daemon::dirs_state().join("campaigns.json") }
pub fn edit(path: &std::path::Path, change: impl FnOnce(&mut Book)) -> std::io::Result<Book> { crate::state::edit(path, change) }

#[cfg(test)]
mod tests {
    use super::*;
    fn done() -> Mission { Mission { id: 7, cwd: "/world".into(), text: "Inspect auth".into(), state: "done".into(), at: 10, ..Default::default() } }

    #[test]
    fn finishing_a_mission_does_not_verify_accept_or_advance_the_campaign() {
        let mut b = Book::default();
        let id = b.create("Ship authentication", "Anonymous access is rejected", 1).unwrap();
        let scout = b.add_stage(id, "/world", "Investigate auth", 2).unwrap();
        let build = b.add_stage(id, "/frontend", "Implement the accepted plan", 3).unwrap();
        b.link(id, scout, &done(), 11).unwrap();
        assert_eq!(b.campaigns[0].stages[0].status(), "reported finished");
        assert!(b.accept(id, scout, 12).is_err());
        assert!(b.campaigns[0].ready(build).is_err());
        assert!(b.complete(id, 12).is_err());
        b.handoff(id, scout, "Found a missing authorization check. Open question: cookie lifetime.", 13).unwrap();
        b.verify(id, scout, "Reviewed the affected route and reproduced the failing auth test", 14).unwrap();
        assert_eq!(b.campaigns[0].stages[0].status(), "verified");
        assert!(b.campaigns[0].ready(build).is_err());
        b.accept(id, scout, 15).unwrap();
        assert!(b.campaigns[0].ready(build).is_ok());
        assert!(b.campaigns[0].stages[1].missions.is_empty());
        assert!(b.complete(id, 16).is_err());
    }
    fn reviewed() -> (Book, u64, u64) {
        let mut b = Book::default();
        let id = b.create("Ship authentication", "Anonymous access is rejected", 1).unwrap();
        let stage = b.add_stage(id, "/world", "Investigate auth", 2).unwrap();
        b.link(id, stage, &done(), 10).unwrap();
        b.handoff(id, stage, "Finding: missing check. Question: cookie lifetime.", 11).unwrap();
        b.verify(id, stage, "Reproduced with the route test", 12).unwrap();
        b.accept(id, stage, 13).unwrap();
        (b, id, stage)
    }

    #[test]
    fn accepted_handoffs_are_carried_forward_and_changes_reopen_dependent_reviews() {
        let (mut b, id, first) = reviewed();
        let second = b.add_stage(id, "/frontend", "Implement the accepted plan", 14).unwrap();
        let brief = b.brief(id, second).unwrap();
        for text in ["Ship authentication", "Anonymous access is rejected", "missing check", "cookie lifetime", "route test", "Implement the accepted plan"] { assert!(brief.contains(text)); }
        let m = Mission { id: 8, cwd: "/frontend".into(), ..done() };
        b.link(id, second, &m, 15).unwrap();
        b.handoff(id, second, "Added auth; no unresolved questions", 16).unwrap();
        b.verify(id, second, "Route tests passed", 17).unwrap();
        b.accept(id, second, 18).unwrap();
        b.complete(id, 19).unwrap();
        assert_eq!(b.campaigns[0].completed, Some(19));
        b.reopen(id, first, 20).unwrap();
        assert!(b.campaigns[0].completed.is_none());
        assert!(b.campaigns[0].stages.iter().all(|s| s.accepted.is_none() && s.verification.is_empty()));
        assert!(b.campaigns[0].stages[1].history.iter().any(|n| n.text == "Route tests passed"));
        assert!(b.brief(id, second).is_err());
    }

    #[test]
    fn live_missions_block_verification_and_unrelated_missions_are_not_inferred() {
        let (mut b, id, stage) = reviewed();
        let active = Mission { id: 11, state: "active".into(), campaign: Some(CampaignRef { campaign: id, stage }), ..done() };
        let unrelated = Mission { id: 12, ..active.clone() };
        let unrelated = Mission { campaign: None, ..unrelated };
        b.refresh(&[done(), active.clone(), unrelated], 15);
        let s = &b.campaigns[0].stages[0];
        assert_eq!(s.missions.len(), 2);
        assert_eq!(s.status(), "under way");
        assert!(b.verify(id, stage, "Looks good", 16).is_err());
        assert!(b.accept(id, stage, 16).is_err());
        assert!(b.brief(id, stage).is_err());
        b.refresh(&[done(), Mission { state: "done".into(), ..active }], 17);
        assert_eq!(b.campaigns[0].stages[0].status(), "reported finished");
        assert!(b.campaigns[0].stages[0].accepted.is_none());
    }

    #[test]
    fn snapshots_survive_missing_missions_and_identical_refreshes_keep_acceptance() {
        let (mut b, id, _) = reviewed();
        b.complete(id, 14).unwrap();
        let before = b.clone();
        b.refresh(&[done()], 15);
        assert_eq!(b, before);
        b.refresh(&[], 16);
        let s = &b.campaigns[0].stages[0];
        assert_eq!(s.missions[0].mission.text, "Inspect auth");
        assert!(!s.missions[0].available);
        assert_eq!(s.accepted, Some(13));
        assert_eq!(b.campaigns[0].completed, Some(14));
        assert_eq!(serde_json::from_str::<Book>(&serde_json::to_string(&b).unwrap()).unwrap(), b);
    }

    #[test]
    fn objective_changes_keep_old_evidence_and_invalidate_acceptance() {
        let (mut b, id, _) = reviewed();
        let before = b.clone();
        assert!(b.update(id, "", "new criteria", 14).is_err());
        assert_eq!(b, before);
        b.update(id, "Ship authentication", "Anonymous and expired sessions are rejected", 15).unwrap();
        let c = &b.campaigns[0];
        assert!(c.stages[0].accepted.is_none());
        assert!(c.history.iter().any(|n| n.text.contains("Anonymous access is rejected")));
        assert!(c.stages[0].history.iter().any(|n| n.kind == "verified by player"));
        b.update_stage(id, 1, "/moons/auth", "Revise the middleware plan", 16).unwrap();
        assert!(b.brief(id, 1).unwrap().contains("Revise the middleware plan"));
        assert!(b.campaigns[0].stages[0].history.iter().any(|n| n.kind == "previous stage" && n.text.contains("/world")));
        b.archive(id, 17).unwrap();
        assert!(b.brief(id, 1).is_err());
        assert_eq!(b.campaigns[0].stages[0].missions[0].mission, done());
    }

    #[test]
    fn concurrent_plans_merge_and_a_corrupt_campaign_is_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("empire-campaign-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap(); let path = dir.join("campaigns.json");
        edit(&path, |b| { b.create("Ship authentication", "Anonymous access is rejected", 1).unwrap(); }).unwrap();
        let threads: Vec<_> = (0..6).map(|i| {
            let path = path.clone();
            std::thread::spawn(move || edit(&path, |b| { b.add_stage(1, &format!("/world-{i}"), "Investigate", 2).unwrap(); }).unwrap())
        }).collect();
        for t in threads { t.join().unwrap(); }
        let b = edit(&path, |_| {}).unwrap();
        for i in 0..6 { assert!(b.campaigns[0].stages.iter().any(|s| s.world == format!("/world-{i}") && s.task == "Investigate")); }
        assert_eq!(b.campaigns[0].stages.iter().map(|s| s.id).collect::<std::collections::HashSet<_>>().len(), 6);
        std::fs::write(&path, "broken").unwrap();
        assert!(edit(&path, |b| { b.archive(1, 3).unwrap(); }).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
        std::fs::remove_dir_all(dir).unwrap();
    }

}
