//! Discoveries on worlds, with evidence and a history that outlives their ships.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State { #[default] Open, Investigating, Deferred, Dismissed, Resolved }

impl State {
    pub fn label(self) -> &'static str {
        match self { Self::Open => "open", Self::Investigating => "investigating", Self::Deferred => "deferred", Self::Dismissed => "dismissed", Self::Resolved => "resolved" }
    }
    pub fn active(self) -> bool { matches!(self, Self::Open | Self::Investigating) }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event { pub at: i64, pub note: String }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: u64,
    pub key: String,
    pub world: String,
    pub host: String,
    pub system: String,
    pub kind: String,
    pub title: String,
    pub evidence: String,
    pub first: i64,
    pub last: i64,
    pub present: bool,
    pub state: State,
    pub history: Vec<Event>,
    #[serde(default)]
    pub missions: Vec<u32>,
    /// How serious Advisor judged a free-text report, on the levels 0 info, 1 low, 2 high, 3 critical
    /// (an expected value, so it can fall between levels). Absent in books written before it
    /// existed, and for everything but reports, which use `kind_severity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<f32>,
}

/// The severity levels Advisor scores a report on, lowest first.
pub const LEVELS: [&str; 4] = ["info", "low", "high", "critical"];

/// Severity by kind: what findings get when Advisor is off, unsure or has not answered yet. CI
/// failures, merge conflicts and uncrewed moons use this only. The map keeps today's order:
/// everything that reaches the attention list ranks as "low", as it did with a fixed rank.
pub fn kind_severity(kind: &str) -> f32 {
    match kind {
        "CI" => 2.0,
        "Git" | "Moon" | "Report" => 1.0,
        _ => 1.0,
    }
}

/// Where a severity sits in the attention list (lower is more urgent): critical 2, high 3,
/// low 4 (what every anomaly used to get), info 5. It only reorders; nothing is hidden by it.
pub fn rank_for(severity: f32) -> u8 {
    let level = if severity.is_finite() { severity.round().clamp(0.0, 3.0) as u8 } else { 1 };
    5 - level
}

impl Finding {
    pub fn tag(&self) -> String { format!("[anomaly #{}]", self.id) }
    /// Advisor's score for a report, else the kind's plain severity.
    pub fn effective_severity(&self) -> f32 { self.severity.unwrap_or_else(|| kind_severity(&self.kind)) }
    pub fn rank(&self) -> u8 { rank_for(self.effective_severity()) }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Book { pub findings: Vec<Finding> }

pub struct Observation {
    pub key: String, pub world: String, pub host: String, pub kind: String,
    pub title: String, pub evidence: String, pub at: i64, pub present: bool,
}

impl Book {
    pub fn observe(&mut self, o: Observation) {
        if let Some(f) = self.findings.iter_mut().find(|f| f.world == o.world && f.key == o.key) {
            if o.at < f.last { return; }
            if f.present != o.present {
                f.history.push(Event { at: o.at, note: if o.present { "Observed again".into() } else { "Survey confirmed recovery".into() } });
                if f.state != State::Dismissed {
                    f.state = if o.present { State::Open } else { State::Resolved };
                }
            }
            f.present = o.present;
            f.last = o.at;
            if o.present {
                if f.evidence != o.evidence || f.title != o.title {
                    f.history.push(Event { at: o.at, note: format!("Evidence updated; previous observation: {}\n{}", f.title, f.evidence) });
                    // new evidence, so any earlier severity no longer applies
                    f.severity = None;
                }
                f.title = o.title;
                f.evidence = o.evidence;
            }
            f.host = o.host;
            f.system = std::path::Path::new(&f.host).parent().unwrap_or(std::path::Path::new("/")).to_string_lossy().into_owned();
        } else if o.present {
            let id = self.findings.iter().map(|f| f.id).max().unwrap_or(0) + 1;
            let system = std::path::Path::new(&o.host).parent().unwrap_or(std::path::Path::new("/")).to_string_lossy().into_owned();
            self.findings.push(Finding { id, key: o.key, world: o.world, host: o.host, system, kind: o.kind, title: o.title, evidence: o.evidence,
                first: o.at, last: o.at, present: true, state: State::Open, history: vec![Event { at: o.at, note: "Discovered".into() }], missions: vec![], severity: None });
        }
    }
    pub fn set_state(&mut self, id: u64, state: State, at: i64) {
        if let Some(f) = self.findings.iter_mut().find(|f| f.id == id) {
            if f.state == state { return; }
            f.state = state;
            f.history.push(Event { at, note: format!("Marked {}", state.label()) });
        }
    }
}

pub fn edit(path: &std::path::Path, change: impl FnOnce(&mut Book)) -> std::io::Result<Book> { crate::state::edit(path, change) }

pub fn path() -> std::path::PathBuf { crate::daemon::dirs_state().join("anomalies.json") }

/// An explicit finding, attributed to its reporter rather than silently treated as verified.
pub fn report(world: &str, title: &str, evidence: &str, source: &str) -> std::io::Result<()> {
    let world = std::fs::canonicalize(world)?;
    if !world.is_dir() || title.trim().is_empty() || evidence.trim().is_empty() {
        return Err(std::io::Error::other("a world directory, title and evidence are required"));
    }
    let world = world.to_string_lossy().into_owned();
    let host = crate::survey::world_host(&world).unwrap_or_else(|| world.clone());
    let b = edit(&path(), |b| b.observe(Observation { key: format!("report:{title}"), world: world.clone(), host, kind: "Report".into(), title: title.into(),
        evidence: format!("Reported by {source} (not independently verified)\n{evidence}"), at: crate::survey::now(), present: true }))?;
    let f = b.findings.iter().find(|f| f.world == world && f.key == format!("report:{title}")).unwrap();
    println!("Anomaly #{} recorded on {}", f.id, f.world);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observation(at: i64, present: bool) -> Observation {
        Observation { key: "ci:42".into(), world: "/work/orion/frontend".into(), host: "/work/orion/frontend".into(), kind: "CI".into(),
            title: "PR #42 has failing checks".into(), evidence: "Fix login".into(), at, present }
    }
    #[test]
    fn repeated_surveys_keep_one_finding_and_its_dismissal() {
        let mut b = Book::default();
        b.observe(observation(10, true));
        assert_eq!(b.findings.len(), 1);
        b.set_state(1, State::Dismissed, 11);
        b.observe(observation(20, true));
        assert_eq!(b.findings.len(), 1);
        let f = &b.findings[0];
        assert_eq!((f.first, f.last, f.state), (10, 20, State::Dismissed));
        assert_eq!(f.system, "/work/orion");
        assert_eq!(f.history.len(), 2);
    }
    #[test]
    fn observed_recovery_resolves_and_recurrence_reopens() {
        let mut b = Book::default();
        b.observe(observation(10, true));
        b.observe(observation(20, false));
        assert_eq!(b.findings[0].state, State::Resolved);
        b.observe(observation(30, true));
        assert_eq!(b.findings[0].state, State::Open);
        assert_eq!(b.findings[0].history.len(), 3);
        b.observe(observation(15, false));
        assert_eq!(b.findings[0].state, State::Open, "old evidence must not resolve a current finding");
    }
    #[test]
    fn dismissals_survive_recovery_and_recurrence() {
        let mut b = Book::default();
        b.observe(observation(10, true));
        b.set_state(1, State::Dismissed, 11);
        b.observe(observation(20, false));
        b.observe(observation(30, true));
        assert_eq!(b.findings[0].state, State::Dismissed);
    }
    #[test]
    fn changed_evidence_preserves_the_previous_observation() {
        let mut b = Book::default();
        b.observe(observation(10, true));
        let mut o = observation(20, true);
        o.evidence = "New evidence".into();
        b.observe(o);
        assert_eq!(b.findings[0].evidence, "New evidence");
        assert!(b.findings[0].history.iter().any(|e| e.note.contains("Fix login")));
    }
    #[test]
    fn old_books_without_severity_still_load() {
        let old = r#"{"findings":[{"id":1,"key":"report:x","world":"/w","host":"/w","system":"/","kind":"Report","title":"t","evidence":"e",
            "first":1,"last":1,"present":true,"state":"open","history":[]}]}"#;
        let book: Book = serde_json::from_str(old).unwrap();
        assert_eq!(book.findings[0].severity, None);
        assert_eq!(book.findings[0].effective_severity(), 1.0);
        // unset severity is not written, so books stay readable by older builds
        assert!(!serde_json::to_string(&book).unwrap().contains("severity"));
        let mut scored = book.clone();
        scored.findings[0].severity = Some(2.4);
        let back: Book = serde_json::from_str(&serde_json::to_string(&scored).unwrap()).unwrap();
        assert_eq!(back.findings[0].severity, Some(2.4));
    }
    #[test]
    fn severity_ranks_the_attention_list_and_kinds_keep_todays_order() {
        // the kind map reproduces the old fixed rank 4 for everything that reaches the list
        for kind in ["Git", "Moon", "Report", "Whatever"] { assert_eq!(rank_for(kind_severity(kind)), 4, "{kind}"); }
        assert_eq!([0.0, 1.0, 2.0, 3.0].map(rank_for), [5, 4, 3, 2]);
        assert_eq!([0.4, 0.6, 1.49, 2.51, 9.0, -3.0].map(rank_for), [5, 4, 4, 2, 2, 5], "between levels rounds to the nearest; out of range is clamped");
        assert_eq!(rank_for(f32::NAN), 4);
        let mut b = Book::default();
        b.observe(observation(10, true));
        assert_eq!(b.findings[0].kind, "CI");
        assert_eq!(b.findings[0].rank(), 3, "CI falls back to its kind");
        b.findings[0].severity = Some(2.9);
        assert_eq!(b.findings[0].rank(), 2, "a score wins over the kind");
    }
    #[test]
    fn new_evidence_clears_a_stale_severity() {
        let mut b = Book::default();
        b.observe(observation(10, true));
        b.findings[0].severity = Some(3.0);
        b.observe(observation(20, true));
        assert_eq!(b.findings[0].severity, Some(3.0), "same evidence keeps it");
        let mut o = observation(30, true);
        o.evidence = "Something else".into();
        b.observe(o);
        assert_eq!(b.findings[0].severity, None);
    }
    #[test]
    fn worlds_do_not_share_findings() {
        let mut b = Book::default();
        b.observe(observation(10, true));
        let mut o = observation(10, true);
        o.world = "/work/other".into();
        b.observe(o);
        assert_eq!(b.findings.len(), 2);
        assert_ne!(b.findings[0].id, b.findings[1].id);
    }
}

#[derive(Default)]
pub struct Filter {
    pub sector: Option<(String, Vec<String>)>,
    pub world: Option<String>,
    pub system: Option<String>,
    pub kind: usize,
    pub state: usize,
    pub query: String,
}

/// Finding kinds as stored in anomalies.json. Worktree findings keep the stored name "Moon"; `kind_label` is what the UI shows.
pub const KINDS: [&str; 5] = ["all kinds", "CI", "Git", "Moon", "Report"];

/// A kind's name in the UI: worktrees are stations now, but saved findings still say "Moon".
pub fn kind_label(kind: &str) -> &str { if kind == "Moon" { "Station" } else { kind } }
pub const STATES: [&str; 7] = ["active", "all statuses", "open", "investigating", "deferred", "dismissed", "resolved"];

impl Filter {
    pub fn rows<'a>(&self, book: &'a Book) -> Vec<&'a Finding> {
        let q = self.query.to_lowercase();
        let mut rows: Vec<_> = book.findings.iter().filter(|f| {
            self.sector.as_ref().is_none_or(|(_, worlds)| worlds.iter().any(|w| w == &f.world || w == &f.host)) &&
            (self.world.as_ref().is_none_or(|w| &f.host == w || &f.world == w)) &&
            (self.system.as_ref().is_none_or(|s| &f.system == s)) &&
            (self.kind == 0 || f.kind == KINDS[self.kind]) &&
            (match self.state { 0 => f.state.active(), 1 => true, i => f.state.label() == STATES[i] }) &&
            (q.is_empty() || format!("{} {} {} {} {}", f.title, f.evidence, f.world, f.system, f.id).to_lowercase().contains(&q))
        }).collect();
        rows.sort_by_key(|f| (f.state != State::Open, std::cmp::Reverse(f.first), f.id));
        rows
    }
}

#[cfg(test)]
mod storage_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn dir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("empire-anomalies-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn concurrent_reports_preserve_findings_and_dismissals_after_reload() {
        let dir = dir();
        let path = dir.join("anomalies.json");
        let threads: Vec<_> = (0..8).map(|i| {
            let path = path.clone();
            std::thread::spawn(move || edit(&path, |b| b.observe(Observation { key: format!("report:{i}"), world: "/world".into(), host: "/world".into(), kind: "Report".into(),
                title: format!("Finding {i}"), evidence: "Explicit evidence".into(), at: 10, present: true })).unwrap())
        }).collect();
        for t in threads { t.join().unwrap(); }
        edit(&path, |b| b.set_state(1, State::Dismissed, 20)).unwrap();
        let b = edit(&path, |_| {}).unwrap();
        assert_eq!(b.findings.len(), 8);
        assert_eq!(b.findings[0].state, State::Dismissed);
        assert_eq!(b.findings[0].history[1].note, "Marked dismissed");
        assert_eq!(b.findings.iter().map(|f| f.id).collect::<std::collections::HashSet<_>>().len(), 8);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn corrupt_storage_is_preserved_and_an_unwritable_store_reports_failure() {
        let dir = dir();
        let path = dir.join("anomalies.json");
        std::fs::write(&path, b"{broken").unwrap();
        assert!(edit(&path, |b| b.findings.clear()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
        assert!(edit(&path.join("impossible.json"), |_| {}).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
