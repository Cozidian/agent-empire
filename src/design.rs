//! Ship designs: a saved outfit for new ships (species, role, standing orders, skills),
//! kept in the shipyard and picked in the mission composer. A ship launched from one keeps
//! its own copy, so renaming or scrapping a design never touches ships already flying.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Design {
    pub id: u64,
    pub name: String,
    /// The species it prefers; the composer can launch it as another.
    pub kind: String,
    /// See `role::Role`, "" for none.
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub orders: String,
    #[serde(default)]
    pub skills: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    #[serde(default)]
    next: u64,
    pub designs: Vec<Design>,
}

fn check_name(name: &str) -> Result<String, String> {
    if name.chars().any(char::is_control) { return Err("Use a single-line design name".into()); }
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 32 { return Err("A design name needs 1–32 characters".into()); }
    Ok(name.into())
}

impl Book {
    pub fn get(&self, id: u64) -> Option<&Design> { self.designs.iter().find(|d| d.id == id) }

    /// Save a design: a new one, or (by `d.id`, or a name already in the book) over an old one.
    pub fn save(&mut self, mut d: Design) -> Result<u64, String> {
        d.name = check_name(&d.name)?;
        if d.kind.trim().is_empty() { return Err("A design needs a species".into()); }
        d.orders = d.orders.trim().into();
        d.skills.retain(|s| !s.trim().is_empty());
        d.skills.dedup();
        let same = self.designs.iter().find(|o| o.name.to_lowercase() == d.name.to_lowercase()).map(|o| o.id);
        if d.id == 0 { d.id = same.unwrap_or(0); }
        if same.is_some_and(|id| id != d.id) { return Err(format!("Another design is already called {}", d.name)); }
        if d.id == 0 {
            d.id = self.next.max(self.designs.iter().map(|o| o.id).max().unwrap_or(0)).checked_add(1).ok_or("No design ids left")?;
            self.next = d.id;
        }
        let id = d.id;
        match self.designs.iter_mut().find(|o| o.id == id) {
            Some(old) => *old = d,
            None => self.designs.push(d),
        }
        Ok(id)
    }

    pub fn rename(&mut self, id: u64, name: &str) -> Result<u64, String> {
        let mut d = self.get(id).cloned().ok_or("This design is no longer in the shipyard")?;
        d.name = name.into();
        self.save(d)
    }

    pub fn remove(&mut self, id: u64) -> Result<u64, String> {
        if self.get(id).is_none() { return Err("This design is no longer in the shipyard".into()); }
        self.designs.retain(|d| d.id != id);
        Ok(id)
    }
}

pub fn path() -> PathBuf { crate::daemon::dirs_state().join("designs.json") }
pub fn edit(path: &Path, change: impl FnOnce(&mut Book)) -> std::io::Result<Book> { crate::state::edit(path, change) }

/// Where a species looks for skills: folders in the world (and up to its repo root), and under home.
fn skill_dirs(kind: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match kind {
        "claude" => (&[".claude/skills"], &[".claude/skills"]),
        "codex" => (&[".agents/skills"], &[".agents/skills", ".codex/skills"]),
        // grok also reads Claude's skills, for compatibility
        "grok" => (&[".grok/skills", ".agents/skills", ".claude/skills"], &[".grok/skills", ".agents/skills", ".claude/skills"]),
        _ => (&[], &[]),
    }
}

/// The world's folder and its parents up to the repo root (the first with a `.git`).
fn world_roots(world: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for p in world.ancestors() {
        v.push(p.to_path_buf());
        if p.join(".git").exists() { return v; }
    }
    v.truncate(1); // not a repo: just the world itself
    v
}

/// Whether a species launched on `world` can find the skill called `name`.
pub fn reachable(kind: &str, name: &str, world: &Path, home: &Path) -> bool {
    let (repo, user) = skill_dirs(kind);
    let here = world_roots(world).into_iter().flat_map(|r| repo.iter().map(move |d| r.join(d)));
    let mine = user.iter().map(|d| home.join(d));
    here.chain(mine).any(|d| d.join(name).join("SKILL.md").is_file())
}

/// Every skill any species could find from `world`, by name, sorted.
pub fn available(world: &Path, home: &Path) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for kind in ["claude", "codex", "grok"] {
        let (repo, user) = skill_dirs(kind);
        let dirs = world_roots(world).into_iter().flat_map(|r| repo.iter().map(move |d| r.join(d))).chain(user.iter().map(|d| home.join(d)));
        for dir in dirs {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if e.path().join("SKILL.md").is_file() && let Some(n) = e.file_name().to_str() { v.push(n.to_string()); }
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(name: &str) -> Design { Design { name: name.into(), kind: "claude".into(), role: "builder".into(), orders: " Keep it small. ".into(), skills: vec!["release".into()], ..Default::default() } }

    #[test]
    fn designs_are_named_renamed_and_scrapped() {
        let mut b = Book::default();
        let a = b.save(d("Frigate")).unwrap();
        let c = b.save(d("Corvette")).unwrap();
        assert_ne!(a, c);
        assert_eq!(b.get(a).unwrap().orders, "Keep it small.");
        // saving under a name already in the book updates that design
        assert_eq!(b.save(Design { orders: "Bigger.".into(), ..d("frigate") }).unwrap(), a);
        assert_eq!(b.designs.len(), 2);
        assert_eq!(b.get(a).unwrap().orders, "Bigger.");
        assert!(b.rename(c, "FRIGATE").is_err());
        assert!(b.save(d("  ")).is_err());
        assert!(b.save(d("two\nlines")).is_err());
        assert!(b.save(Design { kind: String::new(), ..d("Ghost") }).is_err());
        b.rename(a, "Heavy frigate").unwrap();
        assert_eq!(b.get(a).unwrap().name, "Heavy frigate");
        b.remove(a).unwrap();
        assert!(b.remove(a).is_err());
        // ids are never reused
        assert!(b.save(d("New")).unwrap() > a.max(c));
    }

    #[test]
    fn designs_persist_in_their_book() {
        let dir = std::env::temp_dir().join(format!("empire-designs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("designs.json");
        let mut id = 0;
        edit(&path, |b| id = b.save(d("Scout ship")).unwrap()).unwrap();
        let back = edit(&path, |_| {}).unwrap();
        assert_eq!(back.get(id).unwrap().skills, ["release"]);
        assert_eq!(back.get(id).unwrap().role, "builder");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_species_finds_only_the_skills_in_its_own_folders() {
        let root = std::env::temp_dir().join(format!("empire-design-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, repo) = (root.join("home"), root.join("repo"));
        let world = repo.join("app");
        let mk = |p: PathBuf| { std::fs::create_dir_all(&p).unwrap(); std::fs::write(p.join("SKILL.md"), "---\nname: x\n---").unwrap(); };
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        mk(repo.join(".claude/skills/release")); // at the repo root, above the world
        mk(world.join(".agents/skills/migrate"));
        mk(home.join(".codex/skills/review"));
        std::fs::create_dir_all(world.join(".claude/skills/notes")).unwrap(); // no SKILL.md
        assert!(reachable("claude", "release", &world, &home));
        assert!(!reachable("codex", "release", &world, &home));
        assert!(reachable("grok", "release", &world, &home));
        assert!(reachable("codex", "migrate", &world, &home) && reachable("grok", "migrate", &world, &home));
        assert!(!reachable("claude", "migrate", &world, &home));
        assert!(reachable("codex", "review", &world, &home) && !reachable("grok", "review", &world, &home));
        assert!(!reachable("cursor", "release", &world, &home));
        assert_eq!(available(&world, &home), ["migrate", "release", "review"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
