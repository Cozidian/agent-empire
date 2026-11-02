//! Ship roles: what a ship is for, and what it's allowed to do. A role is a standing order
//! the ship carries for its whole life, and for a scout a read-only launch where the
//! species supports one.

use crate::gfx::{V3, v3};
use crate::proto::ShipSpec;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Role {
    #[default]
    None,
    Scout,
    Builder,
    Inspector,
}

/// In the order the mission composer offers them.
pub const ROLES: [Role; 4] = [Role::None, Role::Scout, Role::Builder, Role::Inspector];

impl Role {
    /// As stored and sent over the wire ("" for none).
    pub fn name(self) -> &'static str {
        match self {
            Role::None => "",
            Role::Scout => "scout",
            Role::Builder => "builder",
            Role::Inspector => "inspector",
        }
    }

    pub fn parse(s: &str) -> Self {
        ROLES.into_iter().find(|r| r.name() == s).unwrap_or_default()
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::None => "none",
            r => r.name(),
        }
    }

    /// One line for the composer.
    pub fn about(self) -> &'static str {
        match self {
            Role::None => "a generalist: does whatever the mission asks",
            Role::Scout => "investigates and reports a plan; read-only, changes nothing",
            Role::Builder => "implements, runs the tests and commits on its world; never pushes",
            Role::Inspector => "runs the tests and reviews recent changes; fixes only what's broken",
        }
    }

    pub fn color(self) -> V3 {
        match self {
            Role::None => v3(0.45, 0.52, 0.62),
            Role::Scout => v3(0.45, 0.95, 0.8),
            Role::Builder => v3(1.0, 0.7, 0.35),
            Role::Inspector => v3(0.78, 0.62, 1.0),
        }
    }

    /// The standing order, given before any mission.
    pub fn orders(self) -> &'static str {
        match self {
            Role::None => "",
            Role::Scout => "You are a scout. Investigate and report; do not change anything. Don't edit, create or delete files, don't commit, and run only commands that read. End with what you found and a concrete plan, most important first, so a builder can carry it out.",
            Role::Builder => "You are a builder. Implement what you're asked, keeping changes focused on it. Run the tests and fix what you broke, then commit your work on the current branch with clear messages. Never push, and don't switch this checkout's branch.",
            Role::Inspector => "You are an inspector. Check the work on this world: build it, run the tests, and review the recent changes (uncommitted ones and the latest commits) for bugs and risks. Report problems, most important first. Fix only clear breakage; don't add features or refactor. Never push.",
        }
    }

    /// Everything a ship is told for life: its role's order, then a design's own orders,
    /// then the skills it should lean on.
    pub fn standing(self, spec: &ShipSpec) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self != Role::None { parts.push(self.orders().into()); }
        if !spec.orders.trim().is_empty() { parts.push(spec.orders.trim().into()); }
        if !spec.skills.is_empty() { parts.push(format!("Skills to use when they fit the task: {}.", spec.skills.join(", "))); }
        parts.join("\n\n")
    }

    /// Launch flags for a species: Claude and Grok take the standing order beside the mission
    /// (a system prompt, or `--rules`), and a scout runs read-only where the species can enforce it.
    #[cfg(test)]
    pub fn args(self, kind: &str) -> Vec<String> { self.launch_args(kind, &ShipSpec::default()) }

    /// Launch flags for a ship with its own outfit. Codex takes a design's orders as developer
    /// instructions; with only a role it still gets the order in front of the mission, as before.
    pub fn launch_args(self, kind: &str, spec: &ShipSpec) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        if self == Role::Scout {
            match kind {
                "claude" => v.extend(["--permission-mode".into(), "plan".into()]),
                // plan mode refuses edits outside the plan file; the sandbox refuses the rest
                "grok" => v.extend(["--permission-mode".into(), "plan".into(), "--sandbox".into(), "read-only".into()]),
                "codex" => v.extend(["--sandbox".into(), "read-only".into()]),
                _ => {}
            }
        }
        let standing = self.standing(spec);
        if !standing.is_empty() {
            match kind {
                "claude" => v.extend(["--append-system-prompt".into(), standing]),
                "grok" => v.extend(["--rules".into(), standing]),
                "codex" if !spec.is_bare() => v.extend(["-c".into(), format!("developer_instructions={}", toml_string(&standing))]),
                _ => {}
            }
        }
        v
    }

    /// The opening prompt: species without a system-prompt flag get the standing order first.
    #[cfg(test)]
    pub fn prompt(self, kind: &str, text: &str) -> String { self.opening(kind, &ShipSpec::default(), text) }

    pub fn opening(self, kind: &str, spec: &ShipSpec, text: &str) -> String {
        let standing = self.standing(spec);
        let carried = matches!(kind, "claude" | "grok") || (kind == "codex" && !spec.is_bare());
        if standing.is_empty() || carried { text.to_string() } else { format!("{standing}\n\nYour mission: {text}") }
    }

    /// Whether launching this species read-only is enforced, not just asked for.
    #[cfg(test)]
    pub fn enforced(self, kind: &str) -> bool {
        self == Role::Scout && matches!(kind, "claude" | "codex" | "grok")
    }
}

/// A TOML basic string, for a `-c key=value` override that must stay text whatever it says.
fn toml_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip_and_only_scouts_are_read_only() {
        for r in ROLES {
            assert_eq!(Role::parse(r.name()), r);
        }
        assert_eq!(Role::parse("admiral"), Role::None);
        assert!(Role::Scout.args("claude").windows(2).any(|w| w == ["--permission-mode", "plan"]));
        assert!(Role::Scout.args("codex").windows(2).any(|w| w == ["--sandbox", "read-only"]));
        assert!(Role::Scout.args("grok").windows(2).any(|w| w == ["--permission-mode", "plan"]));
        assert!(Role::Scout.args("grok").windows(2).any(|w| w == ["--sandbox", "read-only"]));
        assert!(Role::Scout.enforced("grok"));
        assert!(!Role::Builder.args("claude").iter().any(|a| a == "plan"));
        assert!(!Role::Builder.args("grok").iter().any(|a| a == "plan" || a == "read-only"));
        assert!(Role::Builder.args("grok").windows(2).any(|w| w[0] == "--rules"));
        assert!(Role::None.args("claude").is_empty());
        // Claude and Grok carry the order beside the mission; the rest get it up front
        assert_eq!(Role::Builder.prompt("claude", "Fix it"), "Fix it");
        assert_eq!(Role::Builder.prompt("grok", "Fix it"), "Fix it");
        assert!(Role::Builder.prompt("codex", "Fix it").ends_with("Your mission: Fix it"));
    }

    #[test]
    fn a_design_outfit_rides_with_each_species() {
        let spec = ShipSpec { orders: "Say \"done\" when done.\nKeep diffs small.".into(), skills: vec!["release".into(), "migrate".into()], design: "Frigate".into() };
        let claude = Role::Builder.launch_args("claude", &spec);
        assert_eq!(claude.iter().filter(|a| *a == "--append-system-prompt").count(), 1);
        let order = &claude[claude.len() - 1];
        assert!(order.starts_with("You are a builder.") && order.contains("Keep diffs small.") && order.ends_with("Skills to use when they fit the task: release, migrate."));
        assert!(Role::None.launch_args("grok", &spec).windows(2).any(|w| w[0] == "--rules" && w[1].starts_with("Say")));
        // codex: developer instructions, quoted as TOML so they can't be read as anything else
        let codex = Role::Scout.launch_args("codex", &spec);
        assert!(codex.windows(2).any(|w| w == ["--sandbox", "read-only"]));
        assert!(codex.windows(2).any(|w| w[0] == "-c" && w[1].starts_with("developer_instructions=\"You are a scout.") && w[1].contains("Say \\\"done\\\" when done.\\nKeep") && w[1].ends_with("migrate.\"")));
        assert_eq!(Role::Scout.opening("codex", &spec, "Fix it"), "Fix it");
        // species without a flag get it all in front of the mission
        assert!(Role::None.launch_args("cursor", &spec).is_empty());
        assert!(Role::None.opening("cursor", &spec, "Fix it").starts_with("Say \"done\"") && Role::None.opening("cursor", &spec, "Fix it").ends_with("Your mission: Fix it"));
        // no outfit: exactly the old launch
        for kind in ["claude", "codex", "grok", "cursor"] {
            for r in ROLES {
                assert_eq!(r.launch_args(kind, &ShipSpec::default()), r.args(kind));
                assert_eq!(r.opening(kind, &ShipSpec::default(), "Fix it"), r.prompt(kind, "Fix it"));
            }
        }
        assert!(!Role::Builder.args("codex").iter().any(|a| a == "-c"));
        assert!(Role::Builder.prompt("codex", "Fix it").starts_with("You are a builder."));
    }
}
