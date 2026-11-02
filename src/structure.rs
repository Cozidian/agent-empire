//! Structures: what a world has built for the agents that work on it. Every AI resource in a
//! folder (AGENTS.md, CLAUDE.md, skills, subagents, commands, MCP servers, hooks, other
//! species' rules) is a building on its planet. The survey finds them; a construction order
//! is a mission to build one, or to renovate one that's already standing; or you found one
//! yourself from a first draft and write it in your own editor.

use crate::gfx::{V3, v3};
use std::io::{self, Write};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Kind {
    Charter,
    Archive,
    Academy,
    Barracks,
    Command,
    Relay,
    Watchtower,
    Embassy,
}

/// In the order the construction menu offers them.
pub const KINDS: [Kind; 8] = [Kind::Charter, Kind::Archive, Kind::Academy, Kind::Barracks, Kind::Command, Kind::Relay, Kind::Watchtower, Kind::Embassy];

/// A structure standing on a world: how many of it, and the files it's made of.
#[derive(Clone, Debug, PartialEq)]
pub struct Built {
    pub kind: Kind,
    pub count: u32,
    /// Relative to the world, in browsing order.
    pub files: Vec<String>,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Charter => "charter hall",
            Kind::Archive => "archive",
            Kind::Academy => "academy",
            Kind::Barracks => "barracks",
            Kind::Command => "command post",
            Kind::Relay => "relay",
            Kind::Watchtower => "watchtower",
            Kind::Embassy => "embassy",
        }
    }

    /// What it is, in the agents' own terms.
    pub fn what(self) -> &'static str {
        match self {
            Kind::Charter => "AGENTS.md",
            Kind::Archive => "CLAUDE.md",
            Kind::Academy => "skills",
            Kind::Barracks => "subagents",
            Kind::Command => "slash commands",
            Kind::Relay => "MCP servers",
            Kind::Watchtower => "hooks",
            Kind::Embassy => "other agents' rules",
        }
    }

    /// One line for the construction menu.
    pub fn about(self) -> &'static str {
        match self {
            Kind::Charter => "instructions every species reads: build, test, conventions",
            Kind::Archive => "Claude's memory of this project",
            Kind::Academy => "SKILL.md packages agents load when a task calls for them",
            Kind::Barracks => "specialist agents a ship can hand work to",
            Kind::Command => "reusable prompts, one slash away",
            Kind::Relay => "tools and data reached through the Model Context Protocol",
            Kind::Watchtower => "commands that run on agent events: format, lint, guard",
            Kind::Embassy => "GEMINI.md, Copilot, Cursor and Windsurf rules",
        }
    }

    pub fn glyph(self) -> char {
        match self {
            Kind::Charter => '⌂',
            Kind::Archive => '▤',
            Kind::Academy => '✧',
            Kind::Barracks => '⚑',
            Kind::Command => '⌘',
            Kind::Relay => '⌁',
            Kind::Watchtower => '⚲',
            Kind::Embassy => '⚐',
        }
    }

    pub fn color(self) -> V3 {
        match self {
            Kind::Charter => v3(1.0, 0.85, 0.45),
            Kind::Archive => v3(0.95, 0.6, 0.4),
            Kind::Academy => v3(0.55, 0.9, 1.0),
            Kind::Barracks => v3(1.0, 0.55, 0.55),
            Kind::Command => v3(0.7, 0.75, 1.0),
            Kind::Relay => v3(0.5, 1.0, 0.7),
            Kind::Watchtower => v3(0.85, 0.65, 1.0),
            Kind::Embassy => v3(0.8, 0.85, 0.7),
        }
    }

    /// The construction order: build it from scratch, or (when it stands) renovate it.
    pub fn brief(self, built: Option<&Built>) -> String {
        let here = built.map(|b| b.files.join(", ")).unwrap_or_default();
        match (self, built.is_some()) {
            (Kind::Charter, false) => "Write an AGENTS.md at the root of this repo for coding agents. Study the project first, then cover: what it is and how it's laid out, how to build, run and test it (exact commands), code style and conventions you can see in the code, and anything an agent must not do. Keep it short and specific to this repo; no generic advice. Commit it.".into(),
            (Kind::Archive, false) => "Write a CLAUDE.md at the root of this repo for Claude Code. If there's an AGENTS.md, keep CLAUDE.md short and point to it (`@AGENTS.md`), adding only what's Claude-specific. Otherwise study the project and cover how to build, run and test it, the layout, and its conventions. Keep it short and specific. Commit it.".into(),
            (Kind::Academy, false) => "Find one task in this repo that agents will do again and again and that needs project knowledge (a release, adding a migration, a recurring kind of change) and write a skill for it: .claude/skills/<name>/SKILL.md with `name` and `description` frontmatter (the description says when to use it), then the steps, with exact commands and files. Tell me which task you picked and why. Commit it.".into(),
            (Kind::Barracks, false) => "Write a subagent for this repo in .claude/agents/<name>.md (frontmatter: name, description saying when to use it, tools it needs), for a focused job that keeps the main agent's context clean, such as a reviewer that knows this codebase's pitfalls or a test runner that reports only failures. Base its prompt on what you learn from the code. Commit it.".into(),
            (Kind::Command, false) => "Write slash commands for workflows people repeat in this repo, as .claude/commands/<name>.md files (a short description in frontmatter, then the prompt; use $ARGUMENTS where it takes input). Pick two or three that would actually get used, from the scripts, docs and history. Commit them.".into(),
            (Kind::Relay, false) => "Look at what this project talks to (databases, issue trackers, cloud services, docs) and propose MCP servers that would help agents working on it. Add the ones that need no secrets to a project .mcp.json, with secrets referenced from environment variables, never written in. Explain what each one is for and what I need to set up. Commit .mcp.json.".into(),
            (Kind::Watchtower, false) => "Add Claude Code hooks for this repo in .claude/settings.json: for example a PostToolUse hook that runs the project's formatter on edited files, and a guard for anything that must never be touched. Use the tools the project already has; keep hooks fast. Merge with any settings already there. Commit it.".into(),
            (Kind::Embassy, false) => "Agents other than Claude and Codex read their own instruction files. If there's an AGENTS.md, add short pointers to it for Gemini (GEMINI.md) and GitHub Copilot (.github/copilot-instructions.md); otherwise write those from what you learn about the project. Don't duplicate content that can be referenced. Commit them.".into(),
            (k, true) => format!("Renovate this repo's {} ({here}). Read them against the code as it is now: fix what's out of date or wrong, fill gaps agents keep running into, cut what's generic or redundant. Keep them short. Commit the changes.", k.what()),
        }
    }
}

impl Kind {
    /// Where a hand-written one goes, and its first draft: what to fill in, nothing more.
    /// Kinds that hold many (skills, subagents, commands) get a fresh name each time.
    fn draft(self, root: &Path) -> (String, String) {
        let fresh = |dir: &str, stem: &str, file: &str| {
            (1..).map(|n| if n == 1 { stem.to_string() } else { format!("{stem}-{n}") })
                .find(|name| !root.join(dir).join(format!("{name}{file}")).exists() && !root.join(dir).join(name).exists())
                .map(|name| (format!("{dir}/{name}{file}"), name))
                .unwrap()
        };
        let agents = root.join("AGENTS.md").exists();
        match self {
            Kind::Charter => ("AGENTS.md".into(), "# AGENTS.md\n\nWhat this project is and how it's laid out, for coding agents.\n\n## Build, run, test\n\n```\n\n```\n\n## Conventions\n\n- \n\n## Don'ts\n\n- \n".into()),
            Kind::Archive if agents => ("CLAUDE.md".into(), "@AGENTS.md\n\n<!-- Claude-specific notes below; everything else lives in AGENTS.md. -->\n".into()),
            Kind::Archive => ("CLAUDE.md".into(), "# CLAUDE.md\n\n## Build, run, test\n\n```\n\n```\n\n## Conventions\n\n- \n".into()),
            Kind::Academy => {
                let (path, name) = fresh(".claude/skills", "new-skill", "");
                (format!("{path}/SKILL.md"), format!("---\nname: {name}\ndescription: When to use this skill, in one sentence.\n---\n\n# {name}\n\n1. \n"))
            }
            Kind::Barracks => {
                let (path, name) = fresh(".claude/agents", "new-agent", ".md");
                (path, format!("---\nname: {name}\ndescription: When to hand work to this agent.\ntools: Read, Grep, Glob\n---\n\nWhat this agent does, and how.\n"))
            }
            Kind::Command => (fresh(".claude/commands", "new-command", ".md").0, "---\ndescription: What this command does.\n---\n\n$ARGUMENTS\n".into()),
            Kind::Relay => (".mcp.json".into(), "{\n  \"mcpServers\": {}\n}\n".into()),
            Kind::Watchtower => (".claude/settings.json".into(), "{\n  \"hooks\": {}\n}\n".into()),
            Kind::Embassy if agents => ("GEMINI.md".into(), "@AGENTS.md\n".into()),
            Kind::Embassy => ("GEMINI.md".into(), "# GEMINI.md\n\n".into()),
        }
    }

    /// Lay a new one on the world at `root` for you to write, returning the file to open.
    /// A file already standing there is opened as it is, never overwritten.
    pub fn found(self, root: &Path) -> io::Result<String> {
        let (path, text) = self.draft(root);
        let file = root.join(&path);
        if !file.exists() {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::OpenOptions::new().write(true).create_new(true).open(&file)?.write_all(text.as_bytes())?;
        }
        Ok(path)
    }
}

/// The editor to write in: `$VISUAL`, then `$EDITOR`, then the first of nvim, vim, vi or nano
/// that's installed. `None` when there's nothing to edit with.
pub fn editor(env: impl Fn(&str) -> Option<String>, installed: impl Fn(&str) -> bool) -> Option<Vec<String>> {
    for var in ["VISUAL", "EDITOR"] {
        let words: Vec<String> = env(var).unwrap_or_default().split_whitespace().map(String::from).collect();
        if words.first().is_some_and(|p| installed(p)) {
            return Some(words);
        }
    }
    ["nvim", "vim", "vi", "nano"].into_iter().find(|p| installed(p)).map(|p| vec![p.to_string()])
}

/// A program by name on `$PATH`, or by path.
pub fn installed(prog: &str) -> bool {
    if prog.contains('/') {
        return Path::new(prog).is_file();
    }
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(prog).is_file()))
}

/// Every structure standing on the world at `path`.
pub fn survey(path: &str) -> Vec<Built> {
    let root = Path::new(path);
    let mut out = Vec::new();
    let mut add = |kind: Kind, files: Vec<String>, count: u32| {
        if count > 0 {
            out.push(Built { kind, count, files });
        }
    };
    let charter = existing(root, &["AGENTS.md", "AGENTS.override.md"]);
    add(Kind::Charter, charter.clone(), charter.len() as u32);
    let archive = existing(root, &["CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"]);
    add(Kind::Archive, archive.clone(), archive.len() as u32);
    // a skill is a folder with a SKILL.md
    let skills: Vec<String> = [".claude/skills", ".agents/skills", ".codex/skills", ".grok/skills"]
        .iter()
        .flat_map(|d| entries(root, d).into_iter().filter(|e| root.join(e).join("SKILL.md").is_file()))
        .collect();
    add(Kind::Academy, skills.clone(), skills.len() as u32);
    let agents = markdown(root, ".claude/agents");
    add(Kind::Barracks, agents.clone(), agents.len() as u32);
    let mut commands = markdown(root, ".claude/commands");
    commands.extend(markdown(root, ".codex/prompts"));
    add(Kind::Command, commands.clone(), commands.len() as u32);
    let relay: Vec<(String, u32)> = [".mcp.json", ".cursor/mcp.json", ".vscode/mcp.json"]
        .iter()
        .filter_map(|f| {
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join(f)).ok()?).ok()?;
            let n = ["mcpServers", "servers"].iter().find_map(|k| v.get(k)?.as_object().map(|o| o.len() as u32))?;
            Some((f.to_string(), n))
        })
        .collect();
    add(Kind::Relay, relay.iter().map(|r| r.0.clone()).collect(), relay.iter().map(|r| r.1).sum());
    let hooks: Vec<(String, u32)> = [".claude/settings.json", ".claude/settings.local.json"]
        .iter()
        .filter_map(|f| {
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join(f)).ok()?).ok()?;
            let n = v.get("hooks")?.as_object()?.values().filter_map(|e| e.as_array()).map(|a| a.len() as u32).sum();
            (n > 0).then(|| (f.to_string(), n))
        })
        .collect();
    add(Kind::Watchtower, hooks.iter().map(|h| h.0.clone()).collect(), hooks.iter().map(|h| h.1).sum());
    let mut embassy = existing(root, &["GEMINI.md", ".github/copilot-instructions.md", ".cursorrules", ".windsurfrules", ".clinerules"]);
    embassy.extend(entries(root, ".cursor/rules"));
    embassy.extend(entries(root, ".github/instructions"));
    add(Kind::Embassy, embassy.clone(), embassy.len() as u32);
    out
}

fn existing(root: &Path, files: &[&str]) -> Vec<String> {
    files.iter().filter(|f| root.join(f).exists()).map(|f| f.to_string()).collect()
}

/// What's in the folder `dir` under `root`, as paths relative to `root`, sorted.
fn entries(root: &Path, dir: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root.join(dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().filter(|n| !n.starts_with('.')).map(|n| format!("{dir}/{n}")))
        .collect();
    v.sort();
    v
}

/// The Markdown files under `dir`, one level of subfolders included (namespaced commands).
fn markdown(root: &Path, dir: &str) -> Vec<String> {
    let mut v = Vec::new();
    for e in entries(root, dir) {
        if root.join(&e).is_dir() {
            v.extend(entries(root, &e).into_iter().filter(|f| f.ends_with(".md")));
        } else if e.ends_with(".md") {
            v.push(e);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_skill_is_available_to_browse() {
        let dir = std::env::temp_dir().join(format!("empire-many-skills-{}", std::process::id()));
        for i in 0..12 {
            let p = dir.join(format!(".agents/skills/skill-{i:02}"));
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("SKILL.md"), format!("# Skill {i}")).unwrap();
        }
        let got = survey(dir.to_str().unwrap());
        let skills = got.iter().find(|b| b.kind == Kind::Academy).unwrap();
        assert_eq!(skills.files.len(), 12);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finds_every_kind_of_structure_and_counts_them() {
        let dir = std::env::temp_dir().join(format!("empire-structures-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mk = |f: &str, s: &str| {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        assert!(survey(dir.to_str().unwrap()).is_empty());
        mk("AGENTS.md", "# agents");
        mk(".claude/CLAUDE.md", "@AGENTS.md");
        mk(".claude/skills/release/SKILL.md", "---\nname: release\n---");
        mk(".claude/skills/not-a-skill/notes.txt", "");
        mk(".claude/agents/reviewer.md", "");
        mk(".claude/commands/ship.md", "");
        mk(".claude/commands/db/migrate.md", "");
        mk(".mcp.json", r#"{"mcpServers":{"a":{},"b":{}}}"#);
        mk(".claude/settings.json", r#"{"hooks":{"PostToolUse":[{},{}],"Stop":[{}]}}"#);
        mk(".cursor/rules/style.mdc", "");
        let got = survey(dir.to_str().unwrap());
        let count = |k: Kind| got.iter().find(|b| b.kind == k).map(|b| b.count);
        assert_eq!(count(Kind::Charter), Some(1));
        assert_eq!(count(Kind::Archive), Some(1));
        assert_eq!(count(Kind::Academy), Some(1));
        assert_eq!(count(Kind::Barracks), Some(1));
        assert_eq!(count(Kind::Command), Some(2));
        assert_eq!(count(Kind::Relay), Some(2));
        assert_eq!(count(Kind::Watchtower), Some(3));
        assert_eq!(count(Kind::Embassy), Some(1));
        assert!(got.iter().find(|b| b.kind == Kind::Command).unwrap().files.contains(&".claude/commands/db/migrate.md".to_string()));
        // hooks without entries, or settings without hooks, aren't a watchtower
        mk(".claude/settings.json", r#"{"hooks":{}}"#);
        assert_eq!(survey(dir.to_str().unwrap()).iter().find(|b| b.kind == Kind::Watchtower), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn founding_lays_a_draft_once_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("empire-found-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Kind::Charter.found(&dir).unwrap(), "AGENTS.md");
        assert!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap().contains("## Build, run, test"));
        std::fs::write(dir.join("AGENTS.md"), "mine").unwrap();
        assert_eq!(Kind::Charter.found(&dir).unwrap(), "AGENTS.md");
        assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), "mine", "an existing file is opened, not replaced");
        // with an AGENTS.md standing, CLAUDE.md just points to it
        Kind::Archive.found(&dir).unwrap();
        assert!(std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap().starts_with("@AGENTS.md"));
        // kinds that hold many get a fresh name each time, and the survey sees them
        assert_eq!(Kind::Academy.found(&dir).unwrap(), ".claude/skills/new-skill/SKILL.md");
        assert_eq!(Kind::Academy.found(&dir).unwrap(), ".claude/skills/new-skill-2/SKILL.md");
        assert_eq!(Kind::Command.found(&dir).unwrap(), ".claude/commands/new-command.md");
        let got = survey(dir.to_str().unwrap());
        assert_eq!(got.iter().find(|b| b.kind == Kind::Academy).map(|b| b.count), Some(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_editor_is_yours_first_then_whatever_is_installed() {
        let env = |vars: &'static [(&'static str, &'static str)]| move |k: &str| vars.iter().find(|v| v.0 == k).map(|v| v.1.to_string());
        let all = |_: &str| true;
        assert_eq!(editor(env(&[("VISUAL", "hx"), ("EDITOR", "vim")]), all), Some(vec!["hx".into()]));
        assert_eq!(editor(env(&[("EDITOR", "code --wait")]), all), Some(vec!["code".into(), "--wait".into()]));
        // a configured editor that isn't installed falls through to what is
        assert_eq!(editor(env(&[("EDITOR", "gone")]), |p| p == "vi"), Some(vec!["vi".into()]));
        assert_eq!(editor(env(&[]), |p| p == "nvim" || p == "nano"), Some(vec!["nvim".into()]));
        assert_eq!(editor(env(&[]), |_| false), None);
    }

    #[test]
    fn briefs_build_or_renovate() {
        assert!(Kind::Charter.brief(None).contains("AGENTS.md"));
        let b = Built { kind: Kind::Academy, count: 1, files: vec![".claude/skills/release".into()] };
        let r = Kind::Academy.brief(Some(&b));
        assert!(r.starts_with("Renovate") && r.contains(".claude/skills/release"));
    }
}
