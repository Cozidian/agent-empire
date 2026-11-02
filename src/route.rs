//! Jev suggests which installed species should take a mission. It only preselects the species
//! in the composer; nothing is launched until you press Enter, and anything you pick by hand wins.
//!
//! On with `TYPESAFE_API_KEY` set and `EMPIRE_JEV_ROUTE=1`. Off, or on any error, the composer
//! is exactly what it was.

use crate::jev::{self, Error, Gate};
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// How long the text must sit still before Jev is asked.
pub const DEBOUNCE: Duration = Duration::from_millis(1000);
const FLAG: &str = "EMPIRE_JEV_ROUTE";

/// Short, neutral descriptions: what each is, with no claim about which is better.
fn about(kind: &str) -> Option<&'static str> {
    match kind {
        "claude" => Some("Claude Code, Anthropic's coding agent for the terminal"),
        "codex" => Some("Codex CLI, OpenAI's coding agent for the terminal"),
        "grok" => Some("Grok CLI, xAI's coding agent for the terminal"),
        _ => None,
    }
}

const INSTRUCTIONS: &str = "Which coding agent should carry out `task` in `project`? The agents are all general-purpose and interchangeable. Pick one only when the task or project gives a reason, for example by naming an agent, its vendor or a tool it ships with. Otherwise answer with low confidence.";

/// What the composer holds when Jev is asked about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub project: String,
    pub task: String,
    /// The installed species, in the composer's order.
    pub kinds: Vec<String>,
}

impl Input {
    /// `None` when there is no text, or fewer than two species to choose between.
    pub fn new(project: &str, task: &str, kinds: &[String]) -> Option<Input> {
        let kinds: Vec<String> = kinds.iter().filter(|k| about(k).is_some()).cloned().collect();
        (!task.trim().is_empty() && kinds.len() >= 2).then(|| Input { project: project.into(), task: task.trim().into(), kinds })
    }

    fn key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }

    pub fn request(&self) -> Value {
        let options: Vec<(&str, &str)> = self.kinds.iter().filter_map(|k| Some((k.as_str(), about(k)?))).collect();
        jev::request(json!({ "task": jev::clip(&self.task), "project": self.project }), vec![("species", jev::choice_q(INSTRUCTIONS, &options))])
    }

    pub fn read(&self, v: &Value) -> Option<Suggestion> {
        let names: Vec<&str> = self.kinds.iter().map(String::as_str).collect();
        let c = jev::choice(v, "species", &names)?;
        Some(Suggestion { kind: c.label, probability: c.probability, confidence: c.confidence, gate: jev::gate(c.confidence) })
    }
}

impl Hash for Input {
    fn hash<H: Hasher>(&self, h: &mut H) {
        (&self.project, &self.task, &self.kinds).hash(h);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub kind: String,
    /// Probability Jev gives the species.
    pub probability: f32,
    pub confidence: f32,
    pub gate: Gate,
}

impl Suggestion {
    /// Shown under the text, or nothing below the confidence floor.
    pub fn line(&self) -> Option<String> {
        (self.gate != Gate::Ignore).then(|| format!("Jev suggests {} · {:.0}%", self.kind, self.probability * 100.0))
    }
}

pub fn ask(input: &Input) -> Receiver<Result<Suggestion, Error>> {
    let i = input.clone();
    jev::spawn(input.request(), move |v| i.read(v))
}

/// The composer's side of it: debounces, asks once per text, keeps the answer for that text.
pub struct Route {
    pub enabled: bool,
    key: Option<u64>,
    changed: Instant,
    asked: bool,
    rx: Option<Receiver<Result<Suggestion, Error>>>,
    pub suggestion: Option<Suggestion>,
    /// You picked a species by hand: Jev no longer preselects.
    pub touched: bool,
}

impl Route {
    pub fn new() -> Route {
        Route { enabled: jev::enabled(FLAG), key: None, changed: Instant::now(), asked: false, rx: None, suggestion: None, touched: false }
    }

    pub fn reset(&mut self) {
        let enabled = self.enabled;
        *self = Route { enabled, ..Route::new() };
    }

    /// Call every frame. `open` is whether the composer is showing; `input` its content, if it
    /// has any worth asking about. Returns the index in `input.kinds` to preselect when an
    /// answer has just arrived.
    pub fn step(&mut self, now: Instant, open: bool, input: Option<&Input>, ask: impl FnOnce(&Input) -> Receiver<Result<Suggestion, Error>>) -> Option<usize> {
        if !self.enabled {
            return None;
        }
        if !open {
            // a new composer starts clean
            if self.key.is_some() || self.touched {
                self.reset();
            }
            return None;
        }
        let Some(input) = input else {
            // nothing to ask about (yet); a species you picked by hand stays picked
            self.key = None;
            self.asked = false;
            self.rx = None;
            self.suggestion = None;
            return None;
        };
        let key = input.key();
        if self.key != Some(key) {
            // the text moved: forget the old answer and start the clock again
            self.key = Some(key);
            self.changed = now;
            self.asked = false;
            self.rx = None;
            self.suggestion = None;
            return None;
        }
        if !self.asked && now.duration_since(self.changed) >= DEBOUNCE {
            self.asked = true;
            self.rx = Some(ask(input));
        }
        let outcome = self.rx.as_ref()?.try_recv();
        match outcome {
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(_) => {
                self.rx = None;
                None
            }
            Ok(result) => {
                self.rx = None;
                let s = result.ok().filter(|s| s.gate != Gate::Ignore && input.kinds.contains(&s.kind))?;
                let pick = (!self.touched).then(|| input.kinds.iter().position(|k| *k == s.kind)).flatten();
                self.suggestion = Some(s);
                pick
            }
        }
    }

    /// What to record when the mission is sent: Jev's suggestion beside your choice.
    pub fn record(&self, chose: &str) {
        if let Some(s) = &self.suggestion {
            jev::log("route", json!({ "suggested": s.kind, "probability": s.probability, "confidence": s.confidence, "chose": chose, "agreed": s.kind == chose }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn kinds(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| s.to_string()).collect()
    }

    fn input(task: &str) -> Input {
        Input::new("agent-empire", task, &kinds(&["claude", "codex", "grok"])).unwrap()
    }

    fn answer(label: &str, conf: f64) -> Value {
        json!({ "answers": { "species": { "type": "choice", "choice": label, "confidence": conf,
            "probabilities": { "claude": 0.1, "codex": 0.8, "grok": 0.1 } } } })
    }

    fn route() -> Route {
        Route { enabled: true, ..Route::new() }
    }

    #[test]
    fn asks_only_about_the_installed_species_with_neutral_descriptions() {
        let two = Input::new("p", "fix it", &kinds(&["claude", "grok"])).unwrap();
        let req = two.request();
        let criteria = req["questions"]["species"]["criteria"].as_object().unwrap();
        assert_eq!(criteria.keys().collect::<Vec<_>>(), ["claude", "grok"]);
        assert_eq!(req["questions"]["species"]["type"], "choice");
        assert_eq!(req["state"], json!({ "task": "fix it", "project": "p" }));
        for text in criteria.values().map(|v| v.as_str().unwrap().to_lowercase()) {
            assert!(!["best", "better", "faster", "smarter", "stronger", "cheaper"].iter().any(|w| text.contains(w)), "{text}");
        }
        assert!(Input::new("p", "fix it", &kinds(&["claude"])).is_none(), "nothing to choose between");
        assert!(Input::new("p", "   ", &kinds(&["claude", "codex"])).is_none(), "no text");
        assert_eq!(Input::new("p", "x", &kinds(&["claude", "bash", "codex"])).unwrap().kinds, ["claude", "codex"], "only known species");
    }

    #[test]
    fn reads_suggestions_and_gates_by_confidence() {
        let i = input("Port this to Codex");
        let s = i.read(&answer("codex", 0.7)).unwrap();
        assert_eq!((s.kind.as_str(), s.gate, s.line().as_deref()), ("codex", Gate::Suggest, Some("Jev suggests codex · 80%")));
        assert_eq!(i.read(&answer("codex", 0.9)).unwrap().gate, Gate::Confident);
        assert_eq!(i.read(&answer("codex", 0.3)).unwrap().line(), None, "below 0.5 nothing is shown");
        assert_eq!(Input::new("p", "x", &kinds(&["claude", "grok"])).unwrap().read(&answer("codex", 0.9)), None, "a species that isn't installed");
        assert_eq!(i.read(&json!({ "answers": { "species": { "type": "choice", "choice": "codex" } } })), None, "missing fields");
        assert_eq!(i.read(&json!({})), None);
    }

    #[test]
    fn waits_a_second_for_the_text_to_settle_then_asks_once() {
        let mut r = route();
        let t0 = Instant::now();
        let (tx, rx) = channel();
        let mut rx = Some(rx);
        let mut asked = 0;
        let i = input("Fix the build");
        assert_eq!(r.step(t0, true, Some(&i), |_| unreachable!()), None);
        assert_eq!(r.step(t0 + Duration::from_millis(999), true, Some(&i), |_| unreachable!()), None, "not yet");
        tx.send(Ok(Suggestion { kind: "codex".into(), probability: 0.8, confidence: 0.7, gate: Gate::Suggest })).unwrap();
        let pick = r.step(t0 + DEBOUNCE, true, Some(&i), |_| {
            asked += 1;
            rx.take().unwrap()
        });
        assert_eq!((asked, pick), (1, Some(1)), "asked, and codex (index 1) is preselected");
        assert_eq!(r.suggestion.as_ref().map(|s| s.kind.as_str()), Some("codex"));
        assert_eq!(r.step(t0 + DEBOUNCE * 2, true, Some(&i), |_| unreachable!()), None, "once per text");
    }

    #[test]
    fn typing_again_restarts_the_clock_and_drops_the_old_answer() {
        let mut r = route();
        let t0 = Instant::now();
        let (_tx, rx) = channel();
        let (a, b) = (input("Fix the"), input("Fix the build"));
        r.step(t0, true, Some(&a), |_| unreachable!());
        r.step(t0 + DEBOUNCE, true, Some(&a), |_| rx);
        r.suggestion = Some(Suggestion { kind: "codex".into(), probability: 0.8, confidence: 0.7, gate: Gate::Suggest });
        r.step(t0 + DEBOUNCE + Duration::from_millis(10), true, Some(&b), |_| unreachable!());
        assert!(r.suggestion.is_none() && r.rx.is_none());
        assert_eq!(r.step(t0 + DEBOUNCE + Duration::from_millis(500), true, Some(&b), |_| unreachable!()), None, "only 500 ms since the last edit");
    }

    #[test]
    fn a_hand_picked_species_is_never_overridden() {
        let mut r = route();
        r.touched = true;
        let t0 = Instant::now();
        let (tx, rx) = channel();
        tx.send(Ok(Suggestion { kind: "grok".into(), probability: 0.9, confidence: 0.9, gate: Gate::Confident })).unwrap();
        let i = input("x");
        r.step(t0, true, Some(&i), |_| unreachable!());
        assert_eq!(r.step(t0 + DEBOUNCE, true, Some(&i), |_| rx), None);
        assert!(r.suggestion.is_some(), "still shown, just not applied");
    }

    #[test]
    fn errors_low_confidence_and_a_closed_composer_leave_things_alone() {
        let t0 = Instant::now();
        let i = input("x");
        for outcome in [Err(Error::Busy { retry_after: None }), Err(Error::Malformed), Ok(Suggestion { kind: "grok".into(), probability: 0.4, confidence: 0.4, gate: Gate::Ignore })] {
            let mut r = route();
            let (tx, rx) = channel();
            tx.send(outcome).unwrap();
            r.step(t0, true, Some(&i), |_| unreachable!());
            assert_eq!(r.step(t0 + DEBOUNCE, true, Some(&i), |_| rx), None);
            assert!(r.suggestion.is_none());
        }
        // a dropped thread (no answer ever) is the same as no answer
        let mut r = route();
        r.step(t0, true, Some(&i), |_| unreachable!());
        assert_eq!(r.step(t0 + DEBOUNCE, true, Some(&i), |_| channel().1), None);
        // leaving the composer clears everything
        r.suggestion = Some(Suggestion { kind: "grok".into(), probability: 0.9, confidence: 0.9, gate: Gate::Confident });
        r.touched = true;
        r.step(t0, false, None, |_| unreachable!());
        assert!(r.suggestion.is_none() && !r.touched && r.enabled);
    }

    #[test]
    fn clearing_the_text_keeps_a_hand_picked_species() {
        let mut r = route();
        r.touched = true;
        r.suggestion = Some(Suggestion { kind: "grok".into(), probability: 0.9, confidence: 0.9, gate: Gate::Confident });
        r.step(Instant::now(), true, None, |_| unreachable!());
        assert!(r.touched && r.suggestion.is_none());
    }

    #[test]
    fn off_means_off() {
        let mut r = Route { enabled: false, ..Route::new() };
        let t0 = Instant::now();
        let i = input("x");
        assert_eq!(r.step(t0, true, Some(&i), |_| unreachable!()), None);
        assert_eq!(r.step(t0 + DEBOUNCE * 5, true, Some(&i), |_| unreachable!()), None);
        assert!(r.suggestion.is_none());
    }
}
