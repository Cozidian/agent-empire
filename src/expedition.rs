//! Approved campaign plans, automatic handoffs, and worktree flight records.
use serde::{Deserialize, Serialize};
use crate::campaign::{Book, Campaign, Stage, Note};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Flight {
    pub worlds: Vec<String>, pub limits: String, pub kind: String,
    pub phase: String, pub feedback: String, pub summary: String,
    pub nonce: String, pub moons: Vec<Moon>, pub planner: Option<Job>,
    pub error: String,
    #[serde(default)] pub retry: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Moon { pub world: String, pub path: String, pub branch: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub token: String, pub world: String, pub role: String, pub task: String,
    pub mission: Option<u32>, pub ship: Option<u32>, pub state: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub summary: String,
    #[serde(default)] pub criteria: String,
    pub stages: Vec<Step>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step { pub world: String, pub task: String, pub role: String, pub evidence: String }
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome { pub status: String, pub summary: String, pub evidence: String, #[serde(default)] pub question: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Report { Plan(Plan), Result(Outcome) }

pub fn nonce() -> Result<String, String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn note(at: u64, kind: &str, text: &str) -> Note { Note { at, kind: kind.into(), text: text.into() } }
fn text(s: &str, required: bool) -> Result<(), String> {
    if s.len() > 32000 || (required && s.trim().is_empty()) || s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') { Err("Use nonempty plain text within 32,000 bytes".into()) } else { Ok(()) }
}
impl Campaign {
    pub fn flight_job(&self) -> Option<&Job> {
        let f = self.flight.as_ref()?;
        f.planner.as_ref().filter(|j| j.state != "complete").or_else(|| self.stages.iter().filter_map(|s| s.job.as_ref()).find(|j| j.state != "complete"))
    }
    pub fn flight_job_mut(&mut self, token: &str) -> Option<&mut Job> {
        if let Some(f) = &mut self.flight && f.planner.as_ref().is_some_and(|j| j.token == token) { return f.planner.as_mut(); }
        self.stages.iter_mut().filter_map(|s| s.job.as_mut()).find(|j| j.token == token)
    }
    pub fn flight_status(&self) -> &str { self.flight.as_ref().map_or("manual", |f| f.phase.as_str()) }
    pub fn proposed_plan(&self) -> Plan {
        Plan { summary: self.flight.as_ref().map_or(String::new(), |f| f.summary.clone()), criteria: self.criteria.clone(),
            stages: self.stages.iter().map(|s| Step { world: s.world.clone(), task: s.task.clone(), role: s.role.clone(), evidence: s.expected.clone() }).collect() }
    }
}
impl Book {
    pub fn automate(&mut self, id: u64, worlds: Vec<String>, limits: String, kind: String, at: u64) -> Result<(), String> {
        text(&limits, false)?;
        if worlds.is_empty() || worlds.len() > 12 || worlds.iter().any(|p| !std::path::Path::new(p).is_absolute() || p.chars().any(char::is_control)) { return Err("Choose 1–12 absolute repository paths".into()); }
        if !["claude", "codex", "grok", "cursor"].contains(&kind.as_str()) { return Err("Choose claude, codex, grok or cursor".into()); }
        let c = self.campaign_mut(id)?;
        if c.flight.is_some() || !c.stages.is_empty() { return Err("Start a new campaign for automatic planning; existing work is kept".into()); }
        c.flight = Some(Flight { worlds, limits, kind, phase: "planning".into(), nonce: nonce()?, ..Default::default() });
        c.history.push(note(at, "planning requested", "A planning ship will propose stages on a campaign moon; execution requires approval"));
        Ok(())
    }
    pub fn propose(&mut self, id: u64, plan: Plan, at: u64) -> Result<(), String> {
        let c = self.campaign_mut(id)?;
        let f = c.flight.as_ref().ok_or("This is a manual campaign")?;
        if !["planning", "proposed"].contains(&f.phase.as_str()) || c.archived { return Err("This plan is no longer open for proposals".into()); }
        text(&plan.summary, true)?; text(&plan.criteria, false)?;
        if plan.stages.is_empty() || plan.stages.len() > 20 { return Err("A plan needs 1–20 sequential stages".into()); }
        for s in &plan.stages {
            if !f.worlds.contains(&s.world) { return Err(format!("Stage is outside the approved scope: {}", s.world)); }
            if !["scout", "builder", "inspector"].contains(&s.role.as_str()) { return Err("Stage role must be scout, builder or inspector".into()); }
            text(&s.task, true)?; text(&s.evidence, true)?;
        }
        // Validate the whole proposal before replacing anything.
        let f = c.flight.as_mut().unwrap(); f.summary = plan.summary; f.phase = "proposed".into(); f.error.clear();
        if let Some(j) = &mut f.planner { j.state = "complete".into(); }
        if c.criteria.trim().is_empty() { c.criteria = plan.criteria; }
        c.stages = plan.stages.into_iter().enumerate().map(|(i, s)| Stage {
            id: i as u64 + 1, world: s.world, task: s.task, role: s.role, expected: s.evidence,
            missions: vec![], handoff: String::new(), verification: String::new(), accepted: None, history: vec![], job: None, outcome: None,
        }).collect();
        c.history.push(note(at, "plan proposed", "Review, edit or reject the plan before approving its execution")); Ok(())
    }
    pub fn approve_plan(&mut self, id: u64, at: u64) -> Result<(), String> {
        let c = self.campaign_mut(id)?; let f = c.flight.as_mut().ok_or("This is a manual campaign")?;
        if c.archived || f.phase != "proposed" || c.stages.is_empty() { return Err("Review a proposed plan first".into()); }
        f.phase = "running".into(); f.error.clear();
        c.history.push(note(at, "plan approved by player", "Run sequential stages on campaign moons; preserve branches for final review")); Ok(())
    }
    pub fn reject_plan(&mut self, id: u64, feedback: String, at: u64) -> Result<(), String> {
        text(&feedback, true)?;
        let c = self.campaign_mut(id)?; let f = c.flight.as_mut().ok_or("This is a manual campaign")?;
        if f.phase != "proposed" { return Err("Only a proposed plan can be rejected".into()); }
        c.history.push(note(at, "plan rejected", &format!("{}\nFeedback: {feedback}", serde_json::to_string(&c.stages).unwrap_or_default())));
        f.feedback = feedback; f.planner = None; f.phase = "planning".into(); c.stages.clear(); Ok(())
    }
    pub fn pause_flight(&mut self, id: u64, at: u64) -> Result<(), String> {
        let c = self.campaign_mut(id)?; let f = c.flight.as_mut().ok_or("This is a manual campaign")?;
        if c.archived || matches!(f.phase.as_str(), "accepted" | "cancelled") { return Err("This campaign is finished; start a follow-up goal".into()); }
        if !f.error.is_empty() { f.retry = true; return Ok(()); }
        match f.phase.as_str() {
            "running" => f.phase = "paused".into(),
            "paused" => f.phase = "running".into(),
            _ => return Err("Pause or resume an approved, running campaign".into()),
        }
        c.history.push(note(at, "dispatch changed", &format!("{}; current ship keeps its work", f.phase))); Ok(())
    }
    pub fn cancel_flight(&mut self, id: u64, at: u64) -> Result<(), String> {
        let c = self.campaign_mut(id)?; let f = c.flight.as_mut().ok_or("This is a manual campaign")?;
        f.phase = "cancelled".into(); c.history.push(note(at, "cancelled", "No further missions will launch; ships and moons are preserved")); Ok(())
    }
    pub fn flight_report(&mut self, token: &str, report: Report, at: u64) -> Result<(), String> {
        let ci = self.campaigns.iter().position(|c| c.flight.as_ref().is_some_and(|f| f.planner.as_ref().is_some_and(|j| j.token == token))
            || c.stages.iter().any(|s| s.job.as_ref().is_some_and(|j| j.token == token))).ok_or("Unknown or superseded campaign receipt")?;
        let c = &mut self.campaigns[ci];
        if c.archived { return Err("Campaign archived; result not applied".into()); }
        let planning = c.flight.as_ref().unwrap().planner.as_ref().is_some_and(|j| j.token == token);
        let job = c.flight_job_mut(token).ok_or("Unknown receipt")?;
        if job.state == "complete" { return Err("This receipt has already been completed".into()); }
        match report {
            Report::Plan(plan) if planning => {
                if c.flight_status() == "cancelled" {
                    c.flight_job_mut(token).unwrap().state = "complete".into();
                    c.history.push(note(at, "late plan after cancellation", &serde_json::to_string(&plan).unwrap_or_default())); Ok(())
                } else { let id = c.id; self.propose(id, plan, at) }
            }
            Report::Result(outcome) if !planning => {
                text(&outcome.summary, true)?; text(&outcome.evidence, outcome.status == "complete")?; text(&outcome.question, false)?;
                if !["complete", "blocked", "failed"].contains(&outcome.status.as_str()) { return Err("Result status must be complete, blocked or failed".into()); }
                let s = c.stages.iter_mut().find(|s| s.job.as_ref().is_some_and(|j| j.token == token)).unwrap();
                s.handoff = outcome.summary.clone(); s.verification = outcome.evidence.clone();
                s.job.as_mut().unwrap().state = if outcome.status == "complete" { "complete" } else { "waiting" }.into();
                s.history.push(note(at, "ship report", &format!("{}: {}\n{}", outcome.status, outcome.summary, outcome.evidence)));
                s.outcome = Some(outcome); c.flight.as_mut().unwrap().error.clear(); Ok(())
            }
            _ => Err("Report type does not match this mission".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> Plan { Plan { summary: "Build then inspect".into(), criteria: "Tests pass".into(), stages: vec![
        Step { world: "/repo".into(), task: "Build it".into(), role: "builder".into(), evidence: "Tests and diff".into() },
        Step { world: "/repo".into(), task: "Inspect it".into(), role: "inspector".into(), evidence: "Independent checks".into() }] } }
    #[test]
    fn plan_requires_approval_and_rejection_keeps_feedback() {
        let mut b = Book::default(); let id = b.create("Ship a feature", "", 1).unwrap();
        b.automate(id, vec!["/repo".into()], "Do not deploy".into(), "codex".into(), 2).unwrap();
        assert!(b.approve_plan(id, 3).is_err());
        b.propose(id, plan(), 4).unwrap(); assert_eq!(b.campaigns[0].flight_status(), "proposed");
        let mut bad = plan(); bad.stages[1].world = "/outside".into(); let before = b.clone();
        assert!(b.propose(id, bad, 4).is_err()); assert_eq!(b, before);
        b.reject_plan(id, "Keep it smaller".into(), 5).unwrap();
        assert_eq!(b.campaigns[0].flight_status(), "planning");
        assert_eq!(b.campaigns[0].flight.as_ref().unwrap().feedback, "Keep it smaller");
        b.propose(id, plan(), 6).unwrap(); b.approve_plan(id, 7).unwrap();
        b.pause_flight(id, 8).unwrap(); assert_eq!(b.campaigns[0].flight_status(), "paused");
        b.pause_flight(id, 9).unwrap(); assert_eq!(b.campaigns[0].flight_status(), "running");
    }
    #[test]
    fn cancelling_preserves_late_results_without_launching_more_work() {
        let mut b = Book::default(); let id = b.create("Goal", "", 1).unwrap();
        b.automate(id, vec!["/repo".into()], String::new(), "codex".into(), 2).unwrap(); b.propose(id, plan(), 3).unwrap(); b.approve_plan(id, 4).unwrap();
        let (_, job) = reserve(&mut b.campaigns[0]).unwrap().unwrap(); b.cancel_flight(id, 5).unwrap();
        b.flight_report(&job.token, Report::Result(Outcome { status: "complete".into(), summary: "Finished existing work".into(), evidence: "Tests passed".into(), question: String::new() }), 6).unwrap();
        assert_eq!(b.campaigns[0].flight_status(), "cancelled"); assert!(reserve(&mut b.campaigns[0]).unwrap().is_none());
        assert!(b.campaigns[0].stages[1].job.is_none()); assert!(b.complete(id, 7).is_err()); assert!(b.pause_flight(id, 7).is_err());
    }

    #[test]
    fn automatic_handoffs_require_reports_and_preserve_pause_and_final_review() {
        let mut b = Book::default(); let id = b.create("Goal", "", 1).unwrap();
        b.automate(id, vec!["/repo".into()], "No deployment".into(), "codex".into(), 2).unwrap();
        let (_, planner) = reserve(&mut b.campaigns[0]).unwrap().unwrap();
        assert!(reserve(&mut b.campaigns[0]).unwrap().is_none());
        b.flight_report(&planner.token, Report::Plan(plan()), 3).unwrap();
        assert!(reserve(&mut b.campaigns[0]).unwrap().is_none()); b.approve_plan(id, 4).unwrap();
        let (_, job) = reserve(&mut b.campaigns[0]).unwrap().unwrap();
        b.campaigns[0].stages[0].missions.push(crate::campaign::Linked { mission: crate::proto::Mission { state: "done".into(), ..Default::default() }, available: true });
        assert!(reserve(&mut b.campaigns[0]).unwrap().is_none(), "Terminal completion alone must not dispatch the next stage");
        b.pause_flight(id, 5).unwrap();
        b.flight_report(&job.token, Report::Result(Outcome { status: "complete".into(), summary: "Built".into(), evidence: "Tests pass".into(), question: String::new() }), 6).unwrap();
        assert!(reserve(&mut b.campaigns[0]).unwrap().is_none()); b.pause_flight(id, 7).unwrap();
        let (stage, next) = reserve(&mut b.campaigns[0]).unwrap().unwrap(); assert_eq!(stage, 2);
        let prompt = brief(&b.campaigns[0], stage, &next, "/bin/empire");
        for s in ["No deployment", "Built", "Tests pass", "Inspect it"] { assert!(prompt.contains(s)); }
        b.flight_report(&next.token, Report::Result(Outcome { status: "complete".into(), summary: "Inspected".into(), evidence: "Independent tests pass".into(), question: String::new() }), 8).unwrap();
        assert!(reserve(&mut b.campaigns[0]).unwrap().is_none()); assert_eq!(b.campaigns[0].flight_status(), "review"); assert!(b.campaigns[0].completed.is_none());
        b.complete(id, 9).unwrap(); assert_eq!(b.campaigns[0].completed, Some(9));
        let saved = serde_json::to_string(&b).unwrap(); assert_eq!(serde_json::from_str::<Book>(&saved).unwrap(), b);
    }

    #[test]
    fn reported_completion_needs_evidence_and_receipts_cannot_be_replayed() {
        let mut b = Book::default(); let id = b.create("Goal", "", 1).unwrap();
        b.automate(id, vec!["/repo".into()], String::new(), "codex".into(), 2).unwrap(); b.propose(id, plan(), 3).unwrap(); b.approve_plan(id, 4).unwrap();
        b.campaigns[0].stages[0].job = Some(Job { token: "receipt".into(), world: "/moon".into(), role: "builder".into(), task: "task".into(), mission: Some(1), ship: Some(1), state: "sent".into() });
        let mut r = Outcome { status: "complete".into(), summary: "Built".into(), ..Default::default() };
        assert!(b.flight_report("receipt", Report::Result(r.clone()), 5).is_err());
        r.status = "blocked".into(); r.question = "Which storage?".into(); b.flight_report("receipt", Report::Result(r.clone()), 6).unwrap();
        assert_eq!(b.campaigns[0].stages[0].job.as_ref().unwrap().state, "waiting");
        r.status = "complete".into(); r.evidence = "cargo test: passed".into(); b.flight_report("receipt", Report::Result(r.clone()), 7).unwrap();
        assert!(b.campaigns[0].stages[0].accepted.is_none());
        assert!(b.flight_report("receipt", Report::Result(r), 8).is_err());
    }
}

/// Reporting is a local socket operation; no model API or native permission bypass is involved.
pub fn report_cli(args: &[String]) -> std::io::Result<()> {
    use std::io::Read;
    if args.len() != 1 { return Err(std::io::Error::other("usage: empire campaign-report RECEIPT < report.json (or pipe JSON on stdin)")); }
    let mut input = String::new(); std::io::stdin().take(262145).read_to_string(&mut input)?;
    if input.len() > 262144 { return Err(std::io::Error::other("Campaign report is too large")); }
    let report = serde_json::from_str(&input).map_err(std::io::Error::other)?;
    match crate::proto::call(&crate::proto::Req::CampaignReport { token: args[0].clone(), report })? {
        crate::proto::Resp::Ok => { println!("Campaign report recorded"); Ok(()) }
        crate::proto::Resp::Err { msg } => Err(std::io::Error::other(msg)),
        _ => Err(std::io::Error::other("Unexpected campaign report response")),
    }
}

/// Reserve once before dispatch. A terminal finishing a turn is deliberately not an outcome.
pub fn reserve(c: &mut Campaign) -> Result<Option<(u64, Job)>, String> {
    if c.archived || c.completed.is_some() { return Ok(None); }
    let Some(f) = &c.flight else { return Ok(None) };
    if !f.error.is_empty() { return Ok(None); }
    if f.phase == "planning" && f.planner.is_none() {
        let job = Job { token: nonce()?, world: f.worlds[0].clone(), role: "scout".into(), task: String::new(), mission: None, ship: None, state: "reserved".into() };
        c.flight.as_mut().unwrap().planner = Some(job.clone()); return Ok(Some((0, job)));
    }
    if f.phase != "running" { return Ok(None); }
    for s in &mut c.stages {
        if s.outcome.as_ref().is_some_and(|r| r.status == "complete") { continue; }
        if s.job.is_some() { return Ok(None); }
        let job = Job { token: nonce()?, world: s.world.clone(), role: s.role.clone(), task: s.task.clone(), mission: None, ship: None, state: "reserved".into() };
        s.job = Some(job.clone()); return Ok(Some((s.id, job)));
    }
    c.flight.as_mut().unwrap().phase = "review".into(); Ok(None)
}

pub fn brief(c: &Campaign, stage: u64, job: &Job, exe: &str) -> String {
    let f = c.flight.as_ref().unwrap();
    let mut p = format!("Campaign goal: {}\nAcceptance criteria (optional):\n{}\nNo-go zones / scope instructions:\n{}\nScoped source repositories:\n{}\nCampaign moons:\n{}\n\nWork ONLY in the listed campaign moons. Never change the source checkouts, merge, dock, push or deploy. Preserve existing changes. No-go zones are instructions, not an extra sandbox. Stop and ask if the approved scope is insufficient.\n",
        c.objective, c.criteria, f.limits, f.worlds.join("\n"), f.moons.iter().map(|m| format!("{} => {}", m.world, m.path)).collect::<Vec<_>>().join("\n"));
    let schema = if stage == 0 {
        p.push_str(&format!("\nInvestigate read-only and propose 1–20 sequential stages for this goal. Do not implement. Each stage must use an exact scoped source repository path, a scout/builder/inspector role, and concrete expected evidence. Include final inspection. Propose criteria if none were provided. Previous feedback:\n{}\n", f.feedback));
        serde_json::json!({"type":"plan", "summary":"Why this plan achieves the goal", "criteria":"Proposed acceptance criteria if missing", "stages":[{"world":f.worlds[0],"task":"A concrete task", "role":"builder", "evidence":"Checks and outputs that demonstrate the result"}]}).to_string()
    } else {
        let s = c.stages.iter().find(|s| s.id == stage).unwrap();
        p.push_str(&format!("\nExecute only this approved stage:\n{}\nExpected evidence:\n{}\n", s.task, s.expected));
        for prev in c.stages.iter().take_while(|s| s.id != stage) {
            if let Some(r) = &prev.outcome { p.push_str(&format!("\nPrevious stage on {} (agent-reported):\n{}\nEvidence:\n{}\n", prev.world, r.summary, r.evidence)); }
        }
        serde_json::json!({"type":"result", "status":"complete", "summary":"Changes, findings and handoff", "evidence":"Actual checks, their results and limitations", "question":""}).to_string()
    };
    let exe = format!("'{}'", exe.replace('\'', "'\\''"));
    p.push_str(&format!("\nSubmit your explicit report through the local campaign receipt command. This records coordination metadata, not repository changes; it is the one reporting operation requested even for a scout. Pipe JSON directly with a shell heredoc, without creating a file:\n{exe} campaign-report {} <<'EMPIRE_REPORT'\n{schema}\nEMPIRE_REPORT\nReplace example values with your actual findings. A finished chat turn alone does not advance the campaign. For execution problems, report status blocked or failed with summary/evidence/question, then wait for the player; use the same receipt to report complete after resolving them. For planning questions, ask in your terminal before submitting a plan. Never invent evidence. Respect native permission prompts.\n", job.token));
    p
}
