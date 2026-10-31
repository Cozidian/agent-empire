//! Ship roles: what a ship is for, and what it's allowed to do. A role is a standing order
//! the ship carries for its whole life, and for a scout a read-only launch where the
//! species supports one.

use crate::gfx::{V3, v3};

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

    /// Launch flags for a species: Claude and Grok take the standing order beside the mission
    /// (a system prompt, or `--rules`), and a scout runs read-only where the species can enforce it.
    pub fn args(self, kind: &str) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        match (kind, self) {
            (_, Role::None) => {}
            ("claude", r) => {
                if r == Role::Scout {
                    v.extend(["--permission-mode".into(), "plan".into()]);
                }
                v.extend(["--append-system-prompt".into(), r.orders().into()]);
            }
            ("grok", r) => {
                if r == Role::Scout {
                    // plan mode refuses edits outside the plan file; the sandbox refuses the rest
                    v.extend(["--permission-mode".into(), "plan".into(), "--sandbox".into(), "read-only".into()]);
                }
                v.extend(["--rules".into(), r.orders().into()]);
            }
            ("codex", Role::Scout) => v.extend(["--sandbox".into(), "read-only".into()]),
            _ => {}
        }
        v
    }

    /// The opening prompt: species without a system-prompt flag get the standing order first.
    pub fn prompt(self, kind: &str, text: &str) -> String {
        if self == Role::None || matches!(kind, "claude" | "grok") {
            text.to_string()
        } else {
            format!("{}\n\nYour mission: {text}", self.orders())
        }
    }

    /// Whether launching this species read-only is enforced, not just asked for.
    pub fn enforced(self, kind: &str) -> bool {
        self == Role::Scout && matches!(kind, "claude" | "codex" | "grok")
    }
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
}
